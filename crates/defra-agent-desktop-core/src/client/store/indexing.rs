use std::collections::HashMap;

use defra_agent_protocol::row::AgentResponseRow;

use super::{ClientStore, ClientStoreRows};

impl ClientStore {
    pub fn from_rows(mut rows: ClientStoreRows) -> Self {
        rows.conversations.sort_by(|left, right| {
            cmp_opt_str_desc(left.updated_at.as_deref(), right.updated_at.as_deref())
                .then_with(|| {
                    cmp_opt_str_desc(left.created_at.as_deref(), right.created_at.as_deref())
                })
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        sort_rows_with_sources(
            &mut rows.messages,
            &mut rows.message_source_agent_dids,
            |left, right| {
                left.session_id
                    .cmp(&right.session_id)
                    .then_with(|| {
                        left.sequence
                            .unwrap_or_default()
                            .cmp(&right.sequence.unwrap_or_default())
                    })
                    .then_with(|| {
                        cmp_opt_str_asc(left.timestamp.as_deref(), right.timestamp.as_deref())
                    })
                    .then_with(|| left.message_key.cmp(&right.message_key))
            },
        );
        rows.requests.sort_by(|left, right| {
            left.session_id
                .cmp(&right.session_id)
                .then_with(|| {
                    cmp_opt_str_asc(left.created_at.as_deref(), right.created_at.as_deref())
                })
                .then_with(|| left.request_id.cmp(&right.request_id))
        });
        sort_rows_with_sources(
            &mut rows.tool_calls,
            &mut rows.tool_call_source_agent_dids,
            |left, right| {
                left.session_id
                    .cmp(&right.session_id)
                    .then_with(|| {
                        left.message_sequence
                            .unwrap_or_default()
                            .cmp(&right.message_sequence.unwrap_or_default())
                    })
                    .then_with(|| {
                        cmp_opt_str_asc(left.started_at.as_deref(), right.started_at.as_deref())
                    })
                    .then_with(|| left.tool_call_key.cmp(&right.tool_call_key))
            },
        );
        sort_rows_with_sources(
            &mut rows.tool_results,
            &mut rows.tool_result_source_agent_dids,
            |left, right| {
                left.session_id
                    .cmp(&right.session_id)
                    .then_with(|| {
                        cmp_opt_str_asc(left.created_at.as_deref(), right.created_at.as_deref())
                    })
                    .then_with(|| left.tool_name.cmp(&right.tool_name))
            },
        );

        normalize_source_agent_dids(&mut rows.session_source_agent_dids, rows.sessions.len());
        normalize_source_agent_dids(
            &mut rows.compaction_entry_source_agent_dids,
            rows.compaction_entries.len(),
        );
        normalize_source_agent_dids(&mut rows.task_source_agent_dids, rows.tasks.len());
        normalize_source_agent_dids(&mut rows.schedule_source_agent_dids, rows.schedules.len());
        normalize_source_agent_dids(
            &mut rows.event_trigger_source_agent_dids,
            rows.event_triggers.len(),
        );
        normalize_source_agent_dids(&mut rows.skill_source_agent_dids, rows.skills.len());
        normalize_source_agent_dids(
            &mut rows.inference_backend_source_agent_dids,
            rows.inference_backends.len(),
        );
        normalize_source_agent_dids(
            &mut rows.inference_profile_source_agent_dids,
            rows.inference_profiles.len(),
        );
        normalize_source_agent_dids(
            &mut rows.tool_service_registry_source_agent_dids,
            rows.tool_service_registries.len(),
        );

        let mut conversations_by_agent_did = HashMap::new();
        for (index, row) in rows.conversations.iter().enumerate() {
            if let Some(agent_did) = row.agent_did.as_deref().filter(|value| !value.is_empty()) {
                conversations_by_agent_did
                    .entry(agent_did.to_owned())
                    .or_insert_with(Vec::new)
                    .push(index);
            }
        }

        let messages_by_session_id =
            build_vec_index(&rows.messages, |row| row.session_id.as_deref());
        let requests_by_session_id =
            build_vec_index(&rows.requests, |row| row.session_id.as_deref());
        let tool_calls_by_session_id =
            build_vec_index(&rows.tool_calls, |row| row.session_id.as_deref());
        let tool_results_by_session_id =
            build_vec_index(&rows.tool_results, |row| row.session_id.as_deref());

        let mut runtimes_by_agent_did = HashMap::new();
        for (index, row) in rows.runtimes.iter().enumerate() {
            runtimes_by_agent_did.insert(row.agent_did.clone(), index);
        }

        let mut latest_response_by_request_id = HashMap::new();
        for (index, row) in rows.responses.iter().enumerate() {
            let Some(request_id) = row.request_id.as_deref().filter(|value| !value.is_empty())
            else {
                continue;
            };

            match latest_response_by_request_id.get(request_id).copied() {
                Some(existing_index)
                    if compare_response_rows(
                        &rows.responses[index],
                        &rows.responses[existing_index],
                    )
                    .is_gt() =>
                {
                    latest_response_by_request_id.insert(request_id.to_owned(), index);
                }
                None => {
                    latest_response_by_request_id.insert(request_id.to_owned(), index);
                }
                _ => {}
            }
        }

        let mut request_index_by_id = HashMap::new();
        for (index, row) in rows.requests.iter().enumerate() {
            request_index_by_id.insert(row.request_id.clone(), index);
        }

        Self {
            agent_principals: rows.agent_principals,
            behaviors: rows.behaviors,
            runtimes: rows.runtimes,
            conversations: rows.conversations,
            requests: rows.requests,
            responses: rows.responses,
            messages: rows.messages,
            sessions: rows.sessions,
            tool_calls: rows.tool_calls,
            tool_results: rows.tool_results,
            compaction_entries: rows.compaction_entries,
            message_source_agent_dids: rows.message_source_agent_dids,
            session_source_agent_dids: rows.session_source_agent_dids,
            tool_call_source_agent_dids: rows.tool_call_source_agent_dids,
            tool_result_source_agent_dids: rows.tool_result_source_agent_dids,
            compaction_entry_source_agent_dids: rows.compaction_entry_source_agent_dids,
            tasks: rows.tasks,
            schedules: rows.schedules,
            event_triggers: rows.event_triggers,
            task_source_agent_dids: rows.task_source_agent_dids,
            schedule_source_agent_dids: rows.schedule_source_agent_dids,
            event_trigger_source_agent_dids: rows.event_trigger_source_agent_dids,
            skills: rows.skills,
            skill_source_agent_dids: rows.skill_source_agent_dids,
            tool_selections: rows.tool_selections,
            inference_backends: rows.inference_backends,
            inference_profiles: rows.inference_profiles,
            tool_service_registries: rows.tool_service_registries,
            inference_backend_source_agent_dids: rows.inference_backend_source_agent_dids,
            inference_profile_source_agent_dids: rows.inference_profile_source_agent_dids,
            tool_service_registry_source_agent_dids: rows.tool_service_registry_source_agent_dids,
            conversations_by_agent_did,
            messages_by_session_id,
            requests_by_session_id,
            tool_calls_by_session_id,
            tool_results_by_session_id,
            runtimes_by_agent_did,
            latest_response_by_request_id,
            request_index_by_id,
        }
    }
}

fn normalize_source_agent_dids(sources: &mut Vec<Option<String>>, row_count: usize) {
    sources.truncate(row_count);
    sources.resize_with(row_count, || None);
}

fn sort_rows_with_sources<T>(
    rows: &mut Vec<T>,
    sources: &mut Vec<Option<String>>,
    compare: impl Fn(&T, &T) -> std::cmp::Ordering,
) {
    normalize_source_agent_dids(sources, rows.len());
    let mut paired = rows
        .drain(..)
        .zip(sources.drain(..))
        .collect::<Vec<(T, Option<String>)>>();
    paired.sort_by(|(left, _), (right, _)| compare(left, right));
    rows.extend(paired.into_iter().map(|(row, source)| {
        sources.push(source);
        row
    }));
}

pub(super) fn build_vec_index<T>(
    rows: &[T],
    key_fn: impl Fn(&T) -> Option<&str>,
) -> HashMap<String, Vec<usize>> {
    let mut index = HashMap::new();
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

fn compare_response_rows(left: &AgentResponseRow, right: &AgentResponseRow) -> std::cmp::Ordering {
    left.progress_seq
        .unwrap_or_default()
        .cmp(&right.progress_seq.unwrap_or_default())
        .then_with(|| cmp_opt_str_asc(left.completed_at.as_deref(), right.completed_at.as_deref()))
        .then_with(|| cmp_opt_str_asc(left.created_at.as_deref(), right.created_at.as_deref()))
        .then_with(|| left.response_key.cmp(&right.response_key))
}
