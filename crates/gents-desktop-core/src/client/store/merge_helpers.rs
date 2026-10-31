use super::*;

pub(super) fn row_node_matches(row_node_did: Option<&str>, node_did: &str) -> bool {
    row_node_did.is_none_or(|row_node_did| row_node_did == node_did)
}

pub(super) fn row_is_in_node_scope(
    row_node_did: Option<&str>,
    source_node_did: Option<&str>,
    node_did: &str,
) -> bool {
    row_node_did.or(source_node_did) == Some(node_did)
}

pub(super) fn upsert_rows_by_key<T>(
    target: &mut Vec<T>,
    incoming: Vec<T>,
    key_fn: impl Fn(&T) -> String,
) {
    let mut indexes = target
        .iter()
        .enumerate()
        .map(|(index, row)| (key_fn(row), index))
        .collect::<RapidHashMap<_, _>>();

    for row in incoming {
        let key = key_fn(&row);
        if let Some(index) = indexes.get(&key).copied() {
            target[index] = row;
        } else {
            indexes.insert(key, target.len());
            target.push(row);
        }
    }
}

pub(super) fn retain_rows_and_sources<T>(
    rows: &mut Vec<T>,
    sources: &mut Vec<Option<String>>,
    mut keep: impl FnMut(&T, Option<&str>) -> bool,
) {
    sources.resize(rows.len(), None);
    let mut kept_rows = Vec::with_capacity(rows.len());
    let mut kept_sources = Vec::with_capacity(sources.len());
    for (row, source) in std::mem::take(rows)
        .into_iter()
        .zip(std::mem::take(sources))
    {
        if keep(&row, source.as_deref()) {
            kept_rows.push(row);
            kept_sources.push(source);
        }
    }
    *rows = kept_rows;
    *sources = kept_sources;
}

// A later-created replicated twin must not replace the canonical runtime
// selection; same creation time and goal ID is an update and replaces in place.
pub(super) fn upsert_goal_rows(target: &mut Vec<GoalRow>, incoming: Vec<GoalRow>) {
    let mut indexes = target
        .iter()
        .enumerate()
        .map(|(index, row)| (goal_merge_key(row), index))
        .collect::<RapidHashMap<_, _>>();

    for row in incoming {
        let key = goal_merge_key(&row);
        if let Some(index) = indexes.get(&key).copied() {
            let canonical_order = row
                .created_at
                .cmp(&target[index].created_at)
                .then_with(|| row.goal_id.cmp(&target[index].goal_id));
            if !canonical_order.is_gt() {
                target[index] = row;
            }
        } else {
            indexes.insert(key, target.len());
            target.push(row);
        }
    }
}

pub(super) fn upsert_rows_with_sources_by_key<T>(
    target: &mut Vec<T>,
    target_sources: &mut Vec<Option<String>>,
    incoming: Vec<T>,
    incoming_sources: Vec<Option<String>>,
    key_fn: impl Fn(&T, Option<&str>) -> String,
) {
    normalize_sources(target_sources, target.len());
    let mut incoming_sources = incoming_sources;
    normalize_sources(&mut incoming_sources, incoming.len());

    let mut indexes = target
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let source = target_sources.get(index).and_then(|value| value.as_deref());
            (key_fn(row, source), index)
        })
        .collect::<RapidHashMap<_, _>>();

    for (row, source) in incoming.into_iter().zip(incoming_sources) {
        let key = key_fn(&row, source.as_deref());
        if let Some(index) = indexes.get(&key).copied() {
            target[index] = row;
            target_sources[index] = source;
        } else {
            indexes.insert(key, target.len());
            target.push(row);
            target_sources.push(source);
        }
    }
}

pub(super) fn normalize_sources(sources: &mut Vec<Option<String>>, row_count: usize) {
    sources.truncate(row_count);
    sources.resize_with(row_count, || None);
}

pub(super) fn agent_merge_key(row: &Agent) -> String {
    format!("{}\0{}", row.node_did, row.agent_id)
}

pub(super) fn request_merge_key(row: &AgentRequestRow) -> String {
    if let Some(doc_id) = row.doc_id.as_deref() {
        format!("document\0{doc_id}")
    } else {
        format!(
            "optimistic\0{}\0{}\0{}",
            row.node_did.as_deref().unwrap_or_default(),
            row.requester_did.as_deref().unwrap_or_default(),
            row.request_id
        )
    }
}

pub(super) fn retire_request_placeholders(
    existing: &mut Vec<AgentRequestRow>,
    incoming: &[AgentRequestRow],
) {
    let mut replacements = RapidHashSet::with_capacity(incoming.len().saturating_mul(4));
    for replacement in incoming
        .iter()
        .filter(|row| row.doc_id.is_some() || row.node_did.is_some())
    {
        let request_id = replacement.request_id.as_str();
        let node_did = replacement.node_did.as_deref();
        let requester_did = replacement.requester_did.as_deref();
        for current_node in [None, node_did] {
            for current_requester in [None, requester_did] {
                replacements.insert((request_id, current_node, current_requester));
            }
        }
    }

    existing.retain(|row| {
        row.doc_id.is_some()
            || !replacements.contains(&(
                row.request_id.as_str(),
                row.node_did.as_deref(),
                row.requester_did.as_deref(),
            ))
    });
}

// Immutable canonical facts are set-unioned; a duplicated physical identity
// with different bytes is retained so reconstruction can classify the
// integrity conflict, never last-writer-wins.
pub(super) fn union_immutable_rows<T>(
    target: &mut Vec<T>,
    incoming: Vec<T>,
    doc_id: impl Fn(&T) -> &str,
) where
    T: std::fmt::Debug,
{
    for row in incoming {
        let id = doc_id(&row).to_string();
        if target
            .iter()
            .any(|existing| doc_id(existing) == id && format!("{existing:?}") == format!("{row:?}"))
        {
            continue;
        }
        target.push(row);
    }
}

pub(super) fn session_merge_key(row: &AgentSession, _source_node_did: Option<&str>) -> String {
    format!("{}\0{}", row.node_did, row.session_id)
}

pub(super) fn goal_merge_key(row: &GoalRow) -> String {
    format!("{}\0{}", row.node_did, row.session_id)
}

pub(super) fn tool_call_merge_key(row: &AgentToolCallRow, source_node_did: Option<&str>) -> String {
    format!(
        "{}\0{}",
        row.node_did
            .as_deref()
            .or(source_node_did)
            .unwrap_or_default(),
        row.tool_call_key
    )
}

pub(super) fn compaction_entry_merge_key(
    row: &CompactionEntryRow,
    source_node_did: Option<&str>,
) -> String {
    format!(
        "{}\0{}",
        source_node_did.unwrap_or_default(),
        row.compaction_key
    )
}

pub(super) fn task_merge_key(row: &Task, _source_node_did: Option<&str>) -> String {
    format!("{}\0{}", row.node_did, row.task_id)
}

pub(super) fn schedule_merge_key(row: &Schedule, _source_node_did: Option<&str>) -> String {
    format!("{}\0{}", row.node_did, row.schedule_id)
}

pub(super) fn schedule_observation_merge_key(
    row: &ScheduleObservation,
    source_node_did: Option<&str>,
) -> String {
    format!(
        "{}\0{}",
        source_node_did.unwrap_or_default(),
        row.trigger_id
    )
}

pub(super) fn trigger_merge_key(row: &Trigger, _source_node_did: Option<&str>) -> String {
    format!("{}\0{}", row.node_did, row.trigger_id)
}

pub(super) fn trigger_observation_merge_key(
    row: &TriggerObservation,
    source_node_did: Option<&str>,
) -> String {
    format!(
        "{}\0{}",
        source_node_did.unwrap_or_default(),
        row.trigger_id
    )
}

pub(super) fn skill_merge_key(row: &SkillDocument, _source_node_did: Option<&str>) -> String {
    format!("{}\0{}", row.node_did, row.skill_id)
}

pub(super) fn tools_merge_key(row: &Tools) -> String {
    format!("{}\0{}", row.node_did, row.tools_id)
}

pub(super) fn inference_backend_merge_key(
    row: &InferenceBackend,
    _source_node_did: Option<&str>,
) -> String {
    format!("{}\0{}", row.node_did, row.backend_id)
}

pub(super) fn backend_observation_merge_key(
    row: &InferenceBackendObservation,
    source_node_did: Option<&str>,
) -> String {
    format!(
        "{}\0{}",
        source_node_did.unwrap_or_default(),
        row.backend_id
    )
}

pub(super) fn inference_profile_merge_key(
    row: &InferenceProfile,
    _source_node_did: Option<&str>,
) -> String {
    format!("{}\0{}", row.node_did, row.profile_id)
}

pub(super) fn tool_service_registry_merge_key(
    row: &ToolServiceRegistry,
    _source_node_did: Option<&str>,
) -> String {
    format!("{}\0{}", row.node_did, row.service_id)
}

pub(super) fn context_merge_key(row: &AgentContext, _source_node_did: Option<&str>) -> String {
    format!("{}\0{}", row.node_did, row.context_id)
}

pub(super) fn compaction_merge_key(
    row: &CompactionConfig,
    _source_node_did: Option<&str>,
) -> String {
    format!("{}\0{}", row.node_did, row.compaction_id)
}

pub(super) fn inference_sampling_merge_key(
    row: &InferenceSampling,
    _source_node_did: Option<&str>,
) -> String {
    format!("{}\0{}", row.node_did, row.sampling_id)
}

pub(super) fn inference_execution_merge_key(
    row: &InferenceExecution,
    _source_node_did: Option<&str>,
) -> String {
    format!("{}\0{}", row.node_did, row.execution_id)
}

pub(super) fn event_source_merge_key(row: &EventSource, _source_node_did: Option<&str>) -> String {
    format!("{}\0{}", row.node_did, row.event_source_id)
}

pub(super) fn agent_target_merge_key(
    row: &AgentTargetDocument,
    _source_node_did: Option<&str>,
) -> String {
    format!("{}\0{}", row.node_did, row.target_id)
}

pub(super) fn datastore_tool_surface_merge_key(
    row: &DatastoreToolSurfaceDocument,
    _source_node_did: Option<&str>,
) -> String {
    format!("{}\0{}", row.node_did, row.surface_id)
}
