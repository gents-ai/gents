use super::*;

fn replace_agent_rows<T>(
    dest: &mut Vec<T>,
    incoming: Vec<T>,
    agent_did: &str,
    row_agent: impl Fn(&T) -> &str,
) {
    dest.retain(|row| row_agent(row) != agent_did);
    dest.extend(
        incoming
            .into_iter()
            .filter(|row| row_agent(row) == agent_did),
    );
}

fn replace_agent_rows_with_sources<T>(
    dest: &mut Vec<T>,
    sources: &mut Vec<Option<String>>,
    incoming: Vec<T>,
    agent_did: &str,
    row_agent: impl Fn(&T) -> &str,
) {
    retain_rows_and_sources(dest, sources, |row, _source| row_agent(row) != agent_did);
    let incoming = incoming
        .into_iter()
        .filter(|row| row_agent(row) == agent_did)
        .collect::<Vec<_>>();
    sources.extend(std::iter::repeat_n(
        Some(agent_did.to_string()),
        incoming.len(),
    ));
    dest.extend(incoming);
}

impl ClientStore {
    /// Replace this agent's operator-owned config and runtime readiness with
    /// rows loaded from the runtime GraphQL endpoint. Local-only leftovers
    /// (wizard writes that never reached the agent) are dropped so the desktop
    /// matches the process that will actually run inference.
    pub fn overlay_agent_operator_config(&self, agent_did: &str, incoming: &ClientStore) -> Self {
        let mut rows = self.to_rows();
        let remote = incoming.to_rows();
        replace_agent_rows(
            &mut rows.agent_principals,
            remote.agent_principals,
            agent_did,
            |row| row.agent_did.as_str(),
        );
        replace_agent_rows(&mut rows.behaviors, remote.behaviors, agent_did, |row| {
            row.agent_did.as_str()
        });
        replace_agent_rows(&mut rows.runtimes, remote.runtimes, agent_did, |row| {
            row.agent_did.as_str()
        });
        replace_agent_rows(
            &mut rows.behavior_readiness,
            remote.behavior_readiness,
            agent_did,
            |row| row.agent_did.as_str(),
        );
        replace_agent_rows_with_sources(
            &mut rows.contexts,
            &mut rows.context_source_agent_dids,
            remote.contexts,
            agent_did,
            |row| row.agent_did.as_str(),
        );
        replace_agent_rows_with_sources(
            &mut rows.tools,
            &mut rows.tools_source_agent_dids,
            remote.tools,
            agent_did,
            |row| row.agent_did.as_str(),
        );
        let replaced_backend_ids = rows
            .inference_backends
            .iter()
            .filter(|row| row.agent_did == agent_did)
            .map(|row| row.backend_id.clone())
            .chain(
                remote
                    .inference_backends
                    .iter()
                    .filter(|row| row.agent_did == agent_did)
                    .map(|row| row.backend_id.clone()),
            )
            .collect::<HashSet<_>>();
        let incoming_backend_ids = remote
            .inference_backends
            .iter()
            .filter(|row| row.agent_did == agent_did)
            .map(|row| row.backend_id.clone())
            .collect::<HashSet<_>>();
        replace_agent_rows_with_sources(
            &mut rows.inference_backends,
            &mut rows.inference_backend_source_agent_dids,
            remote.inference_backends,
            agent_did,
            |row| row.agent_did.as_str(),
        );
        retain_rows_and_sources(
            &mut rows.backend_observations,
            &mut rows.backend_observation_source_agent_dids,
            |row, _source| !replaced_backend_ids.contains(&row.backend_id),
        );
        let backend_observations = remote
            .backend_observations
            .into_iter()
            .filter(|row| incoming_backend_ids.contains(&row.backend_id))
            .collect::<Vec<_>>();
        rows.backend_observation_source_agent_dids
            .extend(std::iter::repeat_n(
                Some(agent_did.to_string()),
                backend_observations.len(),
            ));
        rows.backend_observations.extend(backend_observations);
        replace_agent_rows_with_sources(
            &mut rows.inference_profiles,
            &mut rows.inference_profile_source_agent_dids,
            remote.inference_profiles,
            agent_did,
            |row| row.agent_did.as_str(),
        );
        replace_agent_rows_with_sources(
            &mut rows.inference_sampling,
            &mut rows.inference_sampling_source_agent_dids,
            remote.inference_sampling,
            agent_did,
            |row| row.agent_did.as_str(),
        );
        replace_agent_rows_with_sources(
            &mut rows.inference_execution,
            &mut rows.inference_execution_source_agent_dids,
            remote.inference_execution,
            agent_did,
            |row| row.agent_did.as_str(),
        );
        replace_agent_rows_with_sources(
            &mut rows.sessions,
            &mut rows.session_source_agent_dids,
            remote.sessions,
            agent_did,
            |row| row.agent_did.as_str(),
        );
        replace_agent_rows(&mut rows.requests, remote.requests, agent_did, |row| {
            row.agent_did.as_deref().unwrap_or_default()
        });
        replace_agent_rows(&mut rows.responses, remote.responses, agent_did, |row| {
            row.agent_did.as_deref().unwrap_or_default()
        });
        Self::from_rows(rows)
    }

    pub fn merge_snapshot(&self, snapshot: ClientStore) -> Self {
        let mut rows = self.to_rows();
        let incoming = snapshot.to_rows();

        upsert_rows_by_key(
            &mut rows.agent_principals,
            incoming.agent_principals,
            |row| row.agent_did.clone(),
        );
        upsert_rows_by_key(&mut rows.behaviors, incoming.behaviors, behavior_merge_key);
        upsert_rows_by_key(&mut rows.runtimes, incoming.runtimes, |row| {
            row.agent_did.clone()
        });
        upsert_rows_by_key(
            &mut rows.behavior_readiness,
            incoming.behavior_readiness,
            |row| row.agent_did.clone(),
        );
        upsert_rows_by_key(&mut rows.requests, incoming.requests, request_merge_key);
        upsert_rows_by_key(&mut rows.mailbox_items, incoming.mailbox_items, |row| {
            row.doc_id.clone()
        });
        upsert_rows_by_key(&mut rows.responses, incoming.responses, response_merge_key);
        upsert_rows_with_sources_by_key(
            &mut rows.messages,
            &mut rows.message_source_agent_dids,
            incoming.messages,
            incoming.message_source_agent_dids,
            message_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.sessions,
            &mut rows.session_source_agent_dids,
            incoming.sessions,
            incoming.session_source_agent_dids,
            session_merge_key,
        );
        upsert_goal_rows(&mut rows.goals, incoming.goals);
        upsert_rows_with_sources_by_key(
            &mut rows.tool_calls,
            &mut rows.tool_call_source_agent_dids,
            incoming.tool_calls,
            incoming.tool_call_source_agent_dids,
            tool_call_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.tool_results,
            &mut rows.tool_result_source_agent_dids,
            incoming.tool_results,
            incoming.tool_result_source_agent_dids,
            tool_result_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.compaction_entries,
            &mut rows.compaction_entry_source_agent_dids,
            incoming.compaction_entries,
            incoming.compaction_entry_source_agent_dids,
            compaction_entry_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.tasks,
            &mut rows.task_source_agent_dids,
            incoming.tasks,
            incoming.task_source_agent_dids,
            task_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.schedules,
            &mut rows.schedule_source_agent_dids,
            incoming.schedules,
            incoming.schedule_source_agent_dids,
            schedule_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.schedule_observations,
            &mut rows.schedule_observation_source_agent_dids,
            incoming.schedule_observations,
            incoming.schedule_observation_source_agent_dids,
            schedule_observation_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.triggers,
            &mut rows.trigger_source_agent_dids,
            incoming.triggers,
            incoming.trigger_source_agent_dids,
            trigger_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.trigger_observations,
            &mut rows.trigger_observation_source_agent_dids,
            incoming.trigger_observations,
            incoming.trigger_observation_source_agent_dids,
            trigger_observation_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.skills,
            &mut rows.skill_source_agent_dids,
            incoming.skills,
            incoming.skill_source_agent_dids,
            skill_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.tools,
            &mut rows.tools_source_agent_dids,
            incoming.tools,
            incoming.tools_source_agent_dids,
            |row, _| tools_merge_key(row),
        );
        upsert_rows_with_sources_by_key(
            &mut rows.contexts,
            &mut rows.context_source_agent_dids,
            incoming.contexts,
            incoming.context_source_agent_dids,
            context_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.compactions,
            &mut rows.compaction_source_agent_dids,
            incoming.compactions,
            incoming.compaction_source_agent_dids,
            compaction_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.inference_backends,
            &mut rows.inference_backend_source_agent_dids,
            incoming.inference_backends,
            incoming.inference_backend_source_agent_dids,
            inference_backend_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.backend_observations,
            &mut rows.backend_observation_source_agent_dids,
            incoming.backend_observations,
            incoming.backend_observation_source_agent_dids,
            backend_observation_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.inference_profiles,
            &mut rows.inference_profile_source_agent_dids,
            incoming.inference_profiles,
            incoming.inference_profile_source_agent_dids,
            inference_profile_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.inference_sampling,
            &mut rows.inference_sampling_source_agent_dids,
            incoming.inference_sampling,
            incoming.inference_sampling_source_agent_dids,
            inference_sampling_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.inference_execution,
            &mut rows.inference_execution_source_agent_dids,
            incoming.inference_execution,
            incoming.inference_execution_source_agent_dids,
            inference_execution_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.tool_service_registries,
            &mut rows.tool_service_registry_source_agent_dids,
            incoming.tool_service_registries,
            incoming.tool_service_registry_source_agent_dids,
            tool_service_registry_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.event_sources,
            &mut rows.event_source_source_agent_dids,
            incoming.event_sources,
            incoming.event_source_source_agent_dids,
            event_source_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.subagent_targets,
            &mut rows.subagent_target_source_agent_dids,
            incoming.subagent_targets,
            incoming.subagent_target_source_agent_dids,
            subagent_target_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.datastore_tool_surfaces,
            &mut rows.datastore_tool_surface_source_agent_dids,
            incoming.datastore_tool_surfaces,
            incoming.datastore_tool_surface_source_agent_dids,
            datastore_tool_surface_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.chain_key_bindings,
            &mut rows.chain_key_binding_source_agent_dids,
            incoming.chain_key_bindings,
            incoming.chain_key_binding_source_agent_dids,
            chain_key_binding_merge_key,
        );

        ClientStore::from_rows(rows)
    }

    /// Replace one agent's authoritative projection instead of additively
    /// merging it. This is used for scoped reloads and delete recovery so rows
    /// absent from the database snapshot cannot survive indefinitely in memory.
    pub fn replace_agent_scope(&self, agent_did: &str, snapshot: ClientStore) -> Self {
        let mut rows = self.to_rows();
        let mut agent_session_ids = rows
            .sessions
            .iter()
            .filter(|row| row.agent_did == agent_did)
            .map(|row| row.session_id.clone())
            .collect::<HashSet<_>>();
        agent_session_ids.extend(
            rows.requests
                .iter()
                .filter(|row| row.agent_did.as_deref() == Some(agent_did))
                .filter_map(|row| row.session_id.clone()),
        );

        rows.agent_principals
            .retain(|row| row.agent_did != agent_did);
        rows.behaviors.retain(|row| row.agent_did != agent_did);
        rows.runtimes.retain(|row| row.agent_did != agent_did);
        rows.behavior_readiness
            .retain(|row| row.agent_did != agent_did);
        rows.requests
            .retain(|row| row.agent_did.as_deref() != Some(agent_did));
        rows.mailbox_items.retain(|row| row.agent_did != agent_did);
        rows.responses
            .retain(|row| row.agent_did.as_deref() != Some(agent_did));
        rows.goals.retain(|row| row.agent_did != agent_did);
        rows.tool_results
            .retain(|row| row.agent_did.as_deref() != Some(agent_did));

        retain_rows_and_sources(
            &mut rows.messages,
            &mut rows.message_source_agent_dids,
            |row, source| {
                source != Some(agent_did)
                    && !(source.is_none()
                        && row
                            .session_id
                            .as_deref()
                            .is_some_and(|session_id| agent_session_ids.contains(session_id)))
            },
        );
        retain_rows_and_sources(
            &mut rows.sessions,
            &mut rows.session_source_agent_dids,
            |row, source| {
                row.agent_did != agent_did
                    && source != Some(agent_did)
                    && !(source.is_none() && agent_session_ids.contains(&row.session_id))
            },
        );
        retain_rows_and_sources(
            &mut rows.tool_calls,
            &mut rows.tool_call_source_agent_dids,
            |row, source| {
                source != Some(agent_did)
                    && !(source.is_none()
                        && row
                            .session_id
                            .as_deref()
                            .is_some_and(|session_id| agent_session_ids.contains(session_id)))
            },
        );
        retain_rows_and_sources(
            &mut rows.compaction_entries,
            &mut rows.compaction_entry_source_agent_dids,
            |row, source| {
                source != Some(agent_did)
                    && !(source.is_none()
                        && row
                            .session_id
                            .as_deref()
                            .is_some_and(|session_id| agent_session_ids.contains(session_id)))
            },
        );

        // Scoped snapshots reload the complete local control plane. Replace
        // local rows (source=None) and this remote agent's rows, while retaining
        // rows explicitly stamped as belonging to other remote agents.
        retain_rows_and_sources(
            &mut rows.tasks,
            &mut rows.task_source_agent_dids,
            |row, source| {
                row.agent_did != agent_did && source != Some(agent_did) && source.is_some()
            },
        );
        retain_rows_and_sources(
            &mut rows.schedules,
            &mut rows.schedule_source_agent_dids,
            |row, source| {
                row.agent_did != agent_did && source != Some(agent_did) && source.is_some()
            },
        );
        retain_rows_and_sources(
            &mut rows.schedule_observations,
            &mut rows.schedule_observation_source_agent_dids,
            |_row, source| source != Some(agent_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.triggers,
            &mut rows.trigger_source_agent_dids,
            |row, source| {
                row.agent_did != agent_did && source != Some(agent_did) && source.is_some()
            },
        );
        retain_rows_and_sources(
            &mut rows.trigger_observations,
            &mut rows.trigger_observation_source_agent_dids,
            |_row, source| source != Some(agent_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.skills,
            &mut rows.skill_source_agent_dids,
            |row, source| {
                row.agent_did != agent_did && source != Some(agent_did) && source.is_some()
            },
        );
        retain_rows_and_sources(
            &mut rows.tools,
            &mut rows.tools_source_agent_dids,
            |row, source| {
                row.agent_did != agent_did && source != Some(agent_did) && source.is_some()
            },
        );
        retain_rows_and_sources(
            &mut rows.contexts,
            &mut rows.context_source_agent_dids,
            |row, source| {
                row.agent_did != agent_did && source != Some(agent_did) && source.is_some()
            },
        );
        retain_rows_and_sources(
            &mut rows.compactions,
            &mut rows.compaction_source_agent_dids,
            |row, source| {
                row.agent_did != agent_did && source != Some(agent_did) && source.is_some()
            },
        );
        retain_rows_and_sources(
            &mut rows.inference_backends,
            &mut rows.inference_backend_source_agent_dids,
            |row, source| {
                row.agent_did != agent_did && source != Some(agent_did) && source.is_some()
            },
        );
        retain_rows_and_sources(
            &mut rows.backend_observations,
            &mut rows.backend_observation_source_agent_dids,
            |_row, source| source != Some(agent_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.inference_profiles,
            &mut rows.inference_profile_source_agent_dids,
            |row, source| {
                row.agent_did != agent_did && source != Some(agent_did) && source.is_some()
            },
        );
        retain_rows_and_sources(
            &mut rows.inference_sampling,
            &mut rows.inference_sampling_source_agent_dids,
            |row, source| {
                row.agent_did != agent_did && source != Some(agent_did) && source.is_some()
            },
        );
        retain_rows_and_sources(
            &mut rows.inference_execution,
            &mut rows.inference_execution_source_agent_dids,
            |row, source| {
                row.agent_did != agent_did && source != Some(agent_did) && source.is_some()
            },
        );
        retain_rows_and_sources(
            &mut rows.tool_service_registries,
            &mut rows.tool_service_registry_source_agent_dids,
            |row, source| {
                row.agent_did != agent_did && source != Some(agent_did) && source.is_some()
            },
        );
        retain_rows_and_sources(
            &mut rows.event_sources,
            &mut rows.event_source_source_agent_dids,
            |row, source| {
                row.agent_did != agent_did && source != Some(agent_did) && source.is_some()
            },
        );
        retain_rows_and_sources(
            &mut rows.subagent_targets,
            &mut rows.subagent_target_source_agent_dids,
            |row, source| {
                row.agent_did != agent_did && source != Some(agent_did) && source.is_some()
            },
        );
        retain_rows_and_sources(
            &mut rows.datastore_tool_surfaces,
            &mut rows.datastore_tool_surface_source_agent_dids,
            |row, source| {
                row.agent_did != agent_did && source != Some(agent_did) && source.is_some()
            },
        );
        retain_rows_and_sources(
            &mut rows.chain_key_bindings,
            &mut rows.chain_key_binding_source_agent_dids,
            |row, source| {
                row.agent_did != agent_did && source != Some(agent_did) && source.is_some()
            },
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
            .filter(|row| row.agent_did.is_some())
            .map(|row| row.request_id.clone())
            .collect::<HashSet<_>>();
        rows.requests.retain(|row| {
            row.agent_did.is_some() || !scoped_request_ids.contains(row.request_id.as_str())
        });
        let scoped_response_keys = patch_rows
            .responses
            .iter()
            .filter(|row| row.agent_did.is_some())
            .map(|row| row.response_key.clone())
            .collect::<HashSet<_>>();
        rows.responses.retain(|row| {
            row.agent_did.is_some() || !scoped_response_keys.contains(row.response_key.as_str())
        });

        upsert_rows_by_key(&mut rows.requests, patch_rows.requests, request_merge_key);
        upsert_rows_by_key(
            &mut rows.responses,
            patch_rows.responses,
            response_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.messages,
            &mut rows.message_source_agent_dids,
            patch_rows.messages,
            patch_rows.message_source_agent_dids,
            message_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.sessions,
            &mut rows.session_source_agent_dids,
            patch_rows.sessions,
            patch_rows.session_source_agent_dids,
            session_merge_key,
        );
        upsert_goal_rows(&mut rows.goals, patch_rows.goals);
        upsert_rows_with_sources_by_key(
            &mut rows.tool_calls,
            &mut rows.tool_call_source_agent_dids,
            patch_rows.tool_calls,
            patch_rows.tool_call_source_agent_dids,
            tool_call_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.tool_results,
            &mut rows.tool_result_source_agent_dids,
            patch_rows.tool_results,
            patch_rows.tool_result_source_agent_dids,
            tool_result_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.compaction_entries,
            &mut rows.compaction_entry_source_agent_dids,
            patch_rows.compaction_entries,
            patch_rows.compaction_entry_source_agent_dids,
            compaction_entry_merge_key,
        );

        ClientStore::from_rows(rows)
    }

    pub fn to_rows(&self) -> ClientStoreRows {
        ClientStoreRows {
            agent_principals: self.agent_principals.clone(),
            behaviors: self.behaviors.clone(),
            runtimes: self.runtimes.clone(),
            behavior_readiness: self.behavior_readiness.clone(),
            requests: self.requests.clone(),
            mailbox_items: self.mailbox_items.clone(),
            responses: self.responses.clone(),
            messages: self.messages.clone(),
            sessions: self.sessions.clone(),
            goals: self.goals.clone(),
            tool_calls: self.tool_calls.clone(),
            tool_results: self.tool_results.clone(),
            compaction_entries: self.compaction_entries.clone(),
            message_source_agent_dids: self.message_source_agent_dids.clone(),
            session_source_agent_dids: self.session_source_agent_dids.clone(),
            tool_call_source_agent_dids: self.tool_call_source_agent_dids.clone(),
            tool_result_source_agent_dids: self.tool_result_source_agent_dids.clone(),
            compaction_entry_source_agent_dids: self.compaction_entry_source_agent_dids.clone(),
            tasks: self.tasks.clone(),
            schedules: self.schedules.clone(),
            schedule_observations: self.schedule_observations.clone(),
            triggers: self.triggers.clone(),
            trigger_observations: self.trigger_observations.clone(),
            task_source_agent_dids: self.task_source_agent_dids.clone(),
            schedule_source_agent_dids: self.schedule_source_agent_dids.clone(),
            schedule_observation_source_agent_dids: self
                .schedule_observation_source_agent_dids
                .clone(),
            trigger_source_agent_dids: self.trigger_source_agent_dids.clone(),
            trigger_observation_source_agent_dids: self
                .trigger_observation_source_agent_dids
                .clone(),
            skills: self.skills.clone(),
            skill_source_agent_dids: self.skill_source_agent_dids.clone(),
            tools: self.tools.clone(),
            tools_source_agent_dids: self.tools_source_agent_dids.clone(),
            contexts: self.contexts.clone(),
            context_source_agent_dids: self.context_source_agent_dids.clone(),
            compactions: self.compactions.clone(),
            compaction_source_agent_dids: self.compaction_source_agent_dids.clone(),
            inference_backends: self.inference_backends.clone(),
            backend_observations: self.backend_observations.clone(),
            inference_profiles: self.inference_profiles.clone(),
            inference_sampling: self.inference_sampling.clone(),
            inference_execution: self.inference_execution.clone(),
            tool_service_registries: self.tool_service_registries.clone(),
            event_sources: self.event_sources.clone(),
            subagent_targets: self.subagent_targets.clone(),
            datastore_tool_surfaces: self.datastore_tool_surfaces.clone(),
            chain_key_bindings: self.chain_key_bindings.clone(),
            inference_backend_source_agent_dids: self.inference_backend_source_agent_dids.clone(),
            backend_observation_source_agent_dids: self
                .backend_observation_source_agent_dids
                .clone(),
            inference_profile_source_agent_dids: self.inference_profile_source_agent_dids.clone(),
            inference_sampling_source_agent_dids: self.inference_sampling_source_agent_dids.clone(),
            inference_execution_source_agent_dids: self
                .inference_execution_source_agent_dids
                .clone(),
            tool_service_registry_source_agent_dids: self
                .tool_service_registry_source_agent_dids
                .clone(),
            event_source_source_agent_dids: self.event_source_source_agent_dids.clone(),
            subagent_target_source_agent_dids: self.subagent_target_source_agent_dids.clone(),
            datastore_tool_surface_source_agent_dids: self
                .datastore_tool_surface_source_agent_dids
                .clone(),
            chain_key_binding_source_agent_dids: self.chain_key_binding_source_agent_dids.clone(),
        }
    }
}

#[cfg(test)]
mod overlay_tests {
    use super::*;
    use gents::document_config::{
        AgentBehavior, AgentPrincipal, InferenceBackend, InferenceBackendObservation,
    };
    use gents_protocol::row::{AgentBehaviorReadinessRow, AgentRuntimeRow};

    #[test]
    fn overlay_replaces_this_agent_and_keeps_others() {
        let local = ClientStore::from_rows(ClientStoreRows {
            agent_principals: vec![
                principal("did:test:local", "Desktop leftover"),
                principal("did:test:other", "Other"),
            ],
            behaviors: vec![behavior("did:test:local", "ghost")],
            runtimes: vec![runtime("did:test:local", "desktop")],
            behavior_readiness: vec![readiness("did:test:local", "desktop")],
            inference_backends: vec![backend("did:test:local", "Desktop backend")],
            backend_observations: vec![backend_observation("stale")],
            ..ClientStoreRows::default()
        });
        let remote = ClientStore::from_rows(ClientStoreRows {
            agent_principals: vec![principal("did:test:local", "Mandrake")],
            behaviors: vec![behavior("did:test:local", "Default")],
            runtimes: vec![runtime("did:test:local", "agent")],
            behavior_readiness: vec![readiness("did:test:local", "agent")],
            inference_backends: vec![backend("did:test:local", "Agent backend")],
            backend_observations: vec![backend_observation("healthy")],
            ..ClientStoreRows::default()
        });
        let overlayed = local.overlay_agent_operator_config("did:test:local", &remote);
        assert_eq!(overlayed.agent_principals.len(), 2);
        assert_eq!(
            overlayed
                .agent_principals
                .iter()
                .find(|row| row.agent_did == "did:test:local")
                .and_then(|row| row.display_name.as_deref()),
            Some("Mandrake")
        );
        assert_eq!(
            overlayed
                .agent_principals
                .iter()
                .find(|row| row.agent_did == "did:test:other")
                .and_then(|row| row.display_name.as_deref()),
            Some("Other")
        );
        assert_eq!(overlayed.behaviors.len(), 1);
        assert_eq!(
            overlayed.behaviors[0].display_name.as_deref(),
            Some("Default")
        );
        assert_eq!(
            overlayed.runtimes[0].reconcile_phase.as_deref(),
            Some("agent")
        );
        assert_eq!(overlayed.behavior_readiness[0].snapshot_json, "agent");
        assert_eq!(overlayed.inference_backends[0].name, "Agent backend");
        assert_eq!(
            overlayed.backend_observation_source_agent_dids[0].as_deref(),
            Some("did:test:local")
        );
        assert_eq!(
            overlayed.backend_observations[0].probe_status.as_deref(),
            Some("healthy")
        );
    }

    fn principal(agent_did: &str, display_name: &str) -> AgentPrincipal {
        AgentPrincipal {
            agent_did: agent_did.to_string(),
            display_name: Some(display_name.to_string()),
            default_behavior_id: None,
            enabled: true,
            created_at: None,
            created_by: None,
            tags: Vec::new(),
        }
    }

    fn behavior(agent_did: &str, display_name: &str) -> AgentBehavior {
        serde_json::from_value(serde_json::json!({
            "agent_did": agent_did,
            "behavior_id": format!("{agent_did}:default"),
            "display_name": display_name,
            "enabled": true,
            "inference_profile_id": format!("{agent_did}:profile"),
        }))
        .expect("behavior")
    }

    fn runtime(agent_did: &str, reconcile_phase: &str) -> AgentRuntimeRow {
        AgentRuntimeRow {
            agent_did: agent_did.to_string(),
            reconcile_phase: Some(reconcile_phase.to_string()),
            behavior_executor_capacity: None,
            behavior_executor_queue_depth: None,
            behavior_executor_status_json: None,
            last_reconcile_result: None,
            last_reconcile_error: None,
            last_reconcile_completed_at: None,
            updated_at: None,
        }
    }

    fn readiness(agent_did: &str, snapshot_json: &str) -> AgentBehaviorReadinessRow {
        AgentBehaviorReadinessRow {
            agent_did: agent_did.to_string(),
            snapshot_json: snapshot_json.to_string(),
            updated_at: "2026-09-12T00:00:00Z".to_string(),
        }
    }

    fn backend(agent_did: &str, name: &str) -> InferenceBackend {
        serde_json::from_value(serde_json::json!({
            "agent_did": agent_did,
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
            responses: vec![response(None, "streaming")],
            ..ClientStoreRows::default()
        });
        let remote = ClientStore::from_rows(ClientStoreRows {
            requests: vec![request(Some("did:test:agent"), "completed")],
            responses: vec![response(Some("did:test:agent"), "complete")],
            ..ClientStoreRows::default()
        });

        let merged = local.merge_chat_patch(remote);

        assert_eq!(merged.requests.len(), 1);
        assert_eq!(merged.responses.len(), 1);
        assert_eq!(
            merged.requests[0].agent_did.as_deref(),
            Some("did:test:agent")
        );
        assert!(merged.requests[0].is_terminal());
        assert_eq!(
            merged.responses[0].agent_did.as_deref(),
            Some("did:test:agent")
        );
        assert_eq!(merged.responses[0].status.as_deref(), Some("complete"));
    }

    fn request(agent_did: Option<&str>, lifecycle_state: &str) -> AgentRequestRow {
        serde_json::from_value(serde_json::json!({
            "request_id": "request",
            "agent_did": agent_did,
            "session_id": "session",
            "content": "hello",
            "lifecycle_state": lifecycle_state,
            "created_at": "2026-09-12T00:00:00Z",
        }))
        .expect("request")
    }

    fn response(agent_did: Option<&str>, status: &str) -> AgentResponseRow {
        serde_json::from_value(serde_json::json!({
            "response_key": "request",
            "request_id": "request",
            "agent_did": agent_did,
            "session_id": "session",
            "content": "hello",
            "status": status,
            "created_at": "2026-09-12T00:00:00Z",
        }))
        .expect("response")
    }
}
