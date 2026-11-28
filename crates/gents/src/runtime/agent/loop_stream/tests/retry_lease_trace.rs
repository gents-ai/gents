use super::*;
use gents_inference::admission::{
    AdmissionRegistry, AdmittedCompletionModel, BackendAdmissionConfig,
};
use gents_inference::clients::provider_http::ProviderHttpClient;
use gents_inference::llm::rig_compat::providers::build_openai_chat_completions_client;
use gents_inference::test_support::scripted_backend::{
    ScriptedBackend, StreamPlan, StreamResponse,
};
use gents_lean_vocab::tla::replay::{Red, Trace};
use gents_loop::provider_call::admission::AdmissionCallContext;
use gents_loop::provider_input::ProviderInputProfile;
use gents_loop::stream_processor::StreamProcessor;
use gents_protocol::document_query::{
    fields::agent_request as fields, Filter, Input, Query, Selection, Update,
};
use gents_session::request::lifecycle::renew_execution_lease_once_at_with_access;
use gents_store::test_support::UNREACHED_DEADLINE;
use rig::client::CompletionClient as _;

async fn replay(trace: Trace) {
    let (db, hook, writer, mut lifecycle) = owned_test_hook().await;
    let access = lifecycle.access().unwrap();
    let doc = lifecycle.request().doc_id.clone();
    let generation = lifecycle.execution_generation().unwrap().to_owned();
    let selection = Query::select(
        Selection::new()
            .field(fields::EXECUTION_LEASE_EXPIRES_AT)
            .field(fields::EXECUTION_LEASE_SECS)
            .field(fields::EXECUTION_GENERATION),
    )
    .filter(Filter::eq(fields::DOC_ID, &doc))
    .render();
    let read =
        || async { access.read(&selection).await.unwrap()["data"]["AgentRequest"][0].clone() };
    let init = read().await;
    let initial_deadline: chrono::DateTime<chrono::Utc> = init["execution_lease_expires_at"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let lease = chrono::TimeDelta::seconds(init["execution_lease_secs"].as_i64().unwrap());
    let tick = lease / i32::try_from(trace.constants["Lease"].as_u64().unwrap()).unwrap();
    let mut clock = initial_deadline - lease;
    let backend_id = "trace-retry";
    let marker = "owned loop test";
    let backend = ScriptedBackend::start_with_plans(
        "test-model",
        vec![StreamPlan::new(
            marker,
            vec![
                StreamResponse::service_unavailable("HTTP status 503 at the first provider call"),
                StreamResponse::completes(marker, ["recovered"]),
            ],
        )],
    )
    .unwrap();
    let registry = AdmissionRegistry::new(db.node.clone());
    registry.reconcile(
        1,
        &[(
            backend_id.into(),
            BackendAdmissionConfig {
                backend_id: backend_id.into(),
                max_concurrent: 1,
                max_queue_depth: 1,
                enabled: true,
                measured_unhealthy: false,
                probe_status: gents_inference::backends::registry::HEALTHY_PROBE_STATUS.into(),
                config_fingerprint: "trace-retry".into(),
            },
        )]
        .into_iter()
        .collect(),
    );
    let client = build_openai_chat_completions_client(
        "test",
        backend.endpoint(),
        ProviderHttpClient::default(),
    )
    .unwrap();
    let mut model = AdmittedCompletionModel::new(
        client.completion_model("test-model"),
        registry.clone(),
        "trace-retry",
        UNREACHED_DEADLINE,
    );
    model.agent_signer = lifecycle.managed_agent_signer();
    let mut config = owned_config_for(1, &lifecycle);
    config.retry_policy.transport_backoff = vec![Duration::ZERO];
    config.provider_call.admission = Some(AdmissionCallContext::for_request(
        lifecycle.request(),
        &lifecycle.request().agent_did,
        backend_id,
    ));
    let request_id = gents_store::graphql::escape_graphql_string(&lifecycle.request().request_id);
    let query = format!(
        r#"{{InferenceCall(filter:{{request_id:{{_eq:"{request_id}"}}}},order:{{call_seq:ASC}}){{call_state}}}}"#
    );
    let mut stream = Box::pin(run_loop_stream(
        model,
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user(marker)),
        Vec::new(),
        Arc::new(Vec::new()),
        config,
    ));
    let mut processor = StreamProcessor::new(
        &hook,
        &writer,
        &mut lifecycle,
        &doc,
        ProviderInputProfile::OpenAiChatCompletions,
    );
    let mut observation = None;
    let mut write_landed = None;
    let mut dispatch_live = None;
    for step in &trace.steps {
        match step.action.as_str() {
            "Tick" => clock += tick,
            "Renew" => {
                let deadline = read().await["execution_lease_expires_at"]
                    .as_str()
                    .unwrap()
                    .parse()
                    .unwrap();
                renew_execution_lease_once_at_with_access(
                    &access,
                    &doc,
                    &generation,
                    deadline,
                    clock,
                )
                .await
                .unwrap();
            }
            "FirstCallFails" => {
                loop {
                    let item = stream.next().await.expect("first provider call").unwrap();
                    let failed = matches!(
                        item,
                        LoopStreamItem::AttemptFailed {
                            will_retry: true,
                            ..
                        }
                    );
                    processor.process_item(Ok(item)).await.unwrap();
                    if failed {
                        break;
                    }
                }
                let rows = access.execute(&query).await.unwrap();
                assert_eq!(
                    rows["data"]["InferenceCall"],
                    serde_json::json!([{"call_state":"failed"}])
                );
                assert_eq!(backend.observed_requests(marker), 1);
            }
            "DrivePollStep" => processor.validate_execution().await.unwrap(),
            "FixtureRead" => observation = Some(read().await),
            "FixtureWrite" => {
                let observed = observation.take().unwrap();
                let expired =
                    (chrono::Utc::now() - chrono::TimeDelta::milliseconds(1)).to_rfc3339();
                if trace.config == "MCRetryBackoffLeaseDeadlineCas" {
                    assert_ne!(
                        observed["execution_lease_expires_at"],
                        read().await["execution_lease_expires_at"]
                    );
                }
                let mutation = Update::doc(
                    &doc,
                    Input::new().set(fields::EXECUTION_LEASE_EXPIRES_AT, expired.as_str()),
                )
                .filter(Filter::eq(fields::DOC_ID, &doc).and(Filter::eq(
                    fields::EXECUTION_GENERATION,
                    observed["execution_generation"].as_str().unwrap(),
                )))
                .render();
                let data = access
                    .write_rendered("test.retry_trace.expire", &mutation)
                    .await
                    .unwrap();
                write_landed = Some(gents_store::graphql::response_has_documents(
                    &data["data"]["update_AgentRequest"],
                ));
            }
            "Wake" => {
                let wake = stream.next().await.unwrap().unwrap();
                assert!(matches!(wake, LoopStreamItem::RetryWake));
                let live = processor.validate_execution().await.is_ok();
                if processor.process_item(Ok(wake)).await.is_ok() {
                    while backend.observed_requests(marker) < 2 {
                        let item = stream
                            .next()
                            .await
                            .expect("authorized second provider call");
                        processor.process_item(item).await.unwrap();
                    }
                    dispatch_live = Some(live);
                }
            }
            "Complete" => {
                while let Some(item) = stream.next().await {
                    processor.process_item(item).await.unwrap();
                }
                assert_eq!(backend.observed_requests(marker), 2);
            }
            action => panic!("unmapped retry step {action}"),
        }
    }
    let dispatched = backend.observed_requests(marker) == 2;
    let wall = trace.config == "MCRetryBackoffLeaseWallClock";
    let mut props = vec![
        ("FixtureExpires", write_landed != Some(false)),
        (
            "DispatchOnlyLive",
            !dispatched || dispatch_live == Some(true),
        ),
    ];
    if wall {
        props.push((
            "NoRetryAfterExpiry",
            !(write_landed == Some(true) && dispatched),
        ));
    }
    trace.check(if wall { Red::Premise } else { Red::Defect }, &props);
    drop(stream);
    registry.drop_cleanup_idle().await;
    db.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn retry_backoff_lease_traces() {
    for json in [
        include_str!(
            "../../../../../proofs/tla/generated/traces/MCRetryBackoffLeaseFenced.sample-1.json"
        ),
        include_str!("../../../../../proofs/tla/generated/traces/MCRetryBackoffLeaseUnfenced.json"),
        include_str!(
            "../../../../../proofs/tla/generated/traces/MCRetryBackoffLeaseDeadlineCas.json"
        ),
        include_str!(
            "../../../../../proofs/tla/generated/traces/MCRetryBackoffLeaseWallClock.json"
        ),
    ] {
        replay(Trace::load(json, "RetryBackoffLease", &[])).await;
    }
}
