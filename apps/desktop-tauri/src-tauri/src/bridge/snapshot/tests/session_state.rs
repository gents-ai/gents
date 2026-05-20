use super::*;

#[path = "../../../../../../../crates/defra-agent/src/lean_vocab_test.rs"]
mod lean_vocab_test;

use lean_vocab_test::{
    lean_desktop_client_shell_cases, lean_request_lifecycle_operator_ui_cases, LeanClientShellCase,
};

#[test]
fn session_snapshot_can_be_built_without_conversation_row_when_session_is_observed() {
    let store = ClientStore::from_rows(ClientStoreRows {
        sessions: vec![AgentSessionRow {
            session_id: "session-1".to_string(),
            agent_name: Some("Amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            started: Some("2026-04-21T12:00:00Z".to_string()),
            ended: None,
            status: Some("active".to_string()),
        }],
        requests: vec![AgentRequestRow {
            request_id: "req-1".to_string(),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("session-1".to_string()),
            retry_parent_request: None,
            retry_root_request: None,
            superseded_by_request: None,
            content: Some("follow up question".to_string()),
            status: Some("completed".to_string()),
            lifecycle_state: Some("completed".to_string()),
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
            interrupt_requested_at: None,
            valid_until: None,
        }],
        responses: vec![AgentResponseRow {
            response_key: "resp-1".to_string(),
            request_id: Some("req-1".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("session-1".to_string()),
            content: Some("done".to_string()),
            reasoning: None,
            status: Some("complete".to_string()),
            error_message: None,
            token_count: Some(12),
            progress_seq: Some(1),
            materialized_message_sequence: Some(2),
            materialized_at: Some("2026-04-21T12:01:05Z".to_string()),
            created_at: Some("2026-04-21T12:01:01Z".to_string()),
            completed_at: Some("2026-04-21T12:01:05Z".to_string()),
            interrupted_at: None,
        }],
        ..ClientStoreRows::default()
    });

    let snapshot =
        build_session_snapshot_from_store(&store, "session-1", None).expect("session snapshot");
    assert_eq!(snapshot.session_id, "session-1");
    assert_eq!(snapshot.agent_did.as_deref(), Some("did:defra:amy"));
    assert_eq!(snapshot.behavior_id.as_deref(), Some("amy-default"));
    assert_eq!(snapshot.status.as_deref(), Some("active"));
    assert_eq!(snapshot.turn_state.as_deref(), Some("completed"));
    assert_eq!(snapshot.latest_request_id.as_deref(), Some("req-1"));
}

#[test]
fn session_snapshot_prefers_tracked_request_over_stale_conversation_latest_request() {
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
            latest_request_id: Some("req-1".to_string()),
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
                interrupt_requested_at: None,
                valid_until: None,
            },
        ],
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
        messages: vec![AgentMessageRow {
            message_key: "msg-1".to_string(),
            session_id: Some("session-1".to_string()),
            sequence: Some(1),
            role: Some("user".to_string()),
            content: Some(user_message_json("turn one")),
            timestamp: Some("2026-04-21T12:00:00Z".to_string()),
        }],
        ..ClientStoreRows::default()
    });

    let snapshot = build_session_snapshot_from_store(&store, "session-1", Some("req-2"))
        .expect("session snapshot");

    assert_eq!(snapshot.latest_request_id.as_deref(), Some("req-2"));
    assert_eq!(snapshot.turn_state.as_deref(), Some("streaming"));
    assert_eq!(
        snapshot
            .pending_turn
            .as_ref()
            .map(|turn| turn.request_id.as_str()),
        Some("req-2")
    );
    assert_eq!(
        snapshot
            .active_response_overlay
            .as_ref()
            .and_then(|response| response.content.as_deref()),
        Some("streaming reply")
    );
}

#[test]
fn session_snapshot_does_not_report_unobserved_preferred_request() {
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
            updated_at: Some("2026-04-21T12:00:01Z".to_string()),
            latest_request_id: Some("req-old".to_string()),
        }],
        requests: vec![AgentRequestRow {
            request_id: "req-old".to_string(),
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
        ..ClientStoreRows::default()
    });

    let snapshot = build_session_snapshot_from_store(&store, "session-1", Some("req-new"))
        .expect("session snapshot");

    assert_eq!(
        snapshot.latest_request_id.as_deref(),
        Some("req-old"),
        "Proofs.ClientShell.C9: an awaiting request retires only after the matching request is observed"
    );
    assert_eq!(snapshot.turn_state.as_deref(), Some("completed"));
    assert!(snapshot.pending_turn.is_none());
}

#[test]
fn session_snapshot_projection_consumes_generated_client_shell_contract_cases() {
    let cases = lean_desktop_client_shell_cases();
    assert_eq!(
        cases.len(),
        12,
        "desktop ClientShell contract surface should include every selected-session case"
    );

    for case in cases {
        let name = case.name.as_str();
        let store = client_shell_contract_store(case);
        let selected_session_id = contract_session_id(
            case.desktop_selected_session_id
                .expect("contract case should select a session"),
        );
        let preferred_request_id = case.desktop_preferred_request_id.map(contract_request_id);

        let snapshot = build_session_snapshot_from_store(
            &store,
            &selected_session_id,
            preferred_request_id.as_deref(),
        );

        assert_eq!(
            snapshot.is_some(),
            case.desktop_snapshot_present,
            "case {name} snapshot presence drifted from Lean-selected observation"
        );

        let Some(snapshot) = snapshot else {
            continue;
        };

        assert_eq!(
            snapshot.latest_request_id.as_deref(),
            case.desktop_expected_latest_request_id
                .map(contract_request_id)
                .as_deref(),
            "case {name} should project the Lean-observed latest request"
        );
        assert_eq!(
            snapshot.turn_state.as_deref(),
            case.desktop_expected_turn_state.as_deref(),
            "case {name} should project the Lean-derived turn state"
        );
        if let Some(expect_pending) = case.desktop_expect_pending_turn {
            assert_eq!(
                snapshot.pending_turn.is_some(),
                expect_pending,
                "case {name} pending-turn projection drifted from Lean"
            );
        }
    }
}

#[test]
fn session_snapshot_binds_request_lifecycle_operator_ui_cases() {
    let cases = lean_request_lifecycle_operator_ui_cases();
    assert!(
        !cases.is_empty(),
        "request-lifecycle operator UI contract cases should be emitted"
    );

    let mut saw_nonterminal_turn = false;
    let mut saw_terminal_turn = false;

    for case in cases {
        let name = case.name.as_str();
        let observed_turn = case
            .desktop_observed_turn_state
            .as_deref()
            .expect("request-lifecycle UI cases must observe a request turn");
        let (_request_status, lifecycle_state) = request_state_for_turn(Some(observed_turn));
        saw_nonterminal_turn |= matches!(observed_turn, "waitingForClaim" | "streaming");
        saw_terminal_turn |= !matches!(observed_turn, "waitingForClaim" | "streaming");

        let store = client_shell_contract_store(case);
        let selected_session_id = contract_session_id(
            case.desktop_selected_session_id
                .expect("request-lifecycle UI cases should select a session"),
        );
        let preferred_request_id = case.desktop_preferred_request_id.map(contract_request_id);
        let snapshot = build_session_snapshot_from_store(
            &store,
            &selected_session_id,
            preferred_request_id.as_deref(),
        )
        .expect("request-lifecycle UI case should build a desktop session snapshot");

        assert_eq!(
            snapshot.latest_request_id.as_deref(),
            case.desktop_expected_latest_request_id
                .map(contract_request_id)
                .as_deref(),
            "case {name} should bind the UI snapshot to the observed lifecycle request"
        );
        assert_eq!(
            snapshot.turn_state.as_deref(),
            Some(observed_turn),
            "case {name} should expose request lifecycle state as the UI turn state"
        );
        if let Some(expect_pending) = case.desktop_expect_pending_turn {
            assert_eq!(
                snapshot.pending_turn.is_some(),
                expect_pending,
                "case {name} pending-turn visibility drifted from lifecycle state"
            );
        }
        if let Some(pending_turn) = snapshot.pending_turn.as_ref() {
            assert_eq!(
                pending_turn.lifecycle_state.as_deref(),
                Some(lifecycle_state),
                "case {name} should carry the raw lifecycle state for UI badges"
            );
        }
    }

    assert!(
        saw_nonterminal_turn && saw_terminal_turn,
        "request-lifecycle UI cases should cover active and terminal turn bindings"
    );
}

#[test]
fn session_snapshot_stays_renderable_across_single_turn_observation_updates() {
    let submitted = ClientStore::from_rows(ClientStoreRows {
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
            updated_at: Some("2026-04-21T12:00:01Z".to_string()),
            latest_request_id: None,
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
            status: Some("pending".to_string()),
            lifecycle_state: Some("pending".to_string()),
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
            interrupt_requested_at: None,
            valid_until: None,
        }],
        ..ClientStoreRows::default()
    });
    let submitted_snapshot =
        build_session_snapshot_from_store(&submitted, "session-1", Some("req-1"))
            .expect("submitted snapshot");
    assert_eq!(
        submitted_snapshot.latest_request_id.as_deref(),
        Some("req-1")
    );
    assert_eq!(
        submitted_snapshot.turn_state.as_deref(),
        Some("waitingForClaim")
    );
    assert_eq!(
        submitted_snapshot
            .pending_turn
            .as_ref()
            .map(|turn| turn.request_id.as_str()),
        Some("req-1")
    );

    let streaming = ClientStore::from_rows(ClientStoreRows {
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
            updated_at: Some("2026-04-21T12:00:02Z".to_string()),
            latest_request_id: None,
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
            interrupt_requested_at: None,
            valid_until: None,
        }],
        responses: vec![AgentResponseRow {
            response_key: "resp-1".to_string(),
            request_id: Some("req-1".to_string()),
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
            created_at: Some("2026-04-21T12:00:01Z".to_string()),
            completed_at: None,
            interrupted_at: None,
        }],
        ..ClientStoreRows::default()
    });
    let streaming_snapshot =
        build_session_snapshot_from_store(&streaming, "session-1", Some("req-1"))
            .expect("streaming snapshot");
    assert_eq!(streaming_snapshot.turn_state.as_deref(), Some("streaming"));
    assert_eq!(
        streaming_snapshot
            .active_response_overlay
            .as_ref()
            .and_then(|response| response.content.as_deref()),
        Some("streaming reply")
    );

    let completed = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "session-1".to_string(),
            agent_name: Some("Amy".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            title: Some("conversation".to_string()),
            title_source: Some("generated".to_string()),
            preview_text: Some("final answer".to_string()),
            status: Some("completed".to_string()),
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            updated_at: Some("2026-04-21T12:00:05Z".to_string()),
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
            interrupt_requested_at: None,
            valid_until: None,
        }],
        responses: vec![AgentResponseRow {
            response_key: "resp-1".to_string(),
            request_id: Some("req-1".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("session-1".to_string()),
            content: Some("final answer".to_string()),
            reasoning: None,
            status: Some("complete".to_string()),
            error_message: None,
            token_count: Some(34),
            progress_seq: Some(2),
            materialized_message_sequence: Some(2),
            materialized_at: Some("2026-04-21T12:00:05Z".to_string()),
            created_at: Some("2026-04-21T12:00:01Z".to_string()),
            completed_at: Some("2026-04-21T12:00:05Z".to_string()),
            interrupted_at: None,
        }],
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
                    "{\"role\":\"assistant\",\"content\":[{\"text\":\"final answer\"}]}"
                        .to_string(),
                ),
                timestamp: Some("2026-04-21T12:00:05Z".to_string()),
            },
        ],
        ..ClientStoreRows::default()
    });
    let completed_snapshot =
        build_session_snapshot_from_store(&completed, "session-1", Some("req-1"))
            .expect("completed snapshot");
    assert_eq!(completed_snapshot.turn_state.as_deref(), Some("completed"));
    assert!(completed_snapshot.active_response_overlay.is_none());
    assert!(completed_snapshot.pending_turn.is_none());
}

fn contract_session_id(id: usize) -> String {
    format!("session-{id}")
}

fn contract_request_id(id: usize) -> String {
    format!("req-{id}")
}

fn client_shell_contract_store(case: &LeanClientShellCase) -> ClientStore {
    let session_id = contract_session_id(
        case.desktop_selected_session_id
            .expect("ClientShell desktop case should select a session"),
    );
    let observed_request_id = case.desktop_observed_request_id.map(contract_request_id);
    let turn_state = case.desktop_observed_turn_state.as_deref();
    let (request_status, lifecycle_state) = request_state_for_turn(turn_state);

    assert!(
        !case.frontend_conversation_present || case.desktop_snapshot_present,
        "case {} must not emit a conversation row without a desktop session observation",
        case.name
    );

    let mut rows = ClientStoreRows::default();

    if case.desktop_snapshot_present {
        rows.sessions.push(AgentSessionRow {
            session_id: session_id.clone(),
            agent_name: Some("Contract Agent".to_string()),
            behavior_id: Some("contract-behavior".to_string()),
            started: Some("2026-04-21T12:00:00Z".to_string()),
            ended: None,
            status: Some("active".to_string()),
        });
    }

    if case.frontend_conversation_present {
        rows.conversations.push(AgentConversationRow {
            session_id: session_id.clone(),
            agent_name: Some("Contract Agent".to_string()),
            agent_did: Some("did:defra:contract-agent".to_string()),
            behavior_id: Some("contract-behavior".to_string()),
            title: Some("contract conversation".to_string()),
            title_source: Some("generated".to_string()),
            preview_text: Some("contract prompt".to_string()),
            status: Some("active".to_string()),
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            updated_at: Some("2026-04-21T12:01:00Z".to_string()),
            latest_request_id: observed_request_id.clone(),
        });
    }

    if let Some(request_id) = observed_request_id {
        rows.requests.push(AgentRequestRow {
            request_id: request_id.clone(),
            agent_did: Some("did:defra:contract-agent".to_string()),
            behavior_id: Some("contract-behavior".to_string()),
            session_id: Some(session_id),
            retry_parent_request: None,
            retry_root_request: None,
            superseded_by_request: None,
            content: Some("contract prompt".to_string()),
            status: Some(request_status.to_string()),
            lifecycle_state: Some(lifecycle_state.to_string()),
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
            interrupt_requested_at: None,
            valid_until: None,
        });

        if let Some(response_status) = response_status_for_turn(turn_state) {
            rows.responses.push(AgentResponseRow {
                response_key: format!("resp-{request_id}"),
                request_id: Some(request_id),
                agent_did: Some("did:defra:contract-agent".to_string()),
                behavior_id: Some("contract-behavior".to_string()),
                session_id: rows.requests.last().and_then(|row| row.session_id.clone()),
                content: Some("contract response".to_string()),
                reasoning: None,
                status: Some(response_status.to_string()),
                error_message: None,
                token_count: Some(12),
                progress_seq: Some(1),
                materialized_message_sequence: None,
                materialized_at: None,
                created_at: Some("2026-04-21T12:01:01Z".to_string()),
                completed_at: None,
                interrupted_at: None,
            });
        }
    }

    ClientStore::from_rows(rows)
}

fn request_state_for_turn(turn_state: Option<&str>) -> (&'static str, &'static str) {
    match turn_state {
        Some("waitingForClaim") => ("pending", "pending"),
        Some("streaming") => ("processing", "processing"),
        Some("completed") => ("completed", "completed"),
        Some("failed") => ("failed", "failed"),
        Some("superseded") => ("superseded", "superseded"),
        Some("interrupted") => ("interrupted", "interrupted"),
        Some(other) => panic!("unsupported Lean ClientShell turn state {other:?}"),
        None => ("pending", "pending"),
    }
}

fn response_status_for_turn(turn_state: Option<&str>) -> Option<&'static str> {
    match turn_state {
        Some("streaming") => Some("streaming"),
        Some("completed") => Some("complete"),
        Some("failed") => Some("error"),
        Some("waitingForClaim") | Some("superseded") | Some("interrupted") | None => None,
        Some(other) => panic!("unsupported Lean ClientShell turn state {other:?}"),
    }
}
