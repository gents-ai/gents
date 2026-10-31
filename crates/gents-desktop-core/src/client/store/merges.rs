use super::indexing::normalize_rows;
use super::*;

fn replace_node_rows<T>(
    dest: &mut Vec<T>,
    incoming: Vec<T>,
    node_did: &str,
    row_node: impl Fn(&T) -> &str,
) {
    dest.retain(|row| row_node(row) != node_did);
    dest.extend(incoming.into_iter().filter(|row| row_node(row) == node_did));
}

fn replace_node_rows_with_sources<T>(
    dest: &mut Vec<T>,
    sources: &mut Vec<Option<String>>,
    incoming: Vec<T>,
    node_did: &str,
    row_node: impl Fn(&T) -> &str,
) {
    retain_rows_and_sources(dest, sources, |row, _source| row_node(row) != node_did);
    let incoming = incoming
        .into_iter()
        .filter(|row| row_node(row) == node_did)
        .collect::<Vec<_>>();
    sources.extend(std::iter::repeat_n(
        Some(node_did.to_string()),
        incoming.len(),
    ));
    dest.extend(incoming);
}

macro_rules! retain_node_scope {
    ($store:expr, $node_did:expr, $keep_target:expr) => {{
        let rows = $store;
        let node_did = $node_did;
        let keep_target = $keep_target;
        let node_session_ids = if keep_target {
            RapidHashSet::new()
        } else {
            let mut session_ids = rows
                .sessions
                .iter()
                .filter(|row| row.node_did == node_did)
                .map(|row| row.session_id.clone())
                .collect::<RapidHashSet<_>>();
            session_ids.extend(
                rows.requests
                    .iter()
                    .filter(|row| row.node_did.as_deref() == Some(node_did))
                    .filter_map(|row| row.session_id.clone()),
            );
            session_ids
        };

        rows.operator_config_authoritative_node_dids
            .retain(|owner| (owner == node_did) == keep_target);
        rows.nodes
            .retain(|row| (row.node_did == node_did) == keep_target);
        rows.agents
            .retain(|row| (row.node_did == node_did) == keep_target);
        rows.runtimes
            .retain(|row| (row.node_did == node_did) == keep_target);
        rows.node_readiness
            .retain(|row| (row.node_did == node_did) == keep_target);
        rows.requests
            .retain(|row| (row.node_did.as_deref() == Some(node_did)) == keep_target);
        rows.mailbox_items
            .retain(|row| (row.node_did == node_did) == keep_target);
        rows.goals
            .retain(|row| (row.node_did == node_did) == keep_target);
        rows.transcript_messages
            .retain(|row| (row.message.node_did == node_did) == keep_target);
        rows.output_segments
            .retain(|row| (row.segment.node_did == node_did) == keep_target);
        retain_rows_and_sources(
            &mut rows.sessions,
            &mut rows.session_source_node_dids,
            |row, source| {
                row_is_in_node_scope(Some(&row.node_did), source, node_did) == keep_target
            },
        );
        retain_rows_and_sources(
            &mut rows.tool_calls,
            &mut rows.tool_call_source_node_dids,
            |row, source| {
                let ownerless_target_session = !keep_target
                    && row.node_did.is_none()
                    && source.is_none()
                    && row
                        .session_id
                        .as_deref()
                        .is_some_and(|session_id| node_session_ids.contains(session_id));
                (row_is_in_node_scope(row.node_did.as_deref(), source, node_did)
                    || ownerless_target_session)
                    == keep_target
            },
        );
        retain_rows_and_sources(
            &mut rows.compaction_entries,
            &mut rows.compaction_entry_source_node_dids,
            |row, source| {
                let ownerless_target_session = !keep_target
                    && source.is_none()
                    && row
                        .session_id
                        .as_deref()
                        .is_some_and(|session_id| node_session_ids.contains(session_id));
                (row_is_in_node_scope(None, source, node_did) || ownerless_target_session)
                    == keep_target
            },
        );
        retain_rows_and_sources(
            &mut rows.tasks,
            &mut rows.task_source_node_dids,
            |row, source| {
                row_is_in_node_scope(Some(&row.node_did), source, node_did) == keep_target
            },
        );
        retain_rows_and_sources(
            &mut rows.schedules,
            &mut rows.schedule_source_node_dids,
            |row, source| {
                row_is_in_node_scope(Some(&row.node_did), source, node_did) == keep_target
            },
        );
        retain_rows_and_sources(
            &mut rows.schedule_observations,
            &mut rows.schedule_observation_source_node_dids,
            |_row, source| row_is_in_node_scope(None, source, node_did) == keep_target,
        );
        retain_rows_and_sources(
            &mut rows.triggers,
            &mut rows.trigger_source_node_dids,
            |row, source| {
                row_is_in_node_scope(Some(&row.node_did), source, node_did) == keep_target
            },
        );
        retain_rows_and_sources(
            &mut rows.trigger_observations,
            &mut rows.trigger_observation_source_node_dids,
            |_row, source| row_is_in_node_scope(None, source, node_did) == keep_target,
        );
        retain_rows_and_sources(
            &mut rows.skills,
            &mut rows.skill_source_node_dids,
            |row, source| {
                row_is_in_node_scope(Some(&row.node_did), source, node_did) == keep_target
            },
        );
        retain_rows_and_sources(
            &mut rows.tools,
            &mut rows.tools_source_node_dids,
            |row, source| {
                row_is_in_node_scope(Some(&row.node_did), source, node_did) == keep_target
            },
        );
        retain_rows_and_sources(
            &mut rows.contexts,
            &mut rows.context_source_node_dids,
            |row, source| {
                row_is_in_node_scope(Some(&row.node_did), source, node_did) == keep_target
            },
        );
        retain_rows_and_sources(
            &mut rows.compactions,
            &mut rows.compaction_source_node_dids,
            |row, source| {
                row_is_in_node_scope(Some(&row.node_did), source, node_did) == keep_target
            },
        );
        retain_rows_and_sources(
            &mut rows.inference_backends,
            &mut rows.inference_backend_source_node_dids,
            |row, source| {
                row_is_in_node_scope(Some(&row.node_did), source, node_did) == keep_target
            },
        );
        retain_rows_and_sources(
            &mut rows.backend_observations,
            &mut rows.backend_observation_source_node_dids,
            |_row, source| row_is_in_node_scope(None, source, node_did) == keep_target,
        );
        retain_rows_and_sources(
            &mut rows.inference_profiles,
            &mut rows.inference_profile_source_node_dids,
            |row, source| {
                row_is_in_node_scope(Some(&row.node_did), source, node_did) == keep_target
            },
        );
        retain_rows_and_sources(
            &mut rows.inference_sampling,
            &mut rows.inference_sampling_source_node_dids,
            |row, source| {
                row_is_in_node_scope(Some(&row.node_did), source, node_did) == keep_target
            },
        );
        retain_rows_and_sources(
            &mut rows.inference_execution,
            &mut rows.inference_execution_source_node_dids,
            |row, source| {
                row_is_in_node_scope(Some(&row.node_did), source, node_did) == keep_target
            },
        );
        retain_rows_and_sources(
            &mut rows.tool_service_registries,
            &mut rows.tool_service_registry_source_node_dids,
            |row, source| {
                row_is_in_node_scope(Some(&row.node_did), source, node_did) == keep_target
            },
        );
        retain_rows_and_sources(
            &mut rows.event_sources,
            &mut rows.event_source_source_node_dids,
            |row, source| {
                row_is_in_node_scope(Some(&row.node_did), source, node_did) == keep_target
            },
        );
        retain_rows_and_sources(
            &mut rows.agent_targets,
            &mut rows.agent_target_source_node_dids,
            |row, source| {
                row_is_in_node_scope(Some(&row.node_did), source, node_did) == keep_target
            },
        );
        retain_rows_and_sources(
            &mut rows.datastore_tool_surfaces,
            &mut rows.datastore_tool_surface_source_node_dids,
            |row, source| {
                row_is_in_node_scope(Some(&row.node_did), source, node_did) == keep_target
            },
        );
    }};
}

impl ClientStore {
    pub fn overlay_node_operator_config(&self, node_did: &str, incoming: &ClientStore) -> Self {
        let mut rows = self.to_rows();
        if incoming
            .operator_config_authoritative_node_dids
            .contains(node_did)
        {
            rows.operator_config_authoritative_node_dids
                .insert(node_did.to_owned());
        } else {
            rows.operator_config_authoritative_node_dids
                .remove(node_did);
        }
        let remote = incoming.to_rows();
        replace_node_rows(&mut rows.nodes, remote.nodes, node_did, |row| {
            row.node_did.as_str()
        });
        replace_node_rows(&mut rows.agents, remote.agents, node_did, |row| {
            row.node_did.as_str()
        });
        replace_node_rows(&mut rows.runtimes, remote.runtimes, node_did, |row| {
            row.node_did.as_str()
        });
        replace_node_rows(
            &mut rows.node_readiness,
            remote.node_readiness,
            node_did,
            |row| row.node_did.as_str(),
        );
        replace_node_rows_with_sources(
            &mut rows.contexts,
            &mut rows.context_source_node_dids,
            remote.contexts,
            node_did,
            |row| row.node_did.as_str(),
        );
        replace_node_rows_with_sources(
            &mut rows.tools,
            &mut rows.tools_source_node_dids,
            remote.tools,
            node_did,
            |row| row.node_did.as_str(),
        );
        let target_backend_ids = rows
            .inference_backends
            .iter()
            .filter(|row| row.node_did == node_did)
            .map(|row| row.backend_id.clone())
            .chain(
                remote
                    .inference_backends
                    .iter()
                    .filter(|row| row.node_did == node_did)
                    .map(|row| row.backend_id.clone()),
            )
            .collect::<RapidHashSet<_>>();
        let shared_backend_ids = rows
            .inference_backends
            .iter()
            .filter(|row| row.node_did != node_did)
            .map(|row| row.backend_id.clone())
            .filter(|backend_id| target_backend_ids.contains(backend_id))
            .collect::<RapidHashSet<_>>();
        let incoming_backend_ids = remote
            .inference_backends
            .iter()
            .filter(|row| row.node_did == node_did)
            .map(|row| row.backend_id.clone())
            .collect::<RapidHashSet<_>>();
        replace_node_rows_with_sources(
            &mut rows.inference_backends,
            &mut rows.inference_backend_source_node_dids,
            remote.inference_backends,
            node_did,
            |row| row.node_did.as_str(),
        );
        retain_rows_and_sources(
            &mut rows.backend_observations,
            &mut rows.backend_observation_source_node_dids,
            |row, source| {
                source != Some(node_did)
                    && !(source.is_none()
                        && target_backend_ids.contains(&row.backend_id)
                        && !shared_backend_ids.contains(&row.backend_id))
            },
        );
        let backend_observations = remote
            .backend_observations
            .into_iter()
            .filter(|row| incoming_backend_ids.contains(&row.backend_id))
            .collect::<Vec<_>>();
        rows.backend_observation_source_node_dids
            .extend(std::iter::repeat_n(
                Some(node_did.to_string()),
                backend_observations.len(),
            ));
        rows.backend_observations.extend(backend_observations);
        replace_node_rows_with_sources(
            &mut rows.inference_profiles,
            &mut rows.inference_profile_source_node_dids,
            remote.inference_profiles,
            node_did,
            |row| row.node_did.as_str(),
        );
        replace_node_rows_with_sources(
            &mut rows.inference_sampling,
            &mut rows.inference_sampling_source_node_dids,
            remote.inference_sampling,
            node_did,
            |row| row.node_did.as_str(),
        );
        replace_node_rows_with_sources(
            &mut rows.inference_execution,
            &mut rows.inference_execution_source_node_dids,
            remote.inference_execution,
            node_did,
            |row| row.node_did.as_str(),
        );
        replace_node_rows_with_sources(
            &mut rows.tasks,
            &mut rows.task_source_node_dids,
            remote.tasks,
            node_did,
            |row| row.node_did.as_str(),
        );
        replace_node_rows_with_sources(
            &mut rows.schedules,
            &mut rows.schedule_source_node_dids,
            remote.schedules,
            node_did,
            |row| row.node_did.as_str(),
        );
        let incoming_trigger_ids = remote
            .triggers
            .iter()
            .filter(|row| row.node_did == node_did)
            .map(|row| row.trigger_id.clone())
            .collect::<RapidHashSet<_>>();
        retain_rows_and_sources(
            &mut rows.schedule_observations,
            &mut rows.schedule_observation_source_node_dids,
            |_row, source| source != Some(node_did),
        );
        let schedule_observations = remote
            .schedule_observations
            .into_iter()
            .filter(|row| incoming_trigger_ids.contains(&row.trigger_id))
            .collect::<Vec<_>>();
        rows.schedule_observation_source_node_dids
            .extend(std::iter::repeat_n(
                Some(node_did.to_string()),
                schedule_observations.len(),
            ));
        rows.schedule_observations.extend(schedule_observations);
        replace_node_rows_with_sources(
            &mut rows.triggers,
            &mut rows.trigger_source_node_dids,
            remote.triggers,
            node_did,
            |row| row.node_did.as_str(),
        );
        retain_rows_and_sources(
            &mut rows.trigger_observations,
            &mut rows.trigger_observation_source_node_dids,
            |_row, source| source != Some(node_did),
        );
        let trigger_observations = remote
            .trigger_observations
            .into_iter()
            .filter(|row| incoming_trigger_ids.contains(&row.trigger_id))
            .collect::<Vec<_>>();
        rows.trigger_observation_source_node_dids
            .extend(std::iter::repeat_n(
                Some(node_did.to_string()),
                trigger_observations.len(),
            ));
        rows.trigger_observations.extend(trigger_observations);
        replace_node_rows_with_sources(
            &mut rows.skills,
            &mut rows.skill_source_node_dids,
            remote.skills,
            node_did,
            |row| row.node_did.as_str(),
        );
        replace_node_rows_with_sources(
            &mut rows.compactions,
            &mut rows.compaction_source_node_dids,
            remote.compactions,
            node_did,
            |row| row.node_did.as_str(),
        );
        replace_node_rows_with_sources(
            &mut rows.tool_service_registries,
            &mut rows.tool_service_registry_source_node_dids,
            remote.tool_service_registries,
            node_did,
            |row| row.node_did.as_str(),
        );
        replace_node_rows_with_sources(
            &mut rows.event_sources,
            &mut rows.event_source_source_node_dids,
            remote.event_sources,
            node_did,
            |row| row.node_did.as_str(),
        );
        replace_node_rows_with_sources(
            &mut rows.agent_targets,
            &mut rows.agent_target_source_node_dids,
            remote.agent_targets,
            node_did,
            |row| row.node_did.as_str(),
        );
        replace_node_rows_with_sources(
            &mut rows.datastore_tool_surfaces,
            &mut rows.datastore_tool_surface_source_node_dids,
            remote.datastore_tool_surfaces,
            node_did,
            |row| row.node_did.as_str(),
        );
        replace_node_rows_with_sources(
            &mut rows.sessions,
            &mut rows.session_source_node_dids,
            remote.sessions,
            node_did,
            |row| row.node_did.as_str(),
        );
        replace_node_rows(&mut rows.requests, remote.requests, node_did, |row| {
            row.node_did.as_deref().unwrap_or_default()
        });
        Self::from_rows(rows)
    }

    pub fn merge_snapshot(&self, snapshot: ClientStore) -> Self {
        Self::merge_snapshot_rows(self.to_rows(), snapshot.to_rows())
    }

    fn merge_snapshot_rows(mut rows: ClientStoreRows, incoming: ClientStoreRows) -> Self {
        upsert_rows_by_key(&mut rows.nodes, incoming.nodes, |row| row.node_did.clone());
        upsert_rows_by_key(&mut rows.agents, incoming.agents, agent_merge_key);
        upsert_rows_by_key(&mut rows.runtimes, incoming.runtimes, |row| {
            row.node_did.clone()
        });
        upsert_rows_by_key(&mut rows.node_readiness, incoming.node_readiness, |row| {
            row.node_did.clone()
        });
        retire_request_placeholders(&mut rows.requests, &incoming.requests);
        upsert_rows_by_key(&mut rows.requests, incoming.requests, request_merge_key);
        upsert_rows_by_key(&mut rows.mailbox_items, incoming.mailbox_items, |row| {
            row.doc_id.clone()
        });
        union_immutable_rows(
            &mut rows.transcript_messages,
            incoming.transcript_messages,
            |row| row.doc_id.as_str(),
        );
        union_immutable_rows(&mut rows.output_segments, incoming.output_segments, |row| {
            row.doc_id.as_str()
        });
        upsert_rows_with_sources_by_key(
            &mut rows.sessions,
            &mut rows.session_source_node_dids,
            incoming.sessions,
            incoming.session_source_node_dids,
            session_merge_key,
        );
        upsert_goal_rows(&mut rows.goals, incoming.goals);
        upsert_rows_with_sources_by_key(
            &mut rows.tool_calls,
            &mut rows.tool_call_source_node_dids,
            incoming.tool_calls,
            incoming.tool_call_source_node_dids,
            tool_call_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.compaction_entries,
            &mut rows.compaction_entry_source_node_dids,
            incoming.compaction_entries,
            incoming.compaction_entry_source_node_dids,
            compaction_entry_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.tasks,
            &mut rows.task_source_node_dids,
            incoming.tasks,
            incoming.task_source_node_dids,
            task_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.schedules,
            &mut rows.schedule_source_node_dids,
            incoming.schedules,
            incoming.schedule_source_node_dids,
            schedule_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.schedule_observations,
            &mut rows.schedule_observation_source_node_dids,
            incoming.schedule_observations,
            incoming.schedule_observation_source_node_dids,
            schedule_observation_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.triggers,
            &mut rows.trigger_source_node_dids,
            incoming.triggers,
            incoming.trigger_source_node_dids,
            trigger_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.trigger_observations,
            &mut rows.trigger_observation_source_node_dids,
            incoming.trigger_observations,
            incoming.trigger_observation_source_node_dids,
            trigger_observation_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.skills,
            &mut rows.skill_source_node_dids,
            incoming.skills,
            incoming.skill_source_node_dids,
            skill_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.tools,
            &mut rows.tools_source_node_dids,
            incoming.tools,
            incoming.tools_source_node_dids,
            |row, _| tools_merge_key(row),
        );
        upsert_rows_with_sources_by_key(
            &mut rows.contexts,
            &mut rows.context_source_node_dids,
            incoming.contexts,
            incoming.context_source_node_dids,
            context_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.compactions,
            &mut rows.compaction_source_node_dids,
            incoming.compactions,
            incoming.compaction_source_node_dids,
            compaction_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.inference_backends,
            &mut rows.inference_backend_source_node_dids,
            incoming.inference_backends,
            incoming.inference_backend_source_node_dids,
            inference_backend_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.backend_observations,
            &mut rows.backend_observation_source_node_dids,
            incoming.backend_observations,
            incoming.backend_observation_source_node_dids,
            backend_observation_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.inference_profiles,
            &mut rows.inference_profile_source_node_dids,
            incoming.inference_profiles,
            incoming.inference_profile_source_node_dids,
            inference_profile_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.inference_sampling,
            &mut rows.inference_sampling_source_node_dids,
            incoming.inference_sampling,
            incoming.inference_sampling_source_node_dids,
            inference_sampling_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.inference_execution,
            &mut rows.inference_execution_source_node_dids,
            incoming.inference_execution,
            incoming.inference_execution_source_node_dids,
            inference_execution_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.tool_service_registries,
            &mut rows.tool_service_registry_source_node_dids,
            incoming.tool_service_registries,
            incoming.tool_service_registry_source_node_dids,
            tool_service_registry_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.event_sources,
            &mut rows.event_source_source_node_dids,
            incoming.event_sources,
            incoming.event_source_source_node_dids,
            event_source_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.agent_targets,
            &mut rows.agent_target_source_node_dids,
            incoming.agent_targets,
            incoming.agent_target_source_node_dids,
            agent_target_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.datastore_tool_surfaces,
            &mut rows.datastore_tool_surface_source_node_dids,
            incoming.datastore_tool_surfaces,
            incoming.datastore_tool_surface_source_node_dids,
            datastore_tool_surface_merge_key,
        );

        ClientStore::from_rows(rows)
    }

    pub fn replace_node_scope(&self, node_did: &str, snapshot: ClientStore) -> Self {
        let mut snapshot = snapshot;
        retain_node_scope!(&mut snapshot, node_did, true);

        let mut rows = self.to_rows();
        retain_node_scope!(&mut rows, node_did, false);
        rows.operator_config_authoritative_node_dids.extend(
            snapshot
                .operator_config_authoritative_node_dids
                .iter()
                .cloned(),
        );

        normalize_rows(&mut rows);
        Self::merge_snapshot_rows(rows, snapshot.to_rows())
    }

    pub fn merge_chat_patch(&self, patch: ClientStore) -> Self {
        let mut rows = self.to_rows();
        let patch_rows = patch.to_rows();

        retire_request_placeholders(&mut rows.requests, &patch_rows.requests);
        upsert_rows_by_key(&mut rows.requests, patch_rows.requests, request_merge_key);
        union_immutable_rows(
            &mut rows.transcript_messages,
            patch_rows.transcript_messages,
            |row| row.doc_id.as_str(),
        );
        union_immutable_rows(
            &mut rows.output_segments,
            patch_rows.output_segments,
            |row| row.doc_id.as_str(),
        );
        upsert_rows_with_sources_by_key(
            &mut rows.sessions,
            &mut rows.session_source_node_dids,
            patch_rows.sessions,
            patch_rows.session_source_node_dids,
            session_merge_key,
        );
        upsert_goal_rows(&mut rows.goals, patch_rows.goals);
        upsert_rows_with_sources_by_key(
            &mut rows.tool_calls,
            &mut rows.tool_call_source_node_dids,
            patch_rows.tool_calls,
            patch_rows.tool_call_source_node_dids,
            tool_call_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.compaction_entries,
            &mut rows.compaction_entry_source_node_dids,
            patch_rows.compaction_entries,
            patch_rows.compaction_entry_source_node_dids,
            compaction_entry_merge_key,
        );

        ClientStore::from_rows(rows)
    }

    pub fn to_rows(&self) -> ClientStoreRows {
        ClientStoreRows {
            nodes: self.nodes.clone(),
            agents: self.agents.clone(),
            runtimes: self.runtimes.clone(),
            node_readiness: self.node_readiness.clone(),
            requests: self.requests.clone(),
            mailbox_items: self.mailbox_items.clone(),
            transcript_messages: self.transcript_messages.clone(),
            output_segments: self.output_segments.clone(),
            sessions: self.sessions.clone(),
            goals: self.goals.clone(),
            tool_calls: self.tool_calls.clone(),
            compaction_entries: self.compaction_entries.clone(),
            session_source_node_dids: self.session_source_node_dids.clone(),
            tool_call_source_node_dids: self.tool_call_source_node_dids.clone(),
            compaction_entry_source_node_dids: self.compaction_entry_source_node_dids.clone(),
            tasks: self.tasks.clone(),
            schedules: self.schedules.clone(),
            schedule_observations: self.schedule_observations.clone(),
            triggers: self.triggers.clone(),
            trigger_observations: self.trigger_observations.clone(),
            task_source_node_dids: self.task_source_node_dids.clone(),
            schedule_source_node_dids: self.schedule_source_node_dids.clone(),
            schedule_observation_source_node_dids: self
                .schedule_observation_source_node_dids
                .clone(),
            trigger_source_node_dids: self.trigger_source_node_dids.clone(),
            trigger_observation_source_node_dids: self.trigger_observation_source_node_dids.clone(),
            skills: self.skills.clone(),
            skill_source_node_dids: self.skill_source_node_dids.clone(),
            tools: self.tools.clone(),
            tools_source_node_dids: self.tools_source_node_dids.clone(),
            contexts: self.contexts.clone(),
            context_source_node_dids: self.context_source_node_dids.clone(),
            compactions: self.compactions.clone(),
            compaction_source_node_dids: self.compaction_source_node_dids.clone(),
            inference_backends: self.inference_backends.clone(),
            backend_observations: self.backend_observations.clone(),
            inference_profiles: self.inference_profiles.clone(),
            inference_sampling: self.inference_sampling.clone(),
            inference_execution: self.inference_execution.clone(),
            tool_service_registries: self.tool_service_registries.clone(),
            event_sources: self.event_sources.clone(),
            agent_targets: self.agent_targets.clone(),
            datastore_tool_surfaces: self.datastore_tool_surfaces.clone(),
            inference_backend_source_node_dids: self.inference_backend_source_node_dids.clone(),
            backend_observation_source_node_dids: self.backend_observation_source_node_dids.clone(),
            inference_profile_source_node_dids: self.inference_profile_source_node_dids.clone(),
            inference_sampling_source_node_dids: self.inference_sampling_source_node_dids.clone(),
            inference_execution_source_node_dids: self.inference_execution_source_node_dids.clone(),
            tool_service_registry_source_node_dids: self
                .tool_service_registry_source_node_dids
                .clone(),
            event_source_source_node_dids: self.event_source_source_node_dids.clone(),
            agent_target_source_node_dids: self.agent_target_source_node_dids.clone(),
            datastore_tool_surface_source_node_dids: self
                .datastore_tool_surface_source_node_dids
                .clone(),
            operator_config_authoritative_node_dids: self
                .operator_config_authoritative_node_dids
                .clone(),
        }
    }
}

#[cfg(test)]
mod overlay_tests {
    use super::*;
    use gents::documents::{Agent, InferenceBackend, InferenceBackendObservation, Node, Task};
    use gents_protocol::output::{
        MessagePublication, MessageRole, OutputOutcome, TranscriptMessage,
    };
    use gents_protocol::request_lifecycle::RequestLifecycleState;
    use gents_protocol::row::{NodeReadinessRow, NodeRuntimeRow};
    use gents_protocol::session::{AgentSession, SessionTitle};

    fn session(node_did: &str, title: &str) -> AgentSession {
        serde_json::from_value(serde_json::json!({
            "session_id": "shared-session",
            "node_did": node_did,
            "agent_id": "agent",
            "created_at": "2026-10-14T00:00:00Z",
            "title": {"text": title, "source": "user"}
        }))
        .expect("session")
    }

    fn message(node_did: &str, doc_id: &str) -> TranscriptMessageRow {
        TranscriptMessageRow {
            doc_id: doc_id.to_owned(),
            message: TranscriptMessage {
                message_key: doc_id.to_owned(),
                session_id: "shared-session".to_owned(),
                node_did: node_did.to_owned(),
                requester_did: None,
                request_doc_id: None,
                publication: MessagePublication::RequestExecution {
                    execution_generation: "generation".to_owned(),
                },
                outcome: OutputOutcome::Complete,
                sequence: 1,
                role: MessageRole::Assistant,
                native_id: None,
                blocks: Vec::new(),
                created_at: "2026-10-14T00:00:00Z".to_owned(),
            },
        }
    }

    fn tool_call(
        node_did: Option<&str>,
        tool_call_key: &str,
        session_id: &str,
    ) -> AgentToolCallRow {
        serde_json::from_value(serde_json::json!({
            "node_did": node_did,
            "tool_call_key": tool_call_key,
            "session_id": session_id
        }))
        .expect("tool call")
    }

    fn compaction_entry(compaction_key: &str, session_id: &str) -> CompactionEntryRow {
        serde_json::from_value(serde_json::json!({
            "compaction_key": compaction_key,
            "session_id": session_id
        }))
        .expect("compaction entry")
    }

    #[test]
    fn scoped_replacement_preserves_foreign_rows_with_same_session_id() {
        let current = ClientStore::from_rows(ClientStoreRows {
            sessions: vec![
                session("did:alpha", "stale alpha"),
                session("did:beta", "beta"),
            ],
            session_source_node_dids: vec![None, None],
            transcript_messages: vec![
                message("did:alpha", "alpha-message"),
                message("did:beta", "beta-message"),
            ],
            ..ClientStoreRows::default()
        });
        let incoming = ClientStore::from_rows(ClientStoreRows {
            sessions: vec![session("did:alpha", "fresh alpha")],
            session_source_node_dids: vec![Some("did:alpha".to_owned())],
            ..ClientStoreRows::default()
        });

        let merged = current.replace_node_scope("did:alpha", incoming);
        assert_eq!(merged.sessions.len(), 2);
        assert_eq!(
            merged
                .sessions
                .iter()
                .find(|row| row.node_did == "did:alpha")
                .and_then(|row| row.title.as_ref())
                .map(|title: &SessionTitle| title.text.as_str()),
            Some("fresh alpha")
        );
        assert!(merged
            .sessions
            .iter()
            .any(|row| row.node_did == "did:beta" && row.session_id == "shared-session"));
        assert_eq!(merged.transcript_messages.len(), 1);
        assert_eq!(merged.transcript_messages[0].doc_id, "beta-message");
    }

    #[test]
    fn scoped_replacement_uses_explicit_tool_owner_before_observation_source() {
        let current = ClientStore::from_rows(ClientStoreRows {
            sessions: vec![session("did:alpha", "alpha"), session("did:beta", "beta")],
            tool_calls: vec![
                tool_call(Some("did:alpha"), "stale-alpha", "shared-session"),
                tool_call(Some("did:beta"), "beta", "shared-session"),
                tool_call(None, "unknown-alpha-session", "shared-session"),
                tool_call(None, "source-alpha", "another-session"),
                tool_call(None, "source-beta", "shared-session"),
                tool_call(None, "unknown-other-session", "other-session"),
            ],
            tool_call_source_node_dids: vec![
                None,
                None,
                None,
                Some("did:alpha".to_owned()),
                Some("did:beta".to_owned()),
                None,
            ],
            compaction_entries: vec![
                compaction_entry("selected-session", "shared-session"),
                compaction_entry("unrelated-session", "other-session"),
            ],
            compaction_entry_source_node_dids: vec![None, None],
            ..ClientStoreRows::default()
        });
        let incoming = ClientStore::from_rows(ClientStoreRows {
            tool_calls: vec![tool_call(
                Some("did:alpha"),
                "fresh-alpha",
                "shared-session",
            )],
            tool_call_source_node_dids: vec![None],
            ..ClientStoreRows::default()
        });

        let merged = current.replace_node_scope("did:alpha", incoming);
        let keys = merged
            .tool_calls
            .iter()
            .map(|row| row.tool_call_key.as_str())
            .collect::<RapidHashSet<_>>();
        assert_eq!(
            keys,
            [
                "beta",
                "source-beta",
                "unknown-other-session",
                "fresh-alpha"
            ]
            .into_iter()
            .collect()
        );
        assert_eq!(merged.compaction_entries.len(), 1);
        assert_eq!(
            merged.compaction_entries[0].compaction_key,
            "unrelated-session"
        );
        let scoped_keys = |node_did| {
            merged
                .transcript_for_node("shared-session", node_did)
                .tool_calls
                .iter()
                .map(|row| row.tool_call_key.as_str())
                .collect::<RapidHashSet<_>>()
        };
        assert_eq!(
            scoped_keys("did:alpha"),
            ["fresh-alpha"].into_iter().collect()
        );
        assert_eq!(
            scoped_keys("did:beta"),
            ["beta", "source-beta"].into_iter().collect()
        );
    }

    #[test]
    fn scoped_replacement_removes_ownerless_calls_from_request_only_sessions() {
        let current = ClientStore::from_rows(ClientStoreRows {
            requests: vec![request(Some("did:alpha"), "processing")],
            tool_calls: vec![tool_call(None, "stale-alpha", "session")],
            ..ClientStoreRows::default()
        });

        let merged = current.replace_node_scope(
            "did:alpha",
            ClientStore::from_rows(ClientStoreRows::default()),
        );

        assert!(merged.requests.is_empty());
        assert!(merged.tool_calls.is_empty());
    }

    #[test]
    fn generated_request_identity_cases_preserve_physical_twins_and_fold_owners() {
        let contract = gents_lean_contract::load_contract_snapshot::<serde_json::Value>()
            .expect("executed Lean request cache identity contract");
        let fixture = &contract["client_request_cache_identity_cases"];
        for case in fixture.as_array().expect("identity cases") {
            let all_input_rows = case["current"]
                .as_array()
                .expect("current rows")
                .iter()
                .chain(case["incoming"].as_array().expect("incoming rows").iter())
                .cloned()
                .collect::<Vec<_>>();
            let convert_rows = |values: &[serde_json::Value]| {
                values
                    .iter()
                    .map(|row| {
                        let doc = row["doc"].as_u64();
                        let request = row["request"].as_u64().expect("logical request ID");
                        let folded_into = row["folded_into"].as_u64();
                        let folded_request = folded_into.and_then(|owner_doc| {
                            all_input_rows
                                .iter()
                                .find(|candidate| candidate["doc"].as_u64() == Some(owner_doc))
                                .and_then(|candidate| candidate["request"].as_u64())
                        });
                        let lifecycle_state = match row["state"].as_str().expect("row state") {
                            "unclaimed" => "pending",
                            "active" => "processing",
                            "terminal" if folded_into.is_some() => "superseded",
                            "terminal" => "completed",
                            other => panic!("unexpected modeled request state {other}"),
                        };
                        serde_json::from_value::<AgentRequestRow>(serde_json::json!({
                            "_docID": doc.map(|id| format!("doc-{id}")),
                            "request_id": format!("request-{request}"),
                            "purpose": "normal",
                            "node_did": row["node"].as_u64().map(|id| format!("did:model:{id}")),
                            "requester_did": row["requester"].as_u64().map(|id| format!("did:model:{id}")),
                            "session_id": "session-model",
                            "lifecycle_state": lifecycle_state,
                            "superseded_by_request": folded_request.map(|id| format!("request-{id}")),
                            "superseded_by_request_doc_id": folded_into.map(|id| format!("doc-{id}")),
                            "failure_reason": folded_into.map(|_| "folded into claimed request"),
                            "retry_parent_request": row["retry_parent"].as_u64().map(|id| format!("request-{id}")),
                        }))
                        .expect("modeled request maps to desktop row")
                    })
                    .collect::<Vec<_>>()
            };
            let current = convert_rows(case["current"].as_array().unwrap());
            let incoming = convert_rows(case["incoming"].as_array().unwrap());
            let base = ClientStore::from_rows(ClientStoreRows {
                requests: current,
                ..Default::default()
            });
            let incoming = ClientStore::from_rows(ClientStoreRows {
                requests: incoming,
                ..Default::default()
            });
            let stores = [
                base.clone().merge_snapshot(incoming.clone()),
                base.merge_chat_patch(incoming),
            ];
            for store in stores {
                let mut actual_rows = store
                    .requests
                    .iter()
                    .map(|row| {
                    let parse_num = |value: Option<&str>, prefix: &str| {
                        value.and_then(|value| value.strip_prefix(prefix))
                            .and_then(|value| value.parse::<u64>().ok())
                    };
                    let state = if row.lifecycle_state == Some(RequestLifecycleState::Pending) {
                        "unclaimed"
                    } else if row.lifecycle_state == Some(RequestLifecycleState::Superseded)
                        || row.lifecycle_state.is_some_and(|state| state.is_terminal())
                    {
                        "terminal"
                    } else {
                        "active"
                    };
                    serde_json::json!({
                        "doc": parse_num(row.doc_id.as_deref(), "doc-"),
                        "node": row.node_did.as_deref().and_then(|did| did.strip_prefix("did:model:")).and_then(|id| id.parse::<u64>().ok()),
                        "requester": row.requester_did.as_deref().and_then(|did| did.strip_prefix("did:model:")).and_then(|id| id.parse::<u64>().ok()),
                        "request": parse_num(Some(&row.request_id), "request-"),
                        "state": state,
                        "folded_into": parse_num(row.superseded_by_request_doc_id.as_deref(), "doc-"),
                        "queued_after": null,
                        "retry_parent": row.retry_parent_request.as_deref().and_then(|id| parse_num(Some(id), "request-"))
                    })
                    })
                    .collect::<Vec<_>>();
                let mut expected_rows = case["expected_rows"]
                    .as_array()
                    .expect("modeled rows")
                    .clone();
                let by_physical_identity = |left: &serde_json::Value, right: &serde_json::Value| {
                    left["doc"].as_u64().cmp(&right["doc"].as_u64())
                };
                actual_rows.sort_by(by_physical_identity);
                expected_rows.sort_by(by_physical_identity);
                assert_eq!(
                    serde_json::Value::Array(actual_rows),
                    serde_json::Value::Array(expected_rows),
                    "{}",
                    case["name"]
                );

                if let Some(selected_doc) = case["selected_doc"].as_u64() {
                    let expected_turn_doc = case["expected_turn_doc"]
                        .as_u64()
                        .expect("modeled turn doc");
                    let selected_doc = format!("doc-{selected_doc}");
                    let selected = store
                        .requests
                        .iter()
                        .find(|row| row.doc_id.as_deref() == Some(selected_doc.as_str()))
                        .expect("selected physical request row");
                    let scoped = store
                        .requests
                        .iter()
                        .filter(|row| {
                            row.node_did == selected.node_did
                                && row.session_id == selected.session_id
                        })
                        .collect::<Vec<_>>();
                    let tip = super::super::turns::session_turn_request(&scoped, selected);
                    let expected_turn_doc = format!("doc-{expected_turn_doc}");
                    assert_eq!(tip.doc_id.as_deref(), Some(expected_turn_doc.as_str()));
                }
            }
        }
    }

    #[test]
    fn overlay_replaces_this_agent_and_keeps_others() {
        let local = ClientStore::from_rows(ClientStoreRows {
            nodes: vec![
                node_config("did:test:local", "Desktop leftover"),
                node_config("did:test:other", "Other"),
            ],
            agents: vec![agent_config("did:test:local", "ghost")],
            runtimes: vec![runtime("did:test:local", "desktop")],
            node_readiness: vec![readiness("did:test:local", "desktop")],
            inference_backends: vec![
                backend("did:test:local", "Desktop backend"),
                backend("did:test:other", "Other backend"),
            ],
            inference_backend_source_node_dids: vec![
                Some("did:test:local".to_string()),
                Some("did:test:other".to_string()),
            ],
            backend_observations: vec![
                backend_observation("stale"),
                backend_observation("other-healthy"),
            ],
            backend_observation_source_node_dids: vec![
                Some("did:test:local".to_string()),
                Some("did:test:other".to_string()),
            ],
            tasks: vec![task("did:test:local", "stale-task", false)],
            ..ClientStoreRows::default()
        });
        let remote = ClientStore::from_rows(ClientStoreRows {
            nodes: vec![node_config("did:test:local", "Mandrake")],
            agents: vec![agent_config("did:test:local", "Default")],
            runtimes: vec![runtime("did:test:local", "agent")],
            node_readiness: vec![readiness("did:test:local", "agent")],
            inference_backends: vec![backend("did:test:local", "Agent backend")],
            backend_observations: vec![backend_observation("healthy")],
            tasks: vec![task("did:test:local", "canonical-task", true)],
            operator_config_authoritative_node_dids: ["did:test:local".to_owned()]
                .into_iter()
                .collect(),
            ..ClientStoreRows::default()
        });
        let overlayed = local.overlay_node_operator_config("did:test:local", &remote);
        assert!(overlayed
            .operator_config_authoritative_node_dids
            .contains("did:test:local"));
        assert_eq!(overlayed.nodes.len(), 2);
        assert_eq!(
            overlayed
                .nodes
                .iter()
                .find(|row| row.node_did == "did:test:local")
                .and_then(|row| row.display_name.as_deref()),
            Some("Mandrake")
        );
        assert_eq!(
            overlayed
                .nodes
                .iter()
                .find(|row| row.node_did == "did:test:other")
                .and_then(|row| row.display_name.as_deref()),
            Some("Other")
        );
        assert_eq!(overlayed.agents.len(), 1);
        assert_eq!(overlayed.agents[0].display_name.as_deref(), Some("Default"));
        assert_eq!(
            overlayed.runtimes[0].reconcile_phase.as_deref(),
            Some("agent")
        );
        assert_eq!(overlayed.node_readiness[0].snapshot_json, "agent");
        assert_eq!(overlayed.inference_backends.len(), 2);
        let observations = overlayed
            .backend_observations
            .iter()
            .zip(&overlayed.backend_observation_source_node_dids)
            .map(|(row, source)| {
                (
                    source.as_deref().expect("observation source"),
                    row.probe_status.as_deref().expect("probe status"),
                )
            })
            .collect::<RapidHashMap<_, _>>();
        assert_eq!(observations.get("did:test:local"), Some(&"healthy"));
        assert_eq!(observations.get("did:test:other"), Some(&"other-healthy"));
        assert_eq!(overlayed.tasks.len(), 1);
        assert_eq!(overlayed.tasks[0].task_id, "canonical-task");
        assert!(overlayed.tasks[0].enabled);
        assert_eq!(
            overlayed.task_source_node_dids[0].as_deref(),
            Some("did:test:local")
        );
    }

    fn node_config(node_did: &str, display_name: &str) -> Node {
        Node {
            node_did: node_did.to_string(),
            display_name: Some(display_name.to_string()),
            default_agent_id: None,
            enabled: true,
            created_at: None,
            created_by: None,
            max_request_hop: None,
            tags: Vec::new(),
        }
    }

    fn agent_config(node_did: &str, display_name: &str) -> Agent {
        serde_json::from_value(serde_json::json!({
            "node_did": node_did,
            "agent_id": format!("{node_did}:default"),
            "display_name": display_name,
            "enabled": true,
            "inference_profile_id": format!("{node_did}:profile"),
        }))
        .expect("agent")
    }

    fn task(node_did: &str, task_id: &str, enabled: bool) -> Task {
        serde_json::from_value(serde_json::json!({
            "node_did": node_did,
            "task_id": task_id,
            "agent_id": "agent",
            "prompt_template": "Do the thing",
            "enabled": enabled,
        }))
        .expect("task")
    }

    fn runtime(node_did: &str, reconcile_phase: &str) -> NodeRuntimeRow {
        NodeRuntimeRow {
            node_did: node_did.to_string(),
            reconcile_phase: Some(reconcile_phase.to_string()),
            agent_executor_capacity: None,
            agent_executor_queue_depth: None,
            agent_executor_status_json: None,
            last_reconcile_result: None,
            last_reconcile_error: None,
            last_reconcile_completed_at: None,
            updated_at: None,
        }
    }

    fn readiness(node_did: &str, snapshot_json: &str) -> NodeReadinessRow {
        NodeReadinessRow {
            node_did: node_did.to_string(),
            snapshot_json: snapshot_json.to_string(),
            updated_at: "2026-09-12T00:00:00Z".to_string(),
        }
    }

    fn backend(node_did: &str, name: &str) -> InferenceBackend {
        serde_json::from_value(serde_json::json!({
            "node_did": node_did,
            "backend_id": "backend",
            "name": name,
            "provider_kind": "OpenAiCompatible",
            "openai_wire_api": "chat_completions",
            "endpoint": "http://localhost:8000/v1",
            "auth": {"kind": "unauthenticated"},
            "enabled": true,
        }))
        .expect("backend")
    }

    fn backend_observation(probe_status: &str) -> InferenceBackendObservation {
        InferenceBackendObservation {
            backend_id: "backend".to_string(),
            catalogs: Vec::new(),
            probe_status: Some(probe_status.to_string()),
            last_probe: None,
        }
    }

    #[test]
    fn scoped_chat_patch_replaces_unscoped_optimistic_rows() {
        let local = ClientStore::from_rows(ClientStoreRows {
            requests: vec![request(None, "processing")],
            ..ClientStoreRows::default()
        });
        let remote = ClientStore::from_rows(ClientStoreRows {
            requests: vec![request(Some("did:test:agent"), "completed")],
            ..ClientStoreRows::default()
        });

        let merged = local.merge_chat_patch(remote);

        assert_eq!(merged.requests.len(), 1);
        assert_eq!(
            merged.requests[0].node_did.as_deref(),
            Some("did:test:agent")
        );
        assert!(merged.requests[0].is_terminal());
    }

    fn request(node_did: Option<&str>, lifecycle_state: &str) -> AgentRequestRow {
        serde_json::from_value(serde_json::json!({
            "request_id": "request",
            "purpose": "normal",
            "node_did": node_did,
            "session_id": "session",
            "content": "hello",
            "lifecycle_state": lifecycle_state,
            "created_at": "2026-09-12T00:00:00Z",
        }))
        .expect("request")
    }
}
