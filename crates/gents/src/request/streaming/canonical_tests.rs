use super::*;
use crate::request::lifecycle::{ClaimOutcome, RequestLifecycle};
use std::{sync::Arc, time::Duration};

struct FixtureGuard {
    _db: crate::support::TestDb,
    owner: crate::ConfigAccess,
}

fn fixture_access(node: &Arc<EmbeddedNode>, lifecycle: &RequestLifecycle) -> crate::ConfigAccess {
    crate::ConfigAccess::local_with_agent_signer(
        node.clone(),
        lifecycle.actor_did().unwrap(),
        lifecycle.managed_agent_signer().unwrap(),
    )
    .unwrap()
}

async fn fixture_with_ids(
    name: &str,
    request_id: &str,
    session_id: &str,
) -> (
    Arc<EmbeddedNode>,
    FixtureGuard,
    RequestLifecycle,
    DefraStreamWriter,
) {
    use crate::store::identity::NodeIdentity;
    let (db, user, owner, binding) = crate::test_support::signed_test_node(name).await;
    let node = db.node.clone();
    let agent = crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
        .unwrap();
    let create = crate::request::lifecycle::build_signed_client_request(
        &owner,
        crate::request::lifecycle::RequestSpec::new(
            gents_protocol::request_admission::RequestPurpose::Normal,
            crate::request::lifecycle::RequestIdentity {
                request_id: request_id.into(),
                node_did: db.node_identity.did().into(),
                requester_did: Some(user.did().into()),
                agent_did: agent.did().into(),
                session_id: session_id.into(),
                content: "hello".into(),
                execution_origin: crate::request::lifecycle::ExecutionOrigin::Interactive,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            },
            gents_protocol::request_admission::AgentRequestAdmissionRecord::client(user.did()),
        ),
        crate::request::lifecycle::RequestSigner::Identity(&user),
    )
    .await
    .unwrap();
    crate::request::lifecycle::submit_client_request(&owner, &create)
        .await
        .unwrap();
    let access = crate::ConfigAccess::local_managed_agent(
        node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let response = access
        .execute(&format!(
            "{{ AgentRequest {{ {} }} }}",
            gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
        ))
        .await
        .unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    let request = row.try_into().unwrap();
    let (_authority_owner, authority) =
        crate::runtime::agent::p2p_reconcile::enrollment_authority_channel();
    let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
        node.clone(),
        db.node_identity.clone(),
        authority,
    );
    let (request, admission) = verifier
        .verify_fresh_for_claim(&request, agent.did())
        .await
        .unwrap();
    let mut lifecycle = RequestLifecycle::new_with_node_did(
        node.clone(),
        request,
        60,
    );
    lifecycle.bind_managed_agent(agent.as_ref()).unwrap();
    lifecycle.set_client_claim_admission(admission);
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    let writer =
        DefraStreamWriter::new_managed(node.clone(), agent.as_ref(), Duration::from_millis(1))
            .unwrap();
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    (node, FixtureGuard { _db: db, owner }, lifecycle, writer)
}

async fn fixture_query(access: &crate::ConfigAccess, document: &str) -> defra_node::QueryResponse {
    access
        .transact_readonly("test.canonical_stream.read", |txn| {
            Box::pin(async move { txn.execute_local_response(document).await })
        })
        .await
        .unwrap()
}

async fn fixture_insert_child(
    access: &crate::ConfigAccess,
    lifecycle: &RequestLifecycle,
    document: &str,
    variables: &serde_json::Value,
    collection: &str,
) -> serde_json::Value {
    let session = crate::store::graphql::escape_graphql_string(&lifecycle.request().session_id);
    let rows = access.execute(&format!(r#"{{ AgentSession(filter: {{session_id: {{_eq: "{session}"}}}}, limit: 1) {{ _docID }} }}"#)).await.unwrap();
    let session_doc = rows["data"]["AgentSession"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    access
        .transact_write("test.canonical_stream.insert_child", |txn| {
            let session_doc = &session_doc;
            Box::pin(async move {
                let response = txn.execute_with_variables(document, variables).await?;
                let doc = crate::store::graphql::created_doc_id(&response, collection)?;
                txn.add_resource_relationship(collection, &doc, "parent", "sessions", session_doc)
                    .await?;
                Ok::<_, anyhow::Error>(response)
            })
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn authored_publication_replays_and_reconstructs_exact_message() {
    use crate::store::identity::NodeIdentity;

    let (db, user, owner, binding) =
        crate::test_support::signed_test_node("canonical-authored").await;
    let node = db.node.clone();
    let agent = crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
        .unwrap();
    let create = crate::request::lifecycle::build_signed_client_request(
        &owner,
        crate::request::lifecycle::RequestSpec::new(
            gents_protocol::request_admission::RequestPurpose::Normal,
            crate::request::lifecycle::RequestIdentity {
                request_id: "request-authored".into(),
                node_did: db.node_identity.did().into(),
                requester_did: Some(user.did().into()),
                agent_did: agent.did().into(),
                session_id: "session-authored".into(),
                content: "hello".into(),
                execution_origin: crate::request::lifecycle::ExecutionOrigin::Interactive,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            },
            gents_protocol::request_admission::AgentRequestAdmissionRecord::client(user.did()),
        ),
        crate::request::lifecycle::RequestSigner::Identity(&user),
    )
    .await
    .unwrap();
    crate::request::lifecycle::submit_client_request(&owner, &create)
        .await
        .unwrap();
    let access = crate::store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let response = access
        .execute(&format!(
            "{{ AgentRequest {{ {} }} }}",
            gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
        ))
        .await
        .unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    let request = row.try_into().unwrap();
    let (_authority_owner, authority) =
        crate::runtime::agent::p2p_reconcile::enrollment_authority_channel();
    let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
        node.clone(),
        db.node_identity.clone(),
        authority,
    );
    let (request, admission) = verifier
        .verify_fresh_for_claim(&request, agent.did())
        .await
        .unwrap();
    let mut lifecycle = RequestLifecycle::new_with_node_did(
        node.clone(),
        request,
        60,
    );
    lifecycle.bind_managed_agent(agent.as_ref()).unwrap();
    lifecycle.set_client_claim_admission(admission);
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    let writer =
        DefraStreamWriter::new_managed(node.clone(), agent.as_ref(), Duration::from_millis(1))
            .unwrap();
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
        &access,
        &first,
        db.node_identity.did(),
        Some(user.did()),
    )
    .await
    .unwrap();
    assert_eq!(reconstructed, message);
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn selected_history_missing_later_payload_never_returns_a_shortened_prefix() {
    use gents_protocol::output::rows::{
        transcript_message_create_variables, CREATE_AGENT_MESSAGE_MUTATION,
    };
    use gents_protocol::output::{
        MessageBlock, MessagePublication, MessageRole, OutputOutcome, PayloadPresentation,
        PayloadRef, PresentedPayload, TranscriptMessage,
    };
    let (node, _db, lifecycle, writer) = fixture_with_ids(
        "incomplete-history",
        "incomplete-request",
        "incomplete-session",
    )
    .await;
    let access = fixture_access(&node, &lifecycle);
    writer
        .publish_authored_message(
            &lifecycle,
            "prompt",
            &gents_protocol::message::Message::user("first complete input"),
        )
        .await
        .unwrap();
    assert_eq!(
        crate::session::load_history_with_access(
            &access,
            "incomplete-session",
            &lifecycle.request().node_did,
            lifecycle.request().requester_did.as_deref()
        )
        .await
        .unwrap()
        .len(),
        1
    );
    let header = TranscriptMessage {
        native_request_id: None,
        message_key: "later-input".into(),
        session_id: "incomplete-session".into(),
        node_did: lifecycle.request().node_did.clone(),
        requester_did: lifecycle.request().requester_did.clone(),
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
    fixture_insert_child(
        &access,
        &lifecycle,
        CREATE_AGENT_MESSAGE_MUTATION,
        &transcript_message_create_variables(&header).unwrap(),
        "AgentMessage",
    )
    .await;
    let error = crate::session::load_history_with_access(
        &access,
        "incomplete-session",
        &lifecycle.request().node_did,
        lifecycle.request().requester_did.as_deref(),
    )
    .await
    .expect_err("missing later payload must fail the complete selection");
    assert!(
        format!("{error:#}").contains("not-yet-replicated-close"),
        "{error:#}"
    );
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn owned_input_handoff_publishes_context_and_prompt_once_before_provider_output() {
    use crate::runtime::agent::loop_stream::LoopStreamItem;
    use crate::runtime::agent::stream_processor::StreamProcessor;
    use gents_protocol::message::Message;
    let (node, path, mut lifecycle, writer) = fixture("owned-input").await;
    let request_doc_id = lifecycle.request().doc_id.clone();
    let hook = crate::tools::call::hook::DefraSessionHook::with_identity(
        node.clone(),
        &lifecycle.request().node_did,
        crate::tools::call::hook::FailurePolicy::default(),
    );
    assert!(crate::session::load_history_with_access(
        &fixture_access(&node, &lifecycle),
        "session-owned-input",
        &lifecycle.request().node_did,
        lifecycle.request().requester_did.as_deref()
    )
    .await
    .unwrap()
    .is_empty());
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
        crate::session::load_history_with_access(
            &fixture_access(&node, &lifecycle),
            "session-owned-input",
            &lifecycle.request().node_did,
            lifecycle.request().requester_did.as_deref()
        )
        .await
        .unwrap(),
        vec![context, prompt]
    );
    let response = fixture_query(
        &fixture_access(&node, &lifecycle),
        "{AgentMessage(order:{sequence: ASC}){message_key request_doc_id sequence}}",
    )
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
    node.shutdown().await.expect("shut down test node");
    drop(path);
}

async fn fixture(
    name: &str,
) -> (
    Arc<EmbeddedNode>,
    FixtureGuard,
    RequestLifecycle,
    DefraStreamWriter,
) {
    fixture_with_ids(name, &format!("request-{name}"), &format!("session-{name}")).await
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
    use gents_protocol::output::rows::{
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
        for index in 0..sources {
            noise.source = OutputSource::Authored {
                key: format!("noise-{index}"),
            };
            fixture_insert_child(
                &fixture_access(&node, &lifecycle),
                &lifecycle,
                CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                &output_segment_create_variables(&noise).unwrap(),
                "AgentOutputSegment",
            )
            .await;
        }
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
                super::canonical::publish_provider_turn_with_signer(
                    &node,
                    lifecycle.actor_did().unwrap(),
                    lifecycle.managed_agent_signer(),
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
        node.shutdown().await.expect("shut down test node");
        drop(path);
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
    let error = super::canonical::publish_provider_turn_with_signer(
        &node,
        lifecycle.actor_did().unwrap(),
        lifecycle.managed_agent_signer(),
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
    assert!(format!("{error:#}").contains("first publication native message changed"));
    let rows = fixture_query(&fixture_access(&node, &lifecycle), &format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID }} AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            crate::store::graphql::escape_graphql_string(&lifecycle.request().doc_id),
            crate::store::graphql::escape_graphql_string(&lifecycle.request().doc_id),
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
    node.shutdown().await.expect("shut down test node");
    drop(path);
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
    super::canonical::publish_provider_turn_with_signer(
        node,
        lifecycle.actor_did().unwrap(),
        lifecycle.managed_agent_signer(),
        lifecycle.execution_generation().unwrap(),
        plan,
    )
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
    let query = gents_protocol::output::rows::source_output_segments_query(
        &lifecycle.request().doc_id,
        &gents_protocol::output::OutputSource::ProviderTurn {
            scope: "inference.1".parse().unwrap(),
            turn_index: 0,
            attempt: 0,
        },
    )
    .unwrap();
    let explained = fixture_access(&node, &lifecycle)
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
    let history = crate::session::load_history_with_access(
        &fixture_access(&node, &lifecycle),
        &lifecycle.request().session_id,
        &lifecycle.request().node_did,
        lifecycle.request().requester_did.as_deref(),
    )
    .await
    .unwrap();
    assert_eq!(history.len(), 4);
    assert_eq!(history.last(), Some(&message));
    node.shutdown().await.expect("shut down test node");
    drop(path);
}

/// Observes the request's published children with their session-parent markers.
async fn published_children(
    node: &Arc<EmbeddedNode>,
    lifecycle: &RequestLifecycle,
) -> serde_json::Value {
    let request_doc_id = crate::store::graphql::escape_graphql_string(&lifecycle.request().doc_id);
    let response = fixture_query(&fixture_access(node, lifecycle), &format!(
        r#"{{
            AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ _docID parent_linked }}
            AgentToolCall(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ _docID parent_linked }}
            AgentMessage(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ _docID parent_linked }}
            AgentRequest(filter: {{ _docID: {{ _eq: "{request_doc_id}" }} }}, limit: 1) {{
                {}
            }}
        }}"#,
        gents_protocol::row::AGENT_REQUEST_FIELDS,
    )).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    response.data.unwrap()
}

fn child_ids(children: &serde_json::Value) -> Vec<String> {
    ["AgentOutputSegment", "AgentToolCall", "AgentMessage"]
        .iter()
        .flat_map(|collection| children[*collection].as_array().unwrap().iter())
        .map(|row| row["_docID"].as_str().unwrap().to_owned())
        .collect()
}

fn child_links(children: &serde_json::Value) -> Vec<bool> {
    ["AgentOutputSegment", "AgentToolCall", "AgentMessage"]
        .iter()
        .flat_map(|collection| children[*collection].as_array().unwrap().iter())
        .map(|row| row["parent_linked"].as_bool().unwrap())
        .collect()
}

/// The publication transaction commits its rows together; their session parent
/// grants follow as the detached child-relationship phase that the creator
/// recovers (`Identity.SessionChild.recoverSessionChild`). A failure inside the
/// transaction leaves nothing; a failure after it leaves committed, unlinked
/// children that the next unfaulted attempt replays and links exactly once.
#[tokio::test]
async fn every_successful_publication_mutation_rolls_back_if_the_transaction_fails() {
    use gents_protocol::message::{AssistantContent, Message, ToolCall, ToolFunction};

    let message = Message::Assistant {
        request_id: None,
        id: Some("provider-message".into()),
        content: vec![AssistantContent::ToolCall(ToolCall {
            raw_arguments: None,
            id: "tool-1".into(),
            call_id: Some("provider-call".into()),
            function: ToolFunction::new("read".into(), serde_json::json!({"path":"note.txt"})),
            signature: None,
            additional_params: None,
        })],
    };
    let mut current = fixture("mutation-rollback-0").await;
    let mut transactional = None;
    let mut index = 1;
    loop {
        let (node, _, lifecycle, _) = &current;
        let before = published_children(node, lifecycle).await;
        let (result, observed) =
            crate::store::config_client::ConfigApplyTxn::with_successful_mutation_failure_at(
                Some(index),
                publish_tool_turn_for_rollback_test(node, lifecycle, &message),
            )
            .await;
        let after = published_children(node, lifecycle).await;
        let error = match result {
            Ok(published) => {
                assert_eq!(observed, index - 1, "a committed attempt must not pass the injected position");
                assert!(child_links(&after).iter().all(|linked| *linked));
                assert_eq!(after["AgentMessage"][0]["_docID"], published.message_doc_id.as_str());
                break;
            }
            Err(error) => error,
        };
        let detail = format!("{error:?}");
        assert!(
            detail.contains(&format!("injected failure after successful transaction mutation {index}"))
                || detail.contains(&format!("InjectedMutationFailure {{ count: {index} }}")),
            "unexpected failure at mutation {index}: {detail}"
        );
        assert_eq!(observed, index, "injection must fire exactly once");
        if after == before {
            assert!(
                transactional.is_none(),
                "mutation {index} rolled back after the publication transaction committed"
            );
        } else {
            let committed = *transactional.get_or_insert(index - 1);
            assert!(committed >= 3, "tool-bearing publication must have multiple writes");
            let links = child_links(&after);
            assert_eq!(links.len(), 3, "mutation {index}: the publication commits every child together");
            assert!(
                links.iter().any(|linked| !linked),
                "mutation {index}: a failed relationship phase must leave a recoverable child"
            );
            let replayed = publish_tool_turn_for_rollback_test(node, lifecycle, &message)
                .await
                .unwrap();
            let recovered = published_children(node, lifecycle).await;
            assert!(child_links(&recovered).iter().all(|linked| *linked));
            assert_eq!(child_ids(&recovered), child_ids(&after), "mutation {index}: replay adds no child");
            assert_eq!(recovered["AgentRequest"], after["AgentRequest"]);
            assert_eq!(
                recovered["AgentMessage"][0]["_docID"],
                replayed.message_doc_id.as_str()
            );
            let (node, path, _, _) = std::mem::replace(
                &mut current,
                fixture(&format!("mutation-rollback-{index}")).await,
            );
            node.shutdown().await.expect("shut down test node");
            drop(path);
        }
        index += 1;
    }
    assert!(
        transactional.is_some(),
        "tool-bearing publication must publish its session parent relationships"
    );
    let (node, path, _, _) = current;
    node.shutdown().await.expect("shut down test node");
    drop(path);
}

async fn recovery_receipt_fixture(
    name: &str,
) -> (
    Arc<EmbeddedNode>,
    FixtureGuard,
    RequestLifecycle,
    DefraStreamWriter,
    gents_protocol::row::AgentRequestRow,
    String,
) {
    let (node, path, lifecycle, writer) = fixture(name).await;
    let request = lifecycle.request();
    let session = crate::store::graphql::escape_graphql_string(&request.session_id);
    let observed_session = fixture_query(&fixture_access(&node, &lifecycle), &format!(
        r#"{{ AgentSession(filter: {{session_id: {{_eq: "{session}"}}}}, limit: 1) {{ observation }} }}"#
    )).await;
    let observation = &observed_session.data.as_ref().unwrap()["AgentSession"][0]["observation"];
    assert_eq!(
        observation["latest_request"]["request_doc_id"],
        request.doc_id
    );
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
    let escaped = crate::store::graphql::escape_graphql_string(doc_id);
    let expiry = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    let expire = fixture_access(&node, &lifecycle).write(
        "test.expire_recovery_receipt_fixture",
        &format!(
            r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, input: {{ execution_lease_expires_at: "{}" }}) {{ _docID }} }}"#,
            crate::store::graphql::escape_graphql_string(&expiry),
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
        gents_protocol::row::AGENT_REQUEST_FIELDS,
    );
    let response = fixture_query(&fixture_access(&node, &lifecycle), &query).await;
    let observed = crate::store::graphql::first_row(&response, "AgentRequest")
        .unwrap()
        .unwrap();
    (node, path, lifecycle, writer, observed, expiry)
}

async fn recovery_snapshot(
    node: &Arc<EmbeddedNode>,
    lifecycle: &RequestLifecycle,
    doc_id: &str,
) -> serde_json::Value {
    let escaped = crate::store::graphql::escape_graphql_string(doc_id);
    let response = fixture_query(&fixture_access(node, lifecycle), &format!(
        r#"{{
            AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{escaped}" }} }}) {{ _docID parent_linked }}
            AgentMessage(filter: {{ request_doc_id: {{ _eq: "{escaped}" }} }}) {{ _docID parent_linked }}
            AgentToolCall(filter: {{ request_doc_id: {{ _eq: "{escaped}" }} }}) {{ _docID parent_linked }}
            AgentSession {{ _docID observation }}
            AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}) {{ {} }}
        }}"#,
        gents_protocol::row::AGENT_REQUEST_FIELDS,
    )).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    response.data.unwrap()
}

/// Recovery commits close, publication, generation swap and session refresh in
/// one transaction; the published children's parent grants follow as the
/// detached, creator-recovered relationship phase. A failure inside the
/// transaction leaves nothing; a failure after it is converged by the caller's
/// replay, which adds no transactional mutation.
#[tokio::test]
async fn recovery_mutations_are_atomic_and_caller_replay_has_no_transactional_mutations() {
    use crate::request::lifecycle::{
        recover_expired_generation_with_access_for_test, RecoveryResult,
    };
    use crate::store::config_client::ConfigApplyTxn;

    async fn recover(
        node: &Arc<EmbeddedNode>,
        lifecycle: &RequestLifecycle,
        observed: &gents_protocol::row::AgentRequestRow,
        expiry: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<RecoveryResult> {
        recover_expired_generation_with_access_for_test(
            &fixture_access(node, lifecycle),
            observed,
            observed.execution_generation.as_deref().unwrap(),
            expiry,
            "recovery-atomicity-generation".into(),
            now,
            None,
            None,
        )
        .await
    }

    let mut current = recovery_receipt_fixture("recovery-mutation-rollback-0").await;
    let mut transactional = None;
    let mut index = 1;
    loop {
        let (node, _, lifecycle, _, observed, expiry) = &current;
        // Each fixture expires its own lease; recovery observes time after it.
        let now = chrono::Utc::now();
        let doc_id = observed.doc_id.as_deref().unwrap();
        let before = recovery_snapshot(node, lifecycle, doc_id).await;
        let (result, count) = ConfigApplyTxn::with_successful_mutation_failure_at(
            Some(index),
            recover(node, lifecycle, observed, expiry, now),
        )
        .await;
        let after = recovery_snapshot(node, lifecycle, doc_id).await;
        let error = match result {
            Ok(recovered) => {
                assert_eq!(count, index - 1, "a committed attempt must not pass the injected position");
                assert_eq!(recovered, RecoveryResult::Won { published: 1 });
                assert!(child_links(&after).iter().all(|linked| *linked));
                assert_ne!(
                    after["AgentSession"], before["AgentSession"],
                    "successful recovery must update the session observation"
                );
                let (replay, writes) = ConfigApplyTxn::with_successful_mutation_failure_at(
                    None,
                    recover(node, lifecycle, observed, expiry, now),
                )
                .await;
                assert_eq!(replay.unwrap(), RecoveryResult::Won { published: 1 });
                assert_eq!(writes, 0);
                assert_eq!(recovery_snapshot(node, lifecycle, doc_id).await, after);
                break;
            }
            Err(error) => error,
        };
        let detail = format!("{error:?}");
        assert!(
            detail.contains(&format!("injected failure after successful transaction mutation {index}"))
                || detail.contains(&format!("InjectedMutationFailure {{ count: {index} }}")),
            "unexpected failure at mutation {index}: {detail}"
        );
        assert_eq!(count, index, "injection must fire exactly once");
        if after == before {
            assert!(
                transactional.is_none(),
                "mutation {index} rolled back after the recovery transaction committed"
            );
        } else {
            let committed = *transactional.get_or_insert(index - 1);
            assert!(
                committed >= 4,
                "recovery must close, publish, swap generation and refresh the session"
            );
            assert_ne!(after["AgentSession"], before["AgentSession"]);
            assert!(
                child_links(&after).iter().any(|linked| !linked),
                "mutation {index}: a failed relationship phase must leave a recoverable child"
            );
            let (replay, writes) = ConfigApplyTxn::with_successful_mutation_failure_at(
                None,
                recover(node, lifecycle, observed, expiry, now),
            )
            .await;
            assert_eq!(replay.unwrap(), RecoveryResult::Won { published: 1 });
            assert!(writes > 0, "mutation {index}: replay must link the pending children");
            let recovered = recovery_snapshot(node, lifecycle, doc_id).await;
            assert!(child_links(&recovered).iter().all(|linked| *linked));
            assert_eq!(child_ids(&recovered), child_ids(&after), "mutation {index}: replay adds no child");
            assert_eq!(recovered["AgentRequest"], after["AgentRequest"]);
            assert_eq!(recovered["AgentSession"], after["AgentSession"]);
            let (node, path, ..) = std::mem::replace(
                &mut current,
                recovery_receipt_fixture(&format!("recovery-mutation-rollback-{index}")).await,
            );
            node.shutdown().await.expect("shut down test node");
            drop(path);
        }
        index += 1;
    }
    assert!(
        transactional.is_some(),
        "recovery must publish its children's session parent relationships"
    );
    let (node, path, ..) = current;
    node.shutdown().await.expect("shut down test node");
    drop(path);
}

#[tokio::test]
async fn post_commit_receipt_loss_replays_exact_recovered_generation_and_selection() {
    use crate::request::lifecycle::{
        recover_expired_generation_with_access_for_test, RecoveryResult,
    };
    use crate::store::config_client::ConfigApplyTxn;
    use gents_protocol::output::{MessagePublication, TerminalOutput};

    let (node, path, lifecycle, _writer, observed, expiry) =
        recovery_receipt_fixture("recovery-receipt-loss").await;
    let access = fixture_access(&node, &lifecycle);
    let request_doc_id = observed.doc_id.as_deref().unwrap();
    let old_generation = observed.execution_generation.as_deref().unwrap();
    let fresh_generation = "recovery-receipt-loss-generation";
    assert_ne!(old_generation, fresh_generation);
    let observed_now = chrono::Utc::now();
    let request = crate::store::graphql::escape_graphql_string(request_doc_id);
    let query = format!(
        r#"{{
            AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}, order: {{ ordinal: ASC }}) {{ _docID ordinal source writer runs payload close created_at }}
            AgentMessage(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ _docID message_key sequence publication outcome role blocks created_at }}
            AgentToolCall(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ _docID lifecycle_state }}
            AgentRequest(filter: {{ _docID: {{ _eq: "{request}" }} }}) {{ {} lifecycle_state terminal_output failure_reason terminalized_at }}
            AgentSession {{ _docID observation }}
        }}"#,
        gents_protocol::row::AGENT_REQUEST_FIELDS,
    );
    let snapshot = || async {
        fixture_query(&fixture_access(&node, &lifecycle), &query)
            .await
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
        recover_expired_generation_with_access_for_test(
            &access,
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
    node.shutdown().await.expect("shut down test node");
    drop(path);
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
    super::canonical::append_provider_segment_at_with_access(
        &fixture_access(&node, &lifecycle),
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
    let error = super::canonical::publish_provider_turn_at_with_access(
        &fixture_access(&node, &lifecycle),
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
    assert!(format!("{error:#}").contains("closure timestamp precedes"));
    let rows = fixture_query(&fixture_access(&node, &lifecycle), &format!(
        r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID }} AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID close }} }}"#,
        crate::store::graphql::escape_graphql_string(&lifecycle.request().doc_id),
        crate::store::graphql::escape_graphql_string(&lifecycle.request().doc_id),
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
    node.shutdown().await.expect("shut down test node");
    drop(path);
}

#[tokio::test]
async fn multiple_flushes_close_and_reconstruct_exact_final_message() {
    let (node, path, lifecycle, writer) = fixture("multi-flush").await;
    let lease_query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID lifecycle_state execution_generation execution_lease_expires_at }} }}"#,
        crate::store::graphql::escape_graphql_string(&lifecycle.request().doc_id)
    );
    let before_flushes = fixture_query(&fixture_access(&node, &lifecycle), &lease_query).await;
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
    let after_flushes = fixture_query(&fixture_access(&node, &lifecycle), &lease_query).await;
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
        &fixture_access(&node, &lifecycle),
        &published.message_doc_id,
        &lifecycle.request().node_did,
        lifecycle.request().requester_did.as_deref(),
    )
    .await
    .unwrap();
    assert_eq!(reconstructed, expected);
    let rows = fixture_query(&fixture_access(&node, &lifecycle), &format!(r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ ordinal close }} }}"#, crate::store::graphql::escape_graphql_string(&lifecycle.request().doc_id))).await;
    let rows = rows.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap();
    assert_eq!(
        rows.len(),
        3,
        "two immutable flushes plus terminal-only closure"
    );
    node.shutdown().await.expect("shut down test node");
    drop(path);
}

#[tokio::test]
async fn published_turn_stays_nonterminal_until_request_selection_commits() {
    use crate::request::lifecycle::{RequestTerminalOutcome, TerminalizeResult};
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
        crate::store::graphql::escape_graphql_string(&lifecycle.request().doc_id),
        gents_protocol::row::AGENT_REQUEST_FIELDS,
    );
    let access = fixture_access(&node, &lifecycle);
    writer
        .flush_native_partial(&lifecycle, &expected)
        .await
        .unwrap();
    let live_request = crate::store::graphql::first_row(
        &fixture_query(&fixture_access(&node, &lifecycle), &query).await,
        "AgentRequest",
    )
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
        let row: gents_protocol::row::AgentRequestRow = crate::store::graphql::first_row(
            &fixture_query(&fixture_access(&node, &lifecycle), &query).await,
            "AgentRequest",
        )
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
    node.shutdown().await.expect("shut down test node");
    drop(path);
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
            crate::store::graphql::escape_graphql_string(&lifecycle.request().doc_id),
        );
        let before = fixture_query(&fixture_access(&node, &lifecycle), &query).await;
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
            &fixture_access(&node, &lifecycle),
            &published.message_doc_id,
            &lifecycle.request().node_did,
            lifecycle.request().requester_did.as_deref(),
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
        let after = fixture_query(&fixture_access(&node, &lifecycle), &query).await;
        assert!(!after.has_errors(), "{:?}", after.errors);
        assert_eq!(
            after.data, before.data,
            "lost-ack replay must not append a closing record"
        );
        node.shutdown().await.expect("shut down test node");
        drop(path);
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
    canonical::append_provider_segment_with_signer(
        &node,
        lifecycle.actor_did().unwrap(),
        lifecycle.managed_agent_signer(),
        lifecycle.execution_generation().unwrap(),
        &segment,
    )
    .await
    .unwrap();
    tail.streams = encoded.streams;
    tail.next_ordinal = 1;
    drop(tails);

    let published = publication
        .await
        .expect("publication must timestamp after the earlier flusher");
    let (_, observed) = crate::session::load_canonical_message(
        &fixture_access(&node, &lifecycle),
        &published.message_doc_id,
        &request.node_did,
        request.requester_did.as_deref(),
    )
    .await
    .unwrap();
    assert_eq!(observed, message);
    node.shutdown().await.expect("shut down test node");
    drop(path);
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
    let response = fixture_query(
        &fixture_access(&node, &lifecycle),
        &format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            crate::store::graphql::escape_graphql_string(&lifecycle.request().doc_id),
            gents_protocol::output::rows::AGENT_OUTPUT_SEGMENT_FIELDS
        ),
    )
    .await;
    let rows = response.data.as_ref().unwrap()["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(gents_protocol::output::rows::decode_output_segment_row)
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
        &fixture_access(&node, &lifecycle),
        &message_doc_id,
        &lifecycle.request().node_did,
        lifecycle.request().requester_did.as_deref(),
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
    fixture_insert_child(
        &fixture_access(&node, &lifecycle),
        &lifecycle,
        gents_protocol::output::rows::CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
        &gents_protocol::output::rows::output_segment_create_variables(&late).unwrap(),
        "AgentOutputSegment",
    )
    .await;
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
    let replay = fixture_query(&fixture_access(&node, &lifecycle), &format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID sequence }} }}"#,
            crate::store::graphql::escape_graphql_string(&lifecycle.request().doc_id)
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
    let escaped_message = crate::store::graphql::escape_graphql_string(&message_doc_id);
    crate::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(&lifecycle.request().node_did).unwrap(),
    )
    .grant_node_permission(
        &lifecycle.actor_did().unwrap(),
        crate::store::config_client::NodePermission::DocumentDelete,
    )
    .await
    .unwrap();
    // The Agent created the header, so its own signed access removes it.
    let deleted = fixture_access(&node, &lifecycle)
    .write("test.remove_imported_header", &format!(
            r#"mutation {{ delete_AgentMessage(filter: {{ _docID: {{ _eq: "{escaped_message}" }} }}) {{ _docID }} }}"#
        ))
        .await.unwrap();
    assert_eq!(
        deleted["data"]["delete_AgentMessage"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let escaped_request = crate::store::graphql::escape_graphql_string(&lifecycle.request().doc_id);
    let expired = fixture_access(&node, &lifecycle).write("test.expire_imported_partial", &format!(
        r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_request}" }} }}, input: {{ execution_lease_expires_at: "2020-01-01T00:00:00Z" }}) {{ _docID }} }}"#
    )).await.unwrap();
    assert_eq!(
        expired["data"]["update_AgentRequest"]
            .as_array()
            .unwrap()
            .len(),
        1
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
    let after_missing = fixture_query(&fixture_access(&node, &lifecycle), &format!(
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
    node.shutdown().await.expect("shut down test node");
    drop(path);
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
        request_id: None,
        id: Some("provider-message".into()),
        content: vec![
            AssistantContent::ToolCall(ToolCall {
                raw_arguments: None,
                id: "native-tool".into(),
                call_id: Some("provider-call".into()),
                function: ToolFunction::new("read".into(), serde_json::json!({"path":"文.txt"})),
                signature: Some("sig".into()),
                additional_params: None,
            }),
            AssistantContent::ToolCall(ToolCall {
                raw_arguments: None,
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
    let tool_rows = fixture_query(&fixture_access(&node, &lifecycle), &format!(
            r#"{{ AgentToolCall(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID tool_call_id }} }}"#,
            crate::store::graphql::escape_graphql_string(&lifecycle.request().doc_id),
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
    node.shutdown().await.expect("shut down test node");
    drop(path);
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
        request_id: None,
        id: Some("provider-message".into()),
        content: vec![AssistantContent::ToolCall(ToolCall {
            raw_arguments: None,
            id: "native-tool".into(),
            call_id: Some("provider-call".into()),
            function: ToolFunction::new("read".into(), serde_json::json!({"path":"文.txt"})),
            signature: Some("sig".into()),
            additional_params: None,
        })],
    };
    let (first, fault_fired) =
        crate::store::config_client::ConfigApplyTxn::with_post_commit_receipt_loss(
            writer.publish_native_turn(&lifecycle, 0, 0, &message),
        )
        .await;
    let first = first.expect("idempotent owner must reconstruct the lost receipt");
    assert!(fault_fired, "post-commit receipt-loss fault must fire");
    let request = crate::store::graphql::escape_graphql_string(&lifecycle.request().doc_id);
    let query = format!(
        r#"{{
            AgentMessage(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ _docID sequence }}
            AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}, order: {{ ordinal: ASC }}) {{ _docID ordinal close }}
            AgentToolCall(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ _docID lifecycle_state }}
        }}"#
    );
    let before = fixture_query(&fixture_access(&node, &lifecycle), &query).await;
    let replay = writer
        .publish_native_turn(&lifecycle, 0, 0, &message)
        .await
        .expect("external replay resolves committed publication");
    let replayed = fixture_query(&fixture_access(&node, &lifecycle), &query).await;
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
    let mut first_dispatch = crate::tools::call::lifecycle::ToolCallLifecycle::from_accepted(
        node.clone(),
        lifecycle.request().node_did.clone(),
        lifecycle.request().requester_did.clone(),
        a.clone(),
        deadline,
        gents_protocol::tool_call::AwaitMode::Foreground,
    )
    .unwrap();
    let mut replay_dispatch = crate::tools::call::lifecycle::ToolCallLifecycle::from_accepted(
        node.clone(),
        lifecycle.request().node_did.clone(),
        lifecycle.request().requester_did.clone(),
        b.clone(),
        deadline,
        gents_protocol::tool_call::AwaitMode::Foreground,
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
        format!("{loser_error:#}").contains("no longer pending"),
        "the losing handle must be rejected by the physical pending-row fence: {loser_error:#}"
    );
    let dispatched = fixture_query(&fixture_access(&node, &lifecycle), &query).await;
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
    let completed = fixture_query(&fixture_access(&node, &lifecycle), &query).await;
    let completed_tools = completed.data.as_ref().unwrap()["AgentToolCall"]
        .as_array()
        .unwrap();
    assert_eq!(completed_tools.len(), 1);
    assert_eq!(completed_tools[0]["_docID"], a.tool_call_doc_id);
    assert_eq!(completed_tools[0]["lifecycle_state"], "completed");
    let output = crate::tools::background::status::observe_canonical_tool_output_with_access(
        &fixture_access(&node, &lifecycle),
        &a.tool_call_doc_id,
        &lifecycle.request().doc_id,
        &lifecycle.request().session_id,
        &lifecycle.request().node_did,
        lifecycle.request().requester_did.as_deref(),
    )
    .await
    .unwrap();
    assert_eq!(output.into_text(), "one receipt-loss dispatch result");
    node.shutdown().await.expect("shut down test node");
    drop(path);
}

#[tokio::test]
async fn modeled_inputs_survive_publication_replay_and_forks() {
    use gents_protocol::request_input::RequestInput;
    const SESSION: &str = "captured-documents";
    let (node, path, mut lifecycle, writer) = fixture_with_ids(
        "canonical-captured-documents",
        "captured-documents",
        SESSION,
    )
    .await;
    let access = fixture_access(&node, &lifecycle);
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
        let (_, reconstructed) = crate::session::load_canonical_message(
            &access,
            &first,
            &lifecycle.request().node_did,
            lifecycle.request().requester_did.as_deref(),
        )
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
            crate::request::lifecycle::RequestTerminalOutcome::Failed,
            gents_protocol::output::TerminalOutput::NoMessage,
            Some("fixture finished"),
        )
        .await
        .unwrap();
    let before = crate::session::load_history_with_access(
        &path.owner,
        SESSION,
        &lifecycle.request().node_did,
        lifecycle.request().requester_did.as_deref(),
    )
    .await
    .unwrap();
    assert_eq!(before, expected);
    let child = crate::session::fork_on(
        &path.owner,
        crate::session::ForkParams {
            source_session_id: SESSION,
            boundary: crate::session::ForkBoundary::UserTurn(
                u32::try_from(expected.len()).unwrap(),
            ),
            caller_node_did: &lifecycle.request().node_did,
            caller_requester_did: lifecycle.request().requester_did.as_deref(),
            target_agent_did: None,
        },
    )
    .await
    .unwrap();
    let forked = crate::session::load_history_with_access(
        &path.owner,
        &child.session_id,
        &lifecycle.request().node_did,
        lifecycle.request().requester_did.as_deref(),
    )
    .await
    .unwrap();
    assert_eq!(
        forked, before,
        "fork retains the original payload and capture metadata"
    );
    assert_eq!(
        crate::session::load_history_with_access(
            &path.owner,
            SESSION,
            &lifecycle.request().node_did,
            lifecycle.request().requester_did.as_deref()
        )
        .await
        .unwrap(),
        before
    );
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn cancelled_flush_replays_before_inference_close_or_publication() {
    use gents_protocol::output::rows::AGENT_OUTPUT_SEGMENT_FIELDS;
    use crate::store::config_client::ConfigApplyTxn;
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
        let access = fixture_access(&node, &lifecycle);
        let query = format!(
            "{{AgentOutputSegment(filter: {{request_doc_id: {{_eq: \"{}\"}}}}) {{{AGENT_OUTPUT_SEGMENT_FIELDS}}}}}",
            crate::store::graphql::escape_graphql_string(&request.doc_id),
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
        node.shutdown().await.expect("shut down test node");
        drop(path);
    }
}

#[tokio::test]
async fn dirty_native_flush_completes_before_the_turn_barrier_or_is_recovered_after_stop() {
    use crate::store::config_client::ConfigApplyTxn;
    use futures::StreamExt;
    use gents_protocol::message::Message;
    use std::sync::atomic::{AtomicBool, Ordering};

    for after_commit in [false, true] {
        let (node, path, lifecycle, mut writer) = fixture(if after_commit {
            "handoff-flush-after-commit"
        } else {
            "handoff-flush-before-commit"
        })
        .await;
        writer.batch_interval = Duration::from_secs(60);
        let doc = lifecycle.request().doc_id.clone();
        writer
            .start_provider_attempt(&doc, 0, 0, "inference.1".parse().unwrap())
            .await;
        assert!(writer.write_tokens(&doc, "first").await.unwrap());
        assert!(!writer.write_tokens(&doc, " buffered").await.unwrap());
        assert!(writer.next_flush_deadline(&doc).await.is_some());
        let message = Message::assistant("first buffered");
        let reached = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let hook_started = Arc::new(AtomicBool::new(false));
        let marked = hook_started.clone();
        let mut progress = Box::pin(async_stream::stream! {
            yield crate::runtime::agent::loop_stream::LoopStreamItem::ProviderTurnReady {
                turn: 0, attempt: 0, message: Message::assistant("first buffered"),
            };
            marked.store(true, Ordering::SeqCst);
            yield crate::runtime::agent::loop_stream::LoopStreamItem::Final { text: "done".into() };
        });
        let flush = async {
            if after_commit {
                ConfigApplyTxn::with_embedded_commit_ack_pause(
                    reached.clone(),
                    writer.flush_native_partial(&lifecycle, &message),
                )
                .await
            } else {
                ConfigApplyTxn::with_successful_mutation_pause_at(
                    1,
                    reached.clone(),
                    release.clone(),
                    writer.flush_native_partial(&lifecycle, &message),
                )
                .await
                .0
            }
        };
        let mut combined = Box::pin(gents_loop::stream_processor::flush_while_polling(
            flush,
            progress.next(),
        ));
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                () = reached.notified() => {},
                _ = &mut combined => panic!("flush skipped its selected boundary"),
            }
        })
        .await
        .expect("flush reaches the selected transaction boundary");
        assert!(
            !hook_started.load(Ordering::SeqCst),
            "the next call stays behind the held flush"
        );
        if after_commit {
            drop(combined);
            drop(progress);
            assert!(!hook_started.load(Ordering::SeqCst));
            writer
                .flush_native_partial(&lifecycle, &message)
                .await
                .expect("reconcile the exact pending flush after its unconfirmed acknowledgement");
            let rows = fixture_access(&node, &lifecycle).execute(&format!(
                r#"{{ AgentOutputSegment(filter: {{request_doc_id: {{_eq: "{}"}}}}) {{_docID ordinal}} }}"#,
                crate::store::graphql::escape_graphql_string(&doc),
            )).await.unwrap();
            assert_eq!(
                rows["data"]["AgentOutputSegment"].as_array().unwrap().len(),
                1
            );
        } else {
            release.notify_one();
            let (result, item) = combined.await;
            result.expect("flush commits");
            assert!(matches!(
                item,
                Some(Some(
                    crate::runtime::agent::loop_stream::LoopStreamItem::ProviderTurnReady { .. }
                ))
            ));
            writer
                .publish_native_turn(&lifecycle, 0, 0, &message)
                .await
                .expect("publish accepted turn");
            writer.flush_pending(&doc).await.unwrap();
            assert!(writer.next_flush_deadline(&doc).await.is_none());
            let _ = progress.next().await;
            assert!(hook_started.load(Ordering::SeqCst));
        }
        node.shutdown().await.expect("node shutdown");
        drop(path);
    }
}
