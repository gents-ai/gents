use super::*;

fn make_streaming_store_with_response_content(content: &str) -> ClientStore {
    ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "sess-1".to_string(),
            agent_name: Some("Amy".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            title: Some("conversation".to_string()),
            title_source: Some("generated".to_string()),
            preview_text: Some("hello".to_string()),
            status: Some("active".to_string()),
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            updated_at: Some("2026-04-21T12:01:00Z".to_string()),
            latest_request_id: Some("req-1".to_string()),
        }],
        requests: vec![AgentRequestRow {
            request_id: "req-1".to_string(),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("sess-1".to_string()),
            retry_parent_request: None,
            retry_root_request: None,
            superseded_by_request: None,
            content: Some("hello".to_string()),
            status: Some("processing".to_string()),
            lifecycle_state: Some("processing".to_string()),
            backend_id: None,
            execution_origin: Some("interactive".to_string()),
            failure_reason: None,
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            claimed_at: None,
            deadline: None,
            retry_count: Some(0),
            max_retries: Some(3),
            caused_by_trigger_id: None,
            caused_by_trigger_kind: None,
            caused_by_parent_request_id: None,
            interrupt_requested_at: None,
            valid_until: None,
        }],
        messages: vec![AgentMessageRow {
            message_key: "msg-1".to_string(),
            session_id: Some("sess-1".to_string()),
            sequence: Some(1),
            role: Some("user".to_string()),
            content: Some(user_message_json("hello")),
            timestamp: Some("2026-04-21T12:00:00Z".to_string()),
        }],
        responses: vec![AgentResponseRow {
            response_key: "resp-1".to_string(),
            request_id: Some("req-1".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("sess-1".to_string()),
            content: Some(content.to_string()),
            reasoning: None,
            status: Some("streaming".to_string()),
            error_message: None,
            token_count: Some(4),
            progress_seq: Some(1),
            materialized_message_sequence: None,
            materialized_at: None,
            created_at: Some("2026-04-21T12:00:01Z".to_string()),
            completed_at: None,
            interrupted_at: None,
        }],
        ..ClientStoreRows::default()
    })
}

#[test]
fn overlay_hidden_when_response_tail_is_empty() {
    let store = make_streaming_store_with_response_content("");
    let snapshot = build_session_snapshot_from_store(&store, "sess-1", None).expect("snapshot");
    let has_live = snapshot
        .timeline_items
        .iter()
        .any(|item| matches!(item, RenderedTimelineItem::LiveAssistant { .. }));
    assert!(!has_live, "overlay must be hidden when tail is empty");
}

#[test]
fn session_snapshot_orders_pending_turn_before_orphan_tool_groups_and_live_overlay() {
    let store = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "session-1".to_string(),
            agent_name: Some("Amy".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            title: Some("conversation".to_string()),
            title_source: Some("generated".to_string()),
            preview_text: Some("turn two".to_string()),
            status: Some("active".to_string()),
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            updated_at: Some("2026-04-21T12:02:00Z".to_string()),
            latest_request_id: Some("req-2".to_string()),
        }],
        requests: vec![
            AgentRequestRow {
                request_id: "req-1".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("turn one".to_string()),
                status: Some("completed".to_string()),
                lifecycle_state: Some("completed".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                caused_by_parent_request_id: None,
                interrupt_requested_at: None,
                valid_until: None,
            },
            AgentRequestRow {
                request_id: "req-2".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("turn two".to_string()),
                status: Some("processing".to_string()),
                lifecycle_state: Some("processing".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:01:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                caused_by_parent_request_id: None,
                interrupt_requested_at: None,
                valid_until: None,
            },
        ],
        messages: vec![AgentMessageRow {
            message_key: "msg-1".to_string(),
            session_id: Some("session-1".to_string()),
            sequence: Some(1),
            role: Some("user".to_string()),
            content: Some(user_message_json("turn one")),
            timestamp: Some("2026-04-21T12:00:00Z".to_string()),
        }],
        responses: vec![AgentResponseRow {
            response_key: "resp-2".to_string(),
            request_id: Some("req-2".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("session-1".to_string()),
            content: Some("streaming reply".to_string()),
            reasoning: None,
            status: Some("streaming".to_string()),
            error_message: None,
            token_count: Some(12),
            progress_seq: Some(1),
            materialized_message_sequence: None,
            materialized_at: None,
            created_at: Some("2026-04-21T12:01:01Z".to_string()),
            completed_at: None,
            interrupted_at: None,
        }],
        tool_calls: vec![defra_agent_protocol::row::AgentToolCallRow {
            tool_call_key: "tool-1".to_string(),
            session_id: Some("session-1".to_string()),
            request_id: None,
            message_sequence: None,
            tool_name: Some("glob".to_string()),
            tool_call_id: Some("call-1".to_string()),
            args: Some("{\"pattern\":\"**/*.rs\"}".to_string()),
            result: None,
            status: Some("running".to_string()),
            lifecycle_state: None,
            cancel_policy: None,
            started_at: Some("2026-04-21T12:01:02Z".to_string()),
            deadline_at: None,
            completed_at: None,
            selected_service_id: None,
            selected_tool_name: None,
            tool_failure_class: None,
            cancel_cause: None,
            latency_ms: None,
        }],
        ..ClientStoreRows::default()
    });

    let snapshot = build_session_snapshot_from_store(&store, "session-1", Some("req-2"))
        .expect("session snapshot");
    let kinds = snapshot
        .timeline_items
        .iter()
        .map(|item| match item {
            RenderedTimelineItem::UserMessage { .. } => "user",
            RenderedTimelineItem::AssistantMessage { .. } => "assistant",
            RenderedTimelineItem::ToolGroup { .. } => "tools",
            RenderedTimelineItem::PendingUserTurn { .. } => "pending",
            RenderedTimelineItem::LiveAssistant { .. } => "live",
        })
        .collect::<Vec<_>>();
    assert_eq!(kinds, vec!["user", "pending", "tools", "live"]);
}

#[test]
fn session_snapshot_hides_failed_unmaterialized_response_overlay() {
    let store = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "session-1".to_string(),
            agent_name: Some("Amy".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            title: Some("conversation".to_string()),
            title_source: Some("generated".to_string()),
            preview_text: Some("turn one".to_string()),
            status: Some("active".to_string()),
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            updated_at: Some("2026-04-21T12:15:00Z".to_string()),
            latest_request_id: Some("req-1".to_string()),
        }],
        requests: vec![AgentRequestRow {
            request_id: "req-1".to_string(),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("session-1".to_string()),
            retry_parent_request: None,
            retry_root_request: None,
            superseded_by_request: None,
            content: Some("turn one".to_string()),
            status: Some("error".to_string()),
            lifecycle_state: Some("failed".to_string()),
            backend_id: None,
            execution_origin: Some("interactive".to_string()),
            failure_reason: Some("request deadline exceeded".to_string()),
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            claimed_at: Some("2026-04-21T12:00:01Z".to_string()),
            deadline: Some("2026-04-21T12:15:00Z".to_string()),
            retry_count: Some(0),
            max_retries: Some(3),
            caused_by_trigger_id: None,
            caused_by_trigger_kind: None,
            caused_by_parent_request_id: None,
            interrupt_requested_at: None,
            valid_until: None,
        }],
        messages: vec![AgentMessageRow {
            message_key: "msg-1".to_string(),
            session_id: Some("session-1".to_string()),
            sequence: Some(1),
            role: Some("user".to_string()),
            content: Some(user_message_json("turn one")),
            timestamp: Some("2026-04-21T12:00:00Z".to_string()),
        }],
        responses: vec![AgentResponseRow {
            response_key: "resp-1".to_string(),
            request_id: Some("req-1".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("session-1".to_string()),
            content: Some("partial answer before timeout".to_string()),
            reasoning: None,
            status: Some("error".to_string()),
            error_message: Some("request deadline exceeded".to_string()),
            token_count: Some(12),
            progress_seq: Some(3),
            materialized_message_sequence: None,
            materialized_at: None,
            created_at: Some("2026-04-21T12:00:02Z".to_string()),
            completed_at: Some("2026-04-21T12:15:00Z".to_string()),
            interrupted_at: None,
        }],
        ..ClientStoreRows::default()
    });

    let snapshot = build_session_snapshot_from_store(&store, "session-1", Some("req-1"))
        .expect("session snapshot");

    assert_eq!(snapshot.turn_state.as_deref(), Some("failed"));
    assert_eq!(
        snapshot
            .latest_response
            .as_ref()
            .and_then(|response| response.error_message.as_deref()),
        Some("request deadline exceeded")
    );
    let serialized = serde_json::to_value(&snapshot).expect("serialize snapshot");
    assert_eq!(
        serialized["latestResponse"]["errorMessage"],
        "request deadline exceeded"
    );
    assert!(snapshot.active_response_overlay.is_none());

    let has_live = snapshot
        .timeline_items
        .iter()
        .any(|item| matches!(item, RenderedTimelineItem::LiveAssistant { .. }));
    assert!(!has_live, "failed turns must not render live overlays");
}

#[test]
fn session_snapshot_keeps_full_live_overlay_when_only_prior_turn_shares_prefix() {
    let store = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "session-1".to_string(),
            agent_name: Some("Amy".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            title: Some("conversation".to_string()),
            title_source: Some("generated".to_string()),
            preview_text: Some("turn two".to_string()),
            status: Some("active".to_string()),
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            updated_at: Some("2026-04-21T12:02:00Z".to_string()),
            latest_request_id: Some("req-2".to_string()),
        }],
        requests: vec![
            AgentRequestRow {
                request_id: "req-1".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("turn one".to_string()),
                status: Some("completed".to_string()),
                lifecycle_state: Some("completed".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                caused_by_parent_request_id: None,
                interrupt_requested_at: None,
                valid_until: None,
            },
            AgentRequestRow {
                request_id: "req-2".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("turn two".to_string()),
                status: Some("processing".to_string()),
                lifecycle_state: Some("processing".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:01:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                caused_by_parent_request_id: None,
                interrupt_requested_at: None,
                valid_until: None,
            },
        ],
        messages: vec![
            AgentMessageRow {
                message_key: "msg-1".to_string(),
                session_id: Some("session-1".to_string()),
                sequence: Some(1),
                role: Some("user".to_string()),
                content: Some(user_message_json("turn one")),
                timestamp: Some("2026-04-21T12:00:00Z".to_string()),
            },
            AgentMessageRow {
                message_key: "msg-2".to_string(),
                session_id: Some("session-1".to_string()),
                sequence: Some(2),
                role: Some("assistant".to_string()),
                content: Some(
                    serde_json::to_string(&Message::assistant("I'll investigate"))
                        .expect("serialize assistant"),
                ),
                timestamp: Some("2026-04-21T12:00:01Z".to_string()),
            },
            AgentMessageRow {
                message_key: "msg-3".to_string(),
                session_id: Some("session-1".to_string()),
                sequence: Some(3),
                role: Some("user".to_string()),
                content: Some(user_message_json("turn two")),
                timestamp: Some("2026-04-21T12:01:00Z".to_string()),
            },
        ],
        responses: vec![AgentResponseRow {
            response_key: "resp-2".to_string(),
            request_id: Some("req-2".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("session-1".to_string()),
            content: Some("I'll investigate further into p2p".to_string()),
            reasoning: None,
            status: Some("streaming".to_string()),
            error_message: None,
            token_count: Some(12),
            progress_seq: Some(1),
            materialized_message_sequence: None,
            materialized_at: None,
            created_at: Some("2026-04-21T12:01:01Z".to_string()),
            completed_at: None,
            interrupted_at: None,
        }],
        ..ClientStoreRows::default()
    });

    let snapshot = build_session_snapshot_from_store(&store, "session-1", Some("req-2"))
        .expect("session snapshot");
    let live_content = snapshot.timeline_items.iter().find_map(|item| match item {
        RenderedTimelineItem::LiveAssistant { content, .. } => content.as_deref(),
        _ => None,
    });
    assert_eq!(live_content, Some("I'll investigate further into p2p"));
}

#[test]
fn session_snapshot_renders_structured_tool_payloads_in_timeline() {
    let store = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "session-1".to_string(),
            agent_name: Some("Amy".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            title: Some("conversation".to_string()),
            title_source: Some("generated".to_string()),
            preview_text: Some("turn one".to_string()),
            status: Some("active".to_string()),
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            updated_at: Some("2026-04-21T12:02:00Z".to_string()),
            latest_request_id: Some("req-1".to_string()),
        }],
        requests: vec![AgentRequestRow {
            request_id: "req-1".to_string(),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("session-1".to_string()),
            retry_parent_request: None,
            retry_root_request: None,
            superseded_by_request: None,
            content: Some("turn one".to_string()),
            status: Some("processing".to_string()),
            lifecycle_state: Some("processing".to_string()),
            backend_id: None,
            execution_origin: Some("interactive".to_string()),
            failure_reason: None,
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            claimed_at: None,
            deadline: None,
            retry_count: Some(0),
            max_retries: Some(3),
            caused_by_trigger_id: None,
            caused_by_trigger_kind: None,
            caused_by_parent_request_id: None,
            interrupt_requested_at: None,
            valid_until: None,
        }],
        messages: vec![AgentMessageRow {
            message_key: "msg-1".to_string(),
            session_id: Some("session-1".to_string()),
            sequence: Some(1),
            role: Some("user".to_string()),
            content: Some(user_message_json("turn one")),
            timestamp: Some("2026-04-21T12:00:00Z".to_string()),
        }],
        tool_calls: vec![defra_agent_protocol::row::AgentToolCallRow {
            tool_call_key: "tool-1".to_string(),
            session_id: Some("session-1".to_string()),
            request_id: None,
            message_sequence: Some(2),
            tool_name: Some("glob".to_string()),
            tool_call_id: Some("call-1".to_string()),
            args: Some("{\"pattern\":\"**/*.rs\",\"recursive\":true}".to_string()),
            result: Some("{\"matches\":12}".to_string()),
            status: Some("completed".to_string()),
            lifecycle_state: None,
            cancel_policy: None,
            started_at: Some("2026-04-21T12:00:01Z".to_string()),
            deadline_at: None,
            completed_at: Some("2026-04-21T12:00:02Z".to_string()),
            selected_service_id: None,
            selected_tool_name: None,
            tool_failure_class: None,
            cancel_cause: None,
            latency_ms: None,
        }],
        ..ClientStoreRows::default()
    });

    let snapshot = build_session_snapshot_from_store(&store, "session-1", Some("req-1"))
        .expect("session snapshot");
    let tools = snapshot
        .timeline_items
        .iter()
        .find_map(|item| match item {
            RenderedTimelineItem::ToolGroup { tools, .. } => Some(tools),
            _ => None,
        })
        .expect("tool group");

    assert_eq!(tools.len(), 1);
    let tool = &tools[0];
    assert_eq!(tool.tool_name, "glob");
    assert_eq!(tool.status_kind, "success");
    assert_eq!(
        tool.args.as_ref().map(|value| value
            .fields
            .iter()
            .map(|field| field.key.as_str())
            .collect::<Vec<_>>()),
        Some(vec!["pattern", "recursive"])
    );
    assert_eq!(
        tool.result
            .as_ref()
            .and_then(|value| value.fields.iter().find(|field| field.key == "matches"))
            .map(|field| field.value.as_str()),
        Some("12")
    );
}
