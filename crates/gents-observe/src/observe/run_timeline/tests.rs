use gents_protocol::request_lifecycle::RequestLifecycleState;

use super::build::build_run_timeline;
use super::events::{RunTimelineEvent, TimelineRenderedRequestEvent};
use super::inference::retry_summary_for_request;
use super::requests::resolve_logical_request_id_for_doc;
use super::rows::{
    RunTimelineRows, TimelineCompactionRow, TimelineGoalVersionRow, TimelineInferenceCallRow,
    TimelineMessageRow, TimelineProviderContextReductionRow, TimelineRenderedRequestRow,
    TimelineRequestRow, TimelineSessionRow, TimelineToolCallRow,
};

#[test]
fn physical_message_membership_includes_root_and_rejects_conflicting_aliases() {
    let root = TimelineRequestRow {
        doc_id: Some("physical-root".into()),
        request_id: "logical-root".into(),
        ..Default::default()
    };
    assert_eq!(
        resolve_logical_request_id_for_doc(Some("physical-root"), std::iter::once(&root)),
        Some("logical-root")
    );
    assert_eq!(
        resolve_logical_request_id_for_doc(Some("logical-root"), std::iter::once(&root)),
        None
    );
    let conflict = TimelineRequestRow {
        doc_id: Some("physical-root".into()),
        request_id: "different-logical-id".into(),
        ..Default::default()
    };
    for rows in [[&root, &conflict], [&conflict, &root]] {
        assert_eq!(
            resolve_logical_request_id_for_doc(Some("physical-root"), rows.into_iter()),
            None
        );
    }
}

#[test]
fn session_projection_cannot_supply_missing_request_identity() {
    let timeline = build_run_timeline(RunTimelineRows {
        request: TimelineRequestRow {
            request_id: "root".into(),
            ..Default::default()
        },
        session: Some(TimelineSessionRow {
            doc_id: Some("physical-session".into()),
            session: gents_protocol::session::AgentSession {
                session_id: "session".into(),
                node_did: "projection-owner".into(),
                requester_did: None,
                agent_did: "projection-behavior".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
                closed_at: None,
                archived_at: None,
                archive_revision: 0,
                stopped_at: None,
                title: None,
                tags: Vec::new(),
                provenance: None,
                observation: None,
            },
        }),
        ..Default::default()
    });
    assert!(timeline.node_did.is_none());
    assert!(timeline.agent_did.is_none());
}

#[test]
fn goal_history_emits_semantic_changes_and_preserves_native_parent_cids() {
    let timeline = build_run_timeline(RunTimelineRows {
        request: TimelineRequestRow {
            request_id: "request-goal".to_string(),
            session_id: Some("session-goal".to_string()),
            ..Default::default()
        },
        goal_versions: vec![
            TimelineGoalVersionRow {
                goal_doc_id: "doc-goal".to_string(),
                goal_id: "goal-1".to_string(),
                session_id: "session-goal".to_string(),
                node_did: "did:test:agent".to_string(),
                commit_cid: "cid-1".to_string(),
                height: 1,
                status: "active".to_string(),
                created_at: Some("2026-08-14T12:00:00Z".to_string()),
                updated_at: Some("2026-08-14T12:00:00Z".to_string()),
                ..Default::default()
            },
            TimelineGoalVersionRow {
                goal_doc_id: "doc-goal".to_string(),
                goal_id: "goal-1".to_string(),
                session_id: "session-goal".to_string(),
                node_did: "did:test:agent".to_string(),
                commit_cid: "cid-2".to_string(),
                height: 2,
                parent_commit_cids: vec!["cid-1".to_string()],
                status: "active".to_string(),
                tokens_used: 10,
                updated_at: Some("2026-08-14T12:00:01Z".to_string()),
                ..Default::default()
            },
            TimelineGoalVersionRow {
                goal_doc_id: "doc-goal".to_string(),
                goal_id: "goal-1".to_string(),
                session_id: "session-goal".to_string(),
                node_did: "did:test:agent".to_string(),
                commit_cid: "cid-3".to_string(),
                height: 3,
                parent_commit_cids: vec!["cid-2".to_string()],
                status: "paused".to_string(),
                tokens_used: 10,
                updated_at: Some("2026-08-14T12:00:02Z".to_string()),
                ..Default::default()
            },
        ],
        ..Default::default()
    });

    let transitions = timeline
        .events
        .iter()
        .filter_map(|event| match event {
            RunTimelineEvent::GoalTransition(event) => Some(event),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(transitions.len(), 2, "usage-only commit must be omitted");
    assert_eq!(transitions[0].commit_cid, "cid-1");
    assert!(transitions[0].parents.is_empty());
    assert_eq!(transitions[1].commit_cid, "cid-3");
    assert_eq!(transitions[1].state.status, "paused");
    assert_eq!(transitions[1].parents.len(), 1);
    assert_eq!(transitions[1].parents[0].commit_cid, "cid-2");
    assert_eq!(transitions[1].parents[0].state.status, "active");
}

#[test]
fn timeline_projects_complete_inference_provenance() {
    let timeline = build_run_timeline(RunTimelineRows {
        request: TimelineRequestRow {
            doc_id: Some("doc-request".to_string()),
            request_id: "request-1".to_string(),
            session_id: Some("session-1".to_string()),
            created_at: Some("2026-08-14T12:00:00Z".to_string()),
            ..Default::default()
        },
        requests: vec![TimelineRequestRow {
            doc_id: Some("doc-request".to_string()),
            request_id: "request-1".to_string(),
            session_id: Some("session-1".to_string()),
            ..Default::default()
        }],
        inference_calls: vec![TimelineInferenceCallRow {
            call_id: "inference-1".to_string(),
            runtime_instance_id: Some("runtime-a".to_string()),
            request_id: "request-1".to_string(),
            request_doc_id: Some("doc-request".to_string()),
            call_seq: 1,
            attempt: 2,
            call_state: "completed".to_string(),
            backend_id: Some("backend-a".to_string()),
            agent_did: Some("behavior-a".to_string()),
            node_did: Some("did:test:agent".to_string()),
            call_kind: "inference".to_string(),
            priority: Some(7),
            queue_depth_at_enqueue: Some(3),
            controller_generation: Some(11),
            backend_config_fingerprint: Some("hmac-sha256:process-v1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
            queued_at: Some("2026-08-14T12:00:01Z".to_string()),
            ..Default::default()
        }],
        tool_calls: vec![TimelineToolCallRow {
            doc_id: Some("doc-tool".to_string()),
            request_id: Some("request-1".to_string()),
            request_doc_id: Some("doc-request".to_string()),
            session_id: "session-1".to_string(),
            tool_call_id: "tool-1".to_string(),
            tool_name: "call_tool".to_string(),
            status: "completed".to_string(),
            started_at: Some("2026-08-14T12:00:02Z".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    });

    let inference = timeline.events.iter().find_map(|event| match event {
        RunTimelineEvent::InferenceCall(event) => Some(event),
        _ => None,
    });
    let inference = inference.expect("inference event");
    assert_eq!(inference.runtime_instance_id.as_deref(), Some("runtime-a"));
    assert_eq!(inference.agent_did.as_deref(), Some("behavior-a"));
    assert_eq!(inference.node_did.as_deref(), Some("did:test:agent"));
    assert_eq!(inference.priority, Some(7));
    assert_eq!(inference.queue_depth_at_enqueue, Some(3));
    assert_eq!(inference.controller_generation, Some(11));
    assert_eq!(
        inference.backend_config_fingerprint.as_deref(),
        Some(
            "hmac-sha256:process-v1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        )
    );
}

#[test]
fn builds_ordered_timeline_with_inferred_tool_call_and_child_request() {
    let rows = RunTimelineRows {
        request: TimelineRequestRow {
            doc_id: Some("doc-req-1".to_string()),
            request_id: "req-1".to_string(),
            node_did: Some("did:test:amy".to_string()),
            agent_did: Some("amy".to_string()),
            session_id: Some("session-1".to_string()),
            lifecycle_state: Some(RequestLifecycleState::Completed),
            caused_by_trigger_id: Some("trigger-1".to_string()),
            caused_by_trigger_kind: Some("event".to_string()),
            caused_by_source_doc_id: Some("source-doc-1".to_string()),
            created_at: Some("2026-05-04T12:00:01Z".to_string()),
            ..Default::default()
        },
        requests: vec![
            TimelineRequestRow {
                doc_id: Some("doc-req-1".to_string()),
                request_id: "req-1".to_string(),
                session_id: Some("session-1".to_string()),
                created_at: Some("2026-05-04T12:00:01Z".to_string()),
                ..Default::default()
            },
            TimelineRequestRow {
                doc_id: Some("doc-child-1".to_string()),
                request_id: "child-1".to_string(),
                session_id: Some("session-2".to_string()),
                caused_by_parent_request_id: Some("req-1".to_string()),
                caused_by_parent_request_doc_id: Some("doc-req-1".to_string()),
                caused_by_parent_tool_call_id: Some("call-delegate".to_string()),
                caused_by_parent_tool_call_doc_id: Some("doc-call-delegate".to_string()),
                created_at: Some("2026-05-04T12:00:04Z".to_string()),
                ..Default::default()
            },
        ],
        messages: vec![TimelineMessageRow {
            doc_id: None,
            session_id: "session-1".to_string(),
            request_doc_id: None,
            sequence: 2,
            timestamp: Some("2026-05-04T12:00:02Z".to_string()),
            node_did: None,
            header: gents_protocol::output::TranscriptMessage {
                native_request_id: None,
                message_key: "session-1:2".to_string(),
                session_id: "session-1".to_string(),
                node_did: "did:test:agent".to_string(),
                requester_did: None,
                request_doc_id: None,
                publication: gents_protocol::output::MessagePublication::RequestExecution {
                    execution_generation: "gen-1".to_string(),
                },
                outcome: gents_protocol::output::OutputOutcome::Complete,
                sequence: 2,
                role: gents_protocol::output::MessageRole::Assistant,
                native_id: None,
                blocks: Vec::new(),
                created_at: "2026-05-04T12:00:02Z".to_string(),
            },
            message: gents_protocol::message::Message::assistant("calling tool"),
        }],
        tool_calls: vec![TimelineToolCallRow {
            doc_id: Some("doc-call-delegate".to_string()),
            request_id: Some("req-1".to_string()),
            request_doc_id: Some("doc-req-1".to_string()),
            session_id: "session-1".to_string(),
            message_sequence: Some(2),
            tool_name: "delegate".to_string(),
            tool_call_id: "call-delegate".to_string(),
            status: "completed".to_string(),
            child_request_id: Some("child-1".to_string()),
            denial_reason: Some("not allowed".to_string()),
            started_at: Some("2026-05-04T12:00:03Z".to_string()),
            completed_at: Some("2026-05-04T12:00:03.500Z".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    };

    let timeline = build_run_timeline(rows);

    assert_eq!(timeline.request_id, "req-1");
    assert_eq!(timeline.child_request_ids, vec!["child-1"]);
    assert_eq!(timeline.events.len(), 4);
    assert!(matches!(timeline.events[0], RunTimelineEvent::Request(_)));
    let RunTimelineEvent::Request(request) = &timeline.events[0] else {
        unreachable!();
    };
    assert_eq!(request.caused_by_trigger_id.as_deref(), Some("trigger-1"));
    assert_eq!(request.caused_by_trigger_kind.as_deref(), Some("event"));
    assert_eq!(
        request.caused_by_source_doc_id.as_deref(),
        Some("source-doc-1")
    );
    assert!(matches!(timeline.events[1], RunTimelineEvent::Message(_)));
    assert!(matches!(timeline.events[2], RunTimelineEvent::ToolCall(_)));
    assert!(matches!(timeline.events[3], RunTimelineEvent::Request(_)));
    let RunTimelineEvent::ToolCall(tool) = &timeline.events[2] else {
        panic!("expected tool call");
    };
    assert_eq!(tool.request_id.as_deref(), Some("req-1"));
    assert_eq!(tool.child_request_id.as_deref(), Some("child-1"));
    assert_eq!(tool.denial_reason.as_deref(), Some("not allowed"));
}

fn provenance_fixture(scope: &str, call_id: Option<(&str, i64)>) -> String {
    let mut manifest = serde_json::json!({
        "manifest_version": 3,
        "status": "captured_only",
        "status_reason": "fixture",
        "capture_seam": "transport_body",
        "capture_scope": scope,
        "assembly_trace": {
            "trace_version": 4,
            "build_path": "budgeted",
            "effective_message_count": 0,
            "effective_messages": null,
            "assistant_message_ids": [],
            "threaded_tool_results": [],
            "reduction_keys": [],
            "context_accounting": null
        }
    });
    if let Some((call_id, call_seq)) = call_id {
        manifest["admission"] = serde_json::json!({ "call_id": call_id, "call_seq": call_seq });
    }
    manifest.to_string()
}

fn rendered_row(
    capture_key: &str,
    scope: &str,
    turn_index: i64,
    attempt: i64,
    created_at: &str,
    provenance_json: Option<String>,
) -> TimelineRenderedRequestRow {
    TimelineRenderedRequestRow {
        capture_key: capture_key.to_string(),
        request_doc_id: Some("doc-req-1".to_string()),
        request_id: Some("req-1".to_string()),
        session_id: Some("session-1".to_string()),
        capture_scope: Some(scope.to_string()),
        turn_index: Some(turn_index),
        attempt: Some(attempt),
        capture_version: Some(1),
        model_name: Some("test-model".to_string()),
        source: Some("openai_chat_completions".to_string()),
        prompt_hash: Some("aa".to_string()),
        tools_hash: Some("bb".to_string()),
        provenance_json,
        created_at: Some(created_at.to_string()),
        ..Default::default()
    }
}

#[test]
fn rendered_captures_interleave_with_their_inference_calls() {
    let rows = RunTimelineRows {
        request: TimelineRequestRow {
            doc_id: Some("doc-req-1".to_string()),
            request_id: "req-1".to_string(),
            session_id: Some("session-1".to_string()),
            execution_origin: Some("triggered".to_string()),
            caused_by_trigger_id: Some("trigger-9".to_string()),
            caused_by_trigger_kind: Some("schedule".to_string()),
            created_at: Some("2026-08-07T12:00:00Z".to_string()),
            ..Default::default()
        },
        inference_calls: vec![
            TimelineInferenceCallRow {
                call_id: "call-1".to_string(),
                request_id: "req-1".to_string(),
                request_doc_id: Some("doc-req-1".to_string()),
                call_seq: 1,
                attempt: 1,
                call_state: "completed".to_string(),
                call_kind: "inference".to_string(),
                prompt_tokens: Some(120),
                completion_tokens: Some(30),
                cached_input_tokens: Some(64),
                queued_at: Some("2026-08-07T12:00:01Z".to_string()),
                started_at: Some("2026-08-07T12:00:03Z".to_string()),
                ..Default::default()
            },
            TimelineInferenceCallRow {
                call_id: "call-2".to_string(),
                request_id: "req-1".to_string(),
                request_doc_id: Some("doc-req-1".to_string()),
                call_seq: 2,
                attempt: 2,
                call_state: "completed".to_string(),
                call_kind: "inference".to_string(),
                queued_at: Some("2026-08-07T12:00:05Z".to_string()),
                ..Default::default()
            },
        ],
        rendered_requests: vec![
            rendered_row(
                "rendered:v1:b-second",
                "inference.1",
                0,
                1,
                "2026-08-07T12:00:06Z",
                Some(provenance_fixture("inference.1", Some(("call-2", 2)))),
            ),
            rendered_row(
                "rendered:v1:a-first",
                "inference.1",
                0,
                0,
                "2026-08-07T12:00:02Z",
                Some(provenance_fixture("inference.1", Some(("call-1", 1)))),
            ),
            rendered_row(
                "rendered:v1:c-unknown",
                "compaction.1",
                0,
                0,
                "2026-08-07T12:00:04Z",
                Some(r#"{"manifest_version":99,"opaque":true}"#.to_string()),
            ),
            rendered_row(
                "rendered:v1:e-ten",
                "inference.10",
                0,
                0,
                "2026-08-07T12:00:07Z",
                None,
            ),
            rendered_row(
                "rendered:v1:d-two",
                "inference.2",
                3,
                0,
                "2026-08-07T12:00:07Z",
                None,
            ),
        ],
        ..Default::default()
    };

    let timeline = build_run_timeline(rows);

    let kinds: Vec<&str> = timeline
        .events
        .iter()
        .map(|event| match event {
            RunTimelineEvent::Request(_) => "request",
            RunTimelineEvent::RenderedRequest(event) => {
                event.capture_scope.as_deref().unwrap_or("rendered")
            }
            RunTimelineEvent::InferenceCall(event) => match event.call_seq {
                1 => "call-1",
                _ => "call-2",
            },
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            "request",
            "call-1",
            "inference.1",
            "compaction.1",
            "call-2",
            "inference.1",
            "inference.2",
            "inference.10",
        ]
    );

    let captures: Vec<&TimelineRenderedRequestEvent> = timeline
        .events
        .iter()
        .filter_map(|event| match event {
            RunTimelineEvent::RenderedRequest(event) => Some(event),
            _ => None,
        })
        .collect();
    assert_eq!(captures[0].call_id.as_deref(), Some("call-1"));
    assert_eq!(captures[0].call_seq, Some(1));
    assert_eq!(captures[0].provenance_status, "captured_only");
    assert_eq!(
        captures[0].manifest_version,
        Some(gents_protocol::rendered_request::PROVENANCE_MANIFEST_VERSION as i64)
    );
    assert_eq!(captures[0].scope_kind.as_deref(), Some("inference"));
    assert_eq!(captures[0].scope_seq, Some(1));

    assert_eq!(captures[1].provenance_status, "unsupported_manifest");
    assert_eq!(captures[1].manifest_version, Some(99));
    assert_eq!(captures[1].call_seq, None);

    assert_eq!(captures[2].call_id.as_deref(), Some("call-2"));
    assert_eq!(captures[3].provenance_status, "unavailable");

    let request_event = timeline
        .events
        .iter()
        .find_map(|event| match event {
            RunTimelineEvent::Request(event) => Some(event),
            _ => None,
        })
        .expect("request event");
    assert_eq!(request_event.execution_origin.as_deref(), Some("triggered"));
    assert_eq!(
        request_event.caused_by_trigger_id.as_deref(),
        Some("trigger-9")
    );
    assert_eq!(
        request_event.caused_by_trigger_kind.as_deref(),
        Some("schedule")
    );

    let call_event = timeline
        .events
        .iter()
        .find_map(|event| match event {
            RunTimelineEvent::InferenceCall(event) if event.call_seq == 1 => Some(event),
            _ => None,
        })
        .expect("inference call event");
    assert_eq!(call_event.prompt_tokens, Some(120));
    assert_eq!(call_event.completion_tokens, Some(30));
    assert_eq!(call_event.cached_input_tokens, Some(64));
}

#[test]
fn provider_reductions_are_ordered_before_and_linked_from_rendered_capture() {
    let mut provenance: serde_json::Value =
        serde_json::from_str(&provenance_fixture("inference.1", Some(("call-3", 3)))).unwrap();
    provenance["assembly_trace"]["trace_version"] = serde_json::json!(3);
    provenance["assembly_trace"]["reduction_keys"] =
        serde_json::json!(["reduction-1", "reduction-2"]);
    let rows = RunTimelineRows {
        request: TimelineRequestRow {
            doc_id: Some("doc-req-1".to_string()),
            request_id: "req-1".to_string(),
            session_id: Some("session-1".to_string()),
            created_at: Some("2026-08-14T12:00:00Z".to_string()),
            ..Default::default()
        },
        provider_context_reductions: vec![
            TimelineProviderContextReductionRow {
                doc_id: "reduction-doc-1".to_string(),
                reduction_key: "reduction-1".to_string(),
                request_id: "req-1".to_string(),
                request_doc_id: "doc-req-1".to_string(),
                session_id: "session-1".to_string(),
                reduction_index: 1,
                turn_index: 1,
                created_at: Some("2026-08-14T12:00:01Z".to_string()),
                ..Default::default()
            },
            TimelineProviderContextReductionRow {
                doc_id: "reduction-doc-2".to_string(),
                reduction_key: "reduction-2".to_string(),
                request_id: "req-1".to_string(),
                request_doc_id: "doc-req-1".to_string(),
                session_id: "session-1".to_string(),
                reduction_index: 2,
                turn_index: 2,
                parent_reduction_key: Some("reduction-1".to_string()),
                created_at: Some("2026-08-14T12:00:02Z".to_string()),
                ..Default::default()
            },
        ],
        rendered_requests: vec![rendered_row(
            "capture-3",
            "inference.1",
            2,
            0,
            "2026-08-14T12:00:02Z",
            Some(provenance.to_string()),
        )],
        ..Default::default()
    };

    let timeline = build_run_timeline(rows);
    assert!(matches!(
        timeline.events[1],
        RunTimelineEvent::ProviderContextReduction(_)
    ));
    assert!(matches!(
        timeline.events[2],
        RunTimelineEvent::ProviderContextReduction(_)
    ));
    let RunTimelineEvent::RenderedRequest(rendered) = &timeline.events[3] else {
        panic!("expected rendered request after reductions");
    };
    assert_eq!(rendered.reduction_keys, ["reduction-1", "reduction-2"]);
}

#[test]
fn rendered_captures_for_other_requests_are_excluded() {
    let mut row = rendered_row(
        "rendered:v1:other",
        "inference.1",
        0,
        0,
        "2026-08-07T12:00:02Z",
        None,
    );
    row.request_id = Some("req-other".to_string());
    let rows = RunTimelineRows {
        request: TimelineRequestRow {
            request_id: "req-1".to_string(),
            session_id: Some("session-1".to_string()),
            created_at: Some("2026-08-07T12:00:00Z".to_string()),
            ..Default::default()
        },
        rendered_requests: vec![row],
        ..Default::default()
    };

    let timeline = build_run_timeline(rows);
    assert!(!timeline
        .events
        .iter()
        .any(|event| matches!(event, RunTimelineEvent::RenderedRequest(_))));
}

#[test]
fn retry_summary_follows_the_generated_model() {
    gents_lean_vocab::families::run_timeline_retry_summary::replay(|i| {
        let request = TimelineRequestRow {
            request_id: i.name.clone(),
            lifecycle_state: Some(if i.completed {
                RequestLifecycleState::Completed
            } else {
                RequestLifecycleState::Failed
            }),
            ..Default::default()
        };
        let calls = i
            .attempts
            .iter()
            .map(|attempt| TimelineInferenceCallRow {
                request_id: i.name.clone(),
                call_seq: i64::try_from(attempt.seq).unwrap(),
                call_state: if attempt.failed {
                    "failed"
                } else {
                    "completed"
                }
                .to_string(),
                failure_reason: attempt.reason.clone(),
                call_kind: "inference".to_string(),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        let summary = retry_summary_for_request(&request, &calls);
        gents_lean_vocab::families::run_timeline_retry_summary::Summary {
            retries: u64::from(summary.retry_count),
            last_transient: summary.last_transient_error,
            recovered: summary.recovered,
        }
    });
}

#[test]
fn compaction_is_a_stably_identified_timeline_event() {
    let timeline = build_run_timeline(RunTimelineRows {
        request: TimelineRequestRow {
            doc_id: Some("doc-root".to_string()),
            request_id: "req-root".to_string(),
            session_id: Some("session-root".to_string()),
            created_at: Some("2026-05-04T12:00:00Z".to_string()),
            ..Default::default()
        },
        requests: vec![TimelineRequestRow {
            doc_id: Some("doc-root".to_string()),
            request_id: "req-root".to_string(),
            session_id: Some("session-root".to_string()),
            ..Default::default()
        }],
        compactions: vec![TimelineCompactionRow {
            doc_id: Some("doc-compact".to_string()),
            compaction_key: "session-root:1".to_string(),
            request_id: Some("req-root".to_string()),
            request_doc_id: Some("doc-root".to_string()),
            session_id: "session-root".to_string(),
            sequence: 1,
            summary: "durable compact summary".to_string(),
            messages_compacted: 8,
            original_tokens: 900,
            compacted_tokens: 120,
            created_at: Some("2026-05-04T12:00:02Z".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    });

    let event = timeline.events.iter().find_map(|event| match event {
        RunTimelineEvent::Compaction(event) => Some(event),
        _ => None,
    });
    let event = event.expect("compaction event");
    assert_eq!(event.compaction_key, "session-root:1");
    assert_eq!(event.request_id, "req-root");
    assert_eq!(event.summary, "durable compact summary");
    assert!(event.compacted_through_sequence.is_none());
    assert!(event.source_message_versions.is_empty());

    let pinned = vec![
        gents_protocol::transcript::TranscriptFactRef {
            doc_id: "bae-msg-1".to_string(),
            sequence: 1,
            commit_cid: "bafymsg1".to_string(),
        },
        gents_protocol::transcript::TranscriptFactRef {
            doc_id: "bae-msg-2".to_string(),
            sequence: 2,
            commit_cid: "bafymsg2".to_string(),
        },
    ];
    let with_pins = build_run_timeline(RunTimelineRows {
        request: TimelineRequestRow {
            doc_id: Some("doc-root".to_string()),
            request_id: "req-root".to_string(),
            session_id: Some("session-root".to_string()),
            created_at: Some("2026-05-04T12:00:00Z".to_string()),
            ..Default::default()
        },
        requests: vec![TimelineRequestRow {
            doc_id: Some("doc-root".to_string()),
            request_id: "req-root".to_string(),
            session_id: Some("session-root".to_string()),
            ..Default::default()
        }],
        compactions: vec![TimelineCompactionRow {
            doc_id: Some("doc-compact".to_string()),
            compaction_key: "session-root:1".to_string(),
            request_id: Some("req-root".to_string()),
            request_doc_id: Some("doc-root".to_string()),
            session_id: "session-root".to_string(),
            sequence: 1,
            summary: "durable compact summary".to_string(),
            messages_compacted: 2,
            compacted_through_sequence: Some(2),
            source_message_versions: Some(pinned.clone()),
            original_tokens: 900,
            compacted_tokens: 120,
            created_at: Some("2026-05-04T12:00:02Z".to_string()),
        }],
        ..Default::default()
    });
    let pinned_event = with_pins
        .events
        .iter()
        .find_map(|event| match event {
            RunTimelineEvent::Compaction(event) => Some(event),
            _ => None,
        })
        .expect("pinned compaction event");
    assert_eq!(pinned_event.compacted_through_sequence, Some(2));
    assert_eq!(pinned_event.source_message_versions, pinned);
}

#[test]
fn request_only_queued_links_are_included_without_tool_bridges() {
    let control_request = |request_id: &str, source: Option<&str>| {
        let input = source.map_or_else(
            || serde_json::json!({}),
            |source| serde_json::json!({"queue": {"source":source,"policy":"append","queued_after_request_id":"req-root"}}),
        );
        TimelineRequestRow {
            doc_id: Some(format!("doc-{request_id}")),
            request_id: request_id.to_string(),
            input: serde_json::from_value(input).unwrap(),
            caused_by_parent_request_id: Some("req-root".to_string()),
            caused_by_parent_request_doc_id: Some("doc-root".to_string()),
            ..Default::default()
        }
    };
    let timeline = build_run_timeline(RunTimelineRows {
        request: TimelineRequestRow {
            doc_id: Some("doc-root".to_string()),
            request_id: "req-root".to_string(),
            ..Default::default()
        },
        requests: vec![
            control_request("goal-child", Some("goal")),
            control_request("steering-child", Some("steering")),
            control_request("queued-user-child", Some("user")),
            control_request("ordinary-child", None),
        ],
        ..Default::default()
    });

    assert_eq!(
        timeline.child_request_ids,
        vec!["goal-child", "queued-user-child", "steering-child"]
    );
    assert!(!timeline.events.iter().any(|event| {
        matches!(event, RunTimelineEvent::Request(request) if request.request_id == "ordinary-child")
    }));
}

#[test]
fn logical_child_id_without_physical_bridge_is_not_emitted() {
    let timeline = build_run_timeline(RunTimelineRows {
        request: TimelineRequestRow {
            doc_id: Some("doc-root".to_string()),
            request_id: "req-root".to_string(),
            ..Default::default()
        },
        requests: vec![TimelineRequestRow {
            doc_id: Some("doc-forged-child".to_string()),
            request_id: "forged-child".to_string(),
            caused_by_parent_request_id: Some("req-root".to_string()),
            caused_by_parent_request_doc_id: Some("doc-root".to_string()),
            caused_by_parent_tool_call_id: Some("call-real".to_string()),
            caused_by_parent_tool_call_doc_id: Some("doc-wrong-tool".to_string()),
            ..Default::default()
        }],
        tool_calls: vec![TimelineToolCallRow {
            doc_id: Some("doc-real-tool".to_string()),
            request_id: Some("req-root".to_string()),
            request_doc_id: Some("doc-root".to_string()),
            tool_call_id: "call-real".to_string(),
            child_request_id: Some("forged-child".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    });

    assert!(timeline.child_request_ids.is_empty());
    assert!(!timeline.events.iter().any(|event| {
        matches!(event, RunTimelineEvent::Request(request) if request.request_id == "forged-child")
    }));
    assert!(!timeline.events.iter().any(|event| {
        matches!(event, RunTimelineEvent::ToolCall(tool) if tool.child_request_id.is_some())
    }));
}

#[test]
fn external_logical_child_bridge_without_document_ids_is_rejected() {
    let timeline = build_run_timeline(RunTimelineRows {
        request: TimelineRequestRow {
            request_id: "req-root".to_string(),
            ..Default::default()
        },
        requests: vec![TimelineRequestRow {
            request_id: "req-child".to_string(),
            caused_by_parent_request_id: Some("req-root".to_string()),
            caused_by_parent_tool_call_id: Some("call-delegate".to_string()),
            ..Default::default()
        }],
        tool_calls: vec![TimelineToolCallRow {
            request_id: Some("req-root".to_string()),
            tool_call_id: "call-delegate".to_string(),
            child_request_id: Some("req-child".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    });

    assert!(timeline.child_request_ids.is_empty());
}

#[test]
fn external_logical_child_bridge_rejects_partial_physical_ids() {
    let timeline = build_run_timeline(RunTimelineRows {
        request: TimelineRequestRow {
            request_id: "req-root".to_string(),
            ..Default::default()
        },
        requests: vec![TimelineRequestRow {
            request_id: "req-child".to_string(),
            caused_by_parent_request_id: Some("req-root".to_string()),
            caused_by_parent_request_doc_id: Some("doc-root".to_string()),
            caused_by_parent_tool_call_id: Some("call-delegate".to_string()),
            ..Default::default()
        }],
        tool_calls: vec![TimelineToolCallRow {
            request_id: Some("req-root".to_string()),
            tool_call_id: "call-delegate".to_string(),
            child_request_id: Some("req-child".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    });

    assert!(timeline.child_request_ids.is_empty());
}

#[test]
fn background_completion_is_a_corroborated_request_only_continuation() {
    let timeline = build_run_timeline(RunTimelineRows {
        request: TimelineRequestRow {
            doc_id: Some("doc-root".to_string()),
            request_id: "req-root".to_string(),
            ..Default::default()
        },
        requests: vec![TimelineRequestRow {
            doc_id: Some("doc-wake".to_string()),
            request_id: "req-wake".to_string(),
            caused_by_parent_request_id: Some("req-root".to_string()),
            caused_by_parent_request_doc_id: Some("doc-root".to_string()),
            input: serde_json::from_value(serde_json::json!({"queue": {
                "source":"background_completion", "policy":"coalesce", "key":"wake:1",
                "queued_after_request_id":"req-root", "background_completion_wake_version":1
            }}))
            .unwrap(),
            ..Default::default()
        }],
        ..Default::default()
    });

    assert_eq!(timeline.child_request_ids, vec!["req-wake"]);
}
