use rapidhash::HashSetExt;

use super::*;

pub(super) fn row_node_matches(row_node_did: Option<&str>, node_did: &str) -> bool {
    row_node_did.is_none_or(|row_node_did| row_node_did == node_did)
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

pub(super) fn agent_merge_key(row: &Agent) -> String {
    format!("{}\0{}", row.node_did, row.agent_did)
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

pub(super) fn session_merge_key(row: &AgentSession) -> String {
    format!("{}\0{}", row.node_did, row.session_id)
}

pub(super) fn goal_merge_key(row: &GoalRow) -> String {
    format!("{}\0{}", row.node_did, row.session_id)
}

pub(super) fn tool_call_merge_key(row: &AgentToolCallRow) -> String {
    format!(
        "{}\0{}",
        row.node_did.as_deref().unwrap_or_default(),
        row.tool_call_key
    )
}

pub(super) fn compaction_entry_merge_key(row: &CompactionEntryRow) -> String {
    row.compaction_key.clone()
}

pub(super) fn task_merge_key(row: &Task) -> String {
    format!("{}\0{}", row.node_did, row.task_id)
}

pub(super) fn schedule_merge_key(row: &Schedule) -> String {
    format!("{}\0{}", row.node_did, row.schedule_id)
}

pub(super) fn schedule_observation_merge_key(row: &ScheduleObservation) -> String {
    row.trigger_id.clone()
}

pub(super) fn trigger_merge_key(row: &Trigger) -> String {
    format!("{}\0{}", row.node_did, row.trigger_id)
}

pub(super) fn trigger_observation_merge_key(row: &TriggerObservation) -> String {
    row.trigger_id.clone()
}

pub(super) fn skill_merge_key(row: &SkillDocument) -> String {
    format!("{}\0{}", row.node_did, row.skill_id)
}

pub(super) fn tools_merge_key(row: &Tools) -> String {
    format!("{}\0{}", row.node_did, row.tools_id)
}

pub(super) fn inference_backend_merge_key(row: &InferenceBackend) -> String {
    format!("{}\0{}", row.node_did, row.backend_id)
}

pub(super) fn backend_observation_merge_key(row: &InferenceBackendObservation) -> String {
    row.backend_id.clone()
}

pub(super) fn inference_profile_merge_key(row: &InferenceProfile) -> String {
    format!("{}\0{}", row.node_did, row.profile_id)
}

pub(super) fn tool_service_registry_merge_key(row: &ToolServiceRegistry) -> String {
    format!("{}\0{}", row.node_did, row.service_id)
}

pub(super) fn context_merge_key(row: &AgentContext) -> String {
    format!("{}\0{}", row.node_did, row.context_id)
}

pub(super) fn compaction_merge_key(row: &CompactionConfig) -> String {
    format!("{}\0{}", row.node_did, row.compaction_id)
}

pub(super) fn inference_sampling_merge_key(row: &InferenceSampling) -> String {
    format!("{}\0{}", row.node_did, row.sampling_id)
}

pub(super) fn inference_execution_merge_key(row: &InferenceExecution) -> String {
    format!("{}\0{}", row.node_did, row.execution_id)
}

pub(super) fn event_source_merge_key(row: &EventSource) -> String {
    format!("{}\0{}", row.node_did, row.event_source_id)
}

pub(super) fn agent_target_merge_key(row: &AgentTargetDocument) -> String {
    format!("{}\0{}", row.node_did, row.target_id)
}

pub(super) fn datastore_tool_surface_merge_key(row: &DatastoreToolSurfaceDocument) -> String {
    format!("{}\0{}", row.node_did, row.surface_id)
}
