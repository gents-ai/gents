use super::*;

pub(super) fn row_agent_matches(row_agent_did: Option<&str>, agent_did: &str) -> bool {
    row_agent_did.map_or(true, |row_agent_did| row_agent_did == agent_did)
}

pub fn is_deprecated_background_completion_request(request: &AgentRequestRow) -> bool {
    gents::lifecycle::is_deprecated_background_completion_request(
        request.execution_origin.as_deref(),
        request.metadata.as_deref(),
    )
}

pub(super) fn source_agent_matches(
    sources: &[Option<String>],
    row_index: usize,
    agent_did: &str,
) -> bool {
    sources
        .get(row_index)
        .and_then(|source| source.as_deref())
        .map_or(true, |source_agent_did| source_agent_did == agent_did)
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
        .collect::<HashMap<_, _>>();

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

/// Merge durable goals without allowing a later-created replicated twin to
/// replace the canonical row selected by the runtime. A row with the same
/// creation time and goal ID is treated as an update and replaces in place.
pub(super) fn upsert_goal_rows(target: &mut Vec<GoalRow>, incoming: Vec<GoalRow>) {
    let mut indexes = target
        .iter()
        .enumerate()
        .map(|(index, row)| (goal_merge_key(row), index))
        .collect::<HashMap<_, _>>();

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
        .collect::<HashMap<_, _>>();

    for (row, source) in incoming.into_iter().zip(incoming_sources.into_iter()) {
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

pub(super) fn conversation_merge_key(row: &AgentConversationRow) -> String {
    format!(
        "{}\0{}",
        row.agent_did.as_deref().unwrap_or_default(),
        row.session_id
    )
}

pub(super) fn behavior_merge_key(row: &AgentBehaviorRow) -> String {
    format!(
        "{}\0{}",
        row.agent_did.as_deref().unwrap_or_default(),
        row.behavior_id
    )
}

pub(super) fn request_merge_key(row: &AgentRequestRow) -> String {
    format!(
        "{}\0{}",
        row.agent_did.as_deref().unwrap_or_default(),
        row.request_id
    )
}

pub(super) fn response_merge_key(row: &AgentResponseRow) -> String {
    format!(
        "{}\0{}",
        row.agent_did.as_deref().unwrap_or_default(),
        row.response_key
    )
}

pub(super) fn message_merge_key(row: &AgentMessageRow, source_agent_did: Option<&str>) -> String {
    format!(
        "{}\0{}",
        source_agent_did.unwrap_or_default(),
        row.message_key
    )
}

pub(super) fn session_merge_key(row: &AgentSessionRow, source_agent_did: Option<&str>) -> String {
    format!(
        "{}\0{}",
        source_agent_did.unwrap_or_default(),
        row.session_id
    )
}

pub(super) fn goal_merge_key(row: &GoalRow) -> String {
    format!("{}\0{}", row.agent_did, row.session_id)
}

pub(super) fn tool_call_merge_key(
    row: &AgentToolCallRow,
    source_agent_did: Option<&str>,
) -> String {
    format!(
        "{}\0{}",
        source_agent_did.unwrap_or_default(),
        row.tool_call_key
    )
}

pub(super) fn tool_result_merge_key(
    row: &AgentToolResultRow,
    source_agent_did: Option<&str>,
) -> String {
    format!(
        "{}\0{}\0{}\0{}\0{}\0{}\0{}",
        source_agent_did.unwrap_or_default(),
        row.agent_did.as_deref().unwrap_or_default(),
        row.session_id.as_deref().unwrap_or_default(),
        row.tool_name.as_deref().unwrap_or_default(),
        row.tool_input.as_deref().unwrap_or_default(),
        row.conversation_doc_id.as_deref().unwrap_or_default(),
        row.created_at.as_deref().unwrap_or_default()
    )
}

pub(super) fn compaction_entry_merge_key(
    row: &CompactionEntryRow,
    source_agent_did: Option<&str>,
) -> String {
    format!(
        "{}\0{}",
        source_agent_did.unwrap_or_default(),
        row.compaction_key
    )
}

pub(super) fn task_merge_key(row: &TaskRow, source_agent_did: Option<&str>) -> String {
    format!("{}\0{}", source_agent_did.unwrap_or_default(), row.task_id)
}

pub(super) fn schedule_merge_key(row: &ScheduleRow, source_agent_did: Option<&str>) -> String {
    format!(
        "{}\0{}",
        source_agent_did.unwrap_or_default(),
        row.schedule_id
    )
}

pub(super) fn event_trigger_merge_key(
    row: &EventTriggerRow,
    source_agent_did: Option<&str>,
) -> String {
    format!(
        "{}\0{}",
        source_agent_did.unwrap_or_default(),
        row.trigger_id
    )
}

pub(super) fn skill_merge_key(row: &SkillRow, source_agent_did: Option<&str>) -> String {
    format!("{}\0{}", source_agent_did.unwrap_or_default(), row.skill_id)
}

pub(super) fn tool_selection_merge_key(row: &ToolSelectionRow) -> String {
    format!(
        "{}\0{}",
        row.agent_did.as_deref().unwrap_or_default(),
        row.selection_id
    )
}

pub(super) fn inference_backend_merge_key(
    row: &InferenceBackendRow,
    source_agent_did: Option<&str>,
) -> String {
    format!(
        "{}\0{}",
        source_agent_did.unwrap_or_default(),
        row.backend_id
    )
}

pub(super) fn inference_profile_merge_key(
    row: &InferenceProfileRow,
    source_agent_did: Option<&str>,
) -> String {
    format!(
        "{}\0{}",
        source_agent_did.unwrap_or_default(),
        row.profile_id
    )
}

pub(super) fn tool_service_registry_merge_key(
    row: &ToolServiceRegistryRow,
    source_agent_did: Option<&str>,
) -> String {
    format!(
        "{}\0{}",
        source_agent_did.unwrap_or_default(),
        row.service_id
    )
}
