#[tokio::test(start_paused = true)]
async fn generated_rendered_capture_cases_fence_persist_before_send() {
    let cases = gents_lean_vocab::lean_rendered_capture_cases();
    assert!(!cases.is_empty(), "Lean emitted no rendered-capture cases");

    for case in cases {
        let outcomes = Arc::new(Mutex::new(Vec::<String>::new()));

        let outcomes_for_sink = outcomes.clone();
        let expected_outcome = case.capture_outcome.clone();
        let mut loop_config = config(0);
        loop_config.on_rendered_request =
            Some(Arc::new(move |_turn_index, _attempt, _request, _trace| {
                let outcome = expected_outcome.clone();
                let outcomes = outcomes_for_sink.clone();
                Box::pin(async move {
                    outcomes.lock().await.push(outcome.clone());
                    if outcome == "rejected" {
                        Err(gents_loop::rendered_request::RenderedRequestError::Sink(
                            "capture key already names a different canonical request".into(),
                        ))
                    } else {
                        Ok(())
                    }
                })
            }));

        let model = ScriptedModel::new(vec![
            RawStreamingChoice::Message("ok".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ]);
        let stream = run_loop_stream(
            model.clone(),
            None::<gents_toolcalls::call::hook::DefraSessionHook>,
            TaggedMessage::unassociated(Message::user("hi")),
            Vec::new(),
            Arc::new(Vec::new()),
            loop_config,
        );
        let collected = collect_scripted_stream(stream).await;

        assert_eq!(
            outcomes.lock().await.as_slice(),
            &[case.capture_outcome.as_str()],
            "{}: the loop must invoke the configured capture sink",
            case.name
        );
        assert_eq!(
            model.seen_histories().await.len(),
            case.provider_requests_observed,
            "{}: the provider observed a different number of requests than the \
             modeled trace permits (expected final stage {})",
            case.name,
            case.final_stage
        );

        if case.send_permitted {
            assert!(
                collected.error.is_none(),
                "{}: a durable capture must not fail the turn: {:?}",
                case.name,
                collected.error
            );
        } else {
            let error = collected
                .error
                .as_deref()
                .unwrap_or_else(|| panic!("{}: capture failure must be terminal", case.name));
            assert!(
                error.contains("capturing rendered completion request failed"),
                "{}: unexpected terminal error: {error}",
                case.name
            );
        }
    }
}

#[tokio::test]
async fn generated_rendered_capture_cases_hold_against_the_real_defra_sink() {
    use gents_inference::rendered_request::capture_key as derive_capture_key;
    use gents_inference::rendered_request::{
        DefraRenderedRequestSink, ProvenanceManifest, RenderedRequestSource, CAPTURE_VERSION,
    };
    use gents_store::graphql::escape_graphql_string;

    let cases = gents_lean_vocab::lean_rendered_capture_cases();
    assert!(!cases.is_empty(), "Lean emitted no rendered-capture cases");

    // Captures are owned by the capturing Agent, so they are read as it.
    async fn rendered_rows(
        reader: &gents_store::config_client::ConfigAccess,
        capture_key: &str,
    ) -> Vec<serde_json::Value> {
        let query = format!(
            r#"{{ RenderedRequest(filter: {{ capture_key: {{ _eq: "{}" }} }}) {{ capture_key capture_version request_json }} }}"#,
            escape_graphql_string(capture_key),
        );
        agent_query(reader, &query).await["data"]["RenderedRequest"]
            .as_array()
            .cloned()
            .expect("RenderedRequest row array")
    }

    for case in cases {
        // Each case runs under its own held node root, whose managed Agent signs
        // the captures. The node DID, Agent and session are real, so the sink's
        // signer and session-binding checks run unchanged.
        let db = gents_inference::test_support::test_db(
            "capture-case",
            gents_automation::lifecycle_hooks::composition(),
        )
        .await;
        let root = db.node_identity.clone();
        // The Lean node witness only distinguishes cases; each case's held root
        // already gives it a distinct, real node DID.
        let _ = case.node_did;
        let node_did = root.did().to_owned();
        let node = db.node.clone();
        let binding = gents_store::identity::provision_agent_identity(root.as_ref()).unwrap();
        let agent = gents_store::identity::resolve_agent_identity(root.as_ref(), &binding).unwrap();
        let agent_did = binding.agent_did.clone();
        gents_inference::test_support::install_test_agent_binding(
            &node,
            &node_did,
            "Capture test agent",
            &binding,
        )
        .await;
        let session_id = format!("session-{}", case.session_id);
        // The Agent creates the session it later captures into, so its own
        // reads see the physical row (a node-created row is not visible to it).
        let agent_access = gents_store::config_client::ConfigAccess::local_managed_agent(
            node.clone(),
            ::identity::Did::new(&agent_did).unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        let session_created_at =
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        agent_access
            .transact_write("test.capture.session", |txn| {
                Box::pin(async {
                    gents_session::session::ensure_session_in_txn(
                        txn,
                        &session_id,
                        &node_did,
                        &agent_did,
                        None,
                        None,
                        None,
                        &session_created_at,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
        let sink = DefraRenderedRequestSink::new_managed(node.clone(), agent.as_ref()).unwrap();

        let rendered_for_witness =
            |witness: u64| -> gents_inference::rendered_request::RenderedCompletionRequest {
                let node_did = node_did.clone();
                let session_id = session_id.clone();
                let request_doc_id = format!("bae-request-doc-{}", case.request_id);
                let assembly_trace =
                    gents_inference::rendered_request::AssemblyTrace::from_effective_messages(
                        gents_inference::rendered_request::AssemblyBuildPath::Budgeted,
                        Vec::new(),
                    );
                gents_inference::rendered_request::RenderedCompletionRequest {
                    capture_key: derive_capture_key(
                        &node_did,
                        &session_id,
                        &request_doc_id,
                        CAPTURE_SCOPE_SINK,
                        case.turn_index,
                        case.attempt,
                    )
                    .expect("capture key"),
                    capture_version: CAPTURE_VERSION,
                    request_doc_id: request_doc_id.clone(),
                    request_commit_cid: format!("bafy-commit-{}", case.request_id),
                    request_id: format!("req-sink-{}", case.request_id),
                    capture_scope: CAPTURE_SCOPE_SINK.to_string(),
                    turn_index: case.turn_index,
                    attempt: case.attempt,
                    node_did,
                    requester_did: String::new(),
                    agent_did: agent_did.clone(),
                    session_id,
                    model_name: "sink-model".to_string(),
                    source: RenderedRequestSource::OpenAiChatCompletions,
                    request_json: serde_json::json!({"witness": witness}),
                    messages_json: serde_json::json!([]),
                    tools_json: serde_json::json!([]),
                    tool_choice_json: serde_json::Value::Null,
                    sampling_json: serde_json::Value::Null,
                    provenance_json: serde_json::to_value(ProvenanceManifest::captured_only(
                        CAPTURE_SCOPE_SINK.to_string(),
                        None,
                        None,
                        assembly_trace.clone(),
                    ))
                    .expect("provenance manifest"),
                    provenance_payload_json: serde_json::to_value(&assembly_trace)
                        .expect("provenance payload"),
                    assembly_trace,
                }
            };

        if let Some(prior) = case.prior_binding {
            sink.capture(rendered_for_witness(prior))
                .await
                .expect("seeding the prior binding must be a fresh capture");
        }

        let delivery = rendered_for_witness(case.request);
        let delivery_key = delivery.capture_key.clone();
        let captured = sink.capture(delivery).await;
        match case.capture_outcome.as_str() {
            "fresh" | "idempotent" => captured.expect("capture must succeed"),
            "rejected" => {
                let error = captured.expect_err("a rebound key must be rejected");
                assert!(
                    error.to_string().contains("integrity violation"),
                    "{}: unexpected error {error:#}",
                    case.name
                );
            }
            other => panic!("{}: unknown capture outcome {other}", case.name),
        }
        let rows = rendered_rows(&agent_access, &delivery_key).await;
        assert_eq!(
            rows.len(),
            usize::from(case.durable_after.is_some()),
            "{}",
            case.name
        );
        let stored = if let Some(row) = rows.first() {
            let version = row["capture_version"]
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .expect("numeric capture version");
            Some(
                gents_inference::rendered_request::decode_capture_json(
                    &agent_access,
                    version,
                    row["request_json"].as_str().expect("request_json string"),
                    gents_inference::rendered_request::CapturePayloadKind::RequestBody,
                )
                .await
                .expect("stored request_json decodes"),
            )
        } else {
            None
        };
        assert_eq!(
            stored,
            case.durable_after
                .map(|witness| serde_json::json!({"witness": witness})),
            "{}: the durable binding must match the capture contract",
            case.name
        );
        node.shutdown().await.expect("node shutdown");
    }
}

#[tokio::test]
async fn codex_affinity_survives_retry_tool_continuation_and_nested_compaction() {
    use gents_loop::provider_input::routing_affinity::current;
    let (_db, hook, writer, mut lifecycle) = owned_test_hook().await;
    let session = hook.session_id().await.unwrap();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen_for_observer = seen.clone();
    let expected_session = session.clone();
    let model = ScriptedModel::new_calls(vec![
        ScriptedCall::FailStream(transient_provider_error("affinity retry")),
        ScriptedCall::Turn(vec![
            RawStreamingChoice::ToolCall(
                RawStreamingToolCall::new("fc-1".into(), "echo".into(), serde_json::json!({}))
                    .with_call_id("call-1".into()),
            ),
            RawStreamingChoice::FinalResponse(()),
        ]),
        ScriptedCall::Turn(vec![
            RawStreamingChoice::Message("done".into()),
            RawStreamingChoice::FinalResponse(()),
        ]),
    ])
    .with_provider_observer(Arc::new(move |dispatch| {
        let affinity = current().expect("main provider poll has execution affinity");
        assert_eq!(affinity.session(), expected_session);
        if dispatch {
            seen_for_observer
                .lock()
                .unwrap()
                .push(affinity.token().map(str::to_owned));
            affinity.observe(Some("first"));
        } else {
            assert_eq!(affinity.token(), Some("first"));
        }
    }));
    let mut cfg = owned_config(4);
    cfg.provider_input_counter = Arc::new(crate::turn::provider_input::ProviderInputCounter::new(
        gents_inference::backends::provider::BackendProviderKind::ChatGptCodex,
        gents_loop::openai_wire::OpenAiWireApi::Responses,
        "gpt-5-codex",
    ));
    cfg.max_tokens = Some(6_000);
    cfg.context_window = 6_500;
    cfg.compaction_threshold = 0.25;
    let auxiliary_counter = cfg.provider_input_counter.clone();
    cfg.turn_compactor = Some(Arc::new(move |_| {
        let counter = auxiliary_counter.clone();
        Box::pin(async move {
            assert!(
                current().is_none(),
                "compaction must not inherit main affinity"
            );
            let model = ScriptedModel::new(vec![
                RawStreamingChoice::Message("summary".into()),
                RawStreamingChoice::FinalResponse(()),
            ])
            .without_capture()
            .with_provider_observer(Arc::new(|_| assert!(current().is_none())));
            let mut auxiliary_config = config(0);
            auxiliary_config.provider_input_counter = counter;
            let auxiliary = run_loop_stream(
                model,
                None::<gents_loop::session_hook::NoopSessionHook>,
                TaggedMessage::unassociated(Message::user("summarize")),
                Vec::new(),
                Arc::new(Vec::new()),
                auxiliary_config,
            );
            futures::pin_mut!(auxiliary);
            while let Some(item) = auxiliary.next().await {
                item.unwrap();
            }
            Ok(TurnCompactionOutcome::Reduced {
                messages: vec![TaggedMessage::unassociated(Message::user(
                    "compacted prompt",
                ))],
                reduction_key: "affinity-compaction".into(),
                commit: TurnReductionCommit::new(|| async { Ok(()) }),
            })
        })
    }));
    let stream = run_loop_stream(
        model.clone(),
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user("x".repeat(8_000))),
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        cfg,
    );
    let collected = collect_owned_scripted_stream(
        stream,
        &hook,
        &writer,
        &mut lifecycle,
        crate::turn::provider_input::ProviderInputProfile::ChatGptCodexResponses,
    )
    .await;
    assert_eq!(collected.error, None, "{collected:?}");
    assert!(current().is_none());
    let modeled_token = |name: &str| {
        gents_lean_vocab::lean_contract_snapshot()
            .routing_affinity_cases
            .iter()
            .find(|case| case.name == name)
            .unwrap()
            .expected_token
            .clone()
    };
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            modeled_token("initial"),
            modeled_token("first_token_retained"),
            modeled_token("first_token_retained")
        ]
    );
}
