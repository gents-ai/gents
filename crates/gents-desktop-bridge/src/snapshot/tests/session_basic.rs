use super::*;

#[test]
fn session_snapshot_is_agent_scoped_when_session_ids_match() {
    let store = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![
            AgentConversationRow {
                session_id: "shared-session".to_string(),
                agent_name: Some("Mini 1".to_string()),
                agent_did: Some("did:test:mini-1".to_string()),
                requester_did: None,
                behavior_id: Some("default".to_string()),
                title: Some("mini-1 run".to_string()),
                title_source: Some("manual".to_string()),
                preview_text: None,
                status: Some("active".to_string()),
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                updated_at: Some("2026-04-21T12:00:00Z".to_string()),
                latest_request_id: None,
            },
            AgentConversationRow {
                session_id: "shared-session".to_string(),
                agent_name: Some("Mini 2".to_string()),
                agent_did: Some("did:test:mini-2".to_string()),
                requester_did: None,
                behavior_id: Some("default".to_string()),
                title: Some("mini-2 run".to_string()),
                title_source: Some("manual".to_string()),
                preview_text: None,
                status: Some("active".to_string()),
                created_at: Some("2026-04-21T12:01:00Z".to_string()),
                updated_at: Some("2026-04-21T12:01:00Z".to_string()),
                latest_request_id: None,
            },
        ],
        messages: vec![
            AgentMessageRow {
                message_key: "msg-mini-1".to_string(),
                session_id: Some("shared-session".to_string()),
                request_id: None,
                requester_did: None,
                sequence: Some(1),
                role: Some("user".to_string()),
                content: Some(user_message_json("mini 1 only")),
                reasoning: None,
                timestamp: Some("2026-04-21T12:00:01Z".to_string()),
            },
            AgentMessageRow {
                message_key: "msg-mini-2".to_string(),
                session_id: Some("shared-session".to_string()),
                request_id: None,
                requester_did: None,
                sequence: Some(2),
                role: Some("user".to_string()),
                content: Some(user_message_json("mini 2 only")),
                reasoning: None,
                timestamp: Some("2026-04-21T12:01:01Z".to_string()),
            },
        ],
        message_source_agent_dids: vec![
            Some("did:test:mini-1".to_string()),
            Some("did:test:mini-2".to_string()),
        ],
        ..ClientStoreRows::default()
    });

    let snapshot = build_session_snapshot_from_store_for_agent(
        &store,
        Some("did:test:mini-1"),
        "shared-session",
        None,
    )
    .expect("session snapshot");

    assert_eq!(snapshot.agent_did.as_deref(), Some("did:test:mini-1"));
    assert_eq!(snapshot.title.as_deref(), Some("mini-1 run"));
    assert_eq!(snapshot.messages.len(), 1);
    assert_eq!(snapshot.messages[0].message_key, "msg-mini-1");
}

#[test]
fn session_snapshot_exposes_provider_context_pressure_and_compaction_history() {
    let behavior: AgentBehaviorRow = serde_json::from_value(serde_json::json!({
        "behavior_id": "default",
        "agent_did": "did:test:amy",
        "inference_profile_id": "large-context",
        "compaction_strategy": "StripThenSummarize",
        "compaction_threshold": 0.57
    }))
    .expect("behavior row");
    let profile: InferenceProfileRow = serde_json::from_value(serde_json::json!({
        "profile_id": "large-context",
        "context_window": 10_000
    }))
    .expect("profile row");
    let compaction: CompactionEntryRow = serde_json::from_value(serde_json::json!({
        "compaction_key": "session-context:1",
        "session_id": "session-context",
        "sequence": 1,
        "summary": "The first turn established the durable plan.",
        "messages_compacted": 1,
        "compacted_through_sequence": 1,
        "original_tokens": 1_000,
        "compacted_tokens": 200,
        "created_at": "2026-08-24T12:00:00Z"
    }))
    .expect("compaction row");
    let messages = [
        "old turn ".repeat(300),
        "retained turn".into(),
        "latest turn".into(),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, text)| AgentMessageRow {
        message_key: format!("session-context:{}", index + 1),
        session_id: Some("session-context".to_string()),
        request_id: None,
        requester_did: None,
        sequence: Some((index + 1) as i64),
        role: Some("user".to_string()),
        content: Some(user_message_json(&text)),
        reasoning: None,
        timestamp: None,
    })
    .collect();
    let mut store = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "session-context".to_string(),
            agent_name: Some("Amy".to_string()),
            agent_did: Some("did:test:amy".to_string()),
            requester_did: None,
            behavior_id: Some("default".to_string()),
            title: None,
            title_source: None,
            preview_text: None,
            status: Some("active".to_string()),
            created_at: None,
            updated_at: None,
            latest_request_id: None,
        }],
        behaviors: vec![behavior],
        inference_profiles: vec![profile],
        messages,
        compaction_entries: vec![compaction],
        ..ClientStoreRows::default()
    });

    let mut snapshot = build_session_snapshot_from_store_for_agent(
        &store,
        Some("did:test:amy"),
        "session-context",
        None,
    )
    .expect("session snapshot");

    assert_eq!(snapshot.context.context_window, 10_000);
    assert_eq!(snapshot.context.compaction_threshold_tokens, 5_700);
    assert_eq!(snapshot.context.durable_message_count, 3);
    assert_eq!(snapshot.context.provider_message_count, 2);
    assert_eq!(snapshot.context.total_compacted_messages, 1);
    assert!(snapshot.context.estimated_conversation_tokens > 0);
    assert!(
        snapshot.context.estimated_durable_tokens > snapshot.context.estimated_conversation_tokens
    );
    assert_eq!(snapshot.context.compactions.len(), 1);
    assert_eq!(snapshot.context.compactions[0].original_tokens, Some(1_000));
    assert_eq!(snapshot.context.compactions[0].compacted_tokens, Some(200));

    attach_last_request_context(
        &mut snapshot,
        "request-context".to_string(),
        "call-context".to_string(),
        4,
        gents_protocol::rendered_request::ContextAccounting {
            accounting_version: gents_protocol::rendered_request::CONTEXT_ACCOUNTING_VERSION,
            turn_index: 3,
            attempt: 1,
            estimator: "serialized_json_bytes_div_4_v1".to_string(),
            components: gents_protocol::rendered_request::ContextInputComponents {
                messages: 4_000,
                documents: 100,
                tool_schemas: 1_200,
                additional_parameters: 50,
                output_schema: 350,
            },
            estimated_input_tokens: 5_700,
            context_window: 10_000,
            compaction_threshold_basis_points: 5_700,
            compaction_threshold_tokens: 5_700,
            configured_max_output_tokens: Some(2_000),
            effective_max_output_tokens: Some(2_000),
            compaction_reason:
                gents_protocol::rendered_request::ContextCompactionReason::BelowThreshold,
            pre_compaction_input_tokens: None,
        },
    );
    let last = snapshot
        .context
        .last_request
        .expect("last request accounting");
    assert_eq!(last.estimated_input_tokens, 5_700);
    assert_eq!((last.turn_index, last.attempt), (3, 1));
    assert_eq!(last.components.tool_schemas, 1_200);
    assert_eq!(last.compaction_reason, "below_threshold");

    store.messages[1].sequence = None;
    let unsequenced_projection = build_session_snapshot_from_store_for_agent(
        &store,
        Some("did:test:amy"),
        "session-context",
        None,
    )
    .expect("snapshot with partially replicated sequence");
    assert_eq!(
        unsequenced_projection.context.provider_message_count, 3,
        "a missing sequence must project durable rows without inferring a compaction boundary"
    );
}

#[test]
fn session_snapshot_exposes_pending_turn_when_latest_request_is_not_materialized() {
    let store = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "session-1".to_string(),
            agent_name: Some("Amy".to_string()),
            agent_did: Some("did:test:amy".to_string()),
            requester_did: None,
            behavior_id: Some("amy-default".to_string()),
            title: Some("architecture-review".to_string()),
            title_source: Some("generated".to_string()),
            preview_text: Some("follow up question".to_string()),
            status: Some("active".to_string()),
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            updated_at: Some("2026-04-21T12:02:00Z".to_string()),
            latest_request_id: Some("req-2".to_string()),
        }],
        requests: vec![
            AgentRequestRow {
                request_id: "req-1".to_string(),
                agent_did: Some("did:test:amy".to_string()),
                requester_did: None,
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("first question".to_string()),
                temperature: None,
                top_p: None,
                top_k: None,
                seed: None,
                max_tokens: None,
                max_total_tokens: None,
                metadata: None,
                lifecycle_state: Some("completed".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                terminalized_at: None,
                terminal_redrive_attempts: None,
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                caused_by_correlation: None,
            caused_by_trigger_context: None,
            caused_by_trigger_doc_id: None,
            caused_by_source_doc_id: None,
                caused_by_parent_request_id: None,
                interrupt_requested_at: None,
                valid_until: None,
                workspace_id: None,
                workspace_authority: None,
                workspace_owner_deployment_id: None,
                workspace_seal_hash: None,
            },
            AgentRequestRow {
                request_id: "req-2".to_string(),
                agent_did: Some("did:test:amy".to_string()),
                requester_did: None,
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("follow up question".to_string()),
                temperature: None,
                top_p: None,
                top_k: None,
                seed: None,
                max_tokens: None,
                max_total_tokens: None,
                metadata: None,
                lifecycle_state: Some("processing".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                terminalized_at: None,
                terminal_redrive_attempts: None,
                created_at: Some("2026-04-21T12:01:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                caused_by_correlation: None,
            caused_by_trigger_context: None,
            caused_by_trigger_doc_id: None,
            caused_by_source_doc_id: None,
                caused_by_parent_request_id: None,
                interrupt_requested_at: None,
                valid_until: None,
                workspace_id: None,
                workspace_authority: None,
                workspace_owner_deployment_id: None,
                workspace_seal_hash: None,
            },
        ],
        messages: vec![
            AgentMessageRow {
                message_key: "msg-1".to_string(),
                session_id: Some("session-1".to_string()),
                request_id: Some("req-1".to_string()),
                requester_did: None,
                sequence: Some(1),
                role: Some("user".to_string()),
                content: Some(user_message_json("first question")),
                reasoning: None,
                timestamp: Some("2026-04-21T12:00:00Z".to_string()),
            },
            AgentMessageRow {
                message_key: "msg-3".to_string(),
                session_id: Some("session-1".to_string()),
                request_id: Some("req-2".to_string()),
                requester_did: None,
                sequence: Some(3),
                role: Some("assistant".to_string()),
                content: Some(
                    r#"{"role":"assistant","content":[{"text":"continued after the partially replicated request"}]}"#
                        .to_string(),
                ),
                reasoning: None,
                timestamp: Some("2026-04-21T12:01:02Z".to_string()),
            },
        ],
        ..ClientStoreRows::default()
    });

    let snapshot =
        build_session_snapshot_from_store(&store, "session-1", None).expect("session snapshot");
    let pending = snapshot.pending_turn.expect("pending turn");
    assert_eq!(pending.request_id, "req-2");
    assert_eq!(pending.content, "follow up question");
    assert_eq!(pending.lifecycle_state.as_deref(), Some("processing"));
    let pending_position = snapshot
        .timeline_items
        .iter()
        .position(|item| matches!(item, RenderedTimelineItem::PendingUserTurn { .. }))
        .expect("request-owned pending user turn");
    let continued_position = snapshot
        .timeline_items
        .iter()
        .position(|item| {
            matches!(
                item,
                RenderedTimelineItem::AssistantMessage { content, .. }
                    if content.as_deref()
                        == Some("continued after the partially replicated request")
            )
        })
        .expect("later replicated assistant message");
    assert_eq!(pending_position + 1, continued_position);
}

#[test]
fn session_snapshot_hides_pending_turn_once_user_message_is_materialized() {
    let store = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "session-1".to_string(),
            agent_name: Some("Amy".to_string()),
            agent_did: Some("did:test:amy".to_string()),
            requester_did: None,
            behavior_id: Some("amy-default".to_string()),
            title: Some("architecture-review".to_string()),
            title_source: Some("generated".to_string()),
            preview_text: Some("follow up question".to_string()),
            status: Some("active".to_string()),
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            updated_at: Some("2026-04-21T12:02:00Z".to_string()),
            latest_request_id: Some("req-2".to_string()),
        }],
        requests: vec![AgentRequestRow {
            request_id: "req-2".to_string(),
            agent_did: Some("did:test:amy".to_string()),
            requester_did: None,
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("session-1".to_string()),
            retry_parent_request: None,
            retry_root_request: None,
            superseded_by_request: None,
            content: Some("follow up question".to_string()),
            temperature: None,
            top_p: None,
            top_k: None,
            seed: None,
            max_tokens: None,
            max_total_tokens: None,
            metadata: None,
            lifecycle_state: Some("processing".to_string()),
            backend_id: None,
            execution_origin: Some("interactive".to_string()),
            failure_reason: None,
            terminalized_at: None,
            terminal_redrive_attempts: None,
            created_at: Some("2026-04-21T12:01:00Z".to_string()),
            claimed_at: None,
            deadline: None,
            retry_count: Some(0),
            max_retries: Some(3),
            caused_by_trigger_id: None,
            caused_by_trigger_kind: None,
            caused_by_correlation: None,
            caused_by_trigger_context: None,
            caused_by_trigger_doc_id: None,
            caused_by_source_doc_id: None,
            caused_by_parent_request_id: None,
            interrupt_requested_at: None,
            valid_until: None,
            workspace_id: None,
            workspace_authority: None,
            workspace_owner_deployment_id: None,
            workspace_seal_hash: None,
        }],
        messages: vec![AgentMessageRow {
            message_key: "msg-2".to_string(),
            session_id: Some("session-1".to_string()),
            request_id: Some("req-2".to_string()),
            requester_did: None,
            sequence: Some(2),
            role: Some("user".to_string()),
            content: Some(user_message_json("follow up question")),
            reasoning: None,
            timestamp: Some("2026-04-21T12:01:01Z".to_string()),
        }],
        ..ClientStoreRows::default()
    });

    let snapshot =
        build_session_snapshot_from_store(&store, "session-1", None).expect("session snapshot");
    assert!(snapshot.pending_turn.is_none());
}

#[test]
fn unbound_replica_forks_do_not_swallow_a_later_pending_turn() {
    let store = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "session-1".to_string(),
            agent_name: Some("Amy".to_string()),
            agent_did: Some("did:test:amy".to_string()),
            requester_did: None,
            behavior_id: Some("amy-default".to_string()),
            title: Some("conversation".to_string()),
            title_source: Some("generated".to_string()),
            preview_text: Some("same prompt".to_string()),
            status: Some("active".to_string()),
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            updated_at: Some("2026-04-21T12:02:00Z".to_string()),
            latest_request_id: Some("req-2".to_string()),
        }],
        requests: vec![
            AgentRequestRow {
                request_id: "req-1".to_string(),
                agent_did: Some("did:test:amy".to_string()),
                requester_did: None,
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("same prompt".to_string()),
                temperature: None,
                top_p: None,
                top_k: None,
                seed: None,
                max_tokens: None,
                max_total_tokens: None,
                metadata: None,
                lifecycle_state: Some("completed".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                terminalized_at: None,
                terminal_redrive_attempts: None,
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                caused_by_correlation: None,
                caused_by_trigger_context: None,
                caused_by_trigger_doc_id: None,
                caused_by_source_doc_id: None,
                caused_by_parent_request_id: None,
                interrupt_requested_at: None,
                valid_until: None,
                workspace_id: None,
                workspace_authority: None,
                workspace_owner_deployment_id: None,
                workspace_seal_hash: None,
            },
            AgentRequestRow {
                request_id: "req-2".to_string(),
                agent_did: Some("did:test:amy".to_string()),
                requester_did: None,
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("same prompt".to_string()),
                temperature: None,
                top_p: None,
                top_k: None,
                seed: None,
                max_tokens: None,
                max_total_tokens: None,
                metadata: None,
                lifecycle_state: Some("processing".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                terminalized_at: None,
                terminal_redrive_attempts: None,
                created_at: Some("2026-04-21T12:01:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                caused_by_correlation: None,
                caused_by_trigger_context: None,
                caused_by_trigger_doc_id: None,
                caused_by_source_doc_id: None,
                caused_by_parent_request_id: None,
                interrupt_requested_at: None,
                valid_until: None,
                workspace_id: None,
                workspace_authority: None,
                workspace_owner_deployment_id: None,
                workspace_seal_hash: None,
            },
        ],
        messages: vec![
            AgentMessageRow {
                message_key: "unbound-msg-1".to_string(),
                session_id: Some("session-1".to_string()),
                request_id: None,
                requester_did: None,
                sequence: Some(1),
                role: Some("user".to_string()),
                content: Some(user_message_json("same prompt")),
                reasoning: None,
                timestamp: Some("2026-04-21T12:00:01Z".to_string()),
            },
            AgentMessageRow {
                message_key: "replica-fork-msg-1".to_string(),
                session_id: Some("session-1".to_string()),
                request_id: None,
                requester_did: None,
                sequence: Some(7),
                role: Some("user".to_string()),
                content: Some(user_message_json("same prompt")),
                reasoning: None,
                timestamp: Some("2026-04-21T12:00:02Z".to_string()),
            },
        ],
        ..ClientStoreRows::default()
    });

    let snapshot = build_session_snapshot_from_store(&store, "session-1", Some("req-2"))
        .expect("session snapshot");
    assert_eq!(
        snapshot
            .pending_turn
            .as_ref()
            .map(|turn| turn.request_id.as_str()),
        Some("req-2")
    );

    let mut materialized_rows = store.to_rows();
    materialized_rows.messages.push(AgentMessageRow {
        message_key: "unbound-msg-2".to_string(),
        session_id: Some("session-1".to_string()),
        request_id: None,
        requester_did: None,
        sequence: Some(9),
        role: Some("user".to_string()),
        content: Some(user_message_json("same prompt")),
        reasoning: None,
        timestamp: Some("2026-04-21T12:01:01Z".to_string()),
    });
    let materialized_store = ClientStore::from_rows(materialized_rows);
    let materialized =
        build_session_snapshot_from_store(&materialized_store, "session-1", Some("req-2"))
            .expect("materialized snapshot");
    assert_eq!(
        materialized
            .pending_turn
            .as_ref()
            .map(|turn| turn.request_id.as_str()),
        Some("req-2")
    );
}
