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

impl ClientStore {
    /// Replace this agent's operator-owned config and runtime readiness with
    /// rows loaded from the runtime GraphQL endpoint. Local-only leftovers
    /// (wizard writes that never reached the agent) are dropped so the desktop
    /// matches the process that will actually run inference.
    pub fn overlay_node_operator_config(&self, node_did: &str, incoming: &ClientStore) -> Self {
        let mut rows = self.to_rows();
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
            .collect::<HashSet<_>>();
        let shared_backend_ids = rows
            .inference_backends
            .iter()
            .filter(|row| row.node_did != node_did)
            .map(|row| row.backend_id.clone())
            .filter(|backend_id| target_backend_ids.contains(backend_id))
            .collect::<HashSet<_>>();
        let incoming_backend_ids = remote
            .inference_backends
            .iter()
            .filter(|row| row.node_did == node_did)
            .map(|row| row.backend_id.clone())
            .collect::<HashSet<_>>();
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
            .collect::<HashSet<_>>();
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
            &mut rows.chain_key_bindings,
            &mut rows.chain_key_binding_source_node_dids,
            remote.chain_key_bindings,
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
        let mut rows = self.to_rows();
        let incoming = snapshot.to_rows();

        upsert_rows_by_key(&mut rows.nodes, incoming.nodes, |row| row.node_did.clone());
        upsert_rows_by_key(&mut rows.agents, incoming.agents, agent_merge_key);
        upsert_rows_by_key(&mut rows.runtimes, incoming.runtimes, |row| {
            row.node_did.clone()
        });
        upsert_rows_by_key(&mut rows.node_readiness, incoming.node_readiness, |row| {
            row.node_did.clone()
        });
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
        upsert_rows_with_sources_by_key(
            &mut rows.chain_key_bindings,
            &mut rows.chain_key_binding_source_node_dids,
            incoming.chain_key_bindings,
            incoming.chain_key_binding_source_node_dids,
            chain_key_binding_merge_key,
        );

        ClientStore::from_rows(rows)
    }

    /// Replace one agent's authoritative projection instead of additively
    /// merging it. This is used for scoped reloads and delete recovery so rows
    /// absent from the database snapshot cannot survive indefinitely in memory.
    pub fn replace_node_scope(&self, node_did: &str, snapshot: ClientStore) -> Self {
        let mut rows = self.to_rows();
        let mut node_session_ids = rows
            .sessions
            .iter()
            .filter(|row| row.node_did == node_did)
            .map(|row| row.session_id.clone())
            .collect::<HashSet<_>>();
        node_session_ids.extend(
            rows.requests
                .iter()
                .filter(|row| row.node_did.as_deref() == Some(node_did))
                .filter_map(|row| row.session_id.clone()),
        );

        rows.nodes.retain(|row| row.node_did != node_did);
        rows.agents.retain(|row| row.node_did != node_did);
        rows.runtimes.retain(|row| row.node_did != node_did);
        rows.node_readiness.retain(|row| row.node_did != node_did);
        rows.requests
            .retain(|row| row.node_did.as_deref() != Some(node_did));
        rows.mailbox_items.retain(|row| row.node_did != node_did);
        rows.goals.retain(|row| row.node_did != node_did);
        rows.transcript_messages.retain(|row| {
            row.message.node_did != node_did && !node_session_ids.contains(&row.message.session_id)
        });
        rows.output_segments.retain(|row| {
            row.segment.node_did != node_did && !node_session_ids.contains(&row.segment.session_id)
        });
        retain_rows_and_sources(
            &mut rows.sessions,
            &mut rows.session_source_node_dids,
            |row, source| {
                row.node_did != node_did
                    && source != Some(node_did)
                    && !(source.is_none() && node_session_ids.contains(&row.session_id))
            },
        );
        retain_rows_and_sources(
            &mut rows.tool_calls,
            &mut rows.tool_call_source_node_dids,
            |row, source| {
                source != Some(node_did)
                    && !(source.is_none()
                        && row
                            .session_id
                            .as_deref()
                            .is_some_and(|session_id| node_session_ids.contains(session_id)))
            },
        );
        retain_rows_and_sources(
            &mut rows.compaction_entries,
            &mut rows.compaction_entry_source_node_dids,
            |row, source| {
                source != Some(node_did)
                    && !(source.is_none()
                        && row
                            .session_id
                            .as_deref()
                            .is_some_and(|session_id| node_session_ids.contains(session_id)))
            },
        );

        // Scoped snapshots reload the complete local control plane. Replace
        // local rows (source=None) and this remote agent's rows, while retaining
        // rows explicitly stamped as belonging to other remote agents.
        retain_rows_and_sources(
            &mut rows.tasks,
            &mut rows.task_source_node_dids,
            |row, source| row.node_did != node_did && source != Some(node_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.schedules,
            &mut rows.schedule_source_node_dids,
            |row, source| row.node_did != node_did && source != Some(node_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.schedule_observations,
            &mut rows.schedule_observation_source_node_dids,
            |_row, source| source != Some(node_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.triggers,
            &mut rows.trigger_source_node_dids,
            |row, source| row.node_did != node_did && source != Some(node_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.trigger_observations,
            &mut rows.trigger_observation_source_node_dids,
            |_row, source| source != Some(node_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.skills,
            &mut rows.skill_source_node_dids,
            |row, source| row.node_did != node_did && source != Some(node_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.tools,
            &mut rows.tools_source_node_dids,
            |row, source| row.node_did != node_did && source != Some(node_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.contexts,
            &mut rows.context_source_node_dids,
            |row, source| row.node_did != node_did && source != Some(node_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.compactions,
            &mut rows.compaction_source_node_dids,
            |row, source| row.node_did != node_did && source != Some(node_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.inference_backends,
            &mut rows.inference_backend_source_node_dids,
            |row, source| row.node_did != node_did && source != Some(node_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.backend_observations,
            &mut rows.backend_observation_source_node_dids,
            |_row, source| source != Some(node_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.inference_profiles,
            &mut rows.inference_profile_source_node_dids,
            |row, source| row.node_did != node_did && source != Some(node_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.inference_sampling,
            &mut rows.inference_sampling_source_node_dids,
            |row, source| row.node_did != node_did && source != Some(node_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.inference_execution,
            &mut rows.inference_execution_source_node_dids,
            |row, source| row.node_did != node_did && source != Some(node_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.tool_service_registries,
            &mut rows.tool_service_registry_source_node_dids,
            |row, source| row.node_did != node_did && source != Some(node_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.event_sources,
            &mut rows.event_source_source_node_dids,
            |row, source| row.node_did != node_did && source != Some(node_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.agent_targets,
            &mut rows.agent_target_source_node_dids,
            |row, source| row.node_did != node_did && source != Some(node_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.datastore_tool_surfaces,
            &mut rows.datastore_tool_surface_source_node_dids,
            |row, source| row.node_did != node_did && source != Some(node_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.chain_key_bindings,
            &mut rows.chain_key_binding_source_node_dids,
            |row, source| row.node_did != node_did && source != Some(node_did) && source.is_some(),
        );

        ClientStore::from_rows(rows).merge_snapshot(snapshot)
    }

    pub fn merge_chat_patch(&self, patch: ClientStore) -> Self {
        let mut rows = self.to_rows();
        let patch_rows = patch.to_rows();

        // Operator GraphQL supplies the canonical agent scope after a local
        // submit may already have installed an unscoped optimistic row. Let
        // the scoped document replace that placeholder; keeping both makes
        // agent-aware turn lookup select the older processing row first.
        let scoped_request_ids = patch_rows
            .requests
            .iter()
            .filter(|row| row.node_did.is_some())
            .map(|row| row.request_id.clone())
            .collect::<HashSet<_>>();
        rows.requests.retain(|row| {
            row.node_did.is_some() || !scoped_request_ids.contains(row.request_id.as_str())
        });
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
            chain_key_bindings: self.chain_key_bindings.clone(),
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
            chain_key_binding_source_node_dids: self.chain_key_binding_source_node_dids.clone(),
        }
    }
}

#[cfg(test)]
mod overlay_tests {
    use super::*;
    use gents::document_config::{
        Agent, InferenceBackend, InferenceBackendObservation, Node, Task,
    };
    use gents_protocol::row::{NodeReadinessRow, NodeRuntimeRow};

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
            ..ClientStoreRows::default()
        });
        let overlayed = local.overlay_node_operator_config("did:test:local", &remote);
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
            .collect::<HashMap<_, _>>();
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
