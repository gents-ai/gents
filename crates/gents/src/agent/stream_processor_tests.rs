use std::sync::Arc;
use std::time::Duration;

use crate::llm::message::{
    AssistantContent, Message, Reasoning, Text, ToolCall, ToolFunction, ToolResultContent,
    UserContent,
};
use crate::llm::HookAction;
use gents_loop::provider_input::ProviderInputProfile;
use rig::agent::MultiTurnStreamItem;
use rig::streaming::{StreamedAssistantContent, StreamedUserContent};

use super::*;
use crate::ensure_runtime_schemas;
use crate::hook::FailurePolicy;
use crate::lifecycle::{ClaimOutcome, ExecutionOrigin, RequestLifecycle};
use crate::streaming::DefraStreamWriter;
use crate::test_support::first_content;
use crate::watcher::AgentRequest;

#[test]
fn claude_signed_final_seals_preview_without_duplicating_bytes() {
    use crate::llm::message::ReasoningContent;

    let mut accumulator = AssistantTurnAccumulator::default();
    accumulator.push_provider_reasoning_delta(ProviderInputProfile::ClaudeMessages, None, "考");
    accumulator.push_provider_reasoning_delta(ProviderInputProfile::ClaudeMessages, None, "慮");
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
    mismatch.push_provider_reasoning_delta(ProviderInputProfile::ClaudeMessages, None, "prefix");
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
    other.push_provider_reasoning_delta(ProviderInputProfile::OpenAiChatCompletions, None, "first");
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

fn user_text_message(text: &str) -> Message {
    Message::User {
        content: vec![UserContent::Text(Text {
            text: text.to_string(),
        })],
    }
}

// Preserve the created physical request identity and exact owner/requester scope.
fn fixture_agent_request(
    doc_id: String,
    request_id: &str,
    session_id: &str,
    content: &str,
) -> AgentRequest {
    AgentRequest {
        doc_id,
        request_id: request_id.to_string(),
        agent_did: "did:test:test".to_string(),
        requester_did: None,
        behavior_id: "general".to_string(),
        session_id: session_id.to_string(),
        content: content.to_string(),
        max_total_tokens: None,
        input: gents_protocol::request_input::RequestInput::default(),
        execution_origin: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        deadline: None,
        execution_generation: None,
        execution_lease_expires_at: None,
        execution_lease_secs: None,
        subagent_depth: 0,
        caused_by_parent_request_id: None,
        caused_by_parent_request_doc_id: None,
        caused_by_parent_tool_call_id: None,
        caused_by_parent_tool_call_doc_id: None,
        caused_by_trigger_id: None,
        caused_by_trigger_kind: None,
        caused_by_source_doc_id: None,
        caused_by_correlation: None,
        caused_by_trigger_context: None,
        workspace_id: None,
        workspace_owner_agent_did: None,
        workspace_authority: None,
        workspace_seal_hash: None,
    }
}

#[tokio::test]
async fn first_visible_flushes_immediately_and_followup_waits_for_cadence() {
    let data_path =
        std::env::temp_dir().join(format!("processor-cadence-{}", uuid::Uuid::new_v4()));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_runtime_schemas(&node).await.unwrap();
    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:test:test",
        FailurePolicy::default(),
    );
    assert!(matches!(
        hook.on_completion_call(&user_text_message("cadence"), &[])
            .await,
        HookAction::Continue
    ));
    let session_id = hook.session_id().await.unwrap();
    let request_id = uuid::Uuid::new_v4().to_string();
    let request_doc_id = create_pending_request(&node, &request_id, &session_id).await;
    hook.set_active_request_lineage(Some(request_id.clone()), None)
        .await
        .unwrap();
    let request = fixture_agent_request(request_doc_id, &request_id, &session_id, "cadence");
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        "test-agent",
        "did:test:test",
        request,
        30,
        ExecutionOrigin::Interactive,
        "test-backend",
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    let writer = DefraStreamWriter::new(node.clone(), "did:test:test", Duration::from_secs(60));
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    let doc_id = lifecycle.request().doc_id.clone();
    let mut processor = StreamProcessor::new(
        &hook,
        &writer,
        &mut lifecycle,
        &doc_id,
        ProviderInputProfile::OpenAiChatCompletions,
    );
    processor
        .process_item::<()>(Ok(LoopStreamItem::ProviderAttemptStarted {
            turn: 0,
            attempt: 0,
            capture_scope: "inference.1".parse().unwrap(),
        }))
        .await
        .unwrap();
    processor.process_item(text_item("first")).await.unwrap();
    async fn count(node: &defra_node::EmbeddedNode, doc: &str) -> usize {
        let response = node.execute(&format!(r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#, crate::graphql::escape_graphql_string(doc))).await;
        response.data.as_ref().unwrap()["AgentOutputSegment"]
            .as_array()
            .unwrap()
            .len()
    }
    assert_eq!(count(&node, &doc_id).await, 1);
    processor.process_item(text_item(" second")).await.unwrap();
    assert_eq!(count(&node, &doc_id).await, 1);
    assert!(processor.next_flush_deadline().await.is_some());
    processor.flush_pending().await.unwrap();
    assert_eq!(count(&node, &doc_id).await, 2);
    assert!(
        processor.next_flush_deadline().await.is_none(),
        "a committed batch must disarm its timer so provider polling can resume"
    );
    processor.flush_pending().await.unwrap();
    assert_eq!(
        count(&node, &doc_id).await,
        2,
        "no-op flush must not append"
    );
    assert!(processor.next_flush_deadline().await.is_none());
    processor.process_item(text_item(" third")).await.unwrap();
    assert!(processor.next_flush_deadline().await.is_some());
    processor.flush_pending().await.unwrap();
    assert_eq!(count(&node, &doc_id).await, 3);
    assert!(processor.next_flush_deadline().await.is_none());
    processor
        .process_item::<()>(Ok(LoopStreamItem::Item(
            MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::ReasoningDelta {
                id: None,
                reasoning: "first reasoning".into(),
            }),
        )))
        .await
        .unwrap();
    assert_eq!(
        count(&node, &doc_id).await,
        4,
        "first reasoning must be durable immediately too"
    );
    assert!(processor.next_flush_deadline().await.is_none());
    processor.process_item(text_item(" fourth")).await.unwrap();
    assert!(processor.next_flush_deadline().await.is_some());
    let message = processor.assistant_turn.clone().take_message().unwrap();
    processor
        .process_item::<()>(Ok(LoopStreamItem::ProviderTurnReady {
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
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(data_path);
}

#[tokio::test]
async fn pre_stream_failures_do_not_fabricate_provider_attempt_closures() {
    let data_path = std::env::temp_dir().join(format!(
        "processor-pre-stream-failure-{}",
        uuid::Uuid::new_v4()
    ));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_runtime_schemas(&node).await.unwrap();
    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:test:test",
        FailurePolicy::default(),
    );
    assert!(matches!(
        hook.on_completion_call(&user_text_message("retry provider"), &[])
            .await,
        HookAction::Continue
    ));
    let session_id = hook.session_id().await.unwrap();
    let request_id = uuid::Uuid::new_v4().to_string();
    let request_doc_id = create_pending_request(&node, &request_id, &session_id).await;
    hook.set_active_request_lineage(Some(request_id.clone()), None)
        .await
        .unwrap();
    let request = fixture_agent_request(request_doc_id, &request_id, &session_id, "retry provider");
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        "test-agent",
        "did:test:test",
        request,
        30,
        ExecutionOrigin::Interactive,
        "test-backend",
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    let writer = DefraStreamWriter::new(node.clone(), "did:test:test", Duration::from_secs(60));
    lifecycle.begin_owned_execution(&writer).await.unwrap();
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
                retry_after_secs: 1,
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
                .process_item::<()>(Ok(LoopStreamItem::AttemptFailed {
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

    let request_doc_id = crate::graphql::escape_graphql_string(&doc_id);
    let response = node
        .execute(&format!(
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
async fn persist_partial_turn_publishes_text_only_partial_and_retains_reasoning_bytes() {
    let data_path =
        std::env::temp_dir().join(format!("agent-stream-processor-{}", uuid::Uuid::new_v4()));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_runtime_schemas(&node).await.unwrap();

    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:test:test",
        FailurePolicy::default(),
    );
    assert!(matches!(
        hook.on_completion_call(&user_text_message("Inspect the repo"), &[])
            .await,
        HookAction::Continue
    ));

    let session_id = hook.session_id().await.expect("session id");
    let request_id = uuid::Uuid::new_v4().to_string();
    let request_doc_id = create_pending_request(&node, &request_id, &session_id).await;
    hook.set_active_request_lineage(Some(request_id.clone()), None)
        .await
        .expect("bind persisted request lineage");
    hook.set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::seconds(60)))
        .await;
    let request =
        fixture_agent_request(request_doc_id, &request_id, &session_id, "Inspect the repo");
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        "test-agent",
        "did:test:test",
        request,
        30,
        crate::lifecycle::ExecutionOrigin::Interactive,
        "test-backend",
    );
    let stream_writer = crate::streaming::DefraStreamWriter::new(
        node.clone(),
        "did:test:test",
        Duration::from_secs(60),
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    // Begin a streaming response so reset_tail (called by persist_partial_turn)
    // has a live buffer to clear.
    lifecycle
        .begin_owned_execution(&stream_writer)
        .await
        .unwrap();
    let response_doc_id = lifecycle.request().doc_id.clone();
    let mut processor = StreamProcessor::new(
        &hook,
        &stream_writer,
        &mut lifecycle,
        &response_doc_id,
        ProviderInputProfile::OpenAiChatCompletions,
    );

    processor
        .process_item::<()>(Ok(LoopStreamItem::ProviderAttemptStarted {
            turn: 0,
            attempt: 0,
            capture_scope: "inference.1".parse().unwrap(),
        }))
        .await
        .unwrap();

    processor.assistant_turn.push_reasoning(
        Reasoning::new("Need to inspect directory structure first")
            .with_id("rs_partial".to_string()),
    );
    processor
        .assistant_turn
        .push_text("I started by checking the repo layout.");

    assert!(processor.has_observable_activity());
    assert!(processor
        .persist_partial_turn("persist errored assistant turn")
        .await
        .unwrap());

    let history = crate::session::load_history(&node, &session_id, "did:test:test", None)
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
    let (header, native) = crate::session::load_canonical_message_from_node(
        node.as_ref(),
        &message_doc_id,
        "did:test:test",
        None,
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
    use crate::session::canonical_rows::{decode_output_segment_row, AGENT_OUTPUT_SEGMENT_FIELDS};
    use gents_protocol::output::reconstruction::{reconstruct_stream, ObservedSegment};
    use gents_protocol::output::{OutputOutcome, PayloadRef, SourceClose};
    let result = node.execute(&format!(
        r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#,
        crate::graphql::escape_graphql_string(&response_doc_id)
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
    assert_eq!(stream_bytes.len(), 2);
    let facts = records
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    let streams = (0..2)
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
    assert_eq!(streams.len(), 2);
    assert!(streams
        .iter()
        .any(|stream| stream.text == "I started by checking the repo layout."));
    assert!(streams
        .iter()
        .any(|stream| stream.text == "Need to inspect directory structure first"));

    node.shutdown().await;
    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn failed_spawn_preplan_closes_owned_prefix_before_propagating_error() {
    use crate::session::canonical_rows::{decode_output_segment_row, AGENT_OUTPUT_SEGMENT_FIELDS};
    use gents_protocol::output::{OutputOutcome, OutputSource, SourceClose, TerminalOutput};

    let data_path =
        std::env::temp_dir().join(format!("processor-spawn-preplan-{}", uuid::Uuid::new_v4()));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_runtime_schemas(&node).await.unwrap();
    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:test:test",
        FailurePolicy::default(),
    );
    assert!(matches!(
        hook.on_completion_call(&user_text_message("spawn child"), &[])
            .await,
        HookAction::Continue
    ));
    let session_id = hook.session_id().await.unwrap();
    let request_id = uuid::Uuid::new_v4().to_string();
    let request_doc_id = create_pending_request(&node, &request_id, &session_id).await;
    let request = fixture_agent_request(request_doc_id, &request_id, &session_id, "spawn child");
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        "general",
        "did:test:test",
        request,
        30,
        ExecutionOrigin::Interactive,
        "test-backend",
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    let writer = DefraStreamWriter::new(node.clone(), "did:test:test", Duration::from_secs(60));
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    let doc_id = lifecycle.request().doc_id.clone();

    // Only the hook's parent lookup is broken. The real request and its
    // execution lease remain intact so the canonical Partial close is allowed.
    hook.set_active_request_binding(
        Some("missing-parent".to_string()),
        Some("missing-parent-doc".to_string()),
        None,
    )
    .await;
    let mut processor = StreamProcessor::new(&hook, &writer, &mut lifecycle, &doc_id);
    processor
        .process_item::<()>(Ok(LoopStreamItem::ProviderAttemptStarted {
            turn: 0,
            attempt: 0,
            capture_scope: "inference.1".parse().unwrap(),
        }))
        .await
        .unwrap();
    processor
        .process_item(text_item("retained prefix"))
        .await
        .unwrap();
    processor
        .process_item(tool_call_item(
            crate::toolset::SPAWN_SUBAGENT_TOOL_NAME,
            r#"{"name":"child","prompt":"work"}"#,
            "spawn-call",
        ))
        .await
        .unwrap();
    processor.flush_pending().await.unwrap();
    let before_failure = node
        .execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#,
            crate::graphql::escape_graphql_string(&doc_id)
        ))
        .await;
    assert!(!before_failure.has_errors(), "{:?}", before_failure.errors);
    let committed = before_failure.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(decode_output_segment_row)
        .collect::<anyhow::Result<Vec<_>>>()
        .unwrap();
    assert!(
        committed.iter().any(|row| row.segment.ordinal.is_some()),
        "the provider prefix must be durable before preplanning fails"
    );
    assert!(
        committed.iter().all(|row| row.segment.close.is_none()),
        "the committed prefix must still be open before preplanning fails"
    );
    let message = processor.assistant_turn.clone().take_message().unwrap();
    let error = match processor
        .process_item::<()>(Ok(LoopStreamItem::ProviderTurnReady {
            turn: 0,
            attempt: 0,
            message,
        }))
        .await
    {
        Ok(_) => panic!("failed parent lookup must reject provider turn"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("preplan spawn admission for parent request missing-parent"),
        "unexpected preplan error: {error:#}"
    );
    drop(processor);

    let response = node
        .execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#,
            crate::graphql::escape_graphql_string(&doc_id)
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let rows = response.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(decode_output_segment_row)
        .collect::<anyhow::Result<Vec<_>>>()
        .unwrap();
    let closures = rows
        .iter()
        .filter(|row| row.segment.close.is_some())
        .collect::<Vec<_>>();
    assert_eq!(closures.len(), 1, "rejected turn must close exactly once");
    assert!(matches!(
        closures[0].segment.close.as_ref(),
        Some(SourceClose::Closed {
            outcome: OutputOutcome::Partial,
            ..
        })
    ));
    let TerminalOutput::Message { message_doc_id } = writer.terminal_output(&doc_id).await else {
        panic!("retained prefix must be selectable after rejected spawn intent")
    };
    let (header, native) = crate::session::load_canonical_message_from_node(
        node.as_ref(),
        &message_doc_id,
        "did:test:test",
        None,
    )
    .await
    .unwrap();
    assert_eq!(header.outcome, OutputOutcome::Partial);
    assert_eq!(native, Message::assistant("retained prefix"));
    let tool_calls = node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(&doc_id)
        ))
        .await;
    assert!(!tool_calls.has_errors(), "{:?}", tool_calls.errors);
    assert!(
        tool_calls.data.as_ref().unwrap()["AgentToolCall"]
            .as_array()
            .unwrap()
            .is_empty(),
        "failed preplanning must not publish a pending tool call"
    );

    // A second rejected turn exercises the same path after the fixture loses
    // its lease. Its committed prefix may remain open for recovery, but the
    // old owner must not write a closure, header, or tool admission.
    let mut processor = StreamProcessor::new(&hook, &writer, &mut lifecycle, &doc_id);
    processor
        .process_item::<()>(Ok(LoopStreamItem::ProviderAttemptStarted {
            turn: 0,
            attempt: 1,
            capture_scope: "inference.1".parse().unwrap(),
        }))
        .await
        .unwrap();
    processor
        .process_item(text_item("expired prefix"))
        .await
        .unwrap();
    processor
        .process_item(tool_call_item(
            crate::toolset::SPAWN_SUBAGENT_TOOL_NAME,
            r#"{"name":"child","prompt":"work"}"#,
            "expired-spawn-call",
        ))
        .await
        .unwrap();
    processor.flush_pending().await.unwrap();
    let expired_message = processor.assistant_turn.clone().take_message().unwrap();
    let committed = node
        .execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#,
            crate::graphql::escape_graphql_string(&doc_id)
        ))
        .await;
    assert!(!committed.has_errors(), "{:?}", committed.errors);
    assert!(
        committed.data.as_ref().unwrap()["AgentOutputSegment"]
            .as_array()
            .unwrap()
            .iter()
            .map(decode_output_segment_row)
            .collect::<anyhow::Result<Vec<_>>>()
            .unwrap()
            .iter()
            .any(|row| {
                row.segment.ordinal.is_some()
                    && matches!(
                        &row.segment.source,
                        OutputSource::ProviderTurn { attempt: 1, .. }
                    )
            }),
        "the second source must have committed data before lease expiry"
    );
    let expired = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    let response = node
        .execute(&format!(
            r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ execution_lease_expires_at: "{}" }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(&doc_id),
            crate::graphql::escape_graphql_string(&expired)
        ))
        .await;
    assert!(
        !response.has_errors(),
        "expire fixture lease: {:?}",
        response.errors
    );
    let error = match processor
        .process_item::<()>(Ok(LoopStreamItem::ProviderTurnReady {
            turn: 0,
            attempt: 1,
            message: expired_message,
        }))
        .await
    {
        Ok(_) => panic!("expired owner must not close rejected turn"),
        Err(error) => error,
    };
    let error_chain = format!("{error:#}");
    assert!(
        error_chain.contains("preplan spawn admission for parent request missing-parent")
            && error_chain.contains("failed to close rejected provider turn"),
        "both preplan and fenced cleanup failures must remain visible: {error_chain}"
    );
    drop(processor);
    let response = node
        .execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#,
            crate::graphql::escape_graphql_string(&doc_id)
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let rows = response.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(decode_output_segment_row)
        .collect::<anyhow::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        rows.iter()
            .filter(|row| row.segment.close.is_some())
            .count(),
        1,
        "expired owner must not close its second source"
    );
    let headers = node
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(&doc_id)
        ))
        .await;
    assert!(!headers.has_errors(), "{:?}", headers.errors);
    assert_eq!(
        headers.data.as_ref().unwrap()["AgentMessage"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "expired owner must not publish a second header"
    );
    let tool_calls = node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(&doc_id)
        ))
        .await;
    assert!(!tool_calls.has_errors(), "{:?}", tool_calls.errors);
    assert!(
        tool_calls.data.as_ref().unwrap()["AgentToolCall"]
            .as_array()
            .unwrap()
            .is_empty(),
        "expired owner must not admit a tool"
    );
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(data_path);
}

// ---------------------------------------------------------------------------
// Helpers for the tail-reset integration test
// ---------------------------------------------------------------------------

async fn create_pending_request(
    node: &Arc<defra_node::EmbeddedNode>,
    request_id: &str,
    session_id: &str,
) -> String {
    create_pending_request_with_input(node, request_id, session_id, None).await
}

async fn create_pending_request_with_input(
    node: &Arc<defra_node::EmbeddedNode>,
    request_id: &str,
    session_id: &str,
    input: Option<&gents_protocol::request_input::RequestInput>,
) -> String {
    let request_id = crate::graphql::escape_graphql_string(request_id);
    let session_id = crate::graphql::escape_graphql_string(session_id);
    let created_at = crate::graphql::escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let input =
        gents_protocol::graphql::graphql_input_literal(&serde_json::to_value(input).unwrap())
            .unwrap();
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "did:test:test",
                behavior_id: "general",
                session_id: "{session_id}",
                subagent_depth: 0,
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                content: "test prompt",
                input: {input},
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "interactive",
                failure_reason: "",
                created_at: "{created_at}",
                retry_count: 0,
                max_retries: 3
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create_AgentRequest failed: {:?}",
        resp.errors
    );
    // DefraDB returns the doc id in the mutation response or we query for it.
    if let Some(doc_id) = resp
        .data
        .as_ref()
        .and_then(|d| d.get("create_AgentRequest"))
        .and_then(|v| v.get("_docID"))
        .and_then(|v| v.as_str())
    {
        return doc_id.to_string();
    }
    // Fallback: query by request_id.
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                _docID
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "AgentRequest lookup failed: {:?}",
        resp.errors
    );
    resp.data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .expect("request _docID")
}

#[derive(Debug)]
struct PersistedMessageShape {
    sequence: u32,
    role: String,
    content: String,
}

async fn load_message_shapes(
    node: &defra_node::EmbeddedNode,
    session_id: &str,
) -> Vec<PersistedMessageShape> {
    let session_id = crate::graphql::escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{session_id}" }},
                    agent_did: {{ _eq: "did:test:test" }}, requester_did: {{ _eq: null }} }},
                order: {{ sequence: ASC }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "AgentMessage shape query failed: {:?}",
        response.errors
    );
    let rows = response.data.as_ref().unwrap()["AgentMessage"]
        .as_array()
        .unwrap();
    let mut shapes = Vec::with_capacity(rows.len());
    for row in rows {
        let doc_id = row["_docID"].as_str().expect("physical message identity");
        let (header, message) =
            crate::session::load_canonical_message_from_node(node, doc_id, "did:test:test", None)
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

fn text_item(text: &str) -> Result<LoopStreamItem<()>, rig::agent::StreamingError> {
    Ok(LoopStreamItem::Item(
        MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(
            rig::completion::message::Text {
                text: text.to_string(),
            },
        )),
    ))
}

fn tool_call_item(
    name: &str,
    args_json: &str,
    internal_id: &str,
) -> Result<LoopStreamItem<()>, rig::agent::StreamingError> {
    tool_call_item_with_ids(name, args_json, internal_id, internal_id, None)
}

fn tool_call_item_with_ids(
    name: &str,
    args_json: &str,
    tool_id: &str,
    internal_id: &str,
    call_id: Option<&str>,
) -> Result<LoopStreamItem<()>, rig::agent::StreamingError> {
    Ok(LoopStreamItem::Item(
        MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::ToolCall {
            tool_call: rig::completion::message::ToolCall {
                id: tool_id.to_string(),
                call_id: call_id.map(ToOwned::to_owned),
                function: rig::completion::message::ToolFunction {
                    name: name.to_string(),
                    arguments: serde_json::from_str(args_json).unwrap(),
                },
                signature: None,
                additional_params: None,
            },
            internal_call_id: internal_id.to_string(),
        }),
    ))
}

fn tool_result_item(
    tool_id: &str,
    result_json: &str,
    internal_id: &str,
) -> Result<LoopStreamItem<()>, rig::agent::StreamingError> {
    tool_result_item_with_call_id(tool_id, None, result_json, internal_id)
}

fn tool_result_item_with_call_id(
    tool_id: &str,
    call_id: Option<&str>,
    result_json: &str,
    internal_id: &str,
) -> Result<LoopStreamItem<()>, rig::agent::StreamingError> {
    Ok(LoopStreamItem::Item(MultiTurnStreamItem::StreamUserItem(
        StreamedUserContent::ToolResult {
            tool_result: rig::completion::message::ToolResult {
                id: tool_id.to_string(),
                call_id: call_id.map(ToOwned::to_owned),
                content: rig::one_or_many::OneOrMany::one(
                    rig::completion::message::ToolResultContent::Text(
                        rig::completion::message::Text {
                            text: result_json.to_string(),
                        },
                    ),
                ),
            },
            internal_call_id: internal_id.to_string(),
        },
    )))
}

fn final_item(response_text: &str) -> Result<LoopStreamItem<()>, rig::agent::StreamingError> {
    Ok(LoopStreamItem::Item(
        MultiTurnStreamItem::<()>::final_response(response_text, rig::completion::Usage::new()),
    ))
}

fn turn_retracted_item(
    turn: usize,
    attempt: u32,
) -> Result<LoopStreamItem<()>, rig::agent::StreamingError> {
    Ok(LoopStreamItem::TurnRetracted {
        turn,
        attempt,
        backoff: std::time::Duration::ZERO,
    })
}

#[tokio::test]
async fn hook_persisted_tool_result_dedupes_matching_stream_result() {
    let data_path = std::env::temp_dir().join(format!(
        "agent-stream-processor-tool-dedupe-{}",
        uuid::Uuid::new_v4()
    ));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_runtime_schemas(&node).await.unwrap();

    let hook = crate::hook::DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:test:test",
        FailurePolicy::default(),
    );
    assert!(matches!(
        hook.on_completion_call(&user_text_message("discover available tools"), &[])
            .await,
        HookAction::Continue
    ));
    let session_id = hook.session_id().await.expect("session id");

    let request_id = uuid::Uuid::new_v4().to_string();
    let request_doc_id = create_pending_request(&node, &request_id, &session_id).await;
    hook.set_active_request_lineage(Some(request_id.clone()), None)
        .await
        .expect("bind persisted request lineage");
    hook.set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::seconds(60)))
        .await;
    let request = fixture_agent_request(
        request_doc_id,
        &request_id,
        &session_id,
        "discover available tools",
    );

    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        "general",
        "did:test:test",
        request,
        30,
        ExecutionOrigin::Interactive,
        "test-backend",
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);

    let stream_writer =
        DefraStreamWriter::new(node.clone(), "did:test:test", Duration::from_millis(0));
    lifecycle
        .begin_owned_execution(&stream_writer)
        .await
        .unwrap();
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
        .process_item::<()>(Ok(LoopStreamItem::ProviderTurnReady {
            turn: 0,
            attempt: 0,
            message: Message::Assistant {
                id: Some("provider-message".to_string()),
                content: vec![AssistantContent::ToolCall(ToolCall {
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
        matches!(action, crate::llm::ToolCallHookAction::Continue),
        "tool call persistence failed: {action:?}"
    );
    assert!(matches!(
        hook.on_tool_result(
            "discover_tools",
            Some(model_result_id.to_string()),
            stored_call_id,
            tool_args,
            &crate::tool_call_lifecycle::ToolOutcome::Completed(tool_result.to_string()),
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

    let history = crate::session::load_history(&node, &session_id, "did:test:test", None)
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
    let response = node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(model_result_id),
        ))
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
        &crate::config_client::ConfigAccess::Local(node.clone()),
        tool_doc_id,
        "did:test:test",
        &session_id,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        crate::tool_call_lifecycle::query::render_tool_result(&native).unwrap(),
        tool_result
    );

    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn streamed_wait_call_precedes_concurrent_notification_and_tool_result() {
    let data_path = std::env::temp_dir().join(format!(
        "agent-stream-processor-inline-tool-result-{}",
        uuid::Uuid::new_v4()
    ));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_runtime_schemas(&node).await.unwrap();

    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:test:test",
        FailurePolicy::default(),
    );
    assert!(matches!(
        hook.on_completion_call(&user_text_message("read the source"), &[])
            .await,
        HookAction::Continue
    ));
    let session_id = hook.session_id().await.expect("session id");

    let request_id = uuid::Uuid::new_v4().to_string();
    let request_doc_id = create_pending_request(&node, &request_id, &session_id).await;
    hook.set_active_request_lineage(Some(request_id.clone()), None)
        .await
        .expect("bind persisted request lineage");
    hook.set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::seconds(60)))
        .await;
    let request =
        fixture_agent_request(request_doc_id, &request_id, &session_id, "read the source");
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        "general",
        "did:test:test",
        request,
        30,
        ExecutionOrigin::Interactive,
        "test-backend",
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    let stream_writer =
        DefraStreamWriter::new(node.clone(), "did:test:test", Duration::from_millis(0));
    lifecycle
        .begin_owned_execution(&stream_writer)
        .await
        .unwrap();
    stream_writer
        .publish_authored_message(&lifecycle, "prompt", &user_text_message("read the source"))
        .await
        .unwrap();
    let response_doc_id = lifecycle.request().doc_id.clone();
    let notification_request = lifecycle.request().clone();
    let mut processor = StreamProcessor::new(
        &hook,
        &stream_writer,
        &mut lifecycle,
        &response_doc_id,
        ProviderInputProfile::OpenAiChatCompletions,
    );

    processor
        .process_item::<()>(Ok(LoopStreamItem::ProviderAttemptStarted {
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

    // Canonical claimed publication: the streamed tool-call envelope is
    // accepted through the owned completion loop's publication authority
    // (ProviderTurnReady consuming the accumulated assistant turn) before the
    // hook may dispatch it. There is no fabricated lifecycle state here.
    let accepted_message = processor.assistant_turn.clone().take_message().unwrap();
    processor
        .process_item::<()>(Ok(LoopStreamItem::ProviderTurnReady {
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
        crate::llm::ToolCallHookAction::Continue
    ));

    // Reproduce the live wait race: an independent background task appends a
    // user-role completion while inline tool execution is still active. The
    // streamed tool-call envelope must already own its durable assistant row,
    // so this append allocates the following sequence instead of colliding.
    // Seed a closed independent tool source, then let the real notification
    // transaction allocate its header/wake alongside the running foreground
    // call. The helper does not bypass the notification publication owner.
    let queue = gents_protocol::request_input::RequestQueue {
        source: gents_protocol::request_input::QueueSource::BackgroundCompletion,
        policy: gents_protocol::request_input::QueuePolicy::Coalesce,
        key: Some(format!("background_completion:{session_id}")),
        queued_after_request_id: Some(request_id.clone()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    };
    // This unit fixture has a synthetic DID, not a signing identity. Seed an
    // already-admitted pending wake so the real publisher exercises coalescing
    // rather than asking the fixture to sign a new request.
    create_pending_request_with_input(
        &node,
        "concurrent-notification-wake",
        &session_id,
        Some(&gents_protocol::request_input::RequestInput {
            queue: Some(queue.clone()),
            ..Default::default()
        }),
    )
    .await;
    crate::lifecycle::queue::persist_background_completion_with_message(
        &node,
        &notification_request,
        "<tool-notification status=\"completed\" />",
        "background-completion-notification:concurrent-tool:tool",
        "Review background completion",
        queue,
        None,
    )
    .await
    .unwrap();

    assert!(matches!(
        hook.on_tool_result(
            "read_file",
            Some("call-1".to_string()),
            "internal-1",
            r#"{"path":"/work/entry.c"}"#,
            &crate::tool_call_lifecycle::ToolOutcome::Completed("source bytes".to_string()),
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

    let history = crate::session::load_history(&node, &session_id, "did:test:test", None)
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

    let shapes = load_message_shapes(&node, &session_id).await;
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

    let _ = std::fs::remove_dir_all(&data_path);
}

/// Three calls publish in one accepted assistant turn before dispatch. Each
/// result closes independently and its later streamed observation cannot
/// duplicate the durable delivery (Lean: `Transcript.parallel_results_complete_independently`).
#[tokio::test]
async fn multiple_streamed_tool_results_share_one_accumulated_assistant_turn() {
    let data_path = std::env::temp_dir().join(format!(
        "agent-stream-processor-multi-inline-tool-result-{}",
        uuid::Uuid::new_v4()
    ));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_runtime_schemas(&node).await.unwrap();

    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:test:test",
        FailurePolicy::default(),
    );
    assert!(matches!(
        hook.on_completion_call(&user_text_message("read several files"), &[])
            .await,
        HookAction::Continue
    ));
    let session_id = hook.session_id().await.expect("session id");

    let request_id = uuid::Uuid::new_v4().to_string();
    let request_doc_id = create_pending_request(&node, &request_id, &session_id).await;
    hook.set_active_request_lineage(Some(request_id.clone()), None)
        .await
        .expect("bind persisted request lineage");
    hook.set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::seconds(60)))
        .await;
    let request = fixture_agent_request(
        request_doc_id,
        &request_id,
        &session_id,
        "read several files",
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        "general",
        "did:test:test",
        request,
        30,
        ExecutionOrigin::Interactive,
        "test-backend",
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    let stream_writer =
        DefraStreamWriter::new(node.clone(), "did:test:test", Duration::from_millis(0));
    lifecycle
        .begin_owned_execution(&stream_writer)
        .await
        .unwrap();
    let response_doc_id = lifecycle.request().doc_id.clone();
    let mut processor = StreamProcessor::new(
        &hook,
        &stream_writer,
        &mut lifecycle,
        &response_doc_id,
        ProviderInputProfile::OpenAiChatCompletions,
    );

    processor
        .process_item::<()>(Ok(LoopStreamItem::AuthoredInputReady {
            context: None,
            prompt: user_text_message("read several files"),
        }))
        .await
        .unwrap();
    processor
        .process_item::<()>(Ok(LoopStreamItem::ProviderAttemptStarted {
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
        .process_item::<()>(Ok(LoopStreamItem::ProviderTurnReady {
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
            crate::llm::ToolCallHookAction::Continue
        ));
        assert!(matches!(
            hook.on_tool_result(
                "read_file",
                Some(call_id),
                &internal_id,
                &args,
                &crate::tool_call_lifecycle::ToolOutcome::Completed(format!(
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

    let history = crate::session::load_history(&node, &session_id, "did:test:test", None)
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

    node.shutdown().await;
    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn post_tool_resumption_keeps_each_provider_turn_separate() {
    let data_path = std::env::temp_dir().join(format!(
        "agent-stream-processor-tool-reset-{}",
        uuid::Uuid::new_v4()
    ));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_runtime_schemas(&node).await.unwrap();

    // Set up session hook + establish session by persisting user message.
    let hook = crate::hook::DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:test:test",
        FailurePolicy::default(),
    );
    assert!(matches!(
        hook.on_completion_call(&user_text_message("test prompt"), &[])
            .await,
        HookAction::Continue
    ));
    let session_id = hook.session_id().await.expect("session id");

    // Create a pending request in the DB so the lifecycle can be claimed.
    let request_id = uuid::Uuid::new_v4().to_string();
    let request_doc_id = create_pending_request(&node, &request_id, &session_id).await;

    let request = fixture_agent_request(
        request_doc_id.clone(),
        &request_id,
        &session_id,
        "test prompt",
    );

    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        "general",
        "did:test:test",
        request,
        30,
        ExecutionOrigin::Interactive,
        "test-backend",
    );

    // Claim → Streaming so advance() calls will work.
    let outcome = lifecycle.claim().await.unwrap();
    assert_eq!(outcome, ClaimOutcome::Claimed, "expected Claimed outcome");

    // Use 0 ms batch interval so write_tokens flushes immediately to DB.
    let stream_writer =
        DefraStreamWriter::new(node.clone(), "did:test:test", Duration::from_millis(0));
    lifecycle
        .begin_owned_execution(&stream_writer)
        .await
        .unwrap();
    let response_doc_id = lifecycle.request().doc_id.clone();

    let mut processor = StreamProcessor::new(
        &hook,
        &stream_writer,
        &mut lifecycle,
        &response_doc_id,
        ProviderInputProfile::OpenAiChatCompletions,
    );

    hook.set_active_request_lineage(Some(request_id.clone()), None)
        .await
        .unwrap();
    hook.set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::seconds(60)))
        .await;
    processor
        .process_item::<()>(Ok(LoopStreamItem::ProviderAttemptStarted {
            turn: 0,
            attempt: 0,
            capture_scope: "inference.1".parse().unwrap(),
        }))
        .await
        .unwrap();
    // Publication precedes dispatch; the result closes its own tool source.
    processor.process_item(text_item("hello ")).await.unwrap();
    processor.process_item(text_item("world")).await.unwrap();
    let args = r#"{"tool":"discover_tools"}"#;
    processor
        .process_item(tool_call_item("discover_tools", args, "call-1"))
        .await
        .unwrap();
    let accepted_message = processor.assistant_turn.clone().take_message().unwrap();
    processor
        .process_item::<()>(Ok(LoopStreamItem::ProviderTurnReady {
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
        matches!(action, crate::llm::ToolCallHookAction::Continue),
        "{action:?}"
    );
    assert!(matches!(
        hook.on_tool_result(
            "discover_tools",
            None,
            "call-1",
            args,
            &crate::tool_call_lifecycle::ToolOutcome::Completed(r#"{"hit":1}"#.into())
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

    // Feed: Text("done") after the tool boundary.
    processor
        .process_item::<()>(Ok(LoopStreamItem::ProviderAttemptStarted {
            turn: 1,
            attempt: 0,
            capture_scope: "inference.1".parse().unwrap(),
        }))
        .await
        .unwrap();
    processor.process_item(text_item("done")).await.unwrap();
    let final_message = Message::Assistant {
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
        .process_item::<()>(Ok(LoopStreamItem::ProviderTurnReady {
            turn: 1,
            attempt: 0,
            message: final_message.clone(),
        }))
        .await
        .unwrap();

    // Feed: FinalResponse.
    processor.process_item(final_item("done")).await.unwrap();

    let history = crate::session::load_history(&node, &session_id, "did:test:test", None)
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

    node.shutdown().await;
    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn turn_retraction_retains_old_bytes_but_publishes_only_the_retry() {
    let data_path = std::env::temp_dir().join(format!(
        "agent-stream-processor-turn-retract-{}",
        uuid::Uuid::new_v4()
    ));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_runtime_schemas(&node).await.unwrap();

    let hook = crate::hook::DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:test:test",
        FailurePolicy::default(),
    );
    assert!(matches!(
        hook.on_completion_call(&user_text_message("test prompt"), &[])
            .await,
        HookAction::Continue
    ));
    let session_id = hook.session_id().await.expect("session id");

    let request_id = uuid::Uuid::new_v4().to_string();
    let request_doc_id = create_pending_request(&node, &request_id, &session_id).await;
    let request = fixture_agent_request(request_doc_id, &request_id, &session_id, "test prompt");
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        "general",
        "did:test:test",
        request,
        30,
        ExecutionOrigin::Interactive,
        "test-backend",
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);

    let stream_writer =
        DefraStreamWriter::new(node.clone(), "did:test:test", Duration::from_millis(0));
    lifecycle
        .begin_owned_execution(&stream_writer)
        .await
        .unwrap();
    let response_doc_id = lifecycle.request().doc_id.clone();
    let mut processor = StreamProcessor::new(
        &hook,
        &stream_writer,
        &mut lifecycle,
        &response_doc_id,
        ProviderInputProfile::OpenAiChatCompletions,
    );

    processor
        .process_item::<()>(Ok(LoopStreamItem::ProviderAttemptStarted {
            turn: 0,
            attempt: 0,
            capture_scope: "inference.1".parse().unwrap(),
        }))
        .await
        .unwrap();
    processor.process_item(text_item("Hel")).await.unwrap();

    processor
        .process_item(turn_retracted_item(0, 0))
        .await
        .unwrap();
    assert_eq!(processor.streamed_text, "");

    processor
        .process_item::<()>(Ok(LoopStreamItem::ProviderAttemptStarted {
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
        .process_item::<()>(Ok(LoopStreamItem::ProviderTurnReady {
            turn: 0,
            attempt: 1,
            message: Message::Assistant {
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

    let history = crate::session::load_history(&node, &session_id, "did:test:test", None)
        .await
        .unwrap();
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

    use crate::session::canonical_rows::{decode_output_segment_row, AGENT_OUTPUT_SEGMENT_FIELDS};
    use gents_protocol::output::live::reconstruct_dense_prefix;
    use gents_protocol::output::reconstruction::ObservedSegment;
    use gents_protocol::output::{OutputSource, SourceClose};
    let result = node.execute(&format!(
        r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#,
        crate::graphql::escape_graphql_string(&response_doc_id)
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
    assert_eq!(prefix.streams.len(), 1);
    assert_eq!(prefix.streams[0].text, "Hel");

    node.shutdown().await;
    let _ = std::fs::remove_dir_all(&data_path);
}

/// #589 durable-history fence: a streamed tool call whose `arguments` the wire
/// parser left as a raw corrupt string (the production poison shape) must
/// persist OBJECT-shaped into `AgentMessage` — the salvageable payload as its
/// intended object, never as a `Value::String` that would jam every subsequent
/// render of the session (#590).
#[tokio::test]
async fn corrupt_tool_call_arguments_persist_object_shaped() {
    let data_path = std::env::temp_dir().join(format!(
        "agent-stream-processor-corrupt-args-{}",
        uuid::Uuid::new_v4()
    ));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_runtime_schemas(&node).await.unwrap();

    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:test:test",
        FailurePolicy::default(),
    );
    assert!(matches!(
        hook.on_completion_call(&user_text_message("describe list_hosts"), &[])
            .await,
        HookAction::Continue
    ));
    let session_id = hook.session_id().await.expect("session id");

    let request_id = uuid::Uuid::new_v4().to_string();
    let request_doc_id = create_pending_request(&node, &request_id, &session_id).await;
    hook.set_active_request_lineage(Some(request_id.clone()), None)
        .await
        .expect("bind persisted request lineage");
    hook.set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::seconds(60)))
        .await;
    let request = fixture_agent_request(
        request_doc_id,
        &request_id,
        &session_id,
        "describe list_hosts",
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        "general",
        "did:test:test",
        request,
        30,
        ExecutionOrigin::Interactive,
        "test-backend",
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    let stream_writer =
        DefraStreamWriter::new(node.clone(), "did:test:test", Duration::from_millis(0));
    lifecycle
        .begin_owned_execution(&stream_writer)
        .await
        .unwrap();
    let response_doc_id = lifecycle.request().doc_id.clone();
    let mut processor = StreamProcessor::new(
        &hook,
        &stream_writer,
        &mut lifecycle,
        &response_doc_id,
        ProviderInputProfile::OpenAiChatCompletions,
    );

    processor
        .process_item::<()>(Ok(LoopStreamItem::ProviderAttemptStarted {
            turn: 0,
            attempt: 0,
            capture_scope: "inference.1".parse().unwrap(),
        }))
        .await
        .unwrap();
    // The wire parser could not shape the corrupt bytes, so the streamed rig
    // ToolCall carries them as a raw Value::String — the exact production shape.
    let corrupt_call: Result<LoopStreamItem<()>, rig::agent::StreamingError> =
        Ok(LoopStreamItem::Item(
            MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::ToolCall {
                tool_call: rig::completion::message::ToolCall {
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
                internal_call_id: "internal-1".to_string(),
            }),
        ));
    processor.process_item(corrupt_call).await.unwrap();

    let message = processor.assistant_turn.clone().take_message().unwrap();
    processor
        .process_item::<()>(Ok(LoopStreamItem::ProviderTurnReady {
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
        crate::llm::ToolCallHookAction::Continue
    ));
    assert!(matches!(
        hook.on_tool_result(
            "describe_tool",
            Some("call-1".to_string()),
            "internal-1",
            crate::test_support::CORRUPT_TOOL_ARGS_589,
            &crate::tool_call_lifecycle::ToolOutcome::Completed("described:list_hosts".to_string()),
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

    // The durable history's assistant turn carries the SALVAGED object — the
    // intended call — not the raw corrupt string.
    let history = crate::session::load_history(&node, &session_id, "did:test:test", None)
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

    node.shutdown().await;
    let _ = std::fs::remove_dir_all(&data_path);
}
