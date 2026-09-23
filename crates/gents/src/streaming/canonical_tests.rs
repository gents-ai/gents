use super::*;
use crate::lifecycle::{ClaimOutcome, RequestLifecycle};
use std::{sync::Arc, time::Duration};

#[test]
fn delegated_input_replay_requires_exact_presence_source_and_bytes() {
    let source = gents_protocol::output::PayloadRef {
        close_doc_id: "close-1".into(),
        stream: 2,
    };
    let exact = gents_protocol::output::DelegatedToolInput {
        source: source.clone(),
        arguments: r#"{"name":"child"}"#.into(),
        parent_subagent_depth: 1,
    };
    let wrong_source = gents_protocol::output::DelegatedToolInput {
        source: gents_protocol::output::PayloadRef {
            close_doc_id: "close-other".into(),
            stream: 2,
        },
        arguments: exact.arguments.clone(),
        parent_subagent_depth: exact.parent_subagent_depth,
    };

    assert!(super::canonical::delegated_input_matches(
        false, None, &source, None, 0,
    ));
    assert!(!super::canonical::delegated_input_matches(
        false,
        Some(&exact),
        &source,
        Some(&exact.arguments),
        exact.parent_subagent_depth,
    ));
    assert!(super::canonical::delegated_input_matches(
        true,
        Some(&exact),
        &source,
        Some(&exact.arguments),
        exact.parent_subagent_depth,
    ));
    assert!(!super::canonical::delegated_input_matches(
        true,
        Some(&wrong_source),
        &source,
        Some(&exact.arguments),
        exact.parent_subagent_depth,
    ));
    assert!(!super::canonical::delegated_input_matches(
        true,
        Some(&exact),
        &source,
        Some("different"),
        exact.parent_subagent_depth,
    ));
    assert!(!super::canonical::delegated_input_matches(
        true,
        None,
        &source,
        Some(&exact.arguments),
        exact.parent_subagent_depth,
    ));
    assert!(!super::canonical::delegated_input_matches(
        true,
        Some(&exact),
        &source,
        Some(&exact.arguments),
        exact.parent_subagent_depth + 1,
    ));
}

#[test]
fn live_reasoning_preview_preserves_exact_small_text_and_bounded_unicode_tail() {
    let mut preview = String::new();
    super::append_live_reasoning_preview(&mut preview, "考える");
    assert_eq!(preview, "考える");
    let oversized = "é".repeat(super::MAX_LIVE_REASONING_BYTES);
    super::append_live_reasoning_preview(&mut preview, &oversized);
    assert!(preview.len() <= super::MAX_LIVE_REASONING_BYTES);
    assert!(preview.is_char_boundary(0));
    assert!(oversized.ends_with(&preview));
}

async fn claimed(node: &Arc<EmbeddedNode>, request_id: &str, session_id: &str) -> RequestLifecycle {
    let now = chrono::Utc::now().to_rfc3339();
    let request_id = crate::graphql::escape_graphql_string(request_id);
    let session_id = crate::graphql::escape_graphql_string(session_id);
    let now = crate::graphql::escape_graphql_string(&now);
    let response = node.execute(&format!(r#"mutation {{ create_AgentRequest(input: {{ request_id: "{request_id}", agent_did: "did:test:test", behavior_id: "general", session_id: "{session_id}", retry_parent_request: "", retry_root_request: "{request_id}", superseded_by_request: "", content: "hello", lifecycle_state: "pending", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "{now}", retry_count: 0, max_retries: 3, subagent_depth: 0 }}) {{ _docID }} }}"#)).await;
    assert!(!response.has_errors(), "{:#?}", response.errors);
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ {} }} }}"#,
            crate::watcher::AGENT_REQUEST_FIELDS
        ))
        .await;
    let row: gents_protocol::row::AgentRequestRow =
        crate::graphql::first_row(&response, "AgentRequest")
            .unwrap()
            .unwrap();
    let mut lifecycle = RequestLifecycle::new_with_agent_did(
        node.clone(),
        "general",
        "did:test:test",
        row.try_into().unwrap(),
        60,
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle
}

#[tokio::test]
async fn authored_publication_replays_and_reconstructs_exact_message() {
    let path = std::env::temp_dir().join(format!("canonical-authored-{}", uuid::Uuid::new_v4()));
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(&path)
            .build()
            .await
            .unwrap(),
    );
    crate::schema::ensure_runtime_schemas(&node).await.unwrap();
    let mut lifecycle = claimed(&node, "request-authored", "session-authored").await;
    let writer = DefraStreamWriter::new(node.clone(), "did:test:test", Duration::from_millis(1));
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    let message = gents_protocol::message::Message::user("continue ✓");
    let first = writer
        .publish_authored_message(&lifecycle, "reminder:0", &message)
        .await
        .unwrap();
    let replay = writer
        .publish_authored_message(&lifecycle, "reminder:0", &message)
        .await
        .unwrap();
    assert_eq!(first, replay);
    let (_, reconstructed) = crate::session::load_canonical_message(
        &crate::config_client::ConfigAccess::Local(node.clone()),
        &first,
        "did:test:test",
        None,
    )
    .await
    .unwrap();
    assert_eq!(reconstructed, message);
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn selected_history_missing_later_payload_never_returns_a_shortened_prefix() {
    use crate::session::canonical_rows::{
        transcript_message_create_variables, CREATE_AGENT_MESSAGE_MUTATION,
    };
    use gents_protocol::output::{
        MessageBlock, MessagePublication, MessageRole, OutputOutcome, PayloadPresentation,
        PayloadRef, PresentedPayload, TranscriptMessage,
    };
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    crate::schema::ensure_runtime_schemas(&node).await.unwrap();
    let mut lifecycle = claimed(&node, "incomplete-request", "incomplete-session").await;
    let writer = DefraStreamWriter::new(node.clone(), "did:test:test", Duration::from_millis(1));
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    writer
        .publish_authored_message(
            &lifecycle,
            "prompt",
            &gents_protocol::message::Message::user("first complete input"),
        )
        .await
        .unwrap();
    assert_eq!(
        crate::session::load_history(&node, "incomplete-session", "did:test:test", None)
            .await
            .unwrap()
            .len(),
        1
    );
    // A replica may observe a later header before its selected close. Neither
    // provider input nor compaction may accept just the earlier valid prefix.
    let header = TranscriptMessage {
        message_key: "later-input".into(),
        session_id: "incomplete-session".into(),
        agent_did: "did:test:test".into(),
        requester_did: None,
        request_doc_id: Some(lifecycle.request().doc_id.clone()),
        publication: MessagePublication::RequestExecution {
            execution_generation: "later-generation".into(),
        },
        outcome: OutputOutcome::Complete,
        sequence: 100,
        role: MessageRole::User,
        native_id: None,
        blocks: vec![MessageBlock::Text {
            text: PresentedPayload {
                output: PayloadRef {
                    close_doc_id: "not-yet-replicated-close".into(),
                    stream: 0,
                },
                presentation: PayloadPresentation::Full,
            },
        }],
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let response = node
        .execute_request_with_retry(
            defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&header).unwrap()),
            defra_node::ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let error = crate::session::load_history(&node, "incomplete-session", "did:test:test", None)
        .await
        .expect_err("missing later payload must fail the complete selection");
    assert!(
        format!("{error:#}").contains("not-yet-replicated-close"),
        "{error:#}"
    );
    node.shutdown().await;
}

#[tokio::test]
async fn owned_input_handoff_publishes_context_and_prompt_once_before_provider_output() {
    use crate::agent::loop_stream::LoopStreamItem;
    use crate::agent::stream_processor::StreamProcessor;
    use gents_protocol::message::Message;
    let (node, path, mut lifecycle, writer) = fixture("owned-input").await;
    let request_doc_id = lifecycle.request().doc_id.clone();
    let hook = crate::hook::DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:test:test",
        crate::hook::FailurePolicy::default(),
    );
    assert!(
        crate::session::load_history(&node, "session-owned-input", "did:test:test", None)
            .await
            .unwrap()
            .is_empty()
    );
    let context = Message::user("request context");
    let prompt = Message::user("queued prompt ✓");
    {
        let mut processor = StreamProcessor::new(&hook, &writer, &mut lifecycle, &request_doc_id);
        for _ in 0..2 {
            processor
                .process_item::<()>(Ok(LoopStreamItem::AuthoredInputReady {
                    context: Some(context.clone()),
                    prompt: prompt.clone(),
                }))
                .await
                .unwrap();
        }
    }
    assert_eq!(
        crate::session::load_history(&node, "session-owned-input", "did:test:test", None)
            .await
            .unwrap(),
        vec![context, prompt]
    );
    let response = node
        .execute("{AgentMessage(order:{sequence: ASC}){message_key request_doc_id sequence}}")
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let rows = response.data.unwrap()["AgentMessage"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["request_doc_id"], request_doc_id);
    assert_eq!(rows[1]["request_doc_id"], request_doc_id);
    assert_eq!(
        rows[0]["message_key"],
        format!("authored:{request_doc_id}:context")
    );
    assert_eq!(
        rows[1]["message_key"],
        format!("authored:{request_doc_id}:prompt")
    );
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}

async fn fixture(
    name: &str,
) -> (
    Arc<EmbeddedNode>,
    std::path::PathBuf,
    RequestLifecycle,
    DefraStreamWriter,
) {
    let path = std::env::temp_dir().join(format!("canonical-{name}-{}", uuid::Uuid::new_v4()));
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(&path)
            .build()
            .await
            .unwrap(),
    );
    crate::schema::ensure_runtime_schemas(&node).await.unwrap();
    let mut lifecycle = claimed(
        &node,
        &format!("request-{name}"),
        &format!("session-{name}"),
    )
    .await;
    let writer = DefraStreamWriter::new(node.clone(), "did:test:test", Duration::from_millis(1));
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    (node, path, lifecycle, writer)
}

#[tokio::test]
async fn pending_batch_deadline_is_fixed_and_idle_has_no_timer() {
    let (_node, _path, lifecycle, mut writer) = fixture("batch-deadline").await;
    writer.batch_interval = Duration::from_secs(30);
    let request = lifecycle.request().doc_id.as_str();
    assert_eq!(writer.next_flush_deadline(request).await, None);
    assert!(writer.write_tokens(request, "first").await.unwrap());
    assert_eq!(writer.next_flush_deadline(request).await, None);
    assert!(!writer.write_tokens(request, " buffered").await.unwrap());
    let due = writer.next_flush_deadline(request).await.unwrap();
    assert!(!writer.write_tokens(request, " more").await.unwrap());
    assert_eq!(writer.next_flush_deadline(request).await, Some(due));
    assert!(writer.flush_pending(request).await.unwrap());
    assert_eq!(writer.next_flush_deadline(request).await, None);
}

fn provider_segment_for_message(
    lifecycle: &RequestLifecycle,
    message: &gents_protocol::message::Message,
    created_at: String,
) -> (
    gents_protocol::output::OutputSegment,
    Arc<super::native_encoding::EncodedNativeMessage>,
) {
    use gents_protocol::output::{OutputSegment, OutputSource, OutputWriter, SegmentRun};
    let encoded = Arc::new(super::native_encoding::encode_native_message(message).unwrap());
    let mut payload = String::new();
    let runs = encoded
        .streams
        .iter()
        .enumerate()
        .map(|(stream, encoded)| {
            payload.push_str(&encoded.payload);
            SegmentRun {
                stream: u32::try_from(stream).unwrap(),
                bytes: u32::try_from(encoded.payload.len()).unwrap(),
                declaration: Some(encoded.declaration.clone()),
            }
        })
        .collect::<Vec<_>>();
    let request = lifecycle.request();
    (
        OutputSegment {
            agent_did: request.agent_did.clone(),
            requester_did: request.requester_did.clone(),
            session_id: request.session_id.clone(),
            request_doc_id: request.doc_id.clone(),
            source: OutputSource::ProviderTurn {
                scope: "inference.1".parse().unwrap(),
                turn_index: 0,
                attempt: 0,
            },
            writer: OutputWriter::RequestExecution {
                execution_generation: lifecycle.execution_generation().unwrap().to_owned(),
            },
            ordinal: (!runs.is_empty()).then_some(0),
            runs,
            payload,
            close: None,
            created_at,
        },
        encoded,
    )
}

#[tokio::test]
async fn first_publication_rejects_native_message_that_differs_from_reconstruction() {
    let (node, path, lifecycle, _writer) = fixture("first-native-mismatch").await;
    let actual = gents_protocol::message::Message::assistant("persisted bytes");
    let (segment, encoded) =
        provider_segment_for_message(&lifecycle, &actual, chrono::Utc::now().to_rfc3339());
    let expected = gents_protocol::message::Message::assistant("different native message");
    let error = super::canonical::publish_provider_turn(
        &node,
        lifecycle.execution_generation().unwrap(),
        super::canonical::ProviderPublicationPlan {
            final_flush: Some(segment),
            message_key: "provider:first-native-mismatch".into(),
            encoded,
            expected: Arc::new(expected),
            tool_deadline_at: lifecycle.claimed_deadline_at().unwrap().to_rfc3339(),
            spawn_admissions: Vec::new(),
        },
    )
    .await
    .expect_err("first publication must compare reconstructed and native messages");
    assert!(format!("{error:#}").contains("first publication native message changed"));
    let rows = node
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID }} AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(&lifecycle.request().doc_id),
            crate::graphql::escape_graphql_string(&lifecycle.request().doc_id),
        ))
        .await;
    assert!(rows.data.as_ref().unwrap()["AgentMessage"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(rows.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .is_empty());
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}

async fn publish_tool_turn_for_rollback_test(
    node: &EmbeddedNode,
    lifecycle: &RequestLifecycle,
    message: &gents_protocol::message::Message,
) -> anyhow::Result<super::canonical::PublishedProviderTurn> {
    let (segment, encoded) =
        provider_segment_for_message(lifecycle, message, chrono::Utc::now().to_rfc3339());
    let plan = super::canonical::ProviderPublicationPlan {
        final_flush: Some(segment),
        message_key: "provider:mutation-rollback".into(),
        encoded,
        expected: Arc::new(message.clone()),
        tool_deadline_at: lifecycle.claimed_deadline_at().unwrap().to_rfc3339(),
        spawn_admissions: Vec::new(),
    };
    // Exercise the canonical publication owner, not a test-side write script.
    super::canonical::publish_provider_turn(node, lifecycle.execution_generation().unwrap(), plan)
        .await
}

#[tokio::test]
async fn every_successful_publication_mutation_rolls_back_if_the_transaction_fails() {
    use gents_protocol::message::{AssistantContent, Message, ToolCall, ToolFunction};

    let message = Message::Assistant {
        id: Some("provider-message".into()),
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: "tool-1".into(),
            call_id: Some("provider-call".into()),
            function: ToolFunction::new("read".into(), serde_json::json!({"path":"note.txt"})),
            signature: None,
            additional_params: None,
        })],
    };
    let (baseline_node, baseline_path, baseline_lifecycle, _writer) =
        fixture("mutation-rollback-baseline").await;
    let (baseline, writes) =
        crate::config_client::ConfigApplyTxn::with_successful_mutation_failure_at(
            None,
            publish_tool_turn_for_rollback_test(&baseline_node, &baseline_lifecycle, &message),
        )
        .await;
    baseline.expect("baseline tool-bearing publication");
    assert!(
        writes >= 3,
        "tool-bearing publication must have multiple writes"
    );
    baseline_node.shutdown().await;
    let _ = std::fs::remove_dir_all(baseline_path);

    let (node, path, lifecycle, _writer) = fixture("mutation-rollback").await;
    let request_doc_id = crate::graphql::escape_graphql_string(&lifecycle.request().doc_id);
    let rows = || async {
        let response = node.execute(&format!(
            r#"{{
                AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ _docID }}
                AgentToolCall(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ _docID }}
                AgentMessage(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ _docID }}
                AgentRequest(filter: {{ _docID: {{ _eq: "{request_doc_id}" }} }}, limit: 1) {{
                    {}
                }}
            }}"#,
            crate::watcher::AGENT_REQUEST_FIELDS,
        )).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        response.data.unwrap()
    };
    let before = rows().await;
    for index in 1..=writes {
        let (result, observed) =
            crate::config_client::ConfigApplyTxn::with_successful_mutation_failure_at(
                Some(index),
                publish_tool_turn_for_rollback_test(&node, &lifecycle, &message),
            )
            .await;
        let error = result.expect_err("injected publication mutation must roll back");
        assert!(
            format!("{error:#}").contains(&format!(
                "injected failure after successful transaction mutation {index}"
            )),
            "unexpected error at write {index}: {error:#}"
        );
        assert_eq!(observed, index, "injection must fire exactly once");
        assert_eq!(
            rows().await,
            before,
            "write {index} leaked a publication fact"
        );
    }
    let (published, actual_writes) =
        crate::config_client::ConfigApplyTxn::with_successful_mutation_failure_at(
            None,
            publish_tool_turn_for_rollback_test(&node, &lifecycle, &message),
        )
        .await;
    published.expect("publication must succeed after every injected rollback");
    assert_eq!(
        actual_writes, writes,
        "every mutation position must have been faulted"
    );
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn recovery_mutations_are_atomic_and_caller_replay_has_no_transactional_mutations() {
    use crate::config_client::ConfigApplyTxn;
    use crate::lifecycle::{recover_expired_generation_with_facts, RecoveryResult};

    // Discover the mutation count from the owner, then fault every position in
    // a fresh recovery of the same shape. No test-side recovery write script.
    let mut mutation_count = 0;
    for baseline in [true, false] {
        let (node, path, lifecycle, writer) = fixture("recovery-mutation-rollback").await;
        let request = lifecycle.request();
        crate::config_client::ConfigAccess::transact_local(
            &node,
            None,
            "test.recovery_session_observation",
            |txn| {
                Box::pin(async move {
                    let now = chrono::Utc::now().to_rfc3339();
                    crate::session::ensure_session_in_txn(
                        &txn,
                        &request.session_id,
                        &request.agent_did,
                        &request.behavior_id,
                        request.requester_did.as_deref(),
                        None,
                        None,
                        &now,
                    )
                    .await?;
                    let session = crate::session::load_agent_session_row_in_txn(
                        &txn,
                        &request.agent_did,
                        &request.session_id,
                        request.requester_did.as_deref(),
                    )
                    .await?
                    .expect("recovery session");
                    let facts = crate::session::load_scoped_request_facts_in_txn(
                        &txn,
                        &session.session,
                        false,
                    )
                    .await?;
                    let fact = facts
                        .iter()
                        .find(|fact| fact.observed.request_doc_id == request.doc_id)
                        .expect("recovery request fact");
                    assert!(
                        crate::session::advance_session_request_observation_in_txn(
                            &txn,
                            fact,
                            &request.content,
                            &now,
                        )
                        .await?
                    );
                    Ok(())
                })
            },
        )
        .await
        .unwrap();
        let doc_id = &lifecycle.request().doc_id;
        writer
            .start_provider_attempt(doc_id, 0, 0, "inference.1".parse().unwrap())
            .await;
        writer
            .flush_native_partial(
                &lifecycle,
                &gents_protocol::message::Message::assistant("retained recovery prefix"),
            )
            .await
            .unwrap();
        let escaped = crate::graphql::escape_graphql_string(doc_id);
        let expiry = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
        let expire = node.execute(&format!(
            r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, input: {{ execution_lease_expires_at: "{}" }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(&expiry),
        )).await;
        assert!(!expire.has_errors(), "{:?}", expire.errors);
        let snapshot = || async {
            let response = node.execute(&format!(
                r#"{{
                    AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{escaped}" }} }}) {{ _docID }}
                    AgentMessage(filter: {{ request_doc_id: {{ _eq: "{escaped}" }} }}) {{ _docID }}
                    AgentToolCall(filter: {{ request_doc_id: {{ _eq: "{escaped}" }} }}) {{ _docID }}
                    AgentSession {{ _docID observation }}
                    AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}) {{ {} }}
                }}"#,
                crate::watcher::AGENT_REQUEST_FIELDS,
            )).await;
            assert!(!response.has_errors(), "{:?}", response.errors);
            response
        };
        let before = snapshot().await;
        let observed: gents_protocol::row::AgentRequestRow =
            crate::graphql::first_row(&before, "AgentRequest")
                .unwrap()
                .unwrap();
        let generation = observed.execution_generation.as_deref().unwrap();
        let now = chrono::Utc::now();
        let recover = || {
            recover_expired_generation_with_facts(
                &node,
                &observed,
                generation,
                &expiry,
                "recovery-atomicity-generation".into(),
                now,
                None,
                None,
            )
        };
        if baseline {
            let (result, count) =
                ConfigApplyTxn::with_successful_mutation_failure_at(None, recover()).await;
            assert_eq!(result.unwrap(), RecoveryResult::Won { published: 1 });
            assert!(
                count >= 4,
                "recovery must close, publish, swap generation and refresh the session"
            );
            mutation_count = count;
        } else {
            for index in 1..=mutation_count {
                let (result, count) =
                    ConfigApplyTxn::with_successful_mutation_failure_at(Some(index), recover())
                        .await;
                let error = result.expect_err("recovery must propagate injected failure");
                assert!(
                    format!("{error:#}").contains(&format!(
                        "injected failure after successful transaction mutation {index}"
                    )),
                    "unexpected recovery error: {error:#}"
                );
                assert_eq!(count, index);
                assert_eq!(
                    snapshot().await.data,
                    before.data,
                    "recovery write {index} leaked"
                );
            }
            let (recovered, actual_writes) =
                ConfigApplyTxn::with_successful_mutation_failure_at(None, recover()).await;
            assert_eq!(recovered.unwrap(), RecoveryResult::Won { published: 1 });
            assert_eq!(
                actual_writes, mutation_count,
                "every recovery mutation must have been faulted"
            );
            let committed = snapshot().await.data;
            assert_ne!(
                committed.as_ref().unwrap()["AgentSession"],
                before.data.as_ref().unwrap()["AgentSession"],
                "successful recovery must update the session observation"
            );
            // Discard the first acknowledgement and resend the exact recovery
            // identity at the caller boundary. This does not inject an
            // acknowledgement loss inside the transaction owner. The hook
            // counts transactional mutations, not arbitrary auto-commit writes.
            let (replay, writes) =
                ConfigApplyTxn::with_successful_mutation_failure_at(None, recover()).await;
            assert_eq!(replay.unwrap(), RecoveryResult::Won { published: 1 });
            assert_eq!(writes, 0);
            assert_eq!(snapshot().await.data, committed);
        }
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(path);
    }
}

#[tokio::test]
async fn provider_closure_cannot_precede_committed_source_timestamp() {
    let (node, path, lifecycle, _writer) = fixture("close-time-regression").await;
    let message = gents_protocol::message::Message::assistant("persisted bytes");
    let now = chrono::Utc::now();
    let (raw, encoded) = provider_segment_for_message(
        &lifecycle,
        &message,
        (now + chrono::Duration::seconds(1)).to_rfc3339(),
    );
    super::canonical::append_provider_segment_at(
        &node,
        lifecycle.execution_generation().unwrap(),
        &raw,
        now,
    )
    .await
    .unwrap();
    let mut closing = raw.clone();
    closing.ordinal = None;
    closing.runs.clear();
    closing.payload.clear();
    closing.created_at = now.to_rfc3339();
    let error = super::canonical::publish_provider_turn_at(
        &node,
        lifecycle.execution_generation().unwrap(),
        super::canonical::ProviderPublicationPlan {
            final_flush: Some(closing),
            message_key: "provider:close-time-regression".into(),
            encoded,
            expected: Arc::new(message),
            tool_deadline_at: lifecycle.claimed_deadline_at().unwrap().to_rfc3339(),
            spawn_admissions: Vec::new(),
        },
        now,
    )
    .await
    .expect_err("closure time must be nondecreasing against committed data");
    assert!(format!("{error:#}").contains("closure timestamp precedes"));
    let rows = node.execute(&format!(
        r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID }} AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID close }} }}"#,
        crate::graphql::escape_graphql_string(&lifecycle.request().doc_id),
        crate::graphql::escape_graphql_string(&lifecycle.request().doc_id),
    )).await;
    assert!(rows.data.as_ref().unwrap()["AgentMessage"]
        .as_array()
        .unwrap()
        .is_empty());
    let segments = rows.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap();
    assert_eq!(segments.len(), 1);
    assert!(
        segments[0]["close"].is_null(),
        "failed closure writes no close fact"
    );
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn multiple_flushes_close_and_reconstruct_exact_final_message() {
    let (node, path, lifecycle, writer) = fixture("multi-flush").await;
    let lease_query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID lifecycle_state execution_generation execution_lease_expires_at }} }}"#,
        crate::graphql::escape_graphql_string(&lifecycle.request().doc_id)
    );
    let before_flushes = node.execute(&lease_query).await;
    assert!(!before_flushes.has_errors(), "{:?}", before_flushes.errors);
    writer
        .start_provider_attempt(
            &lifecycle.request().doc_id,
            0,
            0,
            "inference.1".parse().unwrap(),
        )
        .await;
    assert!(writer
        .flush_native_partial(
            &lifecycle,
            &gents_protocol::message::Message::assistant("hello ")
        )
        .await
        .unwrap());
    assert!(writer
        .flush_native_partial(
            &lifecycle,
            &gents_protocol::message::Message::assistant("hello 世界")
        )
        .await
        .unwrap());
    let expected = gents_protocol::message::Message::assistant("hello 世界");
    let after_flushes = node.execute(&lease_query).await;
    assert!(!after_flushes.has_errors(), "{:?}", after_flushes.errors);
    assert_eq!(
        after_flushes.data, before_flushes.data,
        "immutable output replaces progress counters; only explicit renewal may extend the lease"
    );
    let published = writer
        .publish_native_turn(&lifecycle, 0, 0, &expected)
        .await
        .unwrap();
    let (_, reconstructed) = crate::session::load_canonical_message(
        &crate::config_client::ConfigAccess::Local(node.clone()),
        &published.message_doc_id,
        "did:test:test",
        None,
    )
    .await
    .unwrap();
    assert_eq!(reconstructed, expected);
    let rows = node.execute(&format!(r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ ordinal close }} }}"#, crate::graphql::escape_graphql_string(&lifecycle.request().doc_id))).await;
    let rows = rows.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap();
    assert_eq!(
        rows.len(),
        3,
        "two immutable flushes plus terminal-only closure"
    );
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn published_turn_stays_nonterminal_until_request_selection_commits() {
    use crate::lifecycle::{RequestTerminalOutcome, TerminalizeResult};
    use crate::session::{observe_request_output, CanonicalRequestOutput};
    use gents_protocol::output::TerminalOutput;

    let (node, path, mut lifecycle, writer) = fixture("published-not-terminal").await;
    writer
        .start_provider_attempt(
            &lifecycle.request().doc_id,
            0,
            0,
            "inference.1".parse().unwrap(),
        )
        .await;
    let expected = gents_protocol::message::Message::assistant("published answer");
    let query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ {} lifecycle_state terminal_output }} }}"#,
        crate::graphql::escape_graphql_string(&lifecycle.request().doc_id),
        crate::watcher::AGENT_REQUEST_FIELDS,
    );
    let access = crate::config_client::ConfigAccess::Local(node.clone());
    writer
        .flush_native_partial(&lifecycle, &expected)
        .await
        .unwrap();
    let live_request = crate::graphql::first_row(&node.execute(&query).await, "AgentRequest")
        .unwrap()
        .unwrap();
    let CanonicalRequestOutput::Live(live) = observe_request_output(&access, &live_request)
        .await
        .unwrap()
    else {
        panic!("unclosed provider output must be live");
    };
    let live_source = live
        .selected_source
        .expect("live output retains selected source");
    let published = writer
        .publish_native_turn(&lifecycle, 0, 0, &expected)
        .await
        .unwrap();
    for terminal in [false, true] {
        if terminal {
            assert_eq!(
                lifecycle
                    .terminalize_owned(
                        RequestTerminalOutcome::Completed,
                        TerminalOutput::Message {
                            message_doc_id: published.message_doc_id.clone(),
                        },
                        None,
                    )
                    .await
                    .unwrap(),
                TerminalizeResult::Won,
            );
        }
        let row: gents_protocol::row::AgentRequestRow =
            crate::graphql::first_row(&node.execute(&query).await, "AgentRequest")
                .unwrap()
                .unwrap();
        assert_eq!(row.is_terminal(), terminal);
        match (
            terminal,
            observe_request_output(&access, &row).await.unwrap(),
        ) {
            (
                false,
                CanonicalRequestOutput::Published {
                    message,
                    presentation,
                    ..
                },
            ) => {
                assert_eq!(message, expected);
                assert_eq!(presentation.selected_source.as_ref(), Some(&live_source));
            }
            (
                true,
                CanonicalRequestOutput::TerminalMessage {
                    message,
                    presentation,
                    ..
                },
            ) => {
                assert_eq!(message, expected);
                assert!(presentation.selected_source.is_none());
            }
            (_, observation) => panic!("terminal={terminal}: unexpected {observation:?}"),
        }
    }
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn final_delta_shares_its_closing_record_and_replay_adds_nothing() {
    for prior_flush in [false, true] {
        let name = if prior_flush {
            "combined-after-flush"
        } else {
            "combined-first"
        };
        let (node, path, lifecycle, writer) = fixture(name).await;
        writer
            .start_provider_attempt(
                &lifecycle.request().doc_id,
                0,
                0,
                "inference.1".parse().unwrap(),
            )
            .await;
        if prior_flush {
            writer
                .flush_native_partial(
                    &lifecycle,
                    &gents_protocol::message::Message::assistant("hello "),
                )
                .await
                .unwrap();
        }
        let expected = gents_protocol::message::Message::assistant("hello 世界");
        let published = writer
            .publish_native_turn(&lifecycle, 0, 0, &expected)
            .await
            .unwrap();
        let query = format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}, order: {{ ordinal: ASC }}) {{ _docID ordinal payload close }} }}"#,
            crate::graphql::escape_graphql_string(&lifecycle.request().doc_id),
        );
        let before = node.execute(&query).await;
        assert!(!before.has_errors(), "{:?}", before.errors);
        let rows = before.data.as_ref().unwrap()["AgentOutputSegment"]
            .as_array()
            .unwrap();
        assert_eq!(rows.len(), 1 + usize::from(prior_flush));
        let closed = rows
            .iter()
            .filter(|row| !row["close"].is_null())
            .collect::<Vec<_>>();
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0]["ordinal"].as_u64(), Some(u64::from(prior_flush)));
        assert_eq!(
            closed[0]["payload"].as_str(),
            Some(if prior_flush {
                "世界"
            } else {
                "hello 世界"
            })
        );
        let (_, reconstructed) = crate::session::load_canonical_message(
            &crate::config_client::ConfigAccess::Local(node.clone()),
            &published.message_doc_id,
            "did:test:test",
            None,
        )
        .await
        .unwrap();
        assert_eq!(reconstructed, expected);
        let replay = writer
            .publish_native_turn(&lifecycle, 0, 0, &expected)
            .await
            .unwrap();
        assert_eq!(replay.message_doc_id, published.message_doc_id);
        assert_eq!(replay.sequence, published.sequence);
        let after = node.execute(&query).await;
        assert!(!after.has_errors(), "{:?}", after.errors);
        assert_eq!(
            after.data, before.data,
            "lost-ack replay must not append a closing record"
        );
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(path);
    }
}

#[tokio::test]
async fn publication_timestamp_follows_a_flush_that_already_owns_the_tail() {
    use gents_protocol::output::{OutputSegment, OutputSource, OutputWriter};

    let (node, path, lifecycle, writer) = fixture("publish-after-held-flush").await;
    let request = lifecycle.request();
    let scope = "inference.1".parse().unwrap();
    writer
        .start_provider_attempt(&request.doc_id, 0, 0, scope)
        .await;
    let message = gents_protocol::message::Message::assistant("committed while publication waits");
    let encoded = native_encoding::encode_native_message(&message).unwrap();

    // The flusher owns the tail. Poll the publisher all the way to its lock
    // wait before committing the flush, without relying on task scheduling.
    let mut tails = writer.provider_tails.lock().await;
    let mut publication = Box::pin(writer.publish_native_turn(&lifecycle, 0, 0, &message));
    assert!(futures::poll!(publication.as_mut()).is_pending());
    // Ensure the committed flush has a later wall-clock stamp even on clocks
    // whose resolution is coarser than the synchronous poll above.
    tokio::time::sleep(Duration::from_millis(2)).await;
    let tail = tails.get_mut(&request.doc_id).unwrap();
    let delta = provider_flush_delta(&tail.streams, &encoded).unwrap();
    let segment = OutputSegment {
        agent_did: request.agent_did.clone(),
        requester_did: request.requester_did.clone(),
        session_id: request.session_id.clone(),
        request_doc_id: request.doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope,
            turn_index: 0,
            attempt: 0,
        },
        writer: OutputWriter::RequestExecution {
            execution_generation: lifecycle.execution_generation().unwrap().into(),
        },
        ordinal: Some(0),
        runs: delta.runs,
        payload: delta.payload,
        close: None,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    canonical::append_provider_segment(&node, lifecycle.execution_generation().unwrap(), &segment)
        .await
        .unwrap();
    tail.streams = encoded.streams;
    tail.next_ordinal = 1;
    drop(tails);

    let published = publication
        .await
        .expect("publication must timestamp after the earlier flusher");
    let (_, observed) = crate::session::load_canonical_message(
        &crate::config_client::ConfigAccess::Local(node.clone()),
        &published.message_doc_id,
        &request.agent_did,
        request.requester_did.as_deref(),
    )
    .await
    .unwrap();
    assert_eq!(observed, message);
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn failed_attempt_closes_the_acknowledged_prefix_as_partial() {
    let (node, path, lifecycle, writer) = fixture("partial").await;
    writer
        .start_provider_attempt(
            &lifecycle.request().doc_id,
            0,
            0,
            "inference.1".parse().unwrap(),
        )
        .await;
    writer
        .flush_native_partial(
            &lifecycle,
            &gents_protocol::message::Message::assistant("kept prefix"),
        )
        .await
        .unwrap();
    writer
        .close_provider_attempt(
            &lifecycle,
            0,
            0,
            super::canonical::ProviderAttemptClose::Partial,
        )
        .await
        .unwrap();
    let response = node
        .execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            crate::graphql::escape_graphql_string(&lifecycle.request().doc_id),
            crate::session::canonical_rows::AGENT_OUTPUT_SEGMENT_FIELDS
        ))
        .await;
    let rows = response.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(crate::session::canonical_rows::decode_output_segment_row)
        .collect::<anyhow::Result<Vec<_>>>()
        .unwrap();
    let close = rows.iter().find(|row| row.segment.close.is_some()).unwrap();
    assert!(
        matches!(close.segment.close, Some(gents_protocol::output::SourceClose::Closed { outcome: gents_protocol::output::OutputOutcome::Partial, segments: 1, ref stream_bytes }) if stream_bytes == &[11])
    );
    let observed = rows
        .iter()
        .map(
            |row| gents_protocol::output::reconstruction::ObservedSegment {
                doc_id: &row.doc_id,
                segment: &row.segment,
            },
        )
        .collect::<Vec<_>>();
    let stream = gents_protocol::output::reconstruction::reconstruct_stream(
        &observed,
        &[],
        &[],
        &gents_protocol::output::PayloadRef {
            close_doc_id: close.doc_id.clone(),
            stream: 0,
        },
    )
    .unwrap();
    assert_eq!(stream.text, "kept prefix");
    let gents_protocol::output::TerminalOutput::Message { message_doc_id } =
        writer.terminal_output(&lifecycle.request().doc_id).await
    else {
        panic!("a retained text prefix must publish a selectable partial header")
    };
    let (header, reconstructed) = crate::session::load_canonical_message(
        &crate::config_client::ConfigAccess::Local(node.clone()),
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
    assert_eq!(
        reconstructed,
        gents_protocol::message::Message::assistant("kept prefix")
    );
    let mut late = rows
        .iter()
        .find(|row| row.segment.ordinal.is_some())
        .expect("partial payload segment")
        .segment
        .clone();
    late.ordinal = Some(1);
    late.payload = "late".to_string();
    late.runs[0].bytes = 4;
    late.runs[0].declaration = None;
    let late = node
        .execute_request_with_retry(
            defra_node::QueryRequest::new(
                crate::session::canonical_rows::CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
            )
            .with_variables(
                crate::session::canonical_rows::output_segment_create_variables(&late).unwrap(),
            ),
            defra_node::ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(
        !late.has_errors(),
        "inject late replicated fact: {:?}",
        late.errors
    );
    writer
        .start_provider_attempt(
            &lifecycle.request().doc_id,
            0,
            0,
            "inference.1".parse().unwrap(),
        )
        .await;
    writer
        .close_provider_attempt(
            &lifecycle,
            0,
            0,
            super::canonical::ProviderAttemptClose::Partial,
        )
        .await
        .unwrap();
    assert_eq!(
        writer.terminal_output(&lifecycle.request().doc_id).await,
        gents_protocol::output::TerminalOutput::Message {
            message_doc_id: message_doc_id.clone()
        },
        "lost-ack replay must resolve the same partial header"
    );
    let replay = node
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID sequence }} }}"#,
            crate::graphql::escape_graphql_string(&lifecycle.request().doc_id)
        ))
        .await;
    assert!(!replay.has_errors(), "{:?}", replay.errors);
    assert_eq!(
        replay.data.as_ref().unwrap()["AgentMessage"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "partial close replay must not allocate another header or sequence"
    );
    let escaped_message = crate::graphql::escape_graphql_string(&message_doc_id);
    let deleted = node
        .execute(&format!(
            r#"mutation {{ delete_AgentMessage(filter: {{ _docID: {{ _eq: "{escaped_message}" }} }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(
        !deleted.has_errors(),
        "remove imported header: {:?}",
        deleted.errors
    );
    let escaped_request = crate::graphql::escape_graphql_string(&lifecycle.request().doc_id);
    let expired = node.execute(&format!(
        r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_request}" }} }}, input: {{ execution_lease_expires_at: "2020-01-01T00:00:00Z" }}) {{ _docID }} }}"#
    )).await;
    assert!(
        !expired.has_errors(),
        "expire imported partial owner: {:?}",
        expired.errors
    );
    writer
        .start_provider_attempt(
            &lifecycle.request().doc_id,
            0,
            0,
            "inference.1".parse().unwrap(),
        )
        .await;
    let missing = writer
        .close_provider_attempt(
            &lifecycle,
            0,
            0,
            super::canonical::ProviderAttemptClose::Partial,
        )
        .await;
    assert!(
        missing.is_err(),
        "an imported text closure without its header must not publish during replay"
    );
    let after_missing = node
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{escaped_request}" }} }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(
        after_missing.data.as_ref().unwrap()["AgentMessage"]
            .as_array()
            .unwrap()
            .is_empty(),
        "headerless replay must remain read-only"
    );
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn accepted_tool_publication_replay_returns_exact_physical_bindings() {
    use gents_protocol::message::{AssistantContent, Message, ToolCall, ToolFunction};
    let (node, path, lifecycle, writer) = fixture("tool-replay").await;
    writer
        .start_provider_attempt(
            &lifecycle.request().doc_id,
            0,
            0,
            "inference.1".parse().unwrap(),
        )
        .await;
    let message = Message::Assistant {
        id: Some("provider-message".into()),
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: "native-tool".into(),
            call_id: Some("provider-call".into()),
            function: ToolFunction::new("read".into(), serde_json::json!({"path":"文.txt"})),
            signature: Some("sig".into()),
            additional_params: None,
        })],
    };
    let first = writer
        .publish_native_turn(&lifecycle, 0, 0, &message)
        .await
        .unwrap();
    let replay = writer
        .publish_native_turn(&lifecycle, 0, 0, &message)
        .await
        .unwrap();
    assert_eq!(first.message_doc_id, replay.message_doc_id);
    assert_eq!(first.sequence, replay.sequence);
    assert_eq!(first.accepted_tools.len(), 1);
    assert_eq!(replay.accepted_tools.len(), 1);
    let a = &first.accepted_tools[0];
    let b = &replay.accepted_tools[0];
    assert_eq!(a.tool_call_doc_id, b.tool_call_doc_id);
    assert_eq!(a.accepted_header_doc_id, b.accepted_header_doc_id);
    assert_eq!(a.arguments, b.arguments);
    assert_eq!(a.id, "native-tool");
    assert_eq!(a.call_id.as_deref(), Some("provider-call"));
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}
