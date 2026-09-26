use super::*;
use crate::types::ReconstructionState;
use gents_protocol::request_lifecycle::RequestLifecycleState;

#[path = "../../../../../crates/gents/src/lean_vocab_test/support.rs"]
mod lean_vocab_test;

use lean_vocab_test::{
    lean_desktop_client_shell_cases, lean_live_overlay_cases,
    lean_request_lifecycle_operator_ui_cases, lean_transcript_cases,
};

fn session(requester_did: Option<&str>) -> AgentSession {
    AgentSession {
        session_id: "session-1".into(),
        agent_did: "did:test:amy".into(),
        requester_did: requester_did.map(str::to_owned),
        behavior_id: "amy-default".into(),
        created_at: "2026-04-21T12:00:00Z".into(),
        closed_at: None,
        title: Some(SessionTitle {
            text: "conversation".into(),
            source: SessionTitleSource::Generated,
        }),
        tags: Vec::new(),
        provenance: None,
        observation: Some(SessionObservation {
            last_activity_at: "2026-04-21T12:02:00Z".into(),
            preview: Some("turn two".into()),
            latest_request: Some(SessionRequestObservation {
                request_doc_id: "req-2".into(),
                request_id: "req-2".into(),
                lifecycle_state: RequestLifecycleState::Processing,
            }),
        }),
    }
}

fn request(id: &str, state: RequestLifecycleState) -> AgentRequestRow {
    AgentRequestRow {
        purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
        doc_id: Some(id.into()),
        request_id: id.into(),
        agent_did: Some("did:test:amy".into()),
        behavior_id: Some("amy-default".into()),
        session_id: Some("session-1".into()),
        content: Some(format!("{id} prompt")),
        lifecycle_state: Some(state),
        execution_origin: Some("interactive".into()),
        created_at: Some("2026-04-21T12:00:00Z".into()),
        ..Default::default()
    }
}

fn contract_session_id(id: usize) -> String {
    format!("session-{id}")
}

fn contract_request_id(id: usize) -> String {
    format!("req-{id}")
}

fn request_state_for_turn(turn_state: Option<&str>) -> RequestLifecycleState {
    match turn_state {
        Some("waitingForClaim") => RequestLifecycleState::Pending,
        Some("running") => RequestLifecycleState::Processing,
        Some("completed") => RequestLifecycleState::Completed,
        Some("failed") => RequestLifecycleState::Failed,
        Some("superseded") => RequestLifecycleState::Superseded,
        Some("interrupted") => RequestLifecycleState::Interrupted,
        Some(other) => panic!("unsupported Lean ClientShell turn state {other:?}"),
        None => RequestLifecycleState::Pending,
    }
}

fn client_shell_contract_store(case: &lean_vocab_test::LeanClientShellCase) -> ClientStore {
    let mut rows = ClientStoreRows::default();
    let session_id = contract_session_id(
        case.desktop_selected_session_id
            .expect("desktop contract case should select a session"),
    );
    let request_id = case.desktop_observed_request_id.map(contract_request_id);
    if case.desktop_snapshot_present {
        rows.sessions.push(AgentSession {
            session_id: session_id.clone(),
            agent_did: "did:test:contract-agent".into(),
            requester_did: None,
            behavior_id: "contract-behavior".into(),
            created_at: "2026-04-21T12:00:00Z".into(),
            closed_at: None,
            title: None,
            tags: Vec::new(),
            provenance: None,
            observation: request_id.clone().map(|request_id| SessionObservation {
                last_activity_at: "2026-04-21T12:01:00Z".into(),
                preview: Some("contract prompt".into()),
                latest_request: Some(SessionRequestObservation {
                    request_doc_id: request_id.clone(),
                    request_id,
                    lifecycle_state: request_state_for_turn(
                        case.desktop_observed_turn_state.as_deref(),
                    ),
                }),
            }),
        });
    }
    if let Some(request_id) = request_id {
        rows.requests.push(AgentRequestRow {
            purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
            doc_id: Some(request_id.clone()),
            request_id,
            agent_did: Some("did:test:contract-agent".into()),
            behavior_id: Some("contract-behavior".into()),
            session_id: Some(session_id),
            content: Some("contract prompt".into()),
            lifecycle_state: Some(request_state_for_turn(
                case.desktop_observed_turn_state.as_deref(),
            )),
            execution_origin: Some("interactive".into()),
            ..Default::default()
        });
    }
    ClientStore::from_rows(rows)
}

#[test]
fn session_snapshot_projects_durable_goal_state() {
    let goal = GoalRow {
        goal_id: "goal-1".into(),
        creation_key: None,
        session_id: "session-1".into(),
        agent_did: "did:test:amy".into(),
        objective: Some("Ship the durable controller".into()),
        tags: Vec::new(),
        status: Some("active".into()),
        token_budget: Some(50_000),
        tokens_used: Some(1_200),
        active_time_seconds: Some(42),
        active_started_at: None,
        consecutive_blocked_audits: Some(2),
        last_blocked_request_id: None,
        last_blocked_reason: Some("needs approval".into()),
        last_continued_from_request_id: None,
        continuation_sequence: Some(3),
        wrapup_requested: Some(false),
        wrapup_completed: Some(false),
        infrastructure_retry_count: Some(0),
        last_failure: None,
        completion_evidence: None,
        created_at: None,
        updated_at: None,
    };
    let snapshot = build_session_snapshot_from_store_for_agent(
        &ClientStore::from_rows(ClientStoreRows {
            sessions: vec![session(None)],
            goals: vec![goal],
            ..ClientStoreRows::default()
        }),
        Some("did:test:amy"),
        "session-1",
        None,
    )
    .expect("snapshot");
    let goal = snapshot.goal.expect("goal");
    assert_eq!(
        goal.objective.as_deref(),
        Some("Ship the durable controller")
    );
    assert_eq!(
        (
            goal.tokens_used,
            goal.consecutive_blocked_audits,
            goal.continuation_sequence
        ),
        (1_200, 2, 3)
    );
}

#[test]
fn session_observation_selects_the_exact_latest_request_identity() {
    let rows = ClientStoreRows {
        sessions: vec![session(None)],
        requests: vec![
            request("req-1", RequestLifecycleState::Completed),
            request("req-2", RequestLifecycleState::Processing),
        ],
        ..ClientStoreRows::default()
    };
    let snapshot =
        build_session_snapshot_from_store(&ClientStore::from_rows(rows), "session-1", None)
            .expect("snapshot");
    assert_eq!(snapshot.latest_request_id.as_deref(), Some("req-2"));
    assert_eq!(snapshot.turn_state.as_deref(), Some("running"));
}

#[test]
fn requester_scope_does_not_cross_canonical_output_facts() {
    let mut rows = ClientStoreRows {
        sessions: vec![session(None), session(Some("did:test:other"))],
        ..ClientStoreRows::default()
    };
    push_canonical_text_message_for_agent(
        &mut rows,
        "owner",
        "session-1",
        None,
        1,
        MessageRole::User,
        "owner prompt",
        "did:test:amy",
        None,
    );
    push_canonical_text_message_for_agent(
        &mut rows,
        "other",
        "session-1",
        None,
        2,
        MessageRole::User,
        "other prompt",
        "did:test:amy",
        Some("did:test:other"),
    );
    let store = ClientStore::from_rows(rows);
    let snapshot = build_session_snapshot_from_store_for_agent(
        &store,
        Some("did:test:amy"),
        "session-1",
        None,
    )
    .expect("snapshot");
    assert_eq!(snapshot.messages.len(), 1);
    assert_eq!(snapshot.messages[0].message_key, "owner");
}

#[test]
fn canonical_transcript_keeps_partial_reconstruction_explicit() {
    let mut rows = ClientStoreRows {
        sessions: vec![session(None)],
        requests: vec![request("req-1", RequestLifecycleState::Processing)],
        ..ClientStoreRows::default()
    };
    push_canonical_text_message(
        &mut rows,
        "incomplete",
        "session-1",
        Some("req-1"),
        1,
        MessageRole::Assistant,
        "must not be fabricated",
    );
    rows.output_segments.clear();
    let snapshot =
        build_session_snapshot_from_store(&ClientStore::from_rows(rows), "session-1", None)
            .expect("snapshot");
    assert!(
        matches!(snapshot.timeline_items.as_slice(), [RenderedTimelineItem::AssistantMessage { reconstruction, content: None, .. }] if reconstruction.state == ReconstructionState::Loading)
    );
}

#[test]
fn session_snapshot_uses_canonical_session_without_materialized_observation() {
    let mut session = session(None);
    session.observation = None;
    let store = ClientStore::from_rows(ClientStoreRows {
        sessions: vec![session],
        requests: vec![request("req-1", RequestLifecycleState::Completed)],
        ..ClientStoreRows::default()
    });
    let snapshot = build_session_snapshot_from_store(&store, "session-1", None).expect("snapshot");
    assert_eq!(snapshot.session_id, "session-1");
    assert_eq!(snapshot.agent_did.as_deref(), Some("did:test:amy"));
    assert_eq!(snapshot.turn_state.as_deref(), Some("completed"));
}

#[test]
fn session_snapshot_prefers_tracked_request_over_stale_session_latest_request() {
    let mut old = request("req-1", RequestLifecycleState::Completed);
    old.created_at = Some("2026-04-21T12:00:00Z".into());
    let mut latest = request("req-2", RequestLifecycleState::Processing);
    latest.created_at = Some("2026-04-21T12:01:00Z".into());
    let snapshot = build_session_snapshot_from_store(
        &ClientStore::from_rows(ClientStoreRows {
            sessions: vec![session(None)],
            requests: vec![old, latest],
            ..ClientStoreRows::default()
        }),
        "session-1",
        None,
    )
    .expect("snapshot");
    assert_eq!(snapshot.latest_request_id.as_deref(), Some("req-2"));
    assert_eq!(snapshot.turn_state.as_deref(), Some("running"));
}

#[test]
fn session_snapshot_does_not_report_unobserved_preferred_request() {
    let mut session = session(None);
    session
        .observation
        .as_mut()
        .expect("observation")
        .latest_request = Some(SessionRequestObservation {
        request_doc_id: "req-old".into(),
        request_id: "req-old".into(),
        lifecycle_state: RequestLifecycleState::Processing,
    });
    let old = AgentRequestRow {
        purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
        doc_id: Some("req-old".into()),
        request_id: "req-old".into(),
        agent_did: Some("did:test:amy".into()),
        session_id: Some("session-1".into()),
        lifecycle_state: Some(RequestLifecycleState::Completed),
        ..Default::default()
    };
    let snapshot = build_session_snapshot_from_store(
        &ClientStore::from_rows(ClientStoreRows {
            sessions: vec![session],
            requests: vec![old],
            ..ClientStoreRows::default()
        }),
        "session-1",
        Some("req-new"),
    )
    .expect("snapshot");
    assert_eq!(snapshot.latest_request_id.as_deref(), Some("req-old"));
    assert_eq!(snapshot.turn_state.as_deref(), Some("completed"));
}

#[test]
fn session_snapshot_projection_consumes_generated_client_shell_contract_cases() {
    let cases = lean_desktop_client_shell_cases();
    assert_eq!(cases.len(), 22);
    for case in cases {
        let session_id = contract_session_id(
            case.desktop_selected_session_id
                .expect("desktop case selects a session"),
        );
        let preferred = case.desktop_preferred_request_id.map(contract_request_id);
        let snapshot = build_session_snapshot_from_store(
            &client_shell_contract_store(case),
            &session_id,
            preferred.as_deref(),
        );
        assert_eq!(
            snapshot.is_some(),
            case.desktop_snapshot_present,
            "{}",
            case.name
        );
        if let Some(snapshot) = snapshot {
            assert_eq!(
                snapshot.latest_request_id.as_deref(),
                case.desktop_expected_latest_request_id
                    .map(contract_request_id)
                    .as_deref(),
                "{}",
                case.name
            );
            assert_eq!(
                snapshot.turn_state.as_deref(),
                case.desktop_expected_turn_state.as_deref(),
                "{}",
                case.name
            );
            if let Some(expected_pending) = case.desktop_expect_pending_turn {
                assert_eq!(
                    snapshot.pending_turn.is_some(),
                    expected_pending,
                    "{}",
                    case.name
                );
            }
        }
    }
}

#[test]
fn session_snapshot_binds_request_lifecycle_operator_ui_cases() {
    let cases = lean_request_lifecycle_operator_ui_cases();
    let mut saw_active = false;
    let mut saw_terminal = false;
    for case in cases {
        let session_id = contract_session_id(
            case.desktop_selected_session_id
                .expect("operator UI case selects a session"),
        );
        let expected = case
            .desktop_observed_turn_state
            .as_deref()
            .expect("operator UI case observes a turn");
        saw_active |= matches!(expected, "waitingForClaim" | "running");
        saw_terminal |= !matches!(expected, "waitingForClaim" | "running");
        let preferred = case.desktop_preferred_request_id.map(contract_request_id);
        let snapshot = build_session_snapshot_from_store(
            &client_shell_contract_store(case),
            &session_id,
            preferred.as_deref(),
        )
        .expect("snapshot");
        assert_eq!(
            snapshot.turn_state.as_deref(),
            Some(expected),
            "{}",
            case.name
        );
        if let Some(pending) = snapshot.pending_turn.as_ref() {
            assert_eq!(
                pending.lifecycle_state.as_deref(),
                Some(request_state_for_turn(Some(expected)).as_str()),
                "{}",
                case.name
            );
        }
    }
    assert!(saw_active && saw_terminal);
}

#[test]
fn session_snapshot_live_overlay_consumes_generated_contract_cases() {
    let cases = lean_live_overlay_cases();
    for case in cases {
        let lifecycle = if case.turn_terminal {
            RequestLifecycleState::Completed
        } else {
            RequestLifecycleState::Processing
        };
        let mut request = request("req-2", lifecycle);
        request.doc_id = Some("req-2".into());
        // `has_durable_owner` is the timeline's materialized user-message
        // owner, not provider-source liveness. A supplied live output already
        // carries the current execution owner.
        request.execution_generation = case
            .live_output_available
            .then(|| "generation-1".to_string());
        request.execution_lease_secs = case.live_output_available.then_some(300);
        request.execution_lease_expires_at = case
            .live_output_available
            .then(|| "2026-04-21T12:05:00Z".to_string());
        let mut rows = ClientStoreRows {
            sessions: vec![session(None)],
            requests: vec![request],
            ..ClientStoreRows::default()
        };
        if case.has_durable_owner {
            push_canonical_text_message(
                &mut rows,
                &format!("{}-owner", case.name),
                "session-1",
                Some("req-2"),
                1,
                MessageRole::Assistant,
                "materialized answer",
            );
        }
        if case.live_output_available && !case.has_durable_owner {
            let mut runs = Vec::new();
            let mut payload = String::new();
            if case.has_content {
                payload.push('C');
                runs.push(SegmentRun {
                    stream: 0,
                    bytes: 1,
                    declaration: Some(StreamDeclaration {
                        block_index: 0,
                        part_index: 0,
                        payload: StreamPayload::Text,
                    }),
                });
            }
            if case.has_reasoning {
                payload.push('R');
                runs.push(SegmentRun {
                    stream: u32::from(case.has_content),
                    bytes: 1,
                    declaration: Some(StreamDeclaration {
                        block_index: 0,
                        part_index: u32::from(case.has_content),
                        payload: StreamPayload::Reasoning,
                    }),
                });
            }
            rows.output_segments.push(OutputSegmentRow {
                doc_id: format!("{}-open", case.name),
                segment: OutputSegment {
                    agent_did: "did:test:amy".into(),
                    requester_did: None,
                    session_id: "session-1".into(),
                    request_doc_id: "req-2".into(),
                    source: OutputSource::ProviderTurn {
                        scope: gents_protocol::rendered_request::CaptureScope {
                            kind: gents_protocol::rendered_request::CaptureScopeKind::Inference,
                            seq: 0,
                        },
                        turn_index: 0,
                        attempt: 0,
                    },
                    writer: OutputWriter::RequestExecution {
                        execution_generation: "generation-1".into(),
                    },
                    ordinal: Some(0),
                    runs,
                    payload,
                    close: None,
                    created_at: "2026-04-21T12:00:00Z".into(),
                },
            });
        }
        let snapshot = build_session_snapshot_from_store(
            &ClientStore::from_rows(rows),
            "session-1",
            Some("req-2"),
        )
        .unwrap_or_else(|| panic!("case {} should produce a snapshot", case.name));
        assert_eq!(
            snapshot
                .timeline_items
                .iter()
                .any(|item| matches!(item, RenderedTimelineItem::LiveAssistant { .. })),
            case.expect_overlay,
            "{}",
            case.name
        );
    }
}

#[test]
fn session_snapshot_transcript_rendering_consumes_generated_transcript_cases() {
    let cases = lean_transcript_cases();
    assert_eq!(cases.len(), 11);
    for case in cases {
        let mut rows = ClientStoreRows {
            sessions: vec![session(None)],
            ..ClientStoreRows::default()
        };
        for sequence in 0..case.post_message_count {
            push_canonical_text_message(
                &mut rows,
                &format!("{}-message-{sequence}", case.name),
                "session-1",
                Some("req-1"),
                u32::try_from(sequence + 1).expect("contract sequence"),
                if sequence == 0 {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                },
                &format!("{} payload {sequence}", case.name),
            );
        }
        for index in 0..case.post_tool_call_count {
            rows.tool_calls.push(
                serde_json::from_value(serde_json::json!({
                    "_docID": format!("{}-tool-{index}", case.name),
                    "agent_did": "did:test:amy",
                    "request_doc_id": "req-1",
                    "tool_call_key": format!("{}-tool-{index}", case.name),
                    "session_id": "session-1",
                    "request_id": "req-1",
                    "message_sequence": case.assistant_sequence,
                    "tool_name": "read",
                    "tool_call_id": format!("result-{}", case.logical_result_id + index),
                    "status": if case.expected_pair_closed { "completed" } else { "running" },
                    "lifecycle_state": if case.expected_pair_closed { "completed" } else { "running" }
                }))
                .expect("canonical tool-call envelope"),
            );
        }
        let snapshot = build_session_snapshot_from_store(
            &ClientStore::from_rows(rows),
            "session-1",
            Some("req-1"),
        )
        .unwrap_or_else(|| panic!("case {} should produce a snapshot", case.name));
        assert_eq!(
            snapshot.messages.len(),
            case.post_message_count,
            "{}",
            case.name
        );
        assert_eq!(
            snapshot.tool_calls.len(),
            case.post_tool_call_count,
            "{}",
            case.name
        );
        assert_eq!(
            snapshot
                .timeline_items
                .iter()
                .filter_map(|item| match item {
                    RenderedTimelineItem::ToolGroup {
                        message_sequence,
                        tools,
                        ..
                    } => {
                        Some((*message_sequence, tools.len()))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>(),
            if case.post_tool_call_count == 0 {
                Vec::new()
            } else {
                vec![(
                    Some(case.assistant_sequence as i64),
                    case.post_tool_call_count,
                )]
            },
            "{} tool pairing/group sequence drifted",
            case.name
        );
        assert!(
            case.expected_ordered,
            "{} must retain transcript ordering",
            case.name
        );
        if case.expected_duplicate_reused_sequence {
            assert_eq!(
                case.pre_message_count, case.post_message_count,
                "{} duplicate observation must not append a message",
                case.name
            );
            assert_eq!(
                case.result_sequence, case.post_message_count,
                "{} must retain the original tool-result sequence",
                case.name
            );
        }
    }
}

#[test]
fn session_snapshot_stays_renderable_across_single_turn_observation_updates() {
    let mut submitted = session(None);
    submitted
        .observation
        .as_mut()
        .expect("observation")
        .latest_request
        .as_mut()
        .expect("request")
        .lifecycle_state = RequestLifecycleState::Pending;
    let pending = build_session_snapshot_from_store(
        &ClientStore::from_rows(ClientStoreRows {
            sessions: vec![submitted],
            requests: vec![request("req-2", RequestLifecycleState::Pending)],
            ..ClientStoreRows::default()
        }),
        "session-1",
        Some("req-2"),
    )
    .expect("pending");
    assert_eq!(pending.turn_state.as_deref(), Some("waitingForClaim"));
    let complete = build_session_snapshot_from_store(
        &ClientStore::from_rows(ClientStoreRows {
            sessions: vec![session(None)],
            requests: vec![request("req-2", RequestLifecycleState::Completed)],
            ..ClientStoreRows::default()
        }),
        "session-1",
        Some("req-2"),
    )
    .expect("complete");
    assert_eq!(complete.turn_state.as_deref(), Some("completed"));
}

#[test]
fn session_snapshot_derives_cancel_causes_from_request_and_tool() {
    let mut interrupted = request("req-2", RequestLifecycleState::Interrupted);
    interrupted.failure_reason = Some("completion cancelled".into());
    interrupted.interrupt_requested_at = Some("2026-04-21T12:03:00Z".into());
    let snapshot = build_session_snapshot_from_store(
        &ClientStore::from_rows(ClientStoreRows {
            sessions: vec![session(None)],
            requests: vec![interrupted],
            tool_calls: vec![serde_json::from_value(serde_json::json!({
                "_docID": "cancelled-tool-doc",
                "tool_call_key": "cancelled-tool",
                "tool_call_id": "cancelled-call",
                "agent_did": "did:test:amy",
                "session_id": "session-1",
                "request_id": "req-2",
                "request_doc_id": "req-2",
                "tool_name": "bash",
                "lifecycle_state": "cancelled",
                "completed_at": "2026-04-21T12:03:01Z"
            }))
            .expect("cancelled tool lifecycle row")],
            ..ClientStoreRows::default()
        }),
        "session-1",
        Some("req-2"),
    )
    .expect("snapshot");
    assert_eq!(snapshot.turn_state.as_deref(), Some("interrupted"));
    let outcome = snapshot.latest_request_outcome.expect("request outcome");
    assert_eq!(
        outcome.failure_reason.as_deref(),
        Some("completion cancelled")
    );
    let cause = outcome.cancel_cause.expect("cancel cause");
    assert_eq!(cause.cause, "userCancelled");
    assert_eq!(cause.source, "requestInterrupt");
    assert_eq!(snapshot.tool_calls.len(), 1);
    let tool_cause = snapshot.tool_calls[0]
        .cancel_cause
        .as_ref()
        .expect("cancelled tool retains request interrupt evidence");
    assert_eq!(tool_cause.cause, "userCancelled");
    assert_eq!(tool_cause.source, "requestInterrupt");
    assert_eq!(tool_cause.at.as_deref(), Some("2026-04-21T12:03:00Z"));
}

#[test]
fn tool_cancel_cause_uses_only_its_physical_request_owner() {
    for request_doc_id in [Some("req-1"), Some("missing-request"), None] {
        let mut latest = request("req-2", RequestLifecycleState::Interrupted);
        latest.interrupt_requested_at = Some("2026-04-21T12:03:00Z".into());
        let snapshot = build_session_snapshot_from_store(
            &ClientStore::from_rows(ClientStoreRows {
                sessions: vec![session(None)],
                requests: vec![request("req-1", RequestLifecycleState::Completed), latest],
                tool_calls: vec![serde_json::from_value(serde_json::json!({
                    "_docID": "cancelled-tool-doc",
                    "tool_call_key": "cancelled-tool",
                    "tool_call_id": "cancelled-call",
                    "agent_did": "did:test:amy",
                    "session_id": "session-1",
                    "request_id": "req-2",
                    "request_doc_id": request_doc_id,
                    "tool_name": "bash",
                    "lifecycle_state": "cancelled"
                }))
                .expect("cancelled tool lifecycle row")],
                ..ClientStoreRows::default()
            }),
            "session-1",
            Some("req-2"),
        )
        .expect("snapshot");
        assert_eq!(snapshot.tool_calls.len(), 1);
        let cause = snapshot.tool_calls[0].cancel_cause.as_ref().expect("cause");
        assert_eq!(cause.cause, "unknown", "owner={request_doc_id:?}");
        assert_eq!(cause.source, "unresolved", "owner={request_doc_id:?}");
    }
}

#[test]
fn session_snapshot_projects_failed_request_reason_without_response_storage() {
    let mut failed = request("req-2", RequestLifecycleState::Failed);
    failed.failure_reason = Some("provider exploded".into());
    let snapshot = build_session_snapshot_from_store(
        &ClientStore::from_rows(ClientStoreRows {
            sessions: vec![session(None)],
            requests: vec![failed],
            ..ClientStoreRows::default()
        }),
        "session-1",
        Some("req-2"),
    )
    .expect("snapshot");
    let outcome = snapshot.latest_request_outcome.expect("request outcome");
    assert_eq!(outcome.failure_reason.as_deref(), Some("provider exploded"));
    assert!(outcome.cancel_cause.is_none());
    assert!(snapshot.retry_eligibility.eligible);
}

#[test]
fn session_snapshot_derives_interrupted_cause_for_caused_request() {
    let mut child = request("req-2", RequestLifecycleState::Interrupted);
    child.caused_by_parent_request_id = Some("parent".into());
    let snapshot = build_session_snapshot_from_store(
        &ClientStore::from_rows(ClientStoreRows {
            sessions: vec![session(None)],
            requests: vec![child],
            ..ClientStoreRows::default()
        }),
        "session-1",
        Some("req-2"),
    )
    .expect("snapshot");
    assert_eq!(snapshot.turn_state.as_deref(), Some("interrupted"));
    let cause = snapshot
        .latest_request_outcome
        .expect("request outcome")
        .cancel_cause
        .expect("cancel cause");
    assert_eq!(cause.cause, "interrupted");
    assert_eq!(cause.source, "requestLifecycle");
}
