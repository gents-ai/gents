use super::indexing::normalize_rows;
use super::*;

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
        rows.default_agent_selections
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
    pub fn merge_snapshot(&self, snapshot: ClientStore) -> Self {
        Self::merge_snapshot_rows(self.to_rows(), snapshot.to_rows())
    }

    fn merge_snapshot_rows(mut rows: ClientStoreRows, incoming: ClientStoreRows) -> Self {
        upsert_rows_by_key(&mut rows.nodes, incoming.nodes, |row| row.node_did.clone());
        upsert_rows_by_key(
            &mut rows.default_agent_selections,
            incoming.default_agent_selections,
            |row| row.node_did.clone(),
        );
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

    /// `ClientDataPublication.publishedRows`: the rows of a snapshot that a
    /// reload of `node_did` publishes. Rows the runtime returns for other
    /// owners are dropped before the reload's footprint is computed.
    pub(crate) fn node_rows(mut self, node_did: &str) -> Self {
        retain_node_scope!(&mut self, node_did, true);
        Self::from_rows(self.to_rows())
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
            default_agent_selections: self.default_agent_selections.clone(),
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
    use gents_lean_vocab::LeanClientRequestCacheIdentityCaseExpectedRowsItem;
    use gents_protocol::output::{
        MessagePublication, MessageRole, OutputOutcome, TranscriptMessage,
    };
    use gents_protocol::request_lifecycle::RequestLifecycleState;
    use gents_protocol::session::{AgentSession, SessionTitle};

    fn session(node_did: &str, title: &str) -> AgentSession {
        serde_json::from_value(serde_json::json!({
            "session_id": "shared-session",
            "node_did": node_did,
            "agent_did": "agent",
            "created_at": "2026-10-14T00:00:00Z",
            "title": {"text": title, "source": "user"}
        }))
        .expect("session")
    }

    fn message(node_did: &str, doc_id: &str) -> TranscriptMessageRow {
        TranscriptMessageRow {
            doc_id: doc_id.to_owned(),
            message: TranscriptMessage {
                native_request_id: None,
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

    struct IdentityRowInput<'a> {
        doc: Option<u64>,
        request: u64,
        folded_into: Option<u64>,
        state: &'a str,
        node: Option<u64>,
        requester: Option<u64>,
        retry_parent: Option<u64>,
    }

    fn identity_request_row(
        input: IdentityRowInput<'_>,
        all_input_rows: &[(Option<u64>, u64)],
    ) -> AgentRequestRow {
        let IdentityRowInput {
            doc,
            request,
            folded_into,
            state,
            node,
            requester,
            retry_parent,
        } = input;
        let folded_request = folded_into.and_then(|owner_doc| {
            all_input_rows
                .iter()
                .find(|(candidate_doc, _)| *candidate_doc == Some(owner_doc))
                .map(|(_, request)| *request)
        });
        let lifecycle_state = match state {
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
            "node_did": node.map(|id| format!("did:model:{id}")),
            "requester_did": requester.map(|id| format!("did:model:{id}")),
            "session_id": "session-model",
            "lifecycle_state": lifecycle_state,
            "superseded_by_request": folded_request.map(|id| format!("request-{id}")),
            "superseded_by_request_doc_id": folded_into.map(|id| format!("doc-{id}")),
            "failure_reason": folded_into.map(|_| "folded into claimed request"),
            "retry_parent_request": retry_parent.map(|id| format!("request-{id}")),
        }))
        .expect("modeled request maps to desktop row")
    }

    #[test]
    fn generated_request_identity_cases_preserve_physical_twins_and_fold_owners() {
        let cases =
            &gents_lean_vocab::lean_contract_snapshot().client_request_cache_identity_cases;
        for case in cases {
            let all_input_rows = case
                .current
                .iter()
                .map(|row| (row.doc, row.request))
                .chain(case.incoming.iter().map(|row| (row.doc, row.request)))
                .collect::<Vec<_>>();
            let current = case
                .current
                .iter()
                .map(|row| {
                    identity_request_row(
                        IdentityRowInput {
                            doc: row.doc,
                            request: row.request,
                            folded_into: row.folded_into,
                            state: &row.state,
                            node: row.node,
                            requester: row.requester,
                            retry_parent: row
                                .retry_parent
                                .as_ref()
                                .and_then(serde_json::Value::as_u64),
                        },
                        &all_input_rows,
                    )
                })
                .collect::<Vec<_>>();
            let incoming = case
                .incoming
                .iter()
                .map(|row| {
                    identity_request_row(
                        IdentityRowInput {
                            doc: row.doc,
                            request: row.request,
                            folded_into: row
                                .folded_into
                                .as_ref()
                                .and_then(serde_json::Value::as_u64),
                            state: &row.state,
                            node: row.node,
                            requester: row.requester,
                            retry_parent: row
                                .retry_parent
                                .as_ref()
                                .and_then(serde_json::Value::as_u64),
                        },
                        &all_input_rows,
                    )
                })
                .collect::<Vec<_>>();
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
                            value
                                .and_then(|value| value.strip_prefix(prefix))
                                .and_then(|value| value.parse::<u64>().ok())
                        };
                        let state =
                            if row.lifecycle_state == Some(RequestLifecycleState::Pending) {
                                "unclaimed"
                            } else if row.lifecycle_state == Some(RequestLifecycleState::Superseded)
                                || row.lifecycle_state.is_some_and(|state| state.is_terminal())
                            {
                                "terminal"
                            } else {
                                "active"
                            };
                        LeanClientRequestCacheIdentityCaseExpectedRowsItem {
                            doc: parse_num(row.doc_id.as_deref(), "doc-"),
                            node: row
                                .node_did
                                .as_deref()
                                .and_then(|did| did.strip_prefix("did:model:"))
                                .and_then(|id| id.parse::<u64>().ok()),
                            requester: row
                                .requester_did
                                .as_deref()
                                .and_then(|did| did.strip_prefix("did:model:"))
                                .and_then(|id| id.parse::<u64>().ok()),
                            request: parse_num(Some(&row.request_id), "request-")
                                .expect("request id"),
                            state: state.to_owned(),
                            folded_into: parse_num(
                                row.superseded_by_request_doc_id.as_deref(),
                                "doc-",
                            ),
                            queued_after: None,
                            retry_parent: row
                                .retry_parent_request
                                .as_deref()
                                .and_then(|id| parse_num(Some(id), "request-"))
                                .map(serde_json::Value::from),
                        }
                    })
                    .collect::<Vec<_>>();
                let mut expected_rows = case.expected_rows.clone();
                actual_rows.sort_by_key(|row| row.doc);
                expected_rows.sort_by_key(|row| row.doc);
                assert_eq!(actual_rows, expected_rows, "{}", case.name);

                if let Some(selected_doc) = case.selected_doc {
                    let expected_turn_doc = case.expected_turn_doc.expect("modeled turn doc");
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
