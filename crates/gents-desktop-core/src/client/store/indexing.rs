use std::collections::HashMap;

use gents_protocol::row::AgentResponseRow;

use super::{ClientStore, ClientStoreRows};

impl ClientStore {
    pub fn from_rows(mut rows: ClientStoreRows) -> Self {
        sort_rows_with_sources(
            &mut rows.sessions,
            &mut rows.session_source_agent_dids,
            |left, right| {
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
                .then_with(|| left.agent_did.cmp(&right.agent_did))
                .then_with(|| left.session_id.cmp(&right.session_id))
            },
        );
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
        rows.mailbox_items.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| left.doc_id.cmp(&right.doc_id))
        });
        rows.goals.sort_by(|left, right| {
            left.agent_did
                .cmp(&right.agent_did)
                .then_with(|| left.session_id.cmp(&right.session_id))
                .then_with(|| {
                    cmp_opt_str_asc(left.created_at.as_deref(), right.created_at.as_deref())
                })
                .then_with(|| left.goal_id.cmp(&right.goal_id))
        });
        rows.goals.dedup_by(|right, left| {
            right.agent_did == left.agent_did && right.session_id == left.session_id
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

        normalize_source_agent_dids(
            &mut rows.compaction_entry_source_agent_dids,
            rows.compaction_entries.len(),
        );
        normalize_source_agent_dids(&mut rows.task_source_agent_dids, rows.tasks.len());
        normalize_source_agent_dids(&mut rows.schedule_source_agent_dids, rows.schedules.len());
        normalize_source_agent_dids(
            &mut rows.schedule_observation_source_agent_dids,
            rows.schedule_observations.len(),
        );
        normalize_source_agent_dids(&mut rows.trigger_source_agent_dids, rows.triggers.len());
        normalize_source_agent_dids(
            &mut rows.trigger_observation_source_agent_dids,
            rows.trigger_observations.len(),
        );
        normalize_source_agent_dids(&mut rows.skill_source_agent_dids, rows.skills.len());
        normalize_source_agent_dids(&mut rows.tools_source_agent_dids, rows.tools.len());
        normalize_source_agent_dids(&mut rows.context_source_agent_dids, rows.contexts.len());
        normalize_source_agent_dids(
            &mut rows.compaction_source_agent_dids,
            rows.compactions.len(),
        );
        normalize_source_agent_dids(
            &mut rows.inference_backend_source_agent_dids,
            rows.inference_backends.len(),
        );
        normalize_source_agent_dids(
            &mut rows.backend_observation_source_agent_dids,
            rows.backend_observations.len(),
        );
        normalize_source_agent_dids(
            &mut rows.inference_profile_source_agent_dids,
            rows.inference_profiles.len(),
        );
        normalize_source_agent_dids(
            &mut rows.inference_sampling_source_agent_dids,
            rows.inference_sampling.len(),
        );
        normalize_source_agent_dids(
            &mut rows.inference_execution_source_agent_dids,
            rows.inference_execution.len(),
        );
        normalize_source_agent_dids(
            &mut rows.tool_service_registry_source_agent_dids,
            rows.tool_service_registries.len(),
        );
        normalize_source_agent_dids(
            &mut rows.event_source_source_agent_dids,
            rows.event_sources.len(),
        );
        normalize_source_agent_dids(
            &mut rows.subagent_target_source_agent_dids,
            rows.subagent_targets.len(),
        );
        normalize_source_agent_dids(
            &mut rows.datastore_tool_surface_source_agent_dids,
            rows.datastore_tool_surfaces.len(),
        );
        normalize_source_agent_dids(
            &mut rows.chain_key_binding_source_agent_dids,
            rows.chain_key_bindings.len(),
        );

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
        let mut behavior_readiness_by_agent_did = HashMap::new();
        for (index, row) in rows.behavior_readiness.iter().enumerate() {
            behavior_readiness_by_agent_did.insert(row.agent_did.clone(), index);
        }

        let mut latest_response_by_request_id = HashMap::new();
        let mut response_index_by_key = HashMap::new();
        for (index, row) in rows.responses.iter().enumerate() {
            response_index_by_key.insert(super::response_merge_key(row), index);
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
            behavior_readiness: rows.behavior_readiness,
            requests: rows.requests,
            mailbox_items: rows.mailbox_items,
            responses: rows.responses,
            messages: rows.messages,
            sessions: rows.sessions,
            goals: rows.goals,
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
            schedule_observations: rows.schedule_observations,
            triggers: rows.triggers,
            trigger_observations: rows.trigger_observations,
            task_source_agent_dids: rows.task_source_agent_dids,
            schedule_source_agent_dids: rows.schedule_source_agent_dids,
            schedule_observation_source_agent_dids: rows.schedule_observation_source_agent_dids,
            trigger_source_agent_dids: rows.trigger_source_agent_dids,
            trigger_observation_source_agent_dids: rows.trigger_observation_source_agent_dids,
            skills: rows.skills,
            skill_source_agent_dids: rows.skill_source_agent_dids,
            tools: rows.tools,
            tools_source_agent_dids: rows.tools_source_agent_dids,
            contexts: rows.contexts,
            context_source_agent_dids: rows.context_source_agent_dids,
            compactions: rows.compactions,
            compaction_source_agent_dids: rows.compaction_source_agent_dids,
            inference_backends: rows.inference_backends,
            backend_observations: rows.backend_observations,
            inference_profiles: rows.inference_profiles,
            inference_sampling: rows.inference_sampling,
            inference_execution: rows.inference_execution,
            tool_service_registries: rows.tool_service_registries,
            event_sources: rows.event_sources,
            subagent_targets: rows.subagent_targets,
            datastore_tool_surfaces: rows.datastore_tool_surfaces,
            chain_key_bindings: rows.chain_key_bindings,
            inference_backend_source_agent_dids: rows.inference_backend_source_agent_dids,
            backend_observation_source_agent_dids: rows.backend_observation_source_agent_dids,
            inference_profile_source_agent_dids: rows.inference_profile_source_agent_dids,
            inference_sampling_source_agent_dids: rows.inference_sampling_source_agent_dids,
            inference_execution_source_agent_dids: rows.inference_execution_source_agent_dids,
            tool_service_registry_source_agent_dids: rows.tool_service_registry_source_agent_dids,
            event_source_source_agent_dids: rows.event_source_source_agent_dids,
            subagent_target_source_agent_dids: rows.subagent_target_source_agent_dids,
            datastore_tool_surface_source_agent_dids: rows.datastore_tool_surface_source_agent_dids,
            chain_key_binding_source_agent_dids: rows.chain_key_binding_source_agent_dids,
            messages_by_session_id,
            requests_by_session_id,
            tool_calls_by_session_id,
            tool_results_by_session_id,
            runtimes_by_agent_did,
            behavior_readiness_by_agent_did,
            latest_response_by_request_id,
            response_index_by_key,
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

pub(super) fn compare_response_rows(
    left: &AgentResponseRow,
    right: &AgentResponseRow,
) -> std::cmp::Ordering {
    left.progress_seq
        .unwrap_or_default()
        .cmp(&right.progress_seq.unwrap_or_default())
        .then_with(|| cmp_opt_str_asc(left.completed_at.as_deref(), right.completed_at.as_deref()))
        .then_with(|| cmp_opt_str_asc(left.created_at.as_deref(), right.created_at.as_deref()))
        .then_with(|| left.response_key.cmp(&right.response_key))
}
