use gents_protocol::file_image_hydration::FileImageScope;
use gents_protocol::file_inventory::{
    FileInventoryAdmission, FileInventoryEntry, FileInventoryOrdinal,
};
use gents_protocol::output::{MessageBlock, MessagePublication, MessageRole};
use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use serde::Serialize;

use crate::tool_call_lifecycle::ToolCallState;

use super::HydrationSelection;

#[cfg(test)]
mod tests;

/// Read-only facts from the existing physical acceptance and terminal owners.
/// This projection neither admits a tool nor changes its execution state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Admission {
    pub document: String,
    pub request: String,
    pub scope: FileImageScope,
    pub state: ToolCallState,
    pub accepted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct Window {
    pub tool: String,
    pub admitted: u32,
    pub delivered: u32,
}

#[derive(Default)]
pub(super) struct Publications<'a> {
    tools: RapidHashMap<&'a str, Vec<&'a Admission>>,
    calls: RapidHashMap<(String, String), Option<u32>>,
    deliveries: RapidHashMap<(String, String), Option<u32>>,
}

fn observe(
    positions: &mut RapidHashMap<(String, String), Option<u32>>,
    document: &str,
    request: &str,
    sequence: u32,
) {
    positions
        .entry((document.to_owned(), request.to_owned()))
        .and_modify(|prior| *prior = None)
        .or_insert(Some(sequence));
}

impl<'a> Publications<'a> {
    /// Origin traces are emitted by the successful canonical closure read;
    /// observing them adds no graph walk, payload read or image permission.
    pub fn new(
        expected: &FileInventoryAdmission,
        actual: &FileInventoryAdmission,
        selected: &HydrationSelection,
        tools: &'a [Admission],
    ) -> Self {
        if expected != actual {
            return Self::default();
        }
        let headers: RapidHashMap<_, _> = selected
            .headers
            .iter()
            .map(|row| (row.doc_id.as_str(), &row.message))
            .collect();
        let retained: RapidHashSet<_> = selected
            .documents
            .iter()
            .filter(|row| {
                row.collection
                    == gents_protocol::session_hydration::SessionHydrationCollection::AgentMessage
                    && row.session_id == expected.request.session_id
            })
            .map(|row| row.doc_id.as_str())
            .collect();
        let segments = selected
            .output_segments
            .iter()
            .map(
                |row| gents_protocol::output::reconstruction::ObservedSegment {
                    doc_id: &row.doc_id,
                    segment: &row.segment,
                },
            )
            .collect::<Vec<_>>();
        let mut result = Self::default();
        for tool in tools {
            result.tools.entry(&tool.document).or_default().push(tool);
        }
        for publication in &selected.publications {
            if !retained.contains(publication.header_doc_id.as_str()) {
                continue;
            }
            let Some(message) = headers.get(publication.origin_doc_id.as_str()) else {
                return Self::default();
            };
            let Some(request) = message.request_doc_id.as_deref() else {
                continue;
            };
            if message.role == MessageRole::Assistant
                && matches!(
                    message.publication,
                    MessagePublication::RequestExecution { .. }
                )
            {
                let mut seen = RapidHashSet::new();
                for block in &message.blocks {
                    if let MessageBlock::ToolCall {
                        tool_call_doc_id, ..
                    } = block
                    {
                        if seen.insert(tool_call_doc_id) {
                            observe(
                                &mut result.calls,
                                tool_call_doc_id,
                                request,
                                publication.sequence,
                            );
                        }
                    }
                }
            }
            if let (MessageRole::User, MessagePublication::ToolDelivery { tool_call_doc_id }) =
                (&message.role, &message.publication)
            {
                let Some(admitted) = result.tools.get(tool_call_doc_id.as_str()) else {
                    continue;
                };
                let references = message.payload_references();
                for tool in admitted {
                    let shape = match message.blocks.as_slice() {
                        [MessageBlock::ToolResult {
                            tool_call_doc_id: named,
                            ..
                        }] => named == tool_call_doc_id && request == &tool.request,
                        blocks => {
                            !blocks.is_empty()
                                && blocks
                                    .iter()
                                    .all(|block| matches!(block, MessageBlock::Text { .. }))
                        }
                    };
                    if shape
                        && references.iter().any(|reference| {
                            gents_protocol::output::reconstruction::tool_output_reference_owned(
                                &segments,
                                reference,
                                &tool.request,
                                tool_call_doc_id,
                            )
                        })
                    {
                        observe(
                            &mut result.deliveries,
                            tool_call_doc_id,
                            &tool.request,
                            publication.sequence,
                        );
                    }
                }
            }
        }
        result
    }

    pub fn window(&self, tool: &Admission) -> Option<Window> {
        if !tool.accepted
            || !tool.state.is_terminal()
            || !self.tools.get(tool.document.as_str())?.contains(&tool)
        {
            return None;
        }
        let key = (tool.document.clone(), tool.request.clone());
        let admitted = (*self.calls.get(&key)?)?;
        let delivered = (*self.deliveries.get(&key)?)?;
        (admitted < delivered).then(|| Window {
            tool: tool.document.clone(),
            admitted,
            delivered,
        })
    }
}

pub(super) fn resolve(windows: &[Window]) -> Option<Vec<&str>> {
    let mut tools = RapidHashSet::new();
    if windows
        .iter()
        .any(|window| window.admitted >= window.delivered || !tools.insert(window.tool.as_str()))
    {
        return None;
    }
    let mut ordered = windows.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|window| window.admitted);
    if ordered
        .windows(2)
        .any(|pair| pair[0].delivered >= pair[1].admitted)
    {
        return None;
    }
    Some(
        ordered
            .into_iter()
            .map(|window| window.tool.as_str())
            .collect(),
    )
}

pub(super) fn project(entries: &mut [FileInventoryEntry], observations: &[Option<Window>]) {
    let mut windows = RapidHashMap::new();
    for window in observations.iter().flatten() {
        windows
            .entry(window.tool.as_str())
            .and_modify(|prior| *prior = None)
            .or_insert(Some(window));
    }
    let mut resources = RapidHashMap::<_, Vec<usize>>::new();
    for (index, entry) in entries.iter_mut().enumerate() {
        entry.order = None;
        resources
            .entry((
                entry.scope.node_did.clone(),
                entry.change.root.clone(),
                entry.change.path.clone(),
            ))
            .or_default()
            .push(index);
    }
    for indices in resources.values() {
        let selected = indices
            .iter()
            .map(|index| {
                windows
                    .get(entries[*index].tool_call_doc_id.as_str())
                    .copied()
                    .flatten()
                    .cloned()
            })
            .collect::<Option<Vec<_>>>();
        let Some(selected) = selected else { continue };
        let Some(ordered) = resolve(&selected) else {
            continue;
        };
        let positions: RapidHashMap<_, _> = ordered
            .iter()
            .enumerate()
            .map(|(index, tool)| (*tool, index as u64))
            .collect();
        for index in indices {
            if let Some(position) = positions.get(entries[*index].tool_call_doc_id.as_str()) {
                entries[*index].order = Some(FileInventoryOrdinal {
                    index: *position,
                    entries: ordered.len() as u64,
                });
            }
        }
    }
}
