use std::sync::Arc;
use std::time::Duration;

use crate::inference::llm::message::{
    AssistantContent, Message, Reasoning, Text, ToolCall, ToolFunction, ToolResultContent,
    UserContent,
};
use crate::inference::llm::HookAction;
use crate::store::identity::NodeIdentity;
use gents_loop::provider_input::ProviderInputProfile;

use super::*;
use crate::request::lifecycle::{ClaimOutcome, RequestLifecycle};
use crate::request::streaming::DefraStreamWriter;
use crate::test_support::first_content;
use crate::tools::call::hook::FailurePolicy;

#[path = "stream_cadence_tests.rs"]
mod cadence;

#[test]
fn claude_signed_final_seals_preview_without_duplicating_bytes() {
    use crate::inference::llm::message::ReasoningContent;

    let mut accumulator = AssistantTurnAccumulator::default();
    accumulator.push_reasoning_delta(None, "考");
    accumulator.push_reasoning_delta(None, "慮");
    accumulator
        .push_provider_reasoning(
            ProviderInputProfile::ClaudeMessages,
            Reasoning {
                id: None,
                content: vec![ReasoningContent::Text {
                    text: "考慮".into(),
                    signature: Some("署名".into()),
                }],
            },
        )
        .unwrap();
    assert_eq!(
        accumulator.take_message(),
        Some(Message::Assistant {
            request_id: None,
            id: None,
            content: vec![AssistantContent::Reasoning(Reasoning {
                id: None,
                content: vec![ReasoningContent::Text {
                    text: "考慮".into(),
                    signature: Some("署名".into()),
                }],
            })],
        })
    );

    let mut mismatch = AssistantTurnAccumulator::default();
    mismatch.push_reasoning_delta(None, "prefix");
    assert!(mismatch
        .push_provider_reasoning(
            ProviderInputProfile::ClaudeMessages,
            Reasoning {
                id: None,
                content: vec![ReasoningContent::Text {
                    text: "rewritten".into(),
                    signature: Some("sig".into()),
                }],
            }
        )
        .is_err());

    let mut other = AssistantTurnAccumulator::default();
    other.push_reasoning_delta(None, "first");
    other
        .push_provider_reasoning(
            ProviderInputProfile::OpenAiChatCompletions,
            Reasoning {
                id: None,
                content: vec![ReasoningContent::Text {
                    text: "last".into(),
                    signature: None,
                }],
            },
        )
        .unwrap();
    let Some(Message::Assistant { content, .. }) = other.take_message() else {
        panic!("other provider reasoning turn");
    };
    let AssistantContent::Reasoning(reasoning) = &content[0] else {
        panic!("reasoning content");
    };
    assert_eq!(
        reasoning.content.len(),
        2,
        "non-Claude final remains append-only"
    );
}

#[test]
fn generated_reasoning_delta_cases_coalesce_native_parts() {
    use crate::inference::llm::message::ReasoningContent;
    use crate::lean_vocab_test::{lean_reasoning_delta_cases, LeanReasoningDeltaPart as Part};

    for case in lean_reasoning_delta_cases() {
        let mut accumulator = AssistantTurnAccumulator::default();
        for part in &case.before {
            match part {
                Part::Reasoning {
                    id,
                    signature,
                    bytes,
                } => accumulator.push_reasoning(Reasoning {
                    id: id.map(|id| id.to_string()),
                    content: vec![ReasoningContent::Text {
                        text: String::from_utf8(bytes.clone()).unwrap(),
                        signature: signature.map(|signature| signature.to_string()),
                    }],
                }),
                Part::Text { bytes } => accumulator.push_text(std::str::from_utf8(bytes).unwrap()),
            }
        }
        accumulator.push_reasoning_delta(
            case.id.map(|id| id.to_string()),
            std::str::from_utf8(&case.fragment).unwrap(),
        );
        let Some(Message::Assistant { content, .. }) = accumulator.take_message() else {
            panic!("{}: missing assistant message", case.name);
        };
        let actual = content
            .into_iter()
            .flat_map(|part| match part {
                AssistantContent::Text(text) => vec![Part::Text {
                    bytes: text.text.into_bytes(),
                }],
                AssistantContent::Reasoning(reasoning) => reasoning
                    .content
                    .into_iter()
                    .map(|part| {
                        let ReasoningContent::Text { text, signature } = part else {
                            panic!("{}: unexpected reasoning type", case.name);
                        };
                        Part::Reasoning {
                            id: reasoning.id.as_ref().map(|id| id.parse().unwrap()),
                            signature: signature.map(|signature| signature.parse().unwrap()),
                            bytes: text.into_bytes(),
                        }
                    })
                    .collect(),
                _ => panic!("{}: unexpected content type", case.name),
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, case.expected, "{}", case.name);
    }
}

fn user_text_message(text: &str) -> Message {
    Message::User {
        content: vec![UserContent::Text(Text {
            text: text.to_string(),
        })],
    }
}

#[tokio::test]
async fn first_visible_flushes_immediately_and_followup_waits_for_cadence() {
    let SignedRun {
        db: _db,
        node,
        access,
        hook,
        mut lifecycle,
        writer,
        ..
    } = signed_run("first-visible", Duration::from_secs(60)).await;
    assert!(matches!(
        hook.on_completion_call(&user_text_message("cadence"), &[])
            .await,
        HookAction::Continue
    ));
    bind_active_request(&hook, &lifecycle).await;
    let doc_id = lifecycle.request().doc_id.clone();
    let mut processor = StreamProcessor::new(
        &hook,
        &writer,
        &mut lifecycle,
        &doc_id,
        ProviderInputProfile::OpenAiChatCompletions,
    );
    processor
        .process_item(Ok(LoopStreamItem::ProviderAttemptStarted {
            turn: 0,
            attempt: 0,
            capture_scope: "inference.1".parse().unwrap(),
        }))
        .await
        .unwrap();
    processor.process_item(text_item("first")).await.unwrap();
    assert_eq!(count_segments(&access, &doc_id).await, 1);
    processor.process_item(text_item(" second")).await.unwrap();
    assert_eq!(count_segments(&access, &doc_id).await, 1);
    assert!(processor.next_flush_deadline().await.is_some());
    processor.flush_pending().await.unwrap();
    assert_eq!(count_segments(&access, &doc_id).await, 2);
    assert!(
        processor.next_flush_deadline().await.is_none(),
        "a committed batch must disarm its timer so provider polling can resume"
    );
    processor.flush_pending().await.unwrap();
    assert_eq!(
        count_segments(&access, &doc_id).await,
        2,
        "no-op flush must not append"
    );
    assert!(processor.next_flush_deadline().await.is_none());
    processor.process_item(text_item(" third")).await.unwrap();
    assert!(processor.next_flush_deadline().await.is_some());
    processor.flush_pending().await.unwrap();
    assert_eq!(count_segments(&access, &doc_id).await, 3);
    assert!(processor.next_flush_deadline().await.is_none());
    processor
        .process_item(Ok(LoopStreamItem::ReasoningDelta {
            id: None,
            reasoning: "first reasoning".into(),
        }))
        .await
        .unwrap();
    assert_eq!(
        count_segments(&access, &doc_id).await,
        4,
        "first reasoning must be durable immediately too"
    );
    assert!(processor.next_flush_deadline().await.is_none());
    processor.process_item(text_item(" fourth")).await.unwrap();
    assert!(processor.next_flush_deadline().await.is_some());
    let message = processor.assistant_turn.clone().take_message().unwrap();
    processor
        .process_item(Ok(LoopStreamItem::ProviderTurnReady {
            turn: 0,
            attempt: 0,
            message,
        }))
        .await
        .unwrap();
    assert!(processor.assistant_turn.clone().take_message().is_none());
    assert!(
        processor.next_flush_deadline().await.is_none(),
        "publication must consume the pending batch timer before the accumulator disappears"
    );
    processor.flush_pending().await.unwrap();
    assert!(
        processor.next_flush_deadline().await.is_none(),
        "the idle processor must let dispatch and finalization run"
    );
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn pre_stream_failures_do_not_fabricate_provider_attempt_closures() {
    let SignedRun {
        db: _db,
        access,
        hook,
        mut lifecycle,
        writer,
        ..
    } = signed_run("pre-stream-failure", Duration::from_secs(60)).await;
    assert!(matches!(
        hook.on_completion_call(&user_text_message("retry provider"), &[])
            .await,
        HookAction::Continue
    ));
    bind_active_request(&hook, &lifecycle).await;
    let doc_id = lifecycle.request().doc_id.clone();
    let mut processor = StreamProcessor::new(
        &hook,
        &writer,
        &mut lifecycle,
        &doc_id,
        ProviderInputProfile::OpenAiChatCompletions,
    );

    for (attempt, will_retry, error) in [
        (
            0,
            true,
            crate::error::InferenceError::RateLimited {
                retry_after: Some(Duration::from_secs(1)),
            },
        ),
        (
            1,
            false,
            crate::error::InferenceError::ModelUnreachable {
                endpoint: "http://provider.invalid".into(),
            },
        ),
    ] {
        assert!(matches!(
            processor
                .process_item(Ok(LoopStreamItem::AttemptFailed {
                    turn: 0,
                    attempt,
                    error,
                    will_retry,
                    backoff: Duration::ZERO,
                }))
                .await
                .unwrap(),
            StreamAction::Continue
        ));
    }
    drop(processor);

    let request_doc_id = crate::store::graphql::escape_graphql_string(&doc_id);
    let response = exec(&access, &format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "load output segments: {:?}",
        response.errors
    );
    assert_eq!(
        response.data.as_ref().unwrap()["AgentOutputSegment"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "a provider failure before stream creation has no canonical output attempt to close"
    );
}

#[tokio::test]
async fn persist_partial_turn_publishes_text_only_and_retains_partial_signature_bytes() {
    let SignedRun {
        db: _db,
        user,
        node,
        runtime_did,
        access,
        hook,
        mut lifecycle,
        writer: stream_writer,
        session_id,
        request_id,
        ..
    } = signed_run("partial-turn", Duration::from_secs(60)).await;
    assert!(matches!(
        hook.on_completion_call(&user_text_message("Inspect the repo"), &[])
            .await,
        HookAction::Continue
    ));
    bind_active_request(&hook, &lifecycle).await;
    let response_doc_id = lifecycle.request().doc_id.clone();
    let mut processor = StreamProcessor::new(
        &hook,
        &stream_writer,
        &mut lifecycle,
        &response_doc_id,
        ProviderInputProfile::ClaudeMessages,
    );

    processor
        .process_item(Ok(LoopStreamItem::ProviderAttemptStarted {
            turn: 0,
            attempt: 0,
            capture_scope: "inference.1".parse().unwrap(),
        }))
        .await
        .unwrap();

    use gents_loop::provider_audit::{ClaudeAuditEvent, ClaudeBlockKind, ProviderAuditObservation};
    processor
        .process_item(Ok(LoopStreamItem::ProviderAudit(
            ProviderAuditObservation {
                capture_scope: "inference.1".parse().unwrap(),
                turn: 0,
                attempt: 0,
                event: ClaudeAuditEvent::BlockStart {
                    index: 0,
                    kind: ClaudeBlockKind::Text,
                },
            },
        )))
        .await
        .unwrap();
    processor
        .assistant_turn
        .push_text("I started by checking the repo layout.");
    for event in [
        ClaudeAuditEvent::BlockStop { index: 0 },
        ClaudeAuditEvent::BlockStart {
            index: 1,
            kind: ClaudeBlockKind::Thinking,
        },
        ClaudeAuditEvent::ThinkingText {
            index: 1,
            fragment: "Need to inspect directory structure first".into(),
        },
        ClaudeAuditEvent::Signature {
            index: 1,
            fragment: "partial-".into(),
        },
    ] {
        processor
            .process_item(Ok(LoopStreamItem::ProviderAudit(
                ProviderAuditObservation {
                    capture_scope: "inference.1".parse().unwrap(),
                    turn: 0,
                    attempt: 0,
                    event,
                },
            )))
            .await
            .unwrap();
    }

    assert!(processor.has_observable_activity());
    use crate::inference::rendered_request::scope::{
        arm, claim_pending, current_audit_sender, scope_request, RequestCaptureScope,
    };
    use crate::inference::rendered_request::{
        AssemblyBuildPath, AssemblyTrace, RenderedRequestCaptureSink, RenderedRequestContext,
    };
    let capture_sink: RenderedRequestCaptureSink = Arc::new(|_| Box::pin(async { Ok(()) }));
    let scope = Arc::new(RequestCaptureScope::new(
        RenderedRequestContext {
            request_doc_id: response_doc_id.clone(),
            request_commit_cid: "bafy-test-capture".into(),
            request_id: request_id.clone(),
            node_did: runtime_did.clone(),
            requester_did: String::new(),
            agent_did: access.actor_did().unwrap().into(),
            session_id: session_id.clone(),
            model_name: "claude".into(),
            provider_family: None,
        },
        capture_sink,
    ));
    scope_request(scope, async {
        let label = arm(
            crate::inference::rendered_request::CaptureScopeKind::Inference,
            0,
            0,
            AssemblyTrace::from_effective_messages(AssemblyBuildPath::Budgeted, Vec::new()),
        )
        .expect("arm exact inference capture");
        assert_eq!(label, "inference.1");
        claim_pending().expect("claim exact inference capture");
        let sender = current_audit_sender().expect("claimed audit sender");
        let mut reservation = sender.reserve().await.expect("audit capacity");
        reservation
            .emit(ClaudeAuditEvent::Signature {
                index: 1,
                fragment: "signature".into(),
            })
            .unwrap();
        drop(reservation);
        let deadline = chrono::Utc::now() + chrono::Duration::milliseconds(5);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(chrono::Utc::now() >= deadline);
        assert!(processor
            .persist_received_partial_turn("persist deadline-expired assistant turn")
            .await
            .unwrap());
    })
    .await;

    let history = crate::session::load_history_with_access(
        &access,
        &session_id,
        &runtime_did,
        Some(user.did()),
    )
    .await
    .unwrap();
    let assistants = history
        .iter()
        .filter(|message| matches!(message, Message::Assistant { .. }))
        .collect::<Vec<_>>();
    assert_eq!(assistants.len(), 1);
    assert_eq!(
        assistants[0],
        &Message::assistant("I started by checking the repo layout."),
        "partial publication must retain only ordinary text, never reasoning or tool intent"
    );
    let gents_protocol::output::TerminalOutput::Message { message_doc_id } =
        stream_writer.terminal_output(&response_doc_id).await
    else {
        panic!("retained ordinary text must be selectable terminal output")
    };
    let (header, native) = crate::session::load_canonical_message(
        &access,
        &message_doc_id,
        &runtime_did,
        Some(user.did()),
    )
    .await
    .unwrap();
    assert_eq!(
        header.outcome,
        gents_protocol::output::OutputOutcome::Partial
    );
    assert!(matches!(
        header.publication,
        gents_protocol::output::MessagePublication::RequestRecovery { .. }
    ));
    assert_eq!(
        native,
        Message::assistant("I started by checking the repo layout.")
    );
    assert!(header
        .blocks
        .iter()
        .all(|block| matches!(block, gents_protocol::output::MessageBlock::Text { .. })));
    use gents_protocol::output::reconstruction::{reconstruct_stream, ObservedSegment};
    use gents_protocol::output::rows::{decode_output_segment_row, AGENT_OUTPUT_SEGMENT_FIELDS};
    use gents_protocol::output::{OutputOutcome, PayloadRef, SourceClose};
    let result = exec(&access, &format!(
        r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#,
        crate::store::graphql::escape_graphql_string(&response_doc_id)
    )).await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    let records = result.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| decode_output_segment_row(row).unwrap())
        .collect::<Vec<_>>();
    let closing = records
        .iter()
        .filter(|row| row.segment.close.is_some())
        .collect::<Vec<_>>();
    assert_eq!(closing.len(), 1, "partial source closes exactly once");
    let closing = closing[0];
    let Some(SourceClose::Closed {
        outcome: OutputOutcome::Partial,
        stream_bytes,
        ..
    }) = &closing.segment.close
    else {
        panic!("expected a partial closure: {:?}", closing.segment.close);
    };
    assert_eq!(stream_bytes.len(), 3);
    let facts = records
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    let streams = (0..3)
        .map(|stream| {
            reconstruct_stream(
                &facts,
                &[],
                &[],
                &PayloadRef {
                    close_doc_id: closing.doc_id.clone(),
                    stream,
                },
            )
            .expect("partial extent must reconstruct exactly")
        })
        .collect::<Vec<_>>();
    assert_eq!(streams.len(), 3);
    assert!(streams
        .iter()
        .any(|stream| stream.text == "I started by checking the repo layout."));
    assert!(streams
        .iter()
        .any(|stream| stream.text == "Need to inspect directory structure first"));
    assert!(streams.iter().any(|stream| {
        stream.text == "partial-signature"
            && stream.declaration.payload
                == gents_protocol::output::StreamPayload::ReasoningSignature
    }));

    node.shutdown().await.expect("node shutdown");
}

/// A request claimed by its signed client and executed by its signed Agent.
/// The hook and stream writer act under the Agent's managed access; reads of
/// Agent-owned rows go through `access`, never the node identity.
struct SignedRun {
    db: crate::support::TestDb,
    user: crate::KeyIdentity,
    node: Arc<defra_node::Node>,
    runtime_did: String,
    access: crate::ConfigAccess,
    hook: DefraSessionHook,
    lifecycle: RequestLifecycle,
    writer: DefraStreamWriter,
    session_id: String,
    request_id: String,
}

async fn signed_run(name: &str, batch_interval: Duration) -> SignedRun {
    use gents_protocol::request_admission::{
        AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
    };

    let (db, user, owner, binding) = crate::test_support::signed_test_node(name).await;
    let node = db.node.clone();
    let runtime_did = db.node_identity.did().to_owned();
    let agent = crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
        .unwrap();
    let access = crate::ConfigAccess::local_managed_agent(
        node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let request_id = format!("request-{name}-{}", uuid::Uuid::new_v4());
    let session_id = format!("session-{name}-{}", uuid::Uuid::new_v4());
    let create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        &request_id,
        runtime_did.clone(),
        user.did(),
        agent.did(),
        &session_id,
        "spawn",
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        AgentRequestAdmissionRecord::client(user.did()),
    );
    let mut lifecycle =
        crate::tools::call::lifecycle::admission_fixture::pending_client_fixture_request(
            &node,
            &owner,
            db.node_identity.clone(),
            &user,
            agent.as_ref(),
            create,
        )
        .await
        .unwrap();
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    let hook = DefraSessionHook::resume_with_actor_policy(
        node.clone(),
        &session_id,
        &runtime_did,
        Some(user.did()),
        agent.did().parse().unwrap(),
        FailurePolicy::default(),
    )
    .await
    .unwrap()
    .with_managed_identity(agent.as_ref())
    .unwrap();
    let writer =
        DefraStreamWriter::new_for_lifecycle(node.clone(), &lifecycle, batch_interval).unwrap();
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    SignedRun {
        db,
        user,
        node,
        runtime_did,
        access,
        hook,
        lifecycle,
        writer,
        session_id,
        request_id,
    }
}

/// Binds the hook to the claimed request before its first provider attempt.
/// Mirrors the daemon: tool-call persistence requires the claimed deadline.
async fn bind_active_request(hook: &DefraSessionHook, lifecycle: &RequestLifecycle) {
    let request = lifecycle.request();
    hook.set_active_request_binding(
        Some(request.request_id.clone()),
        Some(request.doc_id.clone()),
        request.requester_did.clone(),
    )
    .await;
    hook.set_request_deadline_at(lifecycle.claimed_deadline_at())
        .await;
}

/// A read through the Agent's managed access, shaped like the node's GraphQL response.
struct ReadResponse {
    data: Option<serde_json::Value>,
    errors: Vec<serde_json::Value>,
}

impl ReadResponse {
    fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }
}

async fn exec(access: &crate::ConfigAccess, query: &str) -> ReadResponse {
    let response = access.execute(query).await.expect("agent-scoped read");
    ReadResponse {
        data: response.get("data").cloned(),
        errors: response
            .get("errors")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default(),
    }
}

async fn count_segments(access: &crate::ConfigAccess, doc: &str) -> usize {
    let response = access
        .execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            crate::store::graphql::escape_graphql_string(doc)
        ))
        .await
        .unwrap();
    response["data"]["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .len()
}

#[derive(Debug)]
struct PersistedMessageShape {
    sequence: u32,
    role: String,
    content: String,
}

async fn load_message_shapes_in_scope(
    access: &crate::ConfigAccess,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Vec<PersistedMessageShape> {
    let scope_node_did = crate::store::graphql::escape_graphql_string(node_did);
    let scope_requester = requester_did.map_or_else(
        || "null".to_owned(),
        |did| format!("\"{}\"", crate::store::graphql::escape_graphql_string(did)),
    );
    let session_id = crate::store::graphql::escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{session_id}" }},
                    node_did: {{ _eq: "{scope_node_did}" }}, requester_did: {{ _eq: {scope_requester} }} }},
                order: {{ sequence: ASC }}
            ) {{ _docID }}
        }}"#
    );
    let response = access.execute(&query).await.unwrap();
    let rows = response["data"]["AgentMessage"].as_array().unwrap();
    let mut shapes = Vec::with_capacity(rows.len());
    for row in rows {
        let doc_id = row["_docID"].as_str().expect("physical message identity");
        let (header, message) =
            crate::session::load_canonical_message(access, doc_id, node_did, requester_did)
                .await
                .expect("exact canonical message reconstructs");
        shapes.push(PersistedMessageShape {
            sequence: header.sequence,
            role: serde_json::to_value(header.role)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned(),
            content: serde_json::to_string(&message).unwrap(),
        });
    }
    shapes
}

fn text_item(text: &str) -> Result<LoopStreamItem, crate::error::LoopStreamError> {
    Ok(LoopStreamItem::Text(text.to_string()))
}

fn tool_call_item(
    name: &str,
    args_json: &str,
    internal_id: &str,
) -> Result<LoopStreamItem, crate::error::LoopStreamError> {
    tool_call_item_with_ids(name, args_json, internal_id, internal_id, None)
}

fn tool_call_item_with_ids(
    name: &str,
    args_json: &str,
    tool_id: &str,
    internal_id: &str,
    call_id: Option<&str>,
) -> Result<LoopStreamItem, crate::error::LoopStreamError> {
    Ok(LoopStreamItem::ToolCall {
        tool_call: ToolCall {
            raw_arguments: None,
            id: tool_id.to_string(),
            call_id: call_id.map(ToOwned::to_owned),
            function: ToolFunction {
                name: name.to_string(),
                arguments: serde_json::from_str(args_json).unwrap(),
            },
            signature: None,
            additional_params: None,
        },
        internal_call_id: internal_id.to_string(),
    })
}

fn tool_result_item(
    tool_id: &str,
    result_json: &str,
    internal_id: &str,
) -> Result<LoopStreamItem, crate::error::LoopStreamError> {
    tool_result_item_with_call_id(tool_id, None, result_json, internal_id)
}

fn tool_result_item_with_call_id(
    tool_id: &str,
    call_id: Option<&str>,
    result_json: &str,
    internal_id: &str,
) -> Result<LoopStreamItem, crate::error::LoopStreamError> {
    Ok(LoopStreamItem::ToolResult {
        tool_result: crate::inference::llm::message::ToolResult {
            id: tool_id.to_string(),
            call_id: call_id.map(ToOwned::to_owned),
            content: vec![ToolResultContent::text(result_json)],
        },
        internal_call_id: internal_id.to_string(),
    })
}

fn final_item(response_text: &str) -> Result<LoopStreamItem, crate::error::LoopStreamError> {
    Ok(LoopStreamItem::Final {
        text: response_text.to_string(),
    })
}

fn turn_retracted_item(
    turn: usize,
    attempt: u32,
) -> Result<LoopStreamItem, crate::error::LoopStreamError> {
    Ok(LoopStreamItem::TurnRetracted {
        turn,
        attempt,
        backoff: std::time::Duration::ZERO,
    })
}

#[tokio::test]
async fn hook_persisted_tool_result_dedupes_matching_stream_result() {
    let SignedRun {
        db: _db,
        user,
        runtime_did,
        access,
        hook,
        mut lifecycle,
        writer: stream_writer,
        session_id,
        ..
    } = signed_run("hook-persisted-tool-result", Duration::from_millis(0)).await;
    assert!(matches!(
        hook.on_completion_call(&user_text_message("discover available tools"), &[])
            .await,
        HookAction::Continue
    ));
    bind_active_request(&hook, &lifecycle).await;
    let response_doc_id = lifecycle.request().doc_id.clone();
    stream_writer
        .start_provider_attempt(&response_doc_id, 0, 0, "inference.1".parse().unwrap())
        .await;

    let mut processor = StreamProcessor::new(
        &hook,
        &stream_writer,
        &mut lifecycle,
        &response_doc_id,
        ProviderInputProfile::OpenAiChatCompletions,
    );

    let stored_call_id = "OaoTQYzCdoptKiK_mdhBA";
    let model_result_id = "c6b8bdeb-ab92-4481-b763-bdafbd463904";
    let tool_args = r#"{"tool":"discover_tools"}"#;
    let tool_result = r#"{"tools":["discover_tools","describe_tool"]}"#;

    processor
        .process_item(tool_call_item_with_ids(
            "discover_tools",
            tool_args,
            model_result_id,
            stored_call_id,
            Some(model_result_id),
        ))
        .await
        .unwrap();
    processor
        .process_item(Ok(LoopStreamItem::ProviderTurnReady {
            turn: 0,
            attempt: 0,
            message: Message::Assistant {
                request_id: None,
                id: Some("provider-message".to_string()),
                content: vec![AssistantContent::ToolCall(ToolCall {
                    raw_arguments: None,
                    id: model_result_id.to_string(),
                    call_id: Some(model_result_id.to_string()),
                    function: ToolFunction {
                        name: "discover_tools".to_string(),
                        arguments: serde_json::from_str(tool_args).unwrap(),
                    },
                    signature: None,
                    additional_params: None,
                })],
            },
        }))
        .await
        .unwrap();
    let action = hook
        .on_tool_call(
            "discover_tools",
            Some(model_result_id.to_string()),
            stored_call_id,
            tool_args,
        )
        .await;
    assert!(
        matches!(action, crate::inference::llm::ToolCallHookAction::Continue),
        "tool call persistence failed: {action:?}"
    );
    assert!(matches!(
        hook.on_tool_result(
            "discover_tools",
            Some(model_result_id.to_string()),
            stored_call_id,
            tool_args,
            &crate::tools::call::lifecycle::ToolOutcome::Completed(tool_result.to_string()),
        )
        .await,
        HookAction::Continue
    ));

    processor
        .process_item(tool_result_item_with_call_id(
            model_result_id,
            Some(model_result_id),
            tool_result,
            stored_call_id,
        ))
        .await
        .unwrap();

    let history = crate::session::load_history_with_access(
        &access,
        &session_id,
        &runtime_did,
        Some(user.did()),
    )
    .await
    .unwrap();
    let tool_results = history
        .iter()
        .filter_map(|message| match message {
            Message::User { content } => match first_content(content) {
                UserContent::ToolResult(tool_result) => Some(tool_result),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        tool_results.len(),
        1,
        "hook and stream paths must materialize one logical tool result"
    );
    assert_eq!(tool_results[0].id, model_result_id);
    assert_eq!(tool_results[0].call_id.as_deref(), Some(model_result_id));
    assert!(matches!(
        first_content(&tool_results[0].content),
        ToolResultContent::Text(Text { text }) if text == tool_result
    ));
    let response = exec(
        &access,
        &format!(
            r#"{{ AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            crate::store::graphql::escape_graphql_string(model_result_id),
        ),
    )
    .await;
    let tool_doc_id = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(serde_json::Value::as_str)
        .expect("physical tool call identity");
    let native = crate::session::load_tool_call_result(
        &access,
        tool_doc_id,
        &runtime_did,
        &session_id,
        Some(user.did()),
    )
    .await
    .unwrap();
    assert_eq!(
        crate::tools::call::lifecycle::query::render_tool_result(&native).unwrap(),
        tool_result
    );
}

#[tokio::test]
async fn streamed_wait_call_precedes_concurrent_notification_and_tool_result() {
    let SignedRun {
        db,
        user,
        node,
        runtime_did,
        access,
        hook,
        mut lifecycle,
        writer: stream_writer,
        session_id,
        request_id,
        ..
    } = signed_run("streamed-wait-call", Duration::from_millis(0)).await;
    assert!(matches!(
        hook.on_completion_call(&user_text_message("spawn"), &[])
            .await,
        HookAction::Continue
    ));
    bind_active_request(&hook, &lifecycle).await;
    stream_writer
        .publish_authored_message(&lifecycle, "prompt", &user_text_message("spawn"))
        .await
        .unwrap();
    let response_doc_id = lifecycle.request().doc_id.clone();
    let notification_request = lifecycle.request().clone();
    let agent_did = access.actor_did().unwrap();
    let mut processor = StreamProcessor::new(
        &hook,
        &stream_writer,
        &mut lifecycle,
        &response_doc_id,
        ProviderInputProfile::OpenAiChatCompletions,
    );

    processor
        .process_item(Ok(LoopStreamItem::ProviderAttemptStarted {
            turn: 0,
            attempt: 0,
            capture_scope: "inference.1".parse().unwrap(),
        }))
        .await
        .unwrap();
    processor
        .process_item(tool_call_item_with_ids(
            "read_file",
            r#"{"path":"/work/entry.c"}"#,
            "result-1",
            "internal-1",
            Some("call-1"),
        ))
        .await
        .unwrap();

    let accepted_message = processor.assistant_turn.clone().take_message().unwrap();
    processor
        .process_item(Ok(LoopStreamItem::ProviderTurnReady {
            turn: 0,
            attempt: 0,
            message: accepted_message.clone(),
        }))
        .await
        .unwrap();

    assert!(matches!(
        hook.on_tool_call(
            "read_file",
            Some("call-1".to_string()),
            "internal-1",
            r#"{"path":"/work/entry.c"}"#,
        )
        .await,
        crate::inference::llm::ToolCallHookAction::Continue
    ));

    let queue = gents_protocol::request_input::RequestQueue {
        delivery: Default::default(),
        position: None,
        source: gents_protocol::request_input::QueueSource::BackgroundCompletion,
        policy: gents_protocol::request_input::QueuePolicy::Coalesce,
        key: Some(format!("background_completion:{session_id}")),
        queued_after_request_id: Some(request_id.clone()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    };
    let mut wake = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        "concurrent-notification-wake",
        runtime_did.clone(),
        user.did(),
        &agent_did,
        &session_id,
        "Review background completion",
        "scheduled",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_local_control(
            runtime_did.clone(),
            &request_id,
        ),
    );
    wake.input.queue = Some(queue.clone());
    wake.caused_by_parent_request_id = Some(request_id.clone());
    wake.caused_by_parent_request_doc_id = Some(notification_request.doc_id.clone());
    crate::sign_agent_request_create(db.node_identity.as_ref(), &mut wake)
        .await
        .unwrap();
    crate::store::config_client::ConfigAccess::local(node.clone())
        .write(
            "test.stream_processor.pending_wake",
            &wake.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
    // The parent Agent's own managed access supplies the signer the atomic write requires.
    crate::request::lifecycle::queue::persist_background_completion_with_message_waking_with_access(
        &access,
        &node,
        &notification_request,
        "<tool-notification status=\"completed\" />",
        "background-completion-notification:concurrent-tool:tool",
        "Review background completion",
        queue,
        None,
        gents_protocol::request_lifecycle::RequestHopCause::Continuation,
    )
    .await
    .unwrap();

    assert!(matches!(
        hook.on_tool_result(
            "read_file",
            Some("call-1".to_string()),
            "internal-1",
            r#"{"path":"/work/entry.c"}"#,
            &crate::tools::call::lifecycle::ToolOutcome::Completed("source bytes".to_string()),
        )
        .await,
        HookAction::Continue
    ));

    processor
        .process_item(tool_result_item_with_call_id(
            "result-1",
            Some("call-1"),
            "source bytes",
            "internal-1",
        ))
        .await
        .unwrap();

    let history = crate::session::load_history_with_access(
        &access,
        &session_id,
        &runtime_did,
        Some(user.did()),
    )
    .await
    .unwrap();
    assert_eq!(history.len(), 4);
    assert!(matches!(&history[1], Message::Assistant { content, .. }
        if matches!(content.first(), Some(AssistantContent::ToolCall(tool_call))
            if tool_call.call_id.as_deref() == Some("call-1"))));
    assert!(matches!(&history[2], Message::User { content }
        if matches!(content.first(), Some(UserContent::Text(Text { text }))
            if text.contains("<tool-notification"))));
    assert!(matches!(&history[3], Message::User { content }
        if matches!(content.first(), Some(UserContent::ToolResult(tool_result))
            if tool_result.call_id.as_deref() == Some("call-1"))));

    let shapes =
        load_message_shapes_in_scope(&access, &session_id, &runtime_did, Some(user.did())).await;
    assert_eq!(
        shapes
            .iter()
            .map(|row| row.role.as_str())
            .collect::<Vec<_>>(),
        vec!["user", "assistant", "user", "user"]
    );
    assert!(
        shapes
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence),
        "assistant wait, background notification, and result need distinct ordered rows: {shapes:#?}"
    );
    assert_eq!(
        shapes.iter().map(|row| row.sequence).collect::<Vec<_>>(),
        vec![1, 2, 3, 4],
        "the inline wait race must not overwrite a row or leave a phantom reservation"
    );
    assert!(
        shapes[1].content.contains("\"role\":\"assistant\""),
        "assistant database role must agree with its serialized envelope"
    );
}

/// Three calls publish in one accepted assistant turn; each result closes
/// independently and its later streamed observation cannot duplicate the
/// durable delivery (Lean: `Transcript.parallel_results_complete_independently`).
#[tokio::test]
async fn multiple_streamed_tool_results_share_one_accumulated_assistant_turn() {
    let SignedRun {
        db: _db,
        user,
        node,
        runtime_did,
        access,
        hook,
        mut lifecycle,
        writer: stream_writer,
        session_id,
        ..
    } = signed_run("multiple-streamed-tool-results", Duration::from_millis(0)).await;
    assert!(matches!(
        hook.on_completion_call(&user_text_message("read several files"), &[])
            .await,
        HookAction::Continue
    ));
    bind_active_request(&hook, &lifecycle).await;
    let response_doc_id = lifecycle.request().doc_id.clone();
    let mut processor = StreamProcessor::new(
        &hook,
        &stream_writer,
        &mut lifecycle,
        &response_doc_id,
        ProviderInputProfile::OpenAiChatCompletions,
    );

    processor
        .process_item(Ok(LoopStreamItem::AuthoredInputReady {
            context: None,
            prompt: user_text_message("read several files"),
            folded: Vec::new(),
            reply: tokio::sync::oneshot::channel().0,
        }))
        .await
        .unwrap();
    processor
        .process_item(Ok(LoopStreamItem::ProviderAttemptStarted {
            turn: 0,
            attempt: 0,
            capture_scope: "inference.1".parse().unwrap(),
        }))
        .await
        .unwrap();
    for index in 1..=3 {
        let result_id = format!("result-{index}");
        let internal_id = format!("internal-{index}");
        let call_id = format!("call-{index}");
        let args = format!(r#"{{"path":"/work/file-{index}.c"}}"#);
        processor
            .process_item(tool_call_item_with_ids(
                "read_file",
                &args,
                &result_id,
                &internal_id,
                Some(&call_id),
            ))
            .await
            .unwrap();
    }
    let accepted_message = processor.assistant_turn.clone().take_message().unwrap();
    processor
        .process_item(Ok(LoopStreamItem::ProviderTurnReady {
            turn: 0,
            attempt: 0,
            message: accepted_message.clone(),
        }))
        .await
        .unwrap();
    for index in 1..=3 {
        let internal_id = format!("internal-{index}");
        let call_id = format!("call-{index}");
        let args = format!(r#"{{"path":"/work/file-{index}.c"}}"#);
        assert!(matches!(
            hook.on_tool_call("read_file", Some(call_id.clone()), &internal_id, &args,)
                .await,
            crate::inference::llm::ToolCallHookAction::Continue
        ));
        assert!(matches!(
            hook.on_tool_result(
                "read_file",
                Some(call_id),
                &internal_id,
                &args,
                &crate::tools::call::lifecycle::ToolOutcome::Completed(format!(
                    "source bytes {index}"
                )),
            )
            .await,
            HookAction::Continue
        ));
    }

    for index in 1..=3 {
        processor
            .process_item(tool_result_item_with_call_id(
                &format!("result-{index}"),
                Some(&format!("call-{index}")),
                &format!("source bytes {index}"),
                &format!("internal-{index}"),
            ))
            .await
            .unwrap();
    }

    let history = crate::session::load_history_with_access(
        &access,
        &session_id,
        &runtime_did,
        Some(user.did()),
    )
    .await
    .unwrap();
    assert_eq!(history.len(), 5);
    assert_eq!(history[1], accepted_message);
    assert!(matches!(&history[1], Message::Assistant { content, .. }
        if content.iter().filter(|item| matches!(item, AssistantContent::ToolCall(_))).count() == 3));
    let result_count = history
        .iter()
        .filter(|message| {
            matches!(message, Message::User { content }
            if matches!(content.first(), Some(UserContent::ToolResult(_))))
        })
        .count();
    assert_eq!(result_count, 3);

    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn post_tool_resumption_keeps_each_provider_turn_separate() {
    let SignedRun {
        db: _db,
        user,
        node,
        runtime_did,
        access,
        hook,
        mut lifecycle,
        writer: stream_writer,
        session_id,
        ..
    } = signed_run("post-tool-resumption", Duration::from_millis(0)).await;
    assert!(matches!(
        hook.on_completion_call(&user_text_message("test prompt"), &[])
            .await,
        HookAction::Continue
    ));
    bind_active_request(&hook, &lifecycle).await;
    let response_doc_id = lifecycle.request().doc_id.clone();

    let mut processor = StreamProcessor::new(
        &hook,
        &stream_writer,
        &mut lifecycle,
        &response_doc_id,
        ProviderInputProfile::OpenAiChatCompletions,
    );

    processor
        .process_item(Ok(LoopStreamItem::ProviderAttemptStarted {
            turn: 0,
            attempt: 0,
            capture_scope: "inference.1".parse().unwrap(),
        }))
        .await
        .unwrap();
    processor.process_item(text_item("hello ")).await.unwrap();
    processor.process_item(text_item("world")).await.unwrap();
    let args = r#"{"tool":"discover_tools"}"#;
    processor
        .process_item(tool_call_item("discover_tools", args, "call-1"))
        .await
        .unwrap();
    let accepted_message = processor.assistant_turn.clone().take_message().unwrap();
    processor
        .process_item(Ok(LoopStreamItem::ProviderTurnReady {
            turn: 0,
            attempt: 0,
            message: accepted_message.clone(),
        }))
        .await
        .unwrap();
    let action = hook
        .on_tool_call("discover_tools", None, "call-1", args)
        .await;
    assert!(
        matches!(action, crate::inference::llm::ToolCallHookAction::Continue),
        "{action:?}"
    );
    assert!(matches!(
        hook.on_tool_result(
            "discover_tools",
            None,
            "call-1",
            args,
            &crate::tools::call::lifecycle::ToolOutcome::Completed(r#"{"hit":1}"#.into())
        )
        .await,
        HookAction::Continue
    ));
    processor
        .process_item(tool_result_item("call-1", r#"{"hit":1}"#, "call-1"))
        .await
        .unwrap();
    assert!(processor.assistant_turn.clone().take_message().is_none());
    assert!(processor.next_flush_deadline().await.is_none());

    processor
        .process_item(Ok(LoopStreamItem::ProviderAttemptStarted {
            turn: 1,
            attempt: 0,
            capture_scope: "inference.1".parse().unwrap(),
        }))
        .await
        .unwrap();
    processor.process_item(text_item("done")).await.unwrap();
    let final_message = Message::Assistant {
        request_id: None,
        id: None,
        content: vec![AssistantContent::Text(Text {
            text: "done".into(),
        })],
    };
    assert_eq!(
        processor.assistant_turn.clone().take_message(),
        Some(final_message.clone())
    );
    processor
        .process_item(Ok(LoopStreamItem::ProviderTurnReady {
            turn: 1,
            attempt: 0,
            message: final_message.clone(),
        }))
        .await
        .unwrap();

    processor.process_item(final_item("done")).await.unwrap();

    let history = crate::session::load_history_with_access(
        &access,
        &session_id,
        &runtime_did,
        Some(user.did()),
    )
    .await
    .unwrap();
    assert_eq!(
        history.len(),
        3,
        "two provider headers and exactly one tool result"
    );
    assert_eq!(history[0], accepted_message);
    assert!(matches!(&history[1], Message::User { content }
        if matches!(first_content(content), UserContent::ToolResult(result)
            if result.id == "call-1" && matches!(first_content(&result.content),
                ToolResultContent::Text(text) if text.text == r#"{"hit":1}"#))));
    assert_eq!(history[2], final_message);

    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn turn_retraction_retains_old_bytes_but_publishes_only_the_retry() {
    let SignedRun {
        db: _db,
        user,
        node,
        runtime_did,
        access,
        hook,
        mut lifecycle,
        writer: stream_writer,
        session_id,
        ..
    } = signed_run("turn-retraction", Duration::from_millis(0)).await;
    assert!(matches!(
        hook.on_completion_call(&user_text_message("test prompt"), &[])
            .await,
        HookAction::Continue
    ));
    bind_active_request(&hook, &lifecycle).await;
    let response_doc_id = lifecycle.request().doc_id.clone();
    let mut processor = StreamProcessor::new(
        &hook,
        &stream_writer,
        &mut lifecycle,
        &response_doc_id,
        ProviderInputProfile::OpenAiChatCompletions,
    );

    processor
        .process_item(Ok(LoopStreamItem::ProviderAttemptStarted {
            turn: 0,
            attempt: 0,
            capture_scope: "inference.1".parse().unwrap(),
        }))
        .await
        .unwrap();
    processor.process_item(text_item("Hel")).await.unwrap();
    processor
        .process_item(tool_call_item_with_ids(
            "read_file",
            r#"{"path":"abandoned.rs"}"#,
            "abandoned-result",
            "abandoned-internal",
            Some("abandoned-call"),
        ))
        .await
        .unwrap();

    processor
        .process_item(turn_retracted_item(0, 0))
        .await
        .unwrap();
    assert_eq!(processor.streamed_text, "");

    processor
        .process_item(Ok(LoopStreamItem::ProviderAttemptStarted {
            turn: 0,
            attempt: 1,
            capture_scope: "inference.1".parse().unwrap(),
        }))
        .await
        .unwrap();
    processor
        .process_item(text_item("Hello world"))
        .await
        .unwrap();

    processor
        .process_item(Ok(LoopStreamItem::ProviderTurnReady {
            turn: 0,
            attempt: 1,
            message: Message::Assistant {
                request_id: None,
                id: None,
                content: vec![AssistantContent::Text(Text {
                    text: "Hello world".into(),
                })],
            },
        }))
        .await
        .unwrap();

    processor
        .process_item(final_item("Hello world"))
        .await
        .unwrap();
    assert_eq!(processor.streamed_text, "Hello world");

    let history = crate::session::load_history_with_access(
        &access,
        &session_id,
        &runtime_did,
        Some(user.did()),
    )
    .await
    .unwrap();
    assert!(history.iter().all(|message| match message {
        Message::Assistant { content, .. } => !content
            .iter()
            .any(|item| matches!(item, AssistantContent::ToolCall(_))),
        _ => true,
    }));
    let assistant_texts = history
        .iter()
        .filter_map(|message| match message {
            Message::Assistant { content, .. } => content.iter().find_map(|item| match item {
                AssistantContent::Text(text) => Some(text.text.as_str()),
                _ => None,
            }),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        assistant_texts,
        vec!["Hello world"],
        "partial retracted text must not persist as an assistant message"
    );

    use gents_protocol::output::live::reconstruct_dense_prefix;
    use gents_protocol::output::reconstruction::ObservedSegment;
    use gents_protocol::output::rows::{decode_output_segment_row, AGENT_OUTPUT_SEGMENT_FIELDS};
    use gents_protocol::output::{OutputSource, SourceClose};
    let result = exec(&access, &format!(
        r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#,
        crate::store::graphql::escape_graphql_string(&response_doc_id)
    )).await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    let records = result.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| decode_output_segment_row(row).unwrap())
        .collect::<Vec<_>>();
    let old = records
        .iter()
        .find(|row| {
            matches!(
                row.segment.source,
                OutputSource::ProviderTurn { attempt: 0, .. }
            )
        })
        .expect("old attempt remains durable");
    assert!(records
        .iter()
        .any(|row| row.segment.source == old.segment.source
            && matches!(row.segment.close, Some(SourceClose::Retracted))));
    let facts = records
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    let prefix = reconstruct_dense_prefix(
        &facts,
        &response_doc_id,
        &old.segment.source,
        &old.segment.writer,
        None,
    )
    .unwrap();
    assert_eq!(prefix.streams.len(), 2);
    assert_eq!(prefix.streams[0].text, "Hel");

    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn corrupt_tool_call_arguments_persist_object_shaped() {
    let SignedRun {
        db: _db,
        user,
        node,
        runtime_did,
        access,
        hook,
        mut lifecycle,
        writer: stream_writer,
        session_id,
        ..
    } = signed_run("corrupt-tool-args", Duration::from_millis(0)).await;
    assert!(matches!(
        hook.on_completion_call(&user_text_message("describe list_hosts"), &[])
            .await,
        HookAction::Continue
    ));
    bind_active_request(&hook, &lifecycle).await;
    let response_doc_id = lifecycle.request().doc_id.clone();
    let mut processor = StreamProcessor::new(
        &hook,
        &stream_writer,
        &mut lifecycle,
        &response_doc_id,
        ProviderInputProfile::OpenAiChatCompletions,
    );

    processor
        .process_item(Ok(LoopStreamItem::ProviderAttemptStarted {
            turn: 0,
            attempt: 0,
            capture_scope: "inference.1".parse().unwrap(),
        }))
        .await
        .unwrap();
    let corrupt_call: Result<LoopStreamItem, crate::error::LoopStreamError> =
        Ok(LoopStreamItem::ToolCall {
            tool_call: crate::inference::llm::rig_compat::from_rig_tool_call(
                &rig::completion::message::ToolCall {
                    id: "result-1".to_string(),
                    call_id: Some("call-1".to_string()),
                    function: rig::completion::message::ToolFunction {
                        name: "describe_tool".to_string(),
                        arguments: serde_json::Value::String(
                            crate::test_support::CORRUPT_TOOL_ARGS_589.to_string(),
                        ),
                    },
                    signature: None,
                    additional_params: None,
                },
            ),
            internal_call_id: "internal-1".to_string(),
        });
    processor.process_item(corrupt_call).await.unwrap();

    let message = processor.assistant_turn.clone().take_message().unwrap();
    processor
        .process_item(Ok(LoopStreamItem::ProviderTurnReady {
            turn: 0,
            attempt: 0,
            message,
        }))
        .await
        .unwrap();

    assert!(matches!(
        hook.on_tool_call(
            "describe_tool",
            Some("call-1".to_string()),
            "internal-1",
            crate::test_support::CORRUPT_TOOL_ARGS_589,
        )
        .await,
        crate::inference::llm::ToolCallHookAction::Continue
    ));
    assert!(matches!(
        hook.on_tool_result(
            "describe_tool",
            Some("call-1".to_string()),
            "internal-1",
            crate::test_support::CORRUPT_TOOL_ARGS_589,
            &crate::tools::call::lifecycle::ToolOutcome::Completed(
                "described:list_hosts".to_string()
            ),
        )
        .await,
        HookAction::Continue
    ));

    processor
        .process_item(tool_result_item_with_call_id(
            "result-1",
            Some("call-1"),
            "described:list_hosts",
            "internal-1",
        ))
        .await
        .unwrap();

    let history = crate::session::load_history_with_access(
        &access,
        &session_id,
        &runtime_did,
        Some(user.did()),
    )
    .await
    .unwrap();
    let arguments = history
        .iter()
        .find_map(|message| match message {
            Message::Assistant { content, .. } => content.iter().find_map(|item| match item {
                AssistantContent::ToolCall(tool_call) => Some(&tool_call.function.arguments),
                _ => None,
            }),
            _ => None,
        })
        .expect("a persisted assistant tool-call message");
    assert!(
        arguments.is_object(),
        "non-object tool-call arguments persisted to durable history: {arguments:?}"
    );
    assert_eq!(
        arguments["tool_name"], "list_hosts",
        "the salvageable #589 payload must persist its intended object"
    );

    node.shutdown().await.expect("node shutdown");
}

#[test]
fn coalesced_reasoning_fragments_encode_as_one_native_stream() {
    use crate::inference::llm::message::ReasoningContent;
    use crate::request::streaming::native_encoding::encode_native_message;

    let mut accumulator = AssistantTurnAccumulator::default();
    for _ in 0..128 {
        accumulator.push_reasoning_delta(Some("reasoning".into()), "fragment 🦀 ");
    }
    let fragmented = Message::Assistant {
        request_id: None,
        id: None,
        content: vec![AssistantContent::Reasoning(Reasoning {
            id: Some("reasoning".into()),
            content: (0..128)
                .map(|_| ReasoningContent::Text {
                    text: "fragment 🦀 ".into(),
                    signature: None,
                })
                .collect(),
        })],
    };
    let before = encode_native_message(&fragmented).unwrap();
    let after = encode_native_message(&accumulator.take_message().unwrap()).unwrap();
    assert_eq!((before.streams.len(), after.streams.len()), (128, 1));
    let before =
        serde_json::to_vec(&before.build_blocks("closing-document", &[]).unwrap()).unwrap();
    let after = serde_json::to_vec(&after.build_blocks("closing-document", &[]).unwrap()).unwrap();
    assert!(
        after.len() * 32 < before.len(),
        "{} vs {}",
        after.len(),
        before.len()
    );
}
