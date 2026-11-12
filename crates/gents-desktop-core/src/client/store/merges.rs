use super::*;

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
        upsert_rows_by_key(&mut rows.sessions, incoming.sessions, session_merge_key);
        upsert_goal_rows(&mut rows.goals, incoming.goals);
        upsert_rows_by_key(
            &mut rows.tool_calls,
            incoming.tool_calls,
            tool_call_merge_key,
        );
        upsert_rows_by_key(
            &mut rows.compaction_entries,
            incoming.compaction_entries,
            compaction_entry_merge_key,
        );
        upsert_rows_by_key(&mut rows.tasks, incoming.tasks, task_merge_key);
        upsert_rows_by_key(&mut rows.schedules, incoming.schedules, schedule_merge_key);
        upsert_rows_by_key(
            &mut rows.schedule_observations,
            incoming.schedule_observations,
            schedule_observation_merge_key,
        );
        upsert_rows_by_key(&mut rows.triggers, incoming.triggers, trigger_merge_key);
        upsert_rows_by_key(
            &mut rows.trigger_observations,
            incoming.trigger_observations,
            trigger_observation_merge_key,
        );
        upsert_rows_by_key(&mut rows.skills, incoming.skills, skill_merge_key);
        upsert_rows_by_key(&mut rows.tools, incoming.tools, tools_merge_key);
        upsert_rows_by_key(&mut rows.contexts, incoming.contexts, context_merge_key);
        upsert_rows_by_key(
            &mut rows.compactions,
            incoming.compactions,
            compaction_merge_key,
        );
        upsert_rows_by_key(
            &mut rows.inference_backends,
            incoming.inference_backends,
            inference_backend_merge_key,
        );
        upsert_rows_by_key(
            &mut rows.backend_observations,
            incoming.backend_observations,
            backend_observation_merge_key,
        );
        upsert_rows_by_key(
            &mut rows.inference_profiles,
            incoming.inference_profiles,
            inference_profile_merge_key,
        );
        upsert_rows_by_key(
            &mut rows.inference_sampling,
            incoming.inference_sampling,
            inference_sampling_merge_key,
        );
        upsert_rows_by_key(
            &mut rows.inference_execution,
            incoming.inference_execution,
            inference_execution_merge_key,
        );
        upsert_rows_by_key(
            &mut rows.tool_service_registries,
            incoming.tool_service_registries,
            tool_service_registry_merge_key,
        );
        upsert_rows_by_key(
            &mut rows.event_sources,
            incoming.event_sources,
            event_source_merge_key,
        );
        upsert_rows_by_key(
            &mut rows.agent_targets,
            incoming.agent_targets,
            agent_target_merge_key,
        );
        upsert_rows_by_key(
            &mut rows.datastore_tool_surfaces,
            incoming.datastore_tool_surfaces,
            datastore_tool_surface_merge_key,
        );

        ClientStore::from_rows(rows)
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
        upsert_rows_by_key(&mut rows.sessions, patch_rows.sessions, session_merge_key);
        upsert_goal_rows(&mut rows.goals, patch_rows.goals);
        upsert_rows_by_key(
            &mut rows.tool_calls,
            patch_rows.tool_calls,
            tool_call_merge_key,
        );
        upsert_rows_by_key(
            &mut rows.compaction_entries,
            patch_rows.compaction_entries,
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
            tasks: self.tasks.clone(),
            schedules: self.schedules.clone(),
            schedule_observations: self.schedule_observations.clone(),
            triggers: self.triggers.clone(),
            trigger_observations: self.trigger_observations.clone(),
            skills: self.skills.clone(),
            tools: self.tools.clone(),
            contexts: self.contexts.clone(),
            compactions: self.compactions.clone(),
            inference_backends: self.inference_backends.clone(),
            backend_observations: self.backend_observations.clone(),
            inference_profiles: self.inference_profiles.clone(),
            inference_sampling: self.inference_sampling.clone(),
            inference_execution: self.inference_execution.clone(),
            tool_service_registries: self.tool_service_registries.clone(),
            event_sources: self.event_sources.clone(),
            agent_targets: self.agent_targets.clone(),
            datastore_tool_surfaces: self.datastore_tool_surfaces.clone(),
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
    use gents_protocol::request_lifecycle::RequestLifecycleState;

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
        let cases = &gents_lean_vocab::lean_contract_snapshot().client_request_cache_identity_cases;
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
                        let state = if row.lifecycle_state == Some(RequestLifecycleState::Pending) {
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
