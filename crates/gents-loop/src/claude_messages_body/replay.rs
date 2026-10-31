//! Which assistant reasoning blocks a Claude replay keeps: the checkpoint
//! split, the per-turn evidence and the admissible drop (Lean
//! `PromptAssembly` replay model).

use gents_protocol::message::{AssistantContent, ReasoningContent};
use gents_protocol::output::OutputSource;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::messages::{encode_assistant_content, AssistantCodecError};
use crate::provider_input::replay_frontier::{
    admissible_turn_drop, anchored, ords, FlatItem, Turn,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayOrigin {
    ClaudeSubscription,
    AcceptedProvider,
    Foreign,
    Missing,
    Ambiguous,
}

pub type ReasoningWitness = Vec<(usize, Vec<ReasoningContent>)>;

pub fn reasoning_witness(content: &[AssistantContent]) -> ReasoningWitness {
    content
        .iter()
        .enumerate()
        .filter_map(|(index, block)| match block {
            AssistantContent::Reasoning(reasoning) => Some((index, reasoning.content.clone())),
            _ => None,
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayTag {
    pub request_doc_id: String,
    pub source: OutputSource,
}

impl ReplayTag {
    fn is_provider(&self) -> bool {
        !self.request_doc_id.is_empty() && matches!(self.source, OutputSource::ProviderTurn { .. })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TaggedAssistantRow {
    pub source: Option<ReplayTag>,
    pub physical_header: Option<String>,
    pub block_indices: Vec<usize>,
    pub id: Option<String>,
    pub content: Vec<AssistantContent>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReplayCheckpoint {
    pub required: Vec<ReplayTag>,
    pub prefix_rows: Vec<TaggedAssistantRow>,
    pub retained: Vec<TaggedAssistantRow>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedReplayEvidence {
    pub origin: ReplayOrigin,
    pub reasoning: ReasoningWitness,
    pub issuer: ReplayIssuer,
    pub wire: ReplayWire,
    pub physical_header: String,
    pub complete: bool,
    pub captured: Option<Vec<FlatItem>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayIssuer {
    pub family: String,
    pub endpoint: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayWire {
    ClaudeMessages,
    Responses,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReplayCheckpointError {
    #[error("invalidReplaySplit")]
    InvalidSplit,
    #[error("invalidReplayAssociation")]
    InvalidAssociation,
    #[error("duplicateReplayAssociation")]
    DuplicateAssociation,
    #[error("requiredReplayInPrefix")]
    RequiredInPrefix,
    #[error("missingRequiredReplay")]
    MissingRequired,
    #[error("{0}")]
    Codec(#[from] AssistantCodecError),
}

pub fn prepare_replay_checkpoint(
    required: Vec<ReplayTag>,
    rows: Vec<TaggedAssistantRow>,
    split: usize,
) -> Result<ReplayCheckpoint, ReplayCheckpointError> {
    if split > rows.len() {
        return Err(ReplayCheckpointError::InvalidSplit);
    }
    if required.iter().any(|tag| !tag.is_provider())
        || rows
            .iter()
            .filter_map(|row| row.source.as_ref())
            .any(|tag| !tag.is_provider())
    {
        return Err(ReplayCheckpointError::InvalidAssociation);
    }
    if required
        .iter()
        .enumerate()
        .any(|(index, tag)| required[..index].contains(tag))
        || rows.iter().enumerate().any(|(index, row)| {
            row.source.as_ref().is_some_and(|tag| {
                rows[..index]
                    .iter()
                    .any(|prior| prior.source.as_ref() == Some(tag))
            })
        })
    {
        return Err(ReplayCheckpointError::DuplicateAssociation);
    }
    if required.iter().any(|tag| {
        rows[..split]
            .iter()
            .any(|row| row.source.as_ref() == Some(tag))
    }) {
        return Err(ReplayCheckpointError::RequiredInPrefix);
    }
    if required.iter().any(|tag| {
        rows[split..]
            .iter()
            .filter(|row| row.source.as_ref() == Some(tag))
            .count()
            != 1
    }) {
        return Err(ReplayCheckpointError::MissingRequired);
    }
    Ok(ReplayCheckpoint {
        required,
        prefix_rows: rows[..split].to_vec(),
        retained: rows[split..].to_vec(),
    })
}

pub fn restore_contiguous_replay(
    checkpoint: &ReplayCheckpoint,
) -> Result<Vec<Vec<Value>>, ReplayCheckpointError> {
    let rows = checkpoint
        .prefix_rows
        .iter()
        .chain(&checkpoint.retained)
        .cloned()
        .collect();
    let checked = prepare_replay_checkpoint(Vec::new(), rows, checkpoint.prefix_rows.len())?;
    checked
        .retained
        .iter()
        .map(|row| Ok(encode_assistant_content(&row.content)?))
        .collect()
}

struct ReasoningCandidate<'a> {
    source: Option<&'a ReplayTag>,
    physical_header: Option<&'a str>,
    block_index: Option<usize>,
    indices_complete: bool,
    parts: &'a [ReasoningContent],
}

fn row_candidates(row: &TaggedAssistantRow) -> Vec<ReasoningCandidate<'_>> {
    let indices_complete = row.block_indices.len() == row.content.len()
        && row.block_indices.windows(2).all(|pair| pair[0] < pair[1]);
    row.content
        .iter()
        .enumerate()
        .filter_map(|(position, block)| match block {
            AssistantContent::Reasoning(reasoning) => Some(ReasoningCandidate {
                source: row.source.as_ref(),
                physical_header: row.physical_header.as_deref(),
                block_index: row.block_indices.get(position).copied(),
                indices_complete,
                parts: &reasoning.content,
            }),
            _ => None,
        })
        .collect()
}

struct TurnGroup<'a> {
    candidates: Vec<ReasoningCandidate<'a>>,
    ordinary: bool,
}

fn turn_groups(rows: &[TaggedAssistantRow]) -> Vec<TurnGroup<'_>> {
    rows.iter()
        .filter_map(|row| {
            let candidates = row_candidates(row);
            (!candidates.is_empty()).then(|| TurnGroup {
                candidates,
                ordinary: row.content.iter().any(|block| match block {
                    AssistantContent::Text(text) => !text.text.is_empty(),
                    AssistantContent::ToolCall(_) => true,
                    AssistantContent::Reasoning(_) | AssistantContent::Image(_) => false,
                }),
            })
        })
        .collect()
}

fn replayable_reasoning(wire: ReplayWire, parts: &[ReasoningContent]) -> bool {
    if parts.is_empty() {
        return false;
    }
    match wire {
        ReplayWire::ClaudeMessages => encode_assistant_content(&[AssistantContent::Reasoning(
            gents_protocol::message::Reasoning {
                id: None,
                content: parts.to_vec(),
            },
        )])
        .is_ok(),
        ReplayWire::Responses => {
            parts
                .iter()
                .any(|part| matches!(part, ReasoningContent::Encrypted(bytes) if !bytes.is_empty()))
                && parts.iter().all(|part| match part {
                    ReasoningContent::Encrypted(bytes) => !bytes.is_empty(),
                    ReasoningContent::Summary(_) => true,
                    _ => false,
                })
        }
    }
}

fn wire_reasoning_count(wire: ReplayWire, parts: &[ReasoningContent]) -> usize {
    match wire {
        ReplayWire::ClaudeMessages => parts.len(),
        ReplayWire::Responses => 1,
    }
}

fn turn_header_offset(wire: ReplayWire) -> usize {
    match wire {
        ReplayWire::ClaudeMessages => 1,
        ReplayWire::Responses => 0,
    }
}

struct Selection<'a> {
    rows: &'a [TaggedAssistantRow],
    issuer: &'a ReplayIssuer,
    wire: ReplayWire,
    groups: Vec<TurnGroup<'a>>,
    resolved: Vec<(ReplayTag, Vec<ResolvedReplayEvidence>)>,
}

impl<'a> Selection<'a> {
    fn new(
        rows: &'a [TaggedAssistantRow],
        issuer: &'a ReplayIssuer,
        wire: ReplayWire,
        mut resolve: impl FnMut(&ReplayTag) -> Vec<ResolvedReplayEvidence>,
    ) -> Self {
        let groups = turn_groups(rows);
        let mut resolved: Vec<(ReplayTag, Vec<ResolvedReplayEvidence>)> = Vec::new();
        for tag in groups
            .iter()
            .flat_map(|group| &group.candidates)
            .filter_map(|candidate| candidate.source)
        {
            if !resolved.iter().any(|(known, _)| known == tag) {
                resolved.push((tag.clone(), resolve(tag)));
            }
        }
        Self {
            rows,
            issuer,
            wire,
            groups,
            resolved,
        }
    }

    fn evidence(&self, tag: &ReplayTag) -> &[ResolvedReplayEvidence] {
        self.resolved
            .iter()
            .find(|(known, _)| known == tag)
            .map(|(_, evidence)| evidence.as_slice())
            .unwrap_or_default()
    }

    fn candidate_valid(&self, candidate: &ReasoningCandidate<'_>) -> bool {
        let (Some(tag), Some(header), Some(index)) = (
            candidate.source,
            candidate.physical_header,
            candidate.block_index,
        ) else {
            return false;
        };
        if !tag.is_provider()
            || !candidate.indices_complete
            || self
                .groups
                .iter()
                .flat_map(|group| &group.candidates)
                .filter(|other| {
                    other.source == Some(tag)
                        && other.physical_header == Some(header)
                        && other.block_index == Some(index)
                })
                .count()
                != 1
            || !replayable_reasoning(self.wire, candidate.parts)
        {
            return false;
        }
        let [evidence] = self.evidence(tag) else {
            return false;
        };
        evidence.origin == ReplayOrigin::AcceptedProvider
            && &evidence.issuer == self.issuer
            && evidence.wire == self.wire
            && evidence.physical_header == header
            && evidence.complete
            && evidence.reasoning.iter().any(|(block_index, parts)| {
                *block_index == index && parts.as_slice() == candidate.parts
            })
    }

    fn turn_base(&self, group: &TurnGroup<'_>) -> bool {
        !group.candidates.is_empty()
            && (self.wire == ReplayWire::Responses || group.ordinary)
            && group
                .candidates
                .iter()
                .all(|candidate| self.candidate_valid(candidate))
    }

    fn turn_captured(&self, group: &TurnGroup<'_>) -> Option<Vec<FlatItem>> {
        let tag = group.candidates.first()?.source?;
        match self.evidence(tag) {
            [evidence] => evidence.captured.clone(),
            _ => None,
        }
    }

    fn base_start(&self) -> usize {
        self.groups
            .iter()
            .rposition(|group| !self.turn_base(group))
            .map_or(0, |index| index + 1)
    }

    fn candidates_before(&self, group: usize) -> usize {
        self.groups[..group]
            .iter()
            .map(|group| group.candidates.len())
            .sum()
    }
}

type LocatedTurn = (Vec<Vec<u8>>, Vec<(usize, Vec<u8>)>);

fn locate_turns(offset: usize, body: &[FlatItem], counts: &[usize]) -> Option<Vec<LocatedTurn>> {
    let positions = body
        .iter()
        .enumerate()
        .filter_map(|(index, item)| matches!(item, FlatItem::Reasoning(_)).then_some(index))
        .collect::<Vec<_>>();
    if counts.iter().sum::<usize>() != positions.len() || counts.contains(&0) {
        return None;
    }
    let anchored_items = anchored(body);
    let mut seen = 0;
    let mut layouts = Vec::with_capacity(counts.len());
    for count in counts {
        let position = *positions.get(seen)?;
        let start = position.checked_sub(offset)?;
        if offset != 0 && !matches!(body.get(start), Some(FlatItem::Ordinary(_))) {
            return None;
        }
        layouts.push((
            ords(&body[..start])
                .into_iter()
                .map(<[u8]>::to_vec)
                .collect(),
            anchored_items[seen..seen + count]
                .iter()
                .map(|(anchor, bytes)| (*anchor, bytes.to_vec()))
                .collect(),
        ));
        seen += count;
    }
    Some(layouts)
}

fn strip_first_reasoning(rows: &[TaggedAssistantRow], mut count: usize) -> Vec<TaggedAssistantRow> {
    rows.iter()
        .map(|row| {
            let valid_indices = row.block_indices.len() == row.content.len()
                && row.block_indices.windows(2).all(|pair| pair[0] < pair[1]);
            let mut narrowed = row.clone();
            if !valid_indices {
                narrowed.physical_header = None;
            }
            narrowed.content.clear();
            narrowed.block_indices.clear();
            for (position, block) in row.content.iter().enumerate() {
                if matches!(block, AssistantContent::Reasoning(_)) && count > 0 {
                    count -= 1;
                    continue;
                }
                narrowed.content.push(block.clone());
                if let Some(index) = row.block_indices.get(position) {
                    narrowed.block_indices.push(*index);
                }
            }
            narrowed
        })
        .collect()
}

pub fn replay_stage(
    rows: &[TaggedAssistantRow],
    issuer: &ReplayIssuer,
    wire: ReplayWire,
    resolve: impl FnMut(&ReplayTag) -> Vec<ResolvedReplayEvidence>,
) -> Vec<TaggedAssistantRow> {
    let selection = Selection::new(rows, issuer, wire, resolve);
    strip_first_reasoning(rows, selection.candidates_before(selection.base_start()))
}

pub fn select_replay(
    rows: &[TaggedAssistantRow],
    issuer: &ReplayIssuer,
    wire: ReplayWire,
    resolve: impl FnMut(&ReplayTag) -> Vec<ResolvedReplayEvidence>,
    stage_body: &[FlatItem],
) -> Vec<TaggedAssistantRow> {
    let selection = Selection::new(rows, issuer, wire, resolve);
    let total = selection.candidates_before(selection.groups.len());
    let base_start = selection.base_start();
    let base_kept = &selection.groups[base_start..];
    let counts = base_kept
        .iter()
        .map(|group| {
            group
                .candidates
                .iter()
                .map(|candidate| wire_reasoning_count(wire, candidate.parts))
                .sum()
        })
        .collect::<Vec<usize>>();
    let kept_start = match locate_turns(turn_header_offset(wire), stage_body, &counts) {
        None => selection.groups.len(),
        Some(layouts) => {
            let turns = base_kept
                .iter()
                .zip(layouts)
                .map(|(group, (prefix_ords, items))| Turn {
                    base: selection.turn_base(group),
                    prefix_ords,
                    items,
                    captured: selection.turn_captured(group),
                })
                .collect::<Vec<_>>();
            base_start + admissible_turn_drop(&turns)
        }
    };
    let kept = total - selection.candidates_before(kept_start);
    strip_first_reasoning(selection.rows, total - kept)
}
