use gents_protocol::row::AgentRequestRow;
use rapidhash::{HashMapExt, RapidHashMap};

use super::{ClientStore, ClientStoreRows};

pub(super) fn normalize_rows(rows: &mut ClientStoreRows) {
    rows.requests
        .retain(|row| gents_protocol::request_admission::RequestPurpose::is_public(row.purpose));
    rows.sessions.sort_by(|left, right| {
        cmp_opt_str_desc(
            left.observation
                .as_ref()
                .map(|observation| observation.last_activity_at.as_str()),
            right
                .observation
                .as_ref()
                .map(|observation| observation.last_activity_at.as_str()),
        )
        .then_with(|| right.created_at.cmp(&left.created_at))
        .then_with(|| left.node_did.cmp(&right.node_did))
        .then_with(|| left.session_id.cmp(&right.session_id))
    });
    rows.transcript_messages.sort_by(|left, right| {
        left.message
            .session_id
            .cmp(&right.message.session_id)
            .then_with(|| left.message.sequence.cmp(&right.message.sequence))
            .then_with(|| left.doc_id.cmp(&right.doc_id))
    });
    rows.output_segments.sort_by(|left, right| {
        left.segment
            .request_doc_id
            .cmp(&right.segment.request_doc_id)
            .then_with(|| {
                serde_json::to_string(&left.segment.source)
                    .expect("output source serializes")
                    .cmp(
                        &serde_json::to_string(&right.segment.source)
                            .expect("output source serializes"),
                    )
            })
            .then_with(|| left.segment.ordinal.cmp(&right.segment.ordinal))
            .then_with(|| left.doc_id.cmp(&right.doc_id))
    });
    sort_request_rows(&mut rows.requests);
    rows.mailbox_items.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.doc_id.cmp(&right.doc_id))
    });
    rows.goals.sort_by(|left, right| {
        left.node_did
            .cmp(&right.node_did)
            .then_with(|| left.session_id.cmp(&right.session_id))
            .then_with(|| cmp_opt_str_asc(left.created_at.as_deref(), right.created_at.as_deref()))
            .then_with(|| left.goal_id.cmp(&right.goal_id))
    });
    rows.goals.dedup_by(|right, left| {
        right.node_did == left.node_did && right.session_id == left.session_id
    });
    rows.tool_calls.sort_by(|left, right| {
        left.session_id
            .cmp(&right.session_id)
            .then_with(|| {
                left.message_sequence
                    .unwrap_or_default()
                    .cmp(&right.message_sequence.unwrap_or_default())
            })
            .then_with(|| cmp_opt_str_asc(left.started_at.as_deref(), right.started_at.as_deref()))
            .then_with(|| left.tool_call_key.cmp(&right.tool_call_key))
    });
}

impl ClientStore {
    pub fn from_rows(mut rows: ClientStoreRows) -> Self {
        normalize_rows(&mut rows);

        let transcript_messages_by_session_id = build_vec_index(&rows.transcript_messages, |row| {
            Some(row.message.session_id.as_str())
        });
        let output_segments_by_request_doc_id = build_vec_index(&rows.output_segments, |row| {
            Some(row.segment.request_doc_id.as_str())
        });
        let requests_by_session_id =
            build_vec_index(&rows.requests, |row| row.session_id.as_deref());
        let tool_calls_by_session_id =
            build_vec_index(&rows.tool_calls, |row| row.session_id.as_deref());

        let mut runtimes_by_node_did = RapidHashMap::new();
        for (index, row) in rows.runtimes.iter().enumerate() {
            runtimes_by_node_did.insert(row.node_did.clone(), index);
        }
        let mut node_readiness_by_node_did = RapidHashMap::new();
        for (index, row) in rows.node_readiness.iter().enumerate() {
            node_readiness_by_node_did.insert(row.node_did.clone(), index);
        }

        let mut request_index_by_id = RapidHashMap::new();
        let mut request_index_by_doc_id = RapidHashMap::new();
        for (index, row) in rows.requests.iter().enumerate() {
            request_index_by_id.insert(row.request_id.clone(), index);
            if let Some(doc_id) = clean_string(row.doc_id.as_deref()) {
                request_index_by_doc_id.entry(doc_id).or_insert(index);
            }
        }

        Self {
            nodes: rows.nodes,
            default_agent_selections: rows.default_agent_selections,
            agents: rows.agents,
            runtimes: rows.runtimes,
            node_readiness: rows.node_readiness,
            requests: rows.requests,
            mailbox_items: rows.mailbox_items,
            transcript_messages: rows.transcript_messages,
            output_segments: rows.output_segments,
            sessions: rows.sessions,
            goals: rows.goals,
            tool_calls: rows.tool_calls,
            compaction_entries: rows.compaction_entries,
            tasks: rows.tasks,
            schedules: rows.schedules,
            schedule_observations: rows.schedule_observations,
            triggers: rows.triggers,
            trigger_observations: rows.trigger_observations,
            skills: rows.skills,
            tools: rows.tools,
            contexts: rows.contexts,
            compactions: rows.compactions,
            inference_backends: rows.inference_backends,
            backend_observations: rows.backend_observations,
            inference_profiles: rows.inference_profiles,
            inference_sampling: rows.inference_sampling,
            inference_execution: rows.inference_execution,
            tool_service_registries: rows.tool_service_registries,
            event_sources: rows.event_sources,
            agent_targets: rows.agent_targets,
            datastore_tool_surfaces: rows.datastore_tool_surfaces,
            operator_config_authoritative_node_dids: rows.operator_config_authoritative_node_dids,
            transcript_messages_by_session_id,
            output_segments_by_request_doc_id,
            requests_by_session_id,
            tool_calls_by_session_id,
            runtimes_by_node_did,
            node_readiness_by_node_did,
            request_index_by_id,
            request_index_by_doc_id,
        }
    }
}

pub(super) fn build_vec_index<T>(
    rows: &[T],
    key_fn: impl Fn(&T) -> Option<&str>,
) -> RapidHashMap<String, Vec<usize>> {
    let mut index = RapidHashMap::new();
    for (row_index, row) in rows.iter().enumerate() {
        if let Some(key) = clean_string(key_fn(row)) {
            index.entry(key).or_insert_with(Vec::new).push(row_index);
        }
    }
    index
}

pub(super) fn indexes_to_refs<'a, T>(rows: &'a [T], indexes: Option<&Vec<usize>>) -> Vec<&'a T> {
    indexes
        .into_iter()
        .flat_map(|indexes| indexes.iter())
        .map(|index| &rows[*index])
        .collect()
}

pub(super) fn clean_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

pub(super) fn cmp_opt_str_desc(left: Option<&str>, right: Option<&str>) -> std::cmp::Ordering {
    right.unwrap_or_default().cmp(left.unwrap_or_default())
}

pub(super) fn cmp_opt_str_asc(left: Option<&str>, right: Option<&str>) -> std::cmp::Ordering {
    left.unwrap_or_default().cmp(right.unwrap_or_default())
}

/// Physical identity resolves equal display-order keys so full and batched
/// projections give adapters the same order regardless of fetch order.
pub(crate) fn sort_request_rows(requests: &mut [AgentRequestRow]) {
    requests.sort_by(|left, right| {
        left.session_id
            .cmp(&right.session_id)
            .then_with(|| cmp_opt_str_asc(left.created_at.as_deref(), right.created_at.as_deref()))
            .then_with(|| left.request_id.cmp(&right.request_id))
            .then_with(|| left.doc_id.cmp(&right.doc_id))
    });
}
