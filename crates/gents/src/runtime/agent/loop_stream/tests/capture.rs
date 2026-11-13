#[tokio::test]
async fn rendered_capture_follows_the_lean_model() {
    use gents_inference::rendered_request::capture_key as derive_capture_key;
    use gents_inference::rendered_request::sink::CaptureOutcome;
    use gents_inference::rendered_request::{
        DefraRenderedRequestSink, ProvenanceManifest, RenderedRequestSource, CAPTURE_VERSION,
    };
    use gents_store::graphql::escape_graphql_string;

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

    for case in gents_lean_vocab::families::contracts_rendered_capture::cases() {
        let input = &case.input;
        let db = gents_inference::test_support::test_db(
            "capture-case",
            gents_automation::lifecycle_hooks::composition(),
        )
        .await;
        let root = db.node_identity.clone();
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
        let session_id = format!("session-{}", input.session_id);
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
        let turn_index = usize::try_from(input.turn_index).expect("turn index fits");
        let attempt = u32::try_from(input.attempt).expect("attempt fits");

        let rendered_for_witness =
            |witness: u64| -> gents_inference::rendered_request::RenderedCompletionRequest {
                let node_did = node_did.clone();
                let session_id = session_id.clone();
                let request_doc_id = format!("bae-request-doc-{}", input.request_id);
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
                        turn_index,
                        attempt,
                    )
                    .expect("capture key"),
                    capture_version: CAPTURE_VERSION,
                    request_doc_id: request_doc_id.clone(),
                    request_commit_cid: format!("bafy-commit-{}", input.request_id),
                    request_id: format!("req-sink-{}", input.request_id),
                    capture_scope: CAPTURE_SCOPE_SINK.to_string(),
                    turn_index,
                    attempt,
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

        if let Some(prior) = input.prior_binding {
            sink.capture(rendered_for_witness(prior))
                .await
                .expect("seeding the prior binding must be a fresh capture");
        }

        let delivery = rendered_for_witness(input.request);
        let delivery_key = delivery.capture_key.clone();
        let captured = Arc::new(Mutex::new(Vec::<Result<CaptureOutcome, String>>::new()));
        let captured_for_sink = captured.clone();
        let sink_for_loop = sink.clone();
        let mut loop_config = config(0);
        loop_config.on_rendered_request =
            Some(Arc::new(move |_turn_index, _attempt, _request, _trace| {
                let sink = sink_for_loop.clone();
                let delivery = delivery.clone();
                let captured = captured_for_sink.clone();
                Box::pin(async move {
                    let result = sink.capture(delivery).await;
                    let recorded = result
                        .as_ref()
                        .copied()
                        .map_err(|error| format!("{error:#}"));
                    captured.lock().await.push(recorded);
                    result.map(|_| ()).map_err(|error| {
                        gents_loop::rendered_request::RenderedRequestError::Sink(error.into())
                    })
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
        futures::pin_mut!(stream);
        let mut terminal_error = None;
        while let Some(item) = stream.next().await {
            if let Err(error) = item {
                terminal_error = Some(error.to_string());
                break;
            }
        }

        let captures = captured.lock().await.clone();
        let [capture] = captures.as_slice() else {
            panic!("the loop must invoke the capture sink once: {captures:?}");
        };
        let capture_outcome = match capture {
            Ok(CaptureOutcome::Fresh) => "fresh",
            Ok(CaptureOutcome::Idempotent) => "idempotent",
            Err(error) if error.contains("integrity violation") => "rejected",
            Err(error) => panic!("capture failed outside the contract: {error}"),
        };
        let send_permitted = match terminal_error.as_deref() {
            None => true,
            Some(error) => {
                assert!(
                    error.contains("capturing rendered completion request failed"),
                    "unexpected terminal error: {error}"
                );
                false
            }
        };
        let provider_requests_observed = model.seen_histories().await.len() as u64;

        let rows = rendered_rows(&agent_access, &delivery_key).await;
        assert!(rows.len() <= 1, "one capture key binds at most one row");
        let mut durable_after = None;
        if let Some(row) = rows.first() {
            let version = row["capture_version"]
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .expect("numeric capture version");
            let stored = gents_inference::rendered_request::decode_capture_json(
                &agent_access,
                version,
                row["request_json"].as_str().expect("request_json string"),
                gents_inference::rendered_request::CapturePayloadKind::RequestBody,
            )
            .await
            .expect("stored request_json decodes");
            let witness = stored["witness"].as_u64().expect("stored witness");
            assert_eq!(stored, serde_json::json!({"witness": witness}));
            durable_after = Some(witness);
        }

        case.check(
            gents_lean_vocab::families::contracts_rendered_capture::CaptureScenarioOutput {
                capture_outcome: capture_outcome.to_string(),
                capture_durable: capture.is_ok(),
                post_stage: if capture.is_ok() {
                    "durablyCaptured"
                } else {
                    "assembled"
                }
                .to_string(),
                send_permitted,
                provider_requests_observed,
                durable_after,
                final_stage: if provider_requests_observed > 0 {
                    "sent"
                } else {
                    "assembled"
                }
                .to_string(),
            },
        );
        node.shutdown().await.expect("node shutdown");
    }
}

#[tokio::test]
async fn codex_affinity_survives_retry_tool_continuation_and_nested_compaction() {
    use gents_loop::provider_input::routing_affinity::current;
    for case in gents_lean_vocab::families::contracts_routing_affinity_dispatches::cases() {
        let responses = case.input.responses.clone();
        let first = responses.first().cloned().flatten();
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
                let mut seen = seen_for_observer.lock().unwrap();
                seen.push(affinity.token().map(str::to_owned));
                affinity.observe(responses[seen.len() - 1].as_deref());
            } else {
                assert_eq!(affinity.token(), first.as_deref());
            }
        }));
        let mut cfg = owned_config(4);
        cfg.provider_input_counter =
            Arc::new(crate::turn::provider_input::ProviderInputCounter::new(
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
        case.check(seen.lock().unwrap().clone());
    }
}
