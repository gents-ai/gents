use super::*;
use crate::lifecycle::{ClaimOutcome, RequestLifecycle};
use gents_protocol::error_chain::ErrorChain;
use std::{sync::Arc, time::Duration};

async fn claimed(node: &Arc<EmbeddedNode>, request_id: &str, session_id: &str) -> RequestLifecycle {
    let now = chrono::Utc::now().to_rfc3339();
    let request_id = crate::graphql::escape_graphql_string(request_id);
    let session_id = crate::graphql::escape_graphql_string(session_id);
    let now = crate::graphql::escape_graphql_string(&now);
    let response = node.execute(&format!(r#"mutation {{ create_AgentRequest(input: {{ request_id: "{request_id}", purpose: "normal", node_did: "did:test:test", agent_id: "general", session_id: "{session_id}", retry_parent_request: "", retry_root_request: "{request_id}", superseded_by_request: "", content: "hello", lifecycle_state: "pending", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "{now}", retry_count: 0, max_retries: 3, request_hop: 0 }}) {{ _docID }} }}"#)).await;
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
    let mut lifecycle = RequestLifecycle::new_with_node_did(
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
        crate::test_support::node_builder()
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
        &crate::config_client::ConfigAccess::local(node.clone()),
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
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
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
    let header = TranscriptMessage {
        message_key: "later-input".into(),
        session_id: "incomplete-session".into(),
        node_did: "did:test:test".into(),
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
        .execute_request(
            defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&header).unwrap()),
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let error = crate::session::load_history(&node, "incomplete-session", "did:test:test", None)
        .await
        .expect_err("missing later payload must fail the complete selection");
    assert!(
        ErrorChain(&error)
            .to_string()
            .contains("not-yet-replicated-close"),
        "{}",
        ErrorChain(&error)
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
        let mut processor = StreamProcessor::new(
            &hook,
            &writer,
            &mut lifecycle,
            &request_doc_id,
            gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
        );
        for _ in 0..2 {
            processor
                .process_item(Ok(LoopStreamItem::AuthoredInputReady {
                    context: Some(context.clone()),
                    prompt: prompt.clone(),
                    folded: Vec::new(),
                    reply: tokio::sync::oneshot::channel().0,
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
        crate::test_support::node_builder()
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
            node_did: request.node_did.clone(),
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
#[ignore = "manual persistent provider publication benchmark"]
async fn provider_publication_request_size_benchmark() {
    use crate::config_client::ConfigAccess;
    use crate::session::canonical_rows::{
        output_segment_create_variables, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::{OutputSource, SourceClose};
    let mut measurements = Vec::new();
    for sources in [1_usize, 32, 128] {
        let (node, path, lifecycle, _) = fixture(&format!("publication-benchmark-{sources}")).await;
        let message = gents_protocol::message::Message::assistant("x".repeat(64 * 1024));
        let (mut noise, _) =
            provider_segment_for_message(&lifecycle, &message, chrono::Utc::now().to_rfc3339());
        noise.close = Some(SourceClose::Closed {
            outcome: gents_protocol::output::OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![noise.payload.len().try_into().unwrap()],
        });
        ConfigAccess::transact_local_write(&node, None, "test.publication_benchmark_seed", |txn| {
            let mut noise = noise.clone();
            Box::pin(async move {
                for index in 0..sources {
                    noise.source = OutputSource::Authored {
                        key: format!("noise-{index}"),
                    };
                    txn.execute_with_variables(
                        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                        &output_segment_create_variables(&noise)?,
                    )
                    .await?;
                }
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
        let mut fresh = Vec::new();
        let mut replay = Vec::new();
        let mut fresh_scans = Vec::new();
        let mut replay_scans = Vec::new();
        for iteration in 0..6_u32 {
            let (mut segment, encoded) =
                provider_segment_for_message(&lifecycle, &message, chrono::Utc::now().to_rfc3339());
            if let OutputSource::ProviderTurn { turn_index, .. } = &mut segment.source {
                *turn_index = iteration;
            }
            let publish = || {
                super::canonical::publish_provider_turn(
                    &node,
                    lifecycle.execution_generation().unwrap(),
                    super::canonical::ProviderPublicationPlan {
                        final_flush: Some(segment.clone()),
                        message_key: format!("benchmark-turn-{iteration}"),
                        encoded: encoded.clone(),
                        expected: Arc::new(message.clone()),
                        tool_deadline_at: lifecycle.claimed_deadline_at().unwrap().to_rfc3339(),
                        background_calls: Vec::new(),
                        consumes_folded: None,
                    },
                )
            };
            let start = std::time::Instant::now();
            let (published, scans) = crate::session::count_request_output_scans(publish()).await;
            let published = published.unwrap();
            fresh_scans.push(scans);
            let fresh_ms = start.elapsed().as_secs_f64() * 1000.0;
            let start = std::time::Instant::now();
            let (replayed, scans) = crate::session::count_request_output_scans(publish()).await;
            let replayed = replayed.unwrap();
            replay_scans.push(scans);
            let replay_ms = start.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(published.message_doc_id, replayed.message_doc_id);
            if iteration > 0 {
                fresh.push(fresh_ms);
                replay.push(replay_ms);
            }
        }
        fresh.sort_by(f64::total_cmp);
        replay.sort_by(f64::total_cmp);
        measurements.push(serde_json::json!({
            "prior_sources": sources, "bytes_per_source": 64 * 1024,
            "fresh_publication_median_ms": fresh[2], "replay_median_ms": replay[2],
            "fresh_request_scans": fresh_scans, "replay_request_scans": replay_scans,
        }));
        node.shutdown().await;
        std::fs::remove_dir_all(path).unwrap();
    }
    let result = serde_json::json!({"measurements": measurements, "persistent_node": true});
    if let Some(path) = std::env::var_os("GENTS_PUBLICATION_BENCHMARK_OUTPUT") {
        std::fs::write(path, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
    }
    tracing::info!(%result, "provider publication benchmark");
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
            background_calls: Vec::new(),
            consumes_folded: None,
        },
    )
    .await
    .expect_err("first publication must compare reconstructed and native messages");
    assert!(ErrorChain(error.as_ref())
        .to_string()
        .contains("first publication native message changed"));
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
    node: &Arc<EmbeddedNode>,
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
        background_calls: Vec::new(),
        consumes_folded: None,
    };
    super::canonical::publish_provider_turn(node, lifecycle.execution_generation().unwrap(), plan)
        .await
}

#[tokio::test]
async fn publication_and_replay_read_only_the_selected_source() {
    let (node, path, lifecycle, writer) = fixture("publication-single-scan").await;
    for index in 0..3 {
        writer
            .publish_authored_message(
                &lifecycle,
                &format!("authored-{index}"),
                &gents_protocol::message::Message::user(format!("earlier-{index}")),
            )
            .await
            .unwrap();
    }
    let message = gents_protocol::message::Message::assistant("selected source");
    let ((published, scans), source_queries) =
        crate::session::count_source_output_queries(crate::session::count_request_output_scans(
            publish_tool_turn_for_rollback_test(&node, &lifecycle, &message),
        ))
        .await;
    let published = published.unwrap();
    assert_eq!(scans, 0);
    assert_eq!(source_queries, 1);
    let ((replayed, scans), source_queries) =
        crate::session::count_source_output_queries(crate::session::count_request_output_scans(
            publish_tool_turn_for_rollback_test(&node, &lifecycle, &message),
        ))
        .await;
    assert_eq!(scans, 0);
    assert_eq!(source_queries, 1);
    assert_eq!(published.message_doc_id, replayed.unwrap().message_doc_id);
    let query = crate::session::canonical_rows::source_output_segments_query(
        &lifecycle.request().doc_id,
        &gents_protocol::output::OutputSource::ProviderTurn {
            scope: "inference.1".parse().unwrap(),
            turn_index: 0,
            attempt: 0,
        },
    )
    .unwrap();
    let explained = crate::config_client::ConfigAccess::local(node.clone())
        .execute(&format!("query @explain(type: execute) {query}"))
        .await
        .unwrap();
    fn document_fetches(value: &serde_json::Value) -> u64 {
        match value {
            serde_json::Value::Object(fields) => fields
                .iter()
                .map(|(key, value)| {
                    if key == "docFetches" {
                        value.as_u64().unwrap()
                    } else {
                        document_fetches(value)
                    }
                })
                .sum(),
            serde_json::Value::Array(values) => values.iter().map(document_fetches).sum(),
            _ => 0,
        }
    }
    assert_eq!(document_fetches(&explained["data"]["explain"]), 1);
    let history = crate::session::load_history(
        &node,
        &lifecycle.request().session_id,
        "did:test:test",
        None,
    )
    .await
    .unwrap();
    assert_eq!(history.len(), 4);
    assert_eq!(history.last(), Some(&message));
    node.shutdown().await;
    std::fs::remove_dir_all(path).unwrap();
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
    let (_, writes) = crate::config_client::ConfigApplyTxn::assert_every_mutation_rolls_back(
        || publish_tool_turn_for_rollback_test(&node, &lifecycle, &message),
        rows,
    )
    .await;
    assert!(
        writes >= 3,
        "tool-bearing publication must have multiple writes"
    );
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}

async fn recovery_receipt_fixture(
    name: &str,
) -> (
    Arc<EmbeddedNode>,
    std::path::PathBuf,
    RequestLifecycle,
    DefraStreamWriter,
    gents_protocol::row::AgentRequestRow,
    String,
) {
    let (node, path, lifecycle, writer) = fixture(name).await;
    let request = lifecycle.request();
    crate::config_client::ConfigAccess::transact_local_write(
        &node,
        None,
        "test.recovery_session_observation",
        |txn| {
            Box::pin(async move {
                let now = chrono::Utc::now().to_rfc3339();
                crate::session::ensure_session_in_txn(
                    txn,
                    &request.session_id,
                    &request.node_did,
                    &request.agent_id,
                    request.requester_did.as_deref(),
                    None,
                    None,
                    &now,
                )
                .await?;
                let session = crate::session::load_agent_session_row_in_txn(
                    txn,
                    &request.node_did,
                    &request.session_id,
                    request.requester_did.as_deref(),
                )
                .await?
                .expect("recovery session");
                let facts =
                    crate::session::load_scoped_request_facts_in_txn(txn, &session.session, false)
                        .await?;
                let fact = facts
                    .iter()
                    .find(|fact| fact.observed.request_doc_id == request.doc_id)
                    .expect("recovery request fact");
                assert!(
                    crate::session::advance_session_request_observation_in_txn(
                        txn,
                        fact,
                        &request.content,
                        &now,
                    )
                    .await?
                );
                Ok::<_, anyhow::Error>(())
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
    let expire = crate::config_client::ConfigAccess::write_local(
        &node,
        "test.expire_recovery_receipt_fixture",
        &format!(
            r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, input: {{ execution_lease_expires_at: "{}" }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(&expiry),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        expire["data"]["update_AgentRequest"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}) {{ {} }} }}"#,
        crate::watcher::AGENT_REQUEST_FIELDS,
    );
    let response = crate::graphql::graphql_with_transaction_retry(
        &node,
        &query,
        "test.read_recovery_receipt_fixture",
    )
    .await
    .unwrap();
    let observed = crate::graphql::first_row(&response, "AgentRequest")
        .unwrap()
        .unwrap();
    (node, path, lifecycle, writer, observed, expiry)
}

#[tokio::test]
async fn recovery_mutations_are_atomic_and_caller_replay_has_no_transactional_mutations() {
    use crate::config_client::ConfigApplyTxn;
    use crate::lifecycle::{recover_expired_generation_with_facts, RecoveryResult};

    let (node, path, _lifecycle, _writer, observed, expiry) =
        recovery_receipt_fixture("recovery-mutation-rollback").await;
    let doc_id = observed.doc_id.as_deref().unwrap();
    let escaped = crate::graphql::escape_graphql_string(doc_id);
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
        response.data
    };
    let before = snapshot().await;
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
    let (recovered, mutation_count) =
        ConfigApplyTxn::assert_every_mutation_rolls_back(recover, snapshot).await;
    assert_eq!(recovered, RecoveryResult::Won { published: 1 });
    assert!(
        mutation_count >= 4,
        "recovery must close, publish, swap generation and refresh the session"
    );
    let committed = snapshot().await;
    assert_ne!(
        committed.as_ref().unwrap()["AgentSession"],
        before.as_ref().unwrap()["AgentSession"],
        "successful recovery must update the session observation"
    );
    let (replay, writes) =
        ConfigApplyTxn::with_successful_mutation_failure_at(None, recover()).await;
    assert_eq!(replay.unwrap(), RecoveryResult::Won { published: 1 });
    assert_eq!(writes, 0);
    assert_eq!(snapshot().await, committed);
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn post_commit_receipt_loss_replays_exact_recovered_generation_and_selection() {
    use crate::config_client::ConfigApplyTxn;
    use crate::lifecycle::{recover_expired_generation_with_facts, RecoveryResult};
    use gents_protocol::output::{MessagePublication, TerminalOutput};

    let (node, path, _lifecycle, _writer, observed, expiry) =
        recovery_receipt_fixture("recovery-receipt-loss").await;
    let request_doc_id = observed.doc_id.as_deref().unwrap();
    let old_generation = observed.execution_generation.as_deref().unwrap();
    let fresh_generation = "recovery-receipt-loss-generation";
    assert_ne!(old_generation, fresh_generation);
    let observed_now = chrono::Utc::now();
    let request = crate::graphql::escape_graphql_string(request_doc_id);
    let query = format!(
        r#"{{
            AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}, order: {{ ordinal: ASC }}) {{ _docID ordinal source writer runs payload close created_at }}
            AgentMessage(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ _docID message_key sequence publication outcome role blocks created_at }}
            AgentToolCall(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ _docID lifecycle_state }}
            AgentRequest(filter: {{ _docID: {{ _eq: "{request}" }} }}) {{ {} lifecycle_state terminal_output failure_reason terminalized_at }}
            AgentSession {{ _docID observation }}
        }}"#,
        crate::watcher::AGENT_REQUEST_FIELDS,
    );
    let snapshot = || async {
        crate::graphql::graphql_with_transaction_retry(
            &node,
            &query,
            "test.recovery_receipt_snapshot",
        )
        .await
        .expect("read recovery physical facts")
        .data
        .expect("recovery query data")
    };
    let before = snapshot().await;
    assert!(before["AgentMessage"].as_array().unwrap().is_empty());
    let original_segments = before["AgentOutputSegment"].as_array().unwrap();
    assert_eq!(original_segments.len(), 1);
    let original = &original_segments[0];
    assert!(original["close"].is_null());
    let recover = || {
        recover_expired_generation_with_facts(
            &node,
            &observed,
            old_generation,
            &expiry,
            fresh_generation.into(),
            observed_now,
            None,
            None,
        )
    };
    let (first, fault_fired) = ConfigApplyTxn::with_post_commit_receipt_loss(recover()).await;
    assert!(
        fault_fired,
        "the committed recovery receipt must be lost once"
    );
    assert_eq!(first.unwrap(), RecoveryResult::Won { published: 1 });

    let committed = snapshot().await;
    let headers = committed["AgentMessage"].as_array().unwrap();
    assert_eq!(
        headers.len(),
        1,
        "nonempty prefix must publish one recovery header"
    );
    assert!(
        !headers[0]["blocks"].as_array().unwrap().is_empty(),
        "recovery header must retain the flushed provider prefix"
    );
    let committed_segments = committed["AgentOutputSegment"].as_array().unwrap();
    assert_eq!(
        committed_segments.len(),
        before["AgentOutputSegment"].as_array().unwrap().len() + 1,
        "recovery must close the unclosed provider prefix"
    );
    assert!(
        committed_segments.contains(original),
        "recovery must retain the exact original flush"
    );
    let closing = committed_segments
        .iter()
        .find(|row| row["_docID"] != original["_docID"])
        .expect("recovery creates one new closing segment");
    assert!(!closing["close"].is_null());
    assert_eq!(closing["source"], original["source"]);
    assert_eq!(closing["writer"], original["writer"]);
    let header_doc_id = headers[0]["_docID"].as_str().unwrap();
    let publication: MessagePublication =
        serde_json::from_value(headers[0]["publication"].clone()).unwrap();
    assert!(matches!(
        publication,
        MessagePublication::RequestRecovery { execution_generation }
            if execution_generation == fresh_generation
    ));
    assert_eq!(headers[0]["outcome"], "partial");
    let request = &committed["AgentRequest"][0];
    assert_eq!(request["lifecycle_state"], "failed");
    assert_eq!(request["execution_generation"], fresh_generation);
    let selection: TerminalOutput =
        serde_json::from_value(request["terminal_output"].clone()).expect("recovery selection");
    assert_eq!(
        selection,
        TerminalOutput::Message {
            message_doc_id: header_doc_id.to_owned(),
        }
    );
    assert_ne!(
        committed["AgentSession"], before["AgentSession"],
        "recovery must refresh the durable session observation"
    );

    let (replay, writes) =
        ConfigApplyTxn::with_successful_mutation_failure_at(None, recover()).await;
    assert_eq!(replay.unwrap(), RecoveryResult::Won { published: 1 });
    assert_eq!(writes, 0, "same-generation replay must not mutate");
    assert_eq!(
        snapshot().await,
        committed,
        "external replay must preserve all physical IDs, request facts and session observation"
    );
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
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
            background_calls: Vec::new(),
            consumes_folded: None,
        },
        now,
    )
    .await
    .expect_err("closure time must be nondecreasing against committed data");
    assert!(ErrorChain(error.as_ref())
        .to_string()
        .contains("closure timestamp precedes"));
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
        &crate::config_client::ConfigAccess::local(node.clone()),
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
    let access = crate::config_client::ConfigAccess::local(node.clone());
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
            &crate::config_client::ConfigAccess::local(node.clone()),
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

    let mut tails = writer.provider_tails.lock().await;
    let mut publication = Box::pin(writer.publish_native_turn(&lifecycle, 0, 0, &message));
    assert!(futures::poll!(publication.as_mut()).is_pending());
    tokio::time::sleep(Duration::from_millis(2)).await;
    let tail = tails.get_mut(&request.doc_id).unwrap();
    let delta = provider_flush_delta(&tail.streams, &encoded).unwrap();
    let segment = OutputSegment {
        node_did: request.node_did.clone(),
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
        &crate::config_client::ConfigAccess::local(node.clone()),
        &published.message_doc_id,
        &request.node_did,
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
        .collect::<Result<Vec<_>, _>>()
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
        &crate::config_client::ConfigAccess::local(node.clone()),
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
        .execute_request(
            defra_node::QueryRequest::new(
                crate::session::canonical_rows::CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
            )
            .with_variables(
                crate::session::canonical_rows::output_segment_create_variables(&late).unwrap(),
            ),
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
        content: vec![
            AssistantContent::ToolCall(ToolCall {
                id: "native-tool".into(),
                call_id: Some("provider-call".into()),
                function: ToolFunction::new("read".into(), serde_json::json!({"path":"文.txt"})),
                signature: Some("sig".into()),
                additional_params: None,
            }),
            AssistantContent::ToolCall(ToolCall {
                id: "native-tool-2".into(),
                call_id: Some("provider-call-2".into()),
                function: ToolFunction::new(
                    "write".into(),
                    serde_json::json!({"path":"other.txt"}),
                ),
                signature: None,
                additional_params: None,
            }),
        ],
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
    assert_eq!(first.accepted_tools.len(), 2);
    assert_eq!(replay.accepted_tools.len(), 2);
    let tool_rows = node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID tool_call_id }} }}"#,
            crate::graphql::escape_graphql_string(&lifecycle.request().doc_id),
        ))
        .await;
    assert!(!tool_rows.has_errors(), "{:?}", tool_rows.errors);
    let rows = tool_rows.data.unwrap()["AgentToolCall"]
        .as_array()
        .unwrap()
        .clone();
    let doc_id_for = |tool_call_id: &str| {
        rows.iter()
            .find(|row| row["tool_call_id"] == tool_call_id)
            .unwrap()["_docID"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    for (index, (tool_id, call_id)) in [
        ("native-tool", Some("provider-call")),
        ("native-tool-2", Some("provider-call-2")),
    ]
    .into_iter()
    .enumerate()
    {
        let a = &first.accepted_tools[index];
        let b = &replay.accepted_tools[index];
        assert_eq!(a.id, tool_id);
        assert_eq!(a.call_id.as_deref(), call_id);
        assert_eq!(a.tool_call_doc_id, doc_id_for(tool_id));
        assert_eq!(a.tool_call_doc_id, b.tool_call_doc_id);
        assert_eq!(a.accepted_header_doc_id, b.accepted_header_doc_id);
        assert_eq!(a.arguments, b.arguments);
    }
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn post_commit_receipt_loss_replays_into_exact_committed_publication() {
    use gents_protocol::message::{AssistantContent, Message, ToolCall, ToolFunction};
    let (node, path, lifecycle, writer) = fixture("receipt-loss").await;
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
    let (first, fault_fired) = crate::config_client::ConfigApplyTxn::with_post_commit_receipt_loss(
        writer.publish_native_turn(&lifecycle, 0, 0, &message),
    )
    .await;
    let first = first.expect("idempotent owner must reconstruct the lost receipt");
    assert!(fault_fired, "post-commit receipt-loss fault must fire");
    let request = crate::graphql::escape_graphql_string(&lifecycle.request().doc_id);
    let query = format!(
        r#"{{
            AgentMessage(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ _docID sequence }}
            AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}, order: {{ ordinal: ASC }}) {{ _docID ordinal close }}
            AgentToolCall(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ _docID lifecycle_state }}
        }}"#
    );
    let before = crate::graphql::graphql_with_transaction_retry(
        &node,
        &query,
        "receipt_loss_before_external_replay",
    )
    .await
    .expect("read committed receipt-loss publication");
    let replay = writer
        .publish_native_turn(&lifecycle, 0, 0, &message)
        .await
        .expect("external replay resolves committed publication");
    let replayed = crate::graphql::graphql_with_transaction_retry(
        &node,
        &query,
        "receipt_loss_after_external_replay",
    )
    .await
    .expect("read publication after external replay");
    assert_eq!(
        replayed.data, before.data,
        "replay after a lost receipt must not add stored documents"
    );
    assert_eq!(first.message_doc_id, replay.message_doc_id);
    assert_eq!(first.sequence, replay.sequence);
    assert_eq!(first.accepted_tools.len(), replay.accepted_tools.len());
    assert_eq!(first.accepted_tools.len(), 1);
    let a = &first.accepted_tools[0];
    let b = &replay.accepted_tools[0];
    assert_eq!(a.tool_call_doc_id, b.tool_call_doc_id);
    assert_eq!(a.accepted_header_doc_id, b.accepted_header_doc_id);
    assert_eq!(a.arguments, b.arguments);
    assert_eq!(a.id, "native-tool");
    assert_eq!(a.call_id.as_deref(), Some("provider-call"));
    let rows = before.data.as_ref().unwrap();
    let message_rows = rows["AgentMessage"].as_array().unwrap();
    assert_eq!(message_rows.len(), 1, "exactly one committed header");
    assert_eq!(message_rows[0]["_docID"], first.message_doc_id);
    assert_eq!(message_rows[0]["sequence"], first.sequence);
    let segment_rows = rows["AgentOutputSegment"].as_array().unwrap();
    assert_eq!(segment_rows.len(), 1);
    assert_eq!(segment_rows[0]["_docID"], a.arguments.close_doc_id);
    assert!(
        !segment_rows[0]["close"].is_null(),
        "publication closes its delta"
    );
    let tool_rows = rows["AgentToolCall"].as_array().unwrap();
    assert_eq!(tool_rows.len(), 1);
    assert_eq!(tool_rows[0]["_docID"], a.tool_call_doc_id);
    assert_eq!(tool_rows[0]["lifecycle_state"], "pending");
    assert_eq!(a.accepted_header_doc_id, first.message_doc_id);

    let deadline = lifecycle.claimed_deadline_at().unwrap();
    let mut first_dispatch = crate::tool_call_lifecycle::ToolCallLifecycle::from_accepted(
        node.clone(),
        lifecycle.request().node_did.clone(),
        lifecycle.request().requester_did.clone(),
        a.clone(),
        deadline,
        crate::tool_call_lifecycle::AwaitMode::Foreground,
    )
    .unwrap();
    let mut replay_dispatch = crate::tool_call_lifecycle::ToolCallLifecycle::from_accepted(
        node.clone(),
        lifecycle.request().node_did.clone(),
        lifecycle.request().requester_did.clone(),
        b.clone(),
        deadline,
        crate::tool_call_lifecycle::AwaitMode::Foreground,
    )
    .unwrap();
    let (first_start, replay_start) = tokio::join!(
        first_dispatch.start_running(),
        replay_dispatch.start_running(),
    );
    assert_ne!(
        first_start.is_ok(),
        replay_start.is_ok(),
        "exactly one accepted handle must win pending-to-running dispatch: first={first_start:?}, replay={replay_start:?}"
    );
    let loser_error = match (&first_start, &replay_start) {
        (Ok(_), Err(error)) | (Err(error), _) => error,
        (Ok(_), Ok(_)) => unreachable!("exactly one handle wins, asserted above"),
    };
    assert!(
        ErrorChain(&loser_error)
            .to_string()
            .contains("no longer pending"),
        "the losing handle must be rejected by the physical pending-row fence: {}",
        ErrorChain(&loser_error)
    );
    let dispatched = crate::graphql::graphql_with_transaction_retry(
        &node,
        &query,
        "receipt_loss_after_dispatch_race",
    )
    .await
    .expect("read single dispatched tool row");
    let dispatched_tools = dispatched.data.as_ref().unwrap()["AgentToolCall"]
        .as_array()
        .unwrap();
    assert_eq!(dispatched_tools.len(), 1);
    assert_eq!(dispatched_tools[0]["_docID"], a.tool_call_doc_id);
    assert_eq!(dispatched_tools[0]["lifecycle_state"], "running");

    let winner = if first_start.is_ok() {
        &mut first_dispatch
    } else {
        &mut replay_dispatch
    };
    winner
        .complete("one receipt-loss dispatch result")
        .await
        .unwrap();
    let completed = crate::graphql::graphql_with_transaction_retry(
        &node,
        &query,
        "receipt_loss_after_single_completion",
    )
    .await
    .expect("read completed tool row");
    let completed_tools = completed.data.as_ref().unwrap()["AgentToolCall"]
        .as_array()
        .unwrap();
    assert_eq!(completed_tools.len(), 1);
    assert_eq!(completed_tools[0]["_docID"], a.tool_call_doc_id);
    assert_eq!(completed_tools[0]["lifecycle_state"], "completed");
    let output = crate::background_tools::canonical_tool_output(
        &node,
        &a.tool_call_doc_id,
        &lifecycle.request().doc_id,
        &lifecycle.request().session_id,
        &lifecycle.request().node_did,
        lifecycle.request().requester_did.as_deref(),
    )
    .await
    .unwrap();
    assert_eq!(output, "one receipt-loss dispatch result");
    node.shutdown().await;
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn modeled_inputs_survive_publication_replay_and_forks() {
    use gents_protocol::request_input::RequestInput;
    const OWNER: &str = "did:test:test";
    const SESSION: &str = "captured-documents";
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::schema::ensure_runtime_schemas(&node).await.unwrap();
    crate::test_support::install_test_agent(&node, OWNER, "general").await;
    let mut lifecycle = claimed(&node, "captured-documents", SESSION).await;
    let writer = DefraStreamWriter::new(node.clone(), OWNER, Duration::ZERO);
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    let access = crate::config_client::ConfigAccess::local(node.clone());
    let mut expected = Vec::new();
    for (index, case) in crate::lean_vocab_test::lean_contract_snapshot()
        .request_input_cases
        .iter()
        .enumerate()
    {
        let input: RequestInput = serde_json::from_value(case["input"].clone()).unwrap();
        if (input.captured_context.is_empty() && input.attachments.is_empty())
            || case["expected_provider_parts"].is_null()
            || !input.attachments_well_formed()
        {
            continue;
        }
        let message = input
            .authored_message(case["authored_text"].as_str().unwrap())
            .unwrap();
        let key = format!("capture:{index}");
        let first = writer
            .publish_authored_message(&lifecycle, &key, &message)
            .await
            .unwrap();
        let replay = writer
            .publish_authored_message(&lifecycle, &key, &message)
            .await
            .unwrap();
        assert_eq!(first, replay, "{}: publication replay", case["name"]);
        let (_, reconstructed) =
            crate::session::load_canonical_message(&access, &first, OWNER, None)
                .await
                .unwrap();
        assert_eq!(
            serde_json::to_value(&reconstructed).unwrap(),
            case["expected_native_input"],
            "{}: sealed document reconstruction",
            case["name"]
        );
        assert_eq!(
            gents_protocol::request_input::CapturedContext::from_message(&reconstructed).unwrap(),
            input.captured_context
        );
        expected.push(message);
    }
    assert!(!expected.is_empty());
    lifecycle
        .terminalize_owned(
            crate::lifecycle::RequestTerminalOutcome::Failed,
            gents_protocol::output::TerminalOutput::NoMessage,
            Some("fixture finished"),
        )
        .await
        .unwrap();
    let before = crate::session::load_history(&node, SESSION, OWNER, None)
        .await
        .unwrap();
    assert_eq!(before, expected);
    let child = crate::session::fork(
        &node,
        crate::session::ForkParams {
            source_session_id: SESSION,
            boundary: crate::session::ForkBoundary::UserTurn(
                u32::try_from(expected.len()).unwrap(),
            ),
            caller_node_did: OWNER,
            caller_requester_did: None,
            target_agent_id: None,
        },
    )
    .await
    .unwrap();
    let forked = crate::session::load_history(&node, &child.session_id, OWNER, None)
        .await
        .unwrap();
    assert_eq!(
        forked, before,
        "fork retains the original payload and capture metadata"
    );
    assert_eq!(
        crate::session::load_history(&node, SESSION, OWNER, None)
            .await
            .unwrap(),
        before
    );
    node.shutdown().await;
}

#[tokio::test]
async fn cancelled_flush_replays_before_inference_close_or_publication() {
    use crate::config_client::{ConfigAccess, ConfigApplyTxn};
    use crate::session::canonical_rows::AGENT_OUTPUT_SEGMENT_FIELDS;
    use gents_protocol::message::Message;
    use gents_protocol::output::TerminalOutput;

    for publish in [false, true] {
        let name = if publish {
            "cancelled-flush-publication"
        } else {
            "cancelled-flush-close"
        };
        let (node, path, lifecycle, writer) = fixture(name).await;
        let request = lifecycle.request();
        writer
            .start_provider_attempt(&request.doc_id, 0, 0, "inference.1".parse().unwrap())
            .await;
        let prefix = Message::assistant("received 世界");
        let committed = Arc::new(tokio::sync::Notify::new());
        let mut flush = Box::pin(ConfigApplyTxn::with_embedded_commit_ack_pause(
            committed.clone(),
            writer.flush_native_partial(&lifecycle, &prefix),
        ));
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                () = committed.notified() => {},
                result = &mut flush => panic!("flush skipped the commit boundary: {result:?}"),
            }
        })
        .await
        .expect("inference flush commits before acknowledgement");
        drop(flush);
        let access = ConfigAccess::local(node.clone());
        let query = format!(
            "{{AgentOutputSegment(filter: {{request_doc_id: {{_eq: \"{}\"}}}}) {{{AGENT_OUTPUT_SEGMENT_FIELDS}}}}}",
            crate::graphql::escape_graphql_string(&request.doc_id),
        );
        let before = access.execute(&query).await.unwrap();
        let originals = before["data"]["AgentOutputSegment"].as_array().unwrap();
        assert_eq!(originals.len(), 1);
        assert!(originals[0]["close"].is_null());

        let expected = if publish {
            Message::assistant("received 世界 and final output")
        } else {
            prefix
        };
        let message_doc_id = if publish {
            writer
                .publish_native_turn(&lifecycle, 0, 0, &expected)
                .await
                .expect("acceptance replays the pending prefix before appending its final delta")
                .message_doc_id
        } else {
            writer
                .close_provider_attempt(&lifecycle, 0, 0, canonical::ProviderAttemptClose::Partial)
                .await
                .expect("partial closure acknowledges all received output");
            let TerminalOutput::Message { message_doc_id } =
                writer.terminal_output(&request.doc_id).await
            else {
                panic!("partial inference close must retain its message");
            };
            message_doc_id
        };
        let (_, reconstructed) = crate::session::load_canonical_message(
            &access,
            &message_doc_id,
            &request.node_did,
            request.requester_did.as_deref(),
        )
        .await
        .unwrap();
        assert_eq!(reconstructed, expected);
        let after = access.execute(&query).await.unwrap();
        let rows = after["data"]["AgentOutputSegment"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "original flush and one closing record");
        assert!(
            rows.contains(&originals[0]),
            "exact original flush is retained"
        );
        if publish {
            let replay = writer
                .publish_native_turn(&lifecycle, 0, 0, &expected)
                .await
                .unwrap();
            assert_eq!(replay.message_doc_id, message_doc_id);
            assert_eq!(access.execute(&query).await.unwrap(), after);
        }
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(path);
    }
}
