use super::*;
use std::collections::BTreeSet;

use crate::run_timeline::{
    build_run_timeline, RunTimelineRows, TimelineMessageRow, TimelineRequestRow,
    TimelineResponseRow, TimelineToolCallRow, TimelineToolOutputOmissionFact,
    TimelineToolResultFact,
};

fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf)
        .expect("workspace root")
}

fn read_adapter_projection_fixture(fixture_name: &str) -> (AdapterProjectionEnvelope, Value) {
    let path = workspace_root().join(format!(
        "crates/gents/tests/fixtures/adapter_projections/envelopes/{fixture_name}.envelope.json"
    ));
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
    let value = serde_json::from_str::<Value>(&raw)
        .unwrap_or_else(|error| panic!("parsing {} as JSON: {error}", path.display()));
    let envelope = serde_json::from_value::<AdapterProjectionEnvelope>(value.clone())
        .unwrap_or_else(|error| panic!("deserializing {}: {error}", path.display()));
    (envelope, value)
}

fn assert_json_schema_valid(schema: &Value, instance: &Value, label: &str) {
    let validator = jsonschema::validator_for(schema)
        .unwrap_or_else(|error| panic!("{label} schema failed to compile: {error}"));
    let errors = validator
        .iter_errors(instance)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect::<Vec<_>>();
    assert!(
        errors.is_empty(),
        "{label} failed JSON Schema validation:\n{}",
        errors.join("\n")
    );
}

fn assert_adapter_projection_matches_json_schema(envelope: &AdapterProjectionEnvelope) {
    let kind = envelope.output.kind();
    let envelope_value = serde_json::to_value(envelope).unwrap();
    assert_json_schema_valid(
        &adapter_projection_json_schema(kind),
        &envelope_value,
        kind.id(),
    );

    let jsonl_record_schema = adapter_projection_jsonl_record_schema(kind);
    for record in adapter_projection_jsonl_records(envelope) {
        let record_value = serde_json::to_value(&record).unwrap();
        assert_json_schema_valid(
            &jsonl_record_schema,
            &record_value,
            &format!("{} JSONL record {}", kind.id(), record.record_id),
        );
    }

    let eval_jsonl_record_schema = adapter_projection_eval_jsonl_record_schema(kind);
    for record in adapter_projection_eval_jsonl_records(envelope) {
        let record_value = serde_json::to_value(&record).unwrap();
        assert_json_schema_valid(
            &eval_jsonl_record_schema,
            &record_value,
            &format!("{} eval JSONL record {}", kind.id(), record.record_id),
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ProjectionParticipant {
    agent_did: Option<String>,
    behavior_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ProjectionDelegation {
    parent_request_id: String,
    child_request_id: String,
    parent_tool_call_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ProjectionToolCall {
    tool_call_id: String,
    tool_name: String,
    status: String,
}

fn delegated_coherence_timeline() -> RunTimeline {
    build_run_timeline(RunTimelineRows {
        request: TimelineRequestRow {
            request_id: "req-root".to_string(),
            agent_did: Some("did:test:coordinator".to_string()),
            behavior_id: Some("coordinator".to_string()),
            session_id: Some("session-root".to_string()),
            content: Some("root private objective".to_string()),
            status: Some("completed".to_string()),
            lifecycle_state: Some("completed".to_string()),
            created_at: Some("2026-06-05T00:00:00Z".to_string()),
            ..TimelineRequestRow::default()
        },
        requests: vec![TimelineRequestRow {
            request_id: "req-review".to_string(),
            agent_did: Some("did:test:reviewer".to_string()),
            behavior_id: Some("reviewer".to_string()),
            session_id: Some("session-review".to_string()),
            status: Some("completed".to_string()),
            lifecycle_state: Some("completed".to_string()),
            caused_by_parent_request_id: Some("req-root".to_string()),
            caused_by_parent_tool_call_id: Some("call-delegate".to_string()),
            created_at: Some("2026-06-05T00:00:03Z".to_string()),
            ..TimelineRequestRow::default()
        }],
        messages: vec![
            TimelineMessageRow {
                doc_id: None,
                session_id: "session-root".to_string(),
                request_id: Some("req-root".to_string()),
                request_doc_id: Some("request-root-doc".to_string()),
                agent_did: Some("did:test:root".to_string()),
                sequence: 1,
                role: "assistant".to_string(),
                content: "root private assistant note".to_string(),
                timestamp: Some("2026-06-05T00:00:01Z".to_string()),
            },
            TimelineMessageRow {
                doc_id: None,
                session_id: "session-review".to_string(),
                request_id: Some("req-review".to_string()),
                request_doc_id: Some("request-review-doc".to_string()),
                agent_did: Some("did:test:reviewer".to_string()),
                sequence: 1,
                role: "assistant".to_string(),
                content: "child private assistant note".to_string(),
                timestamp: Some("2026-06-05T00:00:03.100Z".to_string()),
            },
        ],
        tool_calls: vec![
            TimelineToolCallRow {
                request_id: Some("req-root".to_string()),
                session_id: "session-root".to_string(),
                message_sequence: Some(1),
                tool_name: "delegate".to_string(),
                tool_call_id: "call-delegate".to_string(),
                args: r#"{"prompt":"delegate private args"}"#.to_string(),
                result: r#"{"summary":"delegate private result"}"#.to_string(),
                result_fact: Some(TimelineToolResultFact {
                    doc_id: "result-delegate-doc".to_string(),
                    composite_commit_cid: "bafy-result-delegate".to_string(),
                    signer_did: "did:test:coordinator".to_string(),
                    tool_call_doc_id: "call-delegate-doc".to_string(),
                    tool_call_composite_commit_cid: "bafy-call-delegate".to_string(),
                    tool_call_signer_did: "did:test:coordinator".to_string(),
                    output_text: r#"{"summary":"delegate private result"}"#.to_string(),
                }),
                status: "completed".to_string(),
                child_request_id: Some("req-review".to_string()),
                started_at: Some("2026-06-05T00:00:02Z".to_string()),
                completed_at: Some("2026-06-05T00:00:03Z".to_string()),
                ..TimelineToolCallRow::default()
            },
            TimelineToolCallRow {
                request_id: Some("req-review".to_string()),
                session_id: "session-review".to_string(),
                message_sequence: Some(1),
                tool_name: "bash".to_string(),
                tool_call_id: "call-review-check".to_string(),
                args: r#"{"cmd":"child private args"}"#.to_string(),
                result: "child private result".to_string(),
                result_fact: Some(TimelineToolResultFact {
                    doc_id: "result-review-doc".to_string(),
                    composite_commit_cid: "bafy-result-review".to_string(),
                    signer_did: "did:test:reviewer".to_string(),
                    tool_call_doc_id: "call-review-doc".to_string(),
                    tool_call_composite_commit_cid: "bafy-call-review".to_string(),
                    tool_call_signer_did: "did:test:reviewer".to_string(),
                    output_text: "child private result".to_string(),
                }),
                status: "denied".to_string(),
                denial_reason: Some("child private denial reason".to_string()),
                selected_service_id: Some("native-shell".to_string()),
                selected_tool_name: Some("bash".to_string()),
                started_at: Some("2026-06-05T00:00:03.200Z".to_string()),
                completed_at: Some("2026-06-05T00:00:03.300Z".to_string()),
                ..TimelineToolCallRow::default()
            },
        ],
        responses: vec![
            TimelineResponseRow {
                request_id: "req-review".to_string(),
                session_id: Some("session-review".to_string()),
                content: Some("child private final".to_string()),
                reasoning: Some("child private reasoning".to_string()),
                status: Some("completed".to_string()),
                completed_at: Some("2026-06-05T00:00:03.500Z".to_string()),
                ..TimelineResponseRow::default()
            },
            TimelineResponseRow {
                request_id: "req-root".to_string(),
                session_id: Some("session-root".to_string()),
                content: Some("root private final".to_string()),
                reasoning: Some("root private reasoning".to_string()),
                status: Some("completed".to_string()),
                completed_at: Some("2026-06-05T00:00:04Z".to_string()),
                ..TimelineResponseRow::default()
            },
        ],
        ..RunTimelineRows::default()
    })
}

fn build_all_adapter_projections(
    timeline: &RunTimeline,
    redaction_mode: ProjectionRedactionMode,
) -> Vec<AdapterProjectionEnvelope> {
    let context = ProjectionContext {
        actor_did: Some("did:test:projection-reader".to_string()),
        redaction_mode,
        ..ProjectionContext::default()
    };
    [
        AdapterProjectionKind::OpenAiCodexRunTrace,
        AdapterProjectionKind::LangGraphStateHistory,
        AdapterProjectionKind::MultiAgentTask,
    ]
    .into_iter()
    .map(|kind| build_adapter_projection(kind, timeline, &context))
    .collect()
}

#[test]
fn tool_output_omission_survives_every_projection_without_fabricated_output() {
    let omission = TimelineToolOutputOmissionFact {
        doc_id: "omission-doc".to_string(),
        composite_commit_cid: "bafy-omission-commit".to_string(),
        signer_did: "did:key:omission-signer".to_string(),
        tool_call_doc_id: "tool-call-doc".to_string(),
        tool_call_composite_commit_cid: "bafy-tool-call-commit".to_string(),
        tool_call_signer_did: "did:key:tool-call-signer".to_string(),
        source_phase: "awaiting_approval".to_string(),
        terminal_phase: "denied".to_string(),
        reason: "policy_denied".to_string(),
        detail: "sensitive policy detail".to_string(),
        created_at: Some("2026-08-08T12:00:00Z".to_string()),
    };
    let timeline = build_run_timeline(RunTimelineRows {
        request: TimelineRequestRow {
            request_id: "req-omission".to_string(),
            agent_did: Some("did:key:agent".to_string()),
            session_id: Some("session-omission".to_string()),
            content: Some("exercise omission projection".to_string()),
            status: Some("completed".to_string()),
            lifecycle_state: Some("completed".to_string()),
            ..TimelineRequestRow::default()
        },
        tool_calls: vec![TimelineToolCallRow {
            request_id: Some("req-omission".to_string()),
            session_id: "session-omission".to_string(),
            tool_name: "shell".to_string(),
            tool_call_id: "call-omission".to_string(),
            args: r#"{"cmd":"denied"}"#.to_string(),
            result: "must not be projected".to_string(),
            omission_fact: Some(omission.clone()),
            status: "denied".to_string(),
            lifecycle_state: Some("denied".to_string()),
            ..TimelineToolCallRow::default()
        }],
        ..RunTimelineRows::default()
    });
    let context = ProjectionContext {
        redaction_mode: ProjectionRedactionMode::Full,
        ..ProjectionContext::default()
    };

    let envelopes = [
        AdapterProjectionKind::AtifTrajectory,
        AdapterProjectionKind::OpenAiCodexRunTrace,
        AdapterProjectionKind::LangGraphStateHistory,
        AdapterProjectionKind::MultiAgentTask,
    ]
    .map(|kind| build_adapter_projection(kind, &timeline, &context));

    for envelope in &envelopes {
        validate_adapter_projection_contract(envelope).unwrap();
        assert_adapter_projection_matches_json_schema(envelope);
    }

    let projected_omissions = envelopes
        .iter()
        .map(|envelope| match &envelope.output {
            AdapterProjection::AtifTrajectory(projection) => {
                let result = projection
                    .steps
                    .iter()
                    .find_map(|step| step.observation.as_ref())
                    .and_then(|observation| observation.results.first())
                    .expect("ATIF observation result");
                assert_eq!(result.content, None);
                result.output_omission.clone().expect("ATIF omission")
            }
            AdapterProjection::OpenAiCodexRunTrace(projection) => projection
                .items
                .iter()
                .find_map(|item| match item {
                    OpenAiCodexTraceItem::ToolCall {
                        output,
                        output_omission,
                        ..
                    } => {
                        assert_eq!(output, &None);
                        output_omission.clone()
                    }
                    _ => None,
                })
                .expect("OpenAI omission"),
            AdapterProjection::LangGraphStateHistory(projection) => {
                let task = projection.tasks.first().expect("LangGraph task");
                assert_eq!(task.output, None);
                task.output_omission.clone().expect("LangGraph omission")
            }
            AdapterProjection::MultiAgentTask(projection) => {
                let event = projection
                    .tool_events
                    .first()
                    .expect("multi-agent tool event");
                assert_eq!(event.output, None);
                event.output_omission.clone().expect("multi-agent omission")
            }
        })
        .collect::<Vec<_>>();

    for projected in projected_omissions {
        assert_eq!(projected.reason, omission.reason);
        assert_eq!(projected.detail, omission.detail);
        assert_eq!(projected.source_phase, omission.source_phase);
        assert_eq!(projected.terminal_phase, omission.terminal_phase);
        assert_eq!(projected.evidence.version.doc_id, omission.doc_id);
        assert_eq!(
            projected.evidence.version.composite_commit_cid,
            omission.composite_commit_cid
        );
        assert_eq!(projected.evidence.signer_did, omission.signer_did);
        assert_eq!(
            projected.tool_call.version.doc_id,
            omission.tool_call_doc_id
        );
        assert_eq!(
            projected.tool_call.version.composite_commit_cid,
            omission.tool_call_composite_commit_cid
        );
        assert_eq!(
            projected.tool_call.signer_did,
            omission.tool_call_signer_did
        );
    }

    for envelope in &envelopes {
        let record = adapter_projection_eval_jsonl_records(envelope)
            .into_iter()
            .find(|record| record.output_omission.is_some())
            .expect("eval record carrying tool-output omission");
        assert_eq!(record.output, None);
        assert_eq!(
            record.output_omission.as_ref().map(|value| &value.reason),
            Some(&omission.reason)
        );
    }
}

fn projection_participants(
    envelope: &AdapterProjectionEnvelope,
) -> BTreeSet<ProjectionParticipant> {
    match &envelope.output {
        AdapterProjection::AtifTrajectory(projection) => participant(
            projection
                .agent
                .extra
                .as_ref()
                .and_then(|extra| extra.get("agent_did"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            projection
                .agent
                .extra
                .as_ref()
                .and_then(|extra| extra.get("behavior_id"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        )
        .into_iter()
        .collect(),
        AdapterProjection::OpenAiCodexRunTrace(projection) => projection
            .items
            .iter()
            .filter_map(|item| match item {
                OpenAiCodexTraceItem::Request {
                    agent_did,
                    behavior_id,
                    ..
                } => participant(agent_did.clone(), behavior_id.clone()),
                _ => None,
            })
            .collect(),
        AdapterProjection::LangGraphStateHistory(projection) => projection
            .nodes
            .iter()
            .filter(|node| node.kind == "request")
            .filter_map(|node| participant(node.agent_did.clone(), node.behavior_id.clone()))
            .collect(),
        AdapterProjection::MultiAgentTask(projection) => projection
            .participants
            .iter()
            .filter_map(|participant| {
                self::participant(
                    participant.agent_did.clone(),
                    participant.behavior_id.clone(),
                )
            })
            .collect(),
    }
}

fn participant(
    agent_did: Option<String>,
    behavior_id: Option<String>,
) -> Option<ProjectionParticipant> {
    if agent_did.is_none() && behavior_id.is_none() {
        return None;
    }
    Some(ProjectionParticipant {
        agent_did,
        behavior_id,
    })
}

fn projection_delegations(envelope: &AdapterProjectionEnvelope) -> BTreeSet<ProjectionDelegation> {
    match &envelope.output {
        AdapterProjection::AtifTrajectory(projection) => projection
            .steps
            .iter()
            .flat_map(|step| {
                step.tool_calls
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .map(move |tool| (step, tool))
            })
            .filter_map(|(step, tool)| {
                let extra = tool.extra.as_ref()?;
                Some(ProjectionDelegation {
                    parent_request_id: step
                        .extra
                        .as_ref()
                        .and_then(|extra| extra.get("request_id"))
                        .and_then(Value::as_str)?
                        .to_string(),
                    child_request_id: extra.get("child_request_id")?.as_str()?.to_string(),
                    parent_tool_call_id: Some(tool.tool_call_id.clone()),
                })
            })
            .collect(),
        AdapterProjection::OpenAiCodexRunTrace(projection) => projection
            .items
            .iter()
            .filter_map(|item| match item {
                OpenAiCodexTraceItem::ToolCall {
                    id,
                    request_id,
                    child_run_id,
                    ..
                } => Some(ProjectionDelegation {
                    parent_request_id: request_id.clone()?,
                    child_request_id: child_run_id.clone()?,
                    parent_tool_call_id: Some(id.clone()),
                }),
                _ => None,
            })
            .collect(),
        AdapterProjection::LangGraphStateHistory(projection) => projection
            .tasks
            .iter()
            .filter_map(|task| {
                Some(ProjectionDelegation {
                    parent_request_id: task.request_id.clone()?,
                    child_request_id: task.child_request_id.clone()?,
                    parent_tool_call_id: Some(task.id.clone()),
                })
            })
            .collect(),
        AdapterProjection::MultiAgentTask(projection) => projection
            .delegations
            .iter()
            .map(|delegation| ProjectionDelegation {
                parent_request_id: delegation.parent_request_id.clone(),
                child_request_id: delegation.child_request_id.clone(),
                parent_tool_call_id: delegation.parent_tool_call_id.clone(),
            })
            .collect(),
    }
}

fn projection_tool_calls(envelope: &AdapterProjectionEnvelope) -> BTreeSet<ProjectionToolCall> {
    match &envelope.output {
        AdapterProjection::AtifTrajectory(projection) => projection
            .steps
            .iter()
            .flat_map(|step| step.tool_calls.as_deref().unwrap_or_default())
            .map(|tool| ProjectionToolCall {
                tool_call_id: tool.tool_call_id.clone(),
                tool_name: tool.function_name.clone(),
                status: tool
                    .extra
                    .as_ref()
                    .and_then(|extra| extra.get("status"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
            .collect(),
        AdapterProjection::OpenAiCodexRunTrace(projection) => projection
            .items
            .iter()
            .filter_map(|item| match item {
                OpenAiCodexTraceItem::ToolCall {
                    id, name, status, ..
                } => Some(ProjectionToolCall {
                    tool_call_id: id.clone(),
                    tool_name: name.clone(),
                    status: status.clone(),
                }),
                _ => None,
            })
            .collect(),
        AdapterProjection::LangGraphStateHistory(projection) => projection
            .tasks
            .iter()
            .map(|task| ProjectionToolCall {
                tool_call_id: task.id.clone(),
                tool_name: task.name.clone(),
                status: task.status.clone(),
            })
            .collect(),
        AdapterProjection::MultiAgentTask(projection) => projection
            .tool_events
            .iter()
            .map(|event| ProjectionToolCall {
                tool_call_id: event.id.clone(),
                tool_name: event.tool_name.clone(),
                status: event.status.clone(),
            })
            .collect(),
    }
}

fn projection_terminal_status(envelope: &AdapterProjectionEnvelope) -> Option<String> {
    match &envelope.output {
        AdapterProjection::AtifTrajectory(projection) => projection
            .final_metrics
            .as_ref()
            .and_then(|metrics| metrics.extra.as_ref())
            .and_then(|extra| extra.get("lifecycle_state").or_else(|| extra.get("status")))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        AdapterProjection::OpenAiCodexRunTrace(projection) => projection.status.clone(),
        AdapterProjection::LangGraphStateHistory(projection) => projection
            .values
            .get("lifecycle_state")
            .and_then(Value::as_str)
            .or_else(|| projection.values.get("status").and_then(Value::as_str))
            .map(ToOwned::to_owned),
        AdapterProjection::MultiAgentTask(projection) => projection.status.clone(),
    }
}

#[test]
fn adapter_projections_are_coherent_for_delegated_timeline() {
    let timeline = delegated_coherence_timeline();
    let full = build_all_adapter_projections(&timeline, ProjectionRedactionMode::Full);
    let expected_participants = BTreeSet::from([
        ProjectionParticipant {
            agent_did: Some("did:test:coordinator".to_string()),
            behavior_id: Some("coordinator".to_string()),
        },
        ProjectionParticipant {
            agent_did: Some("did:test:reviewer".to_string()),
            behavior_id: Some("reviewer".to_string()),
        },
    ]);
    let expected_delegations = BTreeSet::from([ProjectionDelegation {
        parent_request_id: "req-root".to_string(),
        child_request_id: "req-review".to_string(),
        parent_tool_call_id: Some("call-delegate".to_string()),
    }]);
    let expected_tool_calls = BTreeSet::from([
        ProjectionToolCall {
            tool_call_id: "call-delegate".to_string(),
            tool_name: "delegate".to_string(),
            status: "completed".to_string(),
        },
        ProjectionToolCall {
            tool_call_id: "call-review-check".to_string(),
            tool_name: "bash".to_string(),
            status: "denied".to_string(),
        },
    ]);

    for envelope in &full {
        validate_adapter_projection_contract(envelope).unwrap();
        assert_adapter_projection_matches_json_schema(envelope);
        assert_eq!(
            projection_participants(envelope),
            expected_participants,
            "{} participant identities drifted from the shared timeline",
            envelope.projection_id
        );
        assert_eq!(
            projection_delegations(envelope),
            expected_delegations,
            "{} delegation shape drifted from the shared timeline",
            envelope.projection_id
        );
        assert_eq!(
            projection_tool_calls(envelope),
            expected_tool_calls,
            "{} tool calls drifted from the shared timeline",
            envelope.projection_id
        );
        assert_eq!(
            projection_terminal_status(envelope).as_deref(),
            Some("completed"),
            "{} terminal status drifted from the shared timeline",
            envelope.projection_id
        );
    }

    let sensitive_literals = [
        "root private objective",
        "root private assistant note",
        "delegate private args",
        "delegate private result",
        "child private assistant note",
        "child private args",
        "child private result",
        "child private denial reason",
        "child private final",
        "child private reasoning",
        "root private final",
        "root private reasoning",
    ];
    let full_serialized = serde_json::to_string(&full).unwrap();
    for literal in sensitive_literals {
        assert!(
            full_serialized.contains(literal),
            "full projections should retain sensitive literal {literal:?}"
        );
    }

    for (mode, marker) in [
        (
            ProjectionRedactionMode::TrainingSafe,
            "[training_safe_redacted]",
        ),
        (ProjectionRedactionMode::Public, "[redacted]"),
    ] {
        let redacted = build_all_adapter_projections(&timeline, mode);
        for envelope in &redacted {
            validate_adapter_projection_contract(envelope).unwrap();
            assert_adapter_projection_matches_json_schema(envelope);
            assert_eq!(
                projection_participants(envelope),
                expected_participants,
                "{} participant identities changed under {mode:?} redaction",
                envelope.projection_id
            );
            assert_eq!(
                projection_delegations(envelope),
                expected_delegations,
                "{} delegation shape changed under {mode:?} redaction",
                envelope.projection_id
            );
            assert_eq!(
                projection_tool_calls(envelope),
                expected_tool_calls,
                "{} tool calls changed under {mode:?} redaction",
                envelope.projection_id
            );
            assert_eq!(
                projection_terminal_status(envelope).as_deref(),
                Some("completed"),
                "{} terminal status changed under {mode:?} redaction",
                envelope.projection_id
            );
        }

        let serialized = serde_json::to_string(&redacted).unwrap();
        assert!(
            serialized.contains(marker),
            "{mode:?} projections should carry redaction markers"
        );
        for literal in sensitive_literals {
            assert!(
                !serialized.contains(literal),
                "{mode:?} projections leaked sensitive literal {literal:?}"
            );
        }
    }
}

#[test]
fn atif_projection_emits_a_schema_valid_native_harbor_document() {
    let timeline = delegated_coherence_timeline();
    let envelope = build_adapter_projection(
        AdapterProjectionKind::AtifTrajectory,
        &timeline,
        &ProjectionContext::default(),
    );

    validate_adapter_projection_contract(&envelope).unwrap();
    assert_adapter_projection_matches_json_schema(&envelope);

    let native = adapter_projection_native_json(&envelope);
    assert_eq!(
        native.get("schema_version").and_then(Value::as_str),
        Some(ATIF_SCHEMA_VERSION)
    );
    assert!(native.get("projection_id").is_none());
    assert_json_schema_valid(
        &adapter_projection_native_json_schema(AdapterProjectionKind::AtifTrajectory),
        &native,
        "ATIF native JSON",
    );
}

#[test]
fn projection_v3_carries_the_same_exact_manifest_in_envelope_and_records() {
    let exact = crate::SignedDocumentVersionRef {
        version: crate::DocumentVersionRef {
            doc_id: "request-doc-1".to_string(),
            composite_commit_cid: "bafy-request-1".to_string(),
        },
        signer_did: "did:key:requester".to_string(),
    };
    let manifest = crate::run_timeline_manifest::root_only_timeline_manifest(
        exact,
        "bafy-schema-agent-request",
    )
    .expect("root-only manifest");
    let timeline = build_run_timeline(RunTimelineRows {
        source_manifest: Some(manifest.clone()),
        request: TimelineRequestRow {
            doc_id: Some("request-doc-1".to_string()),
            request_id: "request-1".to_string(),
            ..TimelineRequestRow::default()
        },
        ..RunTimelineRows::default()
    });
    let envelope = build_adapter_projection(
        AdapterProjectionKind::OpenAiCodexRunTrace,
        &timeline,
        &ProjectionContext::default(),
    );

    assert_eq!(envelope.projection_version, "v3");
    assert_eq!(
        envelope.provenance.runtime_version,
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(
        envelope.provenance.redaction_algorithm_version,
        PROJECTION_REDACTION_ALGORITHM_VERSION
    );
    assert!(chrono::DateTime::parse_from_rfc3339(&envelope.provenance.built_at).is_ok());
    assert_eq!(
        envelope.provenance.source_manifest_status,
        ProjectionSourceManifestStatus::VerifiedExact
    );
    assert_eq!(
        envelope.provenance.source_manifest.as_ref(),
        Some(&manifest)
    );
    validate_adapter_projection_contract(&envelope).expect("valid v3 envelope");
    for record in adapter_projection_jsonl_records(&envelope) {
        assert_eq!(
            record.source_manifest_status,
            ProjectionSourceManifestStatus::VerifiedExact
        );
        assert_eq!(record.source_manifest.as_ref(), Some(&manifest));
    }
    for record in adapter_projection_eval_jsonl_records(&envelope) {
        assert_eq!(
            record.source_manifest_status,
            ProjectionSourceManifestStatus::VerifiedExact
        );
        assert_eq!(record.source_manifest.as_ref(), Some(&manifest));
    }

    let mut malformed = envelope;
    malformed
        .provenance
        .source_manifest
        .as_mut()
        .expect("manifest")
        .root
        .signer_did
        .clear();
    let error = validate_adapter_projection_contract(&malformed).unwrap_err();
    assert!(error.violations.iter().any(|violation| {
        violation.starts_with("provenance.source_manifest is not a valid exact manifest")
    }));

    malformed.provenance.built_at = "not-a-time".to_string();
    let error = validate_adapter_projection_contract(&malformed).unwrap_err();
    assert!(error
        .violations
        .iter()
        .any(|violation| violation == "provenance.built_at must be RFC3339"));
}

#[test]
fn projection_v3_reports_exact_membership_with_open_coverage_as_partial() {
    let exact = crate::SignedDocumentVersionRef {
        version: crate::DocumentVersionRef {
            doc_id: "request-doc-partial".to_string(),
            composite_commit_cid: "bafy-request-partial".to_string(),
        },
        signer_did: "did:key:requester".to_string(),
    };
    let mut manifest = crate::run_timeline_manifest::root_only_timeline_manifest(
        exact,
        "bafy-schema-agent-request",
    )
    .expect("root-only manifest");
    manifest.status = crate::run_timeline_manifest::TimelineManifestStatus::PartialExact;
    manifest.coverage_gaps = vec![crate::run_timeline_manifest::TimelineCoverageGap {
        kind: crate::run_timeline_manifest::TimelineCoverageGapKind::NonAtomicObservation,
        source_class: crate::run_timeline_manifest::TimelineSourceClass::Request,
        collection: "AgentRequest".to_string(),
        scope_id: "request-doc-partial".to_string(),
    }];
    manifest.validate().expect("canonical partial manifest");

    let timeline = build_run_timeline(RunTimelineRows {
        source_manifest: Some(manifest.clone()),
        request: TimelineRequestRow {
            doc_id: Some("request-doc-partial".to_string()),
            request_id: "request-partial".to_string(),
            ..TimelineRequestRow::default()
        },
        ..RunTimelineRows::default()
    });
    let envelope = build_adapter_projection(
        AdapterProjectionKind::OpenAiCodexRunTrace,
        &timeline,
        &ProjectionContext::default(),
    );
    assert_eq!(
        envelope.provenance.source_manifest_status,
        ProjectionSourceManifestStatus::PartialExact
    );
    assert_eq!(
        envelope.provenance.source_manifest.as_ref(),
        Some(&manifest)
    );
    validate_adapter_projection_contract(&envelope).expect("valid partial projection");

    let mut mislabeled = envelope;
    mislabeled.provenance.source_manifest_status = ProjectionSourceManifestStatus::VerifiedExact;
    let error = validate_adapter_projection_contract(&mislabeled).unwrap_err();
    assert!(error
        .violations
        .iter()
        .any(|violation| violation.contains("disagrees with manifest status")));
}

#[test]
fn builds_three_adapter_shapes_from_one_timeline_with_redaction() {
    let timeline = build_run_timeline(RunTimelineRows {
        request: TimelineRequestRow {
            request_id: "req-1".to_string(),
            agent_did: Some("did:test:root".to_string()),
            behavior_id: Some("root".to_string()),
            session_id: Some("session-1".to_string()),
            content: Some("sensitive prompt".to_string()),
            status: Some("completed".to_string()),
            lifecycle_state: Some("completed".to_string()),
            created_at: Some("2026-06-05T00:00:00Z".to_string()),
            ..TimelineRequestRow::default()
        },
        requests: vec![TimelineRequestRow {
            request_id: "child-1".to_string(),
            agent_did: Some("did:test:child".to_string()),
            behavior_id: Some("child".to_string()),
            session_id: Some("session-1".to_string()),
            status: Some("completed".to_string()),
            lifecycle_state: Some("completed".to_string()),
            caused_by_parent_request_id: Some("req-1".to_string()),
            caused_by_parent_tool_call_id: Some("call-child".to_string()),
            created_at: Some("2026-06-05T00:00:03Z".to_string()),
            ..TimelineRequestRow::default()
        }],
        messages: vec![TimelineMessageRow {
            doc_id: None,
            session_id: "session-1".to_string(),
            request_id: Some("req-1".to_string()),
            request_doc_id: Some("request-doc-1".to_string()),
            agent_did: Some("did:test:root".to_string()),
            sequence: 1,
            role: "assistant".to_string(),
            content: "sensitive assistant text".to_string(),
            timestamp: Some("2026-06-05T00:00:01Z".to_string()),
        }],
        tool_calls: vec![TimelineToolCallRow {
            request_id: Some("req-1".to_string()),
            session_id: "session-1".to_string(),
            message_sequence: Some(1),
            tool_name: "delegate".to_string(),
            tool_call_id: "call-child".to_string(),
            args: "{\"prompt\":\"secret\"}".to_string(),
            result: "{\"ok\":true}".to_string(),
            status: "completed".to_string(),
            child_request_id: Some("child-1".to_string()),
            started_at: Some("2026-06-05T00:00:02Z".to_string()),
            completed_at: Some("2026-06-05T00:00:03Z".to_string()),
            ..TimelineToolCallRow::default()
        }],
        responses: vec![TimelineResponseRow {
            request_id: "req-1".to_string(),
            session_id: Some("session-1".to_string()),
            content: Some("sensitive final".to_string()),
            status: Some("completed".to_string()),
            completed_at: Some("2026-06-05T00:00:04Z".to_string()),
            ..TimelineResponseRow::default()
        }],
        ..RunTimelineRows::default()
    });
    let context = ProjectionContext {
        actor_did: Some("did:test:viewer".to_string()),
        redaction_mode: ProjectionRedactionMode::Public,
        ..ProjectionContext::default()
    };

    let codex = build_adapter_projection(
        AdapterProjectionKind::OpenAiCodexRunTrace,
        &timeline,
        &context,
    );
    let langgraph = build_adapter_projection(
        AdapterProjectionKind::LangGraphStateHistory,
        &timeline,
        &context,
    );
    let multi =
        build_adapter_projection(AdapterProjectionKind::MultiAgentTask, &timeline, &context);

    for kind in [
        AdapterProjectionKind::OpenAiCodexRunTrace,
        AdapterProjectionKind::LangGraphStateHistory,
        AdapterProjectionKind::MultiAgentTask,
    ] {
        let envelope_schema = adapter_projection_json_schema(kind);
        assert_eq!(
            envelope_schema
                .pointer("/properties/projection_id/const")
                .and_then(Value::as_str),
            Some(kind.id())
        );
        assert_eq!(
            envelope_schema
                .pointer("/properties/output/properties/adapter/const")
                .and_then(Value::as_str),
            Some(kind.id())
        );

        let jsonl_schema = adapter_projection_jsonl_record_schema(kind);
        assert_eq!(
            jsonl_schema
                .pointer("/properties/projection_id/const")
                .and_then(Value::as_str),
            Some(kind.id())
        );
        assert!(jsonl_schema.pointer("/properties/record_kind").is_some());
    }
    let schema_index = adapter_projection_schema_index();
    assert_eq!(
        schema_index
            .get("schemas")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(4)
    );

    assert_eq!(codex.projection_id, "openai_codex_run_trace");
    assert_eq!(langgraph.projection_id, "langgraph_state_history");
    assert_eq!(multi.projection_id, "multi_agent_task");
    validate_adapter_projection_contract(&codex).unwrap();
    validate_adapter_projection_contract(&langgraph).unwrap();
    validate_adapter_projection_contract(&multi).unwrap();
    assert_adapter_projection_matches_json_schema(&codex);
    assert_adapter_projection_matches_json_schema(&langgraph);
    assert_adapter_projection_matches_json_schema(&multi);

    let codex_records = adapter_projection_jsonl_records(&codex);
    assert!(!codex_records.is_empty());
    assert_eq!(codex_records[0].projection_id, "openai_codex_run_trace");
    assert_eq!(codex_records[0].source_request_id, "req-1");
    assert_eq!(codex_records[0].record_kind, "openai_codex_trace_item");

    let mut invalid = multi.clone();
    invalid.projection_id.clear();
    let error = validate_adapter_projection_contract(&invalid).unwrap_err();
    assert!(error
        .violations
        .iter()
        .any(|violation| violation == "projection_id is required"));

    assert!(!serde_json::to_string(&codex)
        .unwrap()
        .contains("sensitive prompt"));
    assert!(serde_json::to_string(&langgraph)
        .unwrap()
        .contains("child_request"));
    assert!(serde_json::to_string(&multi)
        .unwrap()
        .contains("\"role\":\"delegate\""));
}

/// Drift guard, not a behavior test: the checked-in envelope fixtures
/// were generated by this serializer, so the round-trip equality below is
/// tautological today. Its job is to fail loudly when a DTO/serde change
/// would silently alter the wire format external consumers parse.
#[test]
fn external_contract_fixtures_validate_without_runtime_dependencies() {
    let cases: &[(AdapterProjectionKind, &str, &[&str])] = &[
        (
            AdapterProjectionKind::AtifTrajectory,
            "atif_trajectory",
            &["atif_agent", "atif_step", "atif_final_metrics"],
        ),
        (
            AdapterProjectionKind::OpenAiCodexRunTrace,
            "openai_codex_run_trace",
            &["openai_codex_trace_item"],
        ),
        (
            AdapterProjectionKind::LangGraphStateHistory,
            "langgraph_state_history",
            &[
                "langgraph_values",
                "langgraph_node",
                "langgraph_edge",
                "langgraph_task",
            ],
        ),
        (
            AdapterProjectionKind::MultiAgentTask,
            "multi_agent_task",
            &[
                "multi_agent_participant",
                "multi_agent_message",
                "multi_agent_delegation",
                "multi_agent_tool_event",
            ],
        ),
    ];

    for (kind, fixture_name, expected_record_kinds) in cases {
        let (envelope, fixture_value) = read_adapter_projection_fixture(fixture_name);
        assert_eq!(envelope.projection_id, kind.id());
        assert_eq!(envelope.output.kind(), *kind);
        validate_adapter_projection_contract(&envelope)
            .unwrap_or_else(|error| panic!("{fixture_name} failed contract: {error}"));
        assert_json_schema_valid(
            &adapter_projection_json_schema(*kind),
            &fixture_value,
            fixture_name,
        );

        let round_trip = serde_json::to_value(&envelope).unwrap();
        assert_eq!(
            round_trip, fixture_value,
            "{fixture_name} fixture drifted from adapter DTO serialization"
        );

        let allowed_record_kinds = adapter_projection_jsonl_record_schema(*kind)
            .pointer("/properties/record_kind/enum")
            .and_then(Value::as_array)
            .expect("JSONL record kind enum")
            .iter()
            .map(|value| value.as_str().expect("string record kind").to_string())
            .collect::<std::collections::BTreeSet<_>>();
        let records = adapter_projection_jsonl_records(&envelope);
        assert!(
            !records.is_empty(),
            "{fixture_name} fixture should produce JSONL records"
        );

        let mut observed_record_kinds = std::collections::BTreeSet::new();
        for (index, record) in records.iter().enumerate() {
            assert_eq!(record.projection_id, kind.id());
            assert_eq!(record.projection_version, ADAPTER_PROJECTION_VERSION);
            assert_eq!(record.source_request_id, envelope.source_request_id);
            assert_eq!(record.record_index, index);
            assert!(
                record.value.is_object(),
                "{fixture_name} JSONL value must be an object: {record:#?}"
            );
            assert!(
                allowed_record_kinds.contains(&record.record_kind),
                "{fixture_name} produced unsupported JSONL record kind {}",
                record.record_kind
            );
            assert_json_schema_valid(
                &adapter_projection_jsonl_record_schema(*kind),
                &serde_json::to_value(record).unwrap(),
                &format!("{fixture_name} JSONL record {}", record.record_id),
            );
            observed_record_kinds.insert(record.record_kind.clone());
        }
        for expected in *expected_record_kinds {
            assert!(
                observed_record_kinds.contains(*expected),
                "{fixture_name} missing expected JSONL record kind {expected}"
            );
        }

        let eval_records = adapter_projection_eval_jsonl_records(&envelope);
        assert!(
            !eval_records.is_empty(),
            "{fixture_name} fixture should produce eval JSONL records"
        );
        for (index, record) in eval_records.iter().enumerate() {
            assert_eq!(record.projection_id, kind.id());
            assert_eq!(record.record_index, index);
            assert_json_schema_valid(
                &adapter_projection_eval_jsonl_record_schema(*kind),
                &serde_json::to_value(record).unwrap(),
                &format!("{fixture_name} eval JSONL record {}", record.record_id),
            );
        }
    }
}
