use super::*;

impl ClientStore {
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
        upsert_rows_by_key(
            &mut rows.conversations,
            incoming.conversations,
            conversation_merge_key,
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
            &mut rows.event_triggers,
            &mut rows.event_trigger_source_agent_dids,
            incoming.event_triggers,
            incoming.event_trigger_source_agent_dids,
            event_trigger_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.skills,
            &mut rows.skill_source_agent_dids,
            incoming.skills,
            incoming.skill_source_agent_dids,
            skill_merge_key,
        );
        upsert_rows_by_key(
            &mut rows.tool_selections,
            incoming.tool_selections,
            tool_selection_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.inference_backends,
            &mut rows.inference_backend_source_agent_dids,
            incoming.inference_backends,
            incoming.inference_backend_source_agent_dids,
            inference_backend_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.inference_profiles,
            &mut rows.inference_profile_source_agent_dids,
            incoming.inference_profiles,
            incoming.inference_profile_source_agent_dids,
            inference_profile_merge_key,
        );
        upsert_rows_with_sources_by_key(
            &mut rows.tool_service_registries,
            &mut rows.tool_service_registry_source_agent_dids,
            incoming.tool_service_registries,
            incoming.tool_service_registry_source_agent_dids,
            tool_service_registry_merge_key,
        );

        ClientStore::from_rows(rows)
    }

    /// Replace one agent's authoritative projection instead of additively
    /// merging it. This is used for scoped reloads and delete recovery so rows
    /// absent from the database snapshot cannot survive indefinitely in memory.
    pub fn replace_agent_scope(&self, agent_did: &str, snapshot: ClientStore) -> Self {
        let mut rows = self.to_rows();
        let mut agent_session_ids = rows
            .conversations
            .iter()
            .filter(|row| row.agent_did.as_deref() == Some(agent_did))
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
        rows.behaviors
            .retain(|row| row.agent_did.as_deref() != Some(agent_did));
        rows.runtimes.retain(|row| row.agent_did != agent_did);
        rows.behavior_readiness
            .retain(|row| row.agent_did != agent_did);
        rows.conversations
            .retain(|row| row.agent_did.as_deref() != Some(agent_did));
        rows.requests
            .retain(|row| row.agent_did.as_deref() != Some(agent_did));
        rows.mailbox_items.retain(|row| row.agent_did != agent_did);
        rows.responses
            .retain(|row| row.agent_did.as_deref() != Some(agent_did));
        rows.goals.retain(|row| row.agent_did != agent_did);
        rows.tool_results
            .retain(|row| row.agent_did.as_deref() != Some(agent_did));
        rows.tool_selections
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
                source != Some(agent_did)
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
            |_row, source| source != Some(agent_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.schedules,
            &mut rows.schedule_source_agent_dids,
            |_row, source| source != Some(agent_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.event_triggers,
            &mut rows.event_trigger_source_agent_dids,
            |_row, source| source != Some(agent_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.skills,
            &mut rows.skill_source_agent_dids,
            |row, source| {
                row.agent_did.as_deref() != Some(agent_did)
                    && source != Some(agent_did)
                    && source.is_some()
            },
        );
        retain_rows_and_sources(
            &mut rows.inference_backends,
            &mut rows.inference_backend_source_agent_dids,
            |_row, source| source != Some(agent_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.inference_profiles,
            &mut rows.inference_profile_source_agent_dids,
            |_row, source| source != Some(agent_did) && source.is_some(),
        );
        retain_rows_and_sources(
            &mut rows.tool_service_registries,
            &mut rows.tool_service_registry_source_agent_dids,
            |_row, source| source != Some(agent_did) && source.is_some(),
        );

        ClientStore::from_rows(rows).merge_snapshot(snapshot)
    }

    pub fn merge_chat_patch(&self, patch: ClientStore) -> Self {
        let mut rows = self.to_rows();
        let patch_rows = patch.to_rows();

        upsert_rows_by_key(
            &mut rows.conversations,
            patch_rows.conversations,
            conversation_merge_key,
        );
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
            conversations: self.conversations.clone(),
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
            event_triggers: self.event_triggers.clone(),
            task_source_agent_dids: self.task_source_agent_dids.clone(),
            schedule_source_agent_dids: self.schedule_source_agent_dids.clone(),
            event_trigger_source_agent_dids: self.event_trigger_source_agent_dids.clone(),
            skills: self.skills.clone(),
            skill_source_agent_dids: self.skill_source_agent_dids.clone(),
            tool_selections: self.tool_selections.clone(),
            inference_backends: self.inference_backends.clone(),
            inference_profiles: self.inference_profiles.clone(),
            tool_service_registries: self.tool_service_registries.clone(),
            inference_backend_source_agent_dids: self.inference_backend_source_agent_dids.clone(),
            inference_profile_source_agent_dids: self.inference_profile_source_agent_dids.clone(),
            tool_service_registry_source_agent_dids: self
                .tool_service_registry_source_agent_dids
                .clone(),
        }
    }
}
