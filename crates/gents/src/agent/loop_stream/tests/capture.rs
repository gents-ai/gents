#[tokio::test]
async fn rendered_request_sink_runs_before_provider_stream() {
    let (_node, hook) = test_hook().await;
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("unreached".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let captures = Arc::new(Mutex::new(Vec::new()));
    let captures_for_sink = captures.clone();
    let mut loop_config = config(0);
    loop_config.on_rendered_request =
        Some(Arc::new(move |turn_index, attempt, request, _trace| {
            let captures = captures_for_sink.clone();
            Box::pin(async move {
                captures
                    .lock()
                    .await
                    .push((turn_index, attempt, request.chat_history.len()));
                Err(anyhow::anyhow!("capture failed"))
            })
        }));

    let stream = run_loop_stream(
        model.clone(),
        Some(hook),
        Message::user("hi"),
        Vec::new(),
        Arc::new(Vec::new()),
        loop_config,
    );
    futures::pin_mut!(stream);

    let item = stream
        .next()
        .await
        .expect("stream should yield the sink error");
    let error = item.expect_err("capture failure should abort the provider call");
    assert!(
        format!("{error:?}").contains("capturing rendered completion request failed"),
        "unexpected error: {error:?}"
    );
    assert_eq!(captures.lock().await.as_slice(), &[(0, 0, 1)]);
    assert!(
        model.seen_histories().await.is_empty(),
        "provider stream must not start after capture failure"
    );
}

/// The loop must honor a scripted capture outcome before sending to the provider.
/// Durable capture semantics are exercised separately against DefraRenderedRequestSink.
#[tokio::test(start_paused = true)]
async fn generated_rendered_capture_cases_fence_persist_before_send() {
    let cases = crate::lean_vocab_test::lean_rendered_capture_cases();
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
                        Err(anyhow::anyhow!(
                            "capture key already names a different canonical request"
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
            None,
            Message::user("hi"),
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

/// The capture seam must hand the sink the loop's own `attempt` counter and its
/// own build path, one row per provider attempt.
///
/// Two things are fenced here that nothing else fences:
///
/// * `attempt` is part of the capture key, so retries must arrive as distinct
///   attempts within one turn.
/// * `AssemblyBuildPath` must flip to `Repair` exactly on the attempt that the
///   `PreStreamDirective::Repair` branch rebuilt with `build_request`. The
///   final dispatch boundary still recounts and clamps that repaired request.
///
/// Both repair injection branches run in this test. One reaches `Repair`
/// through `model.stream` returning `Err` (`ScriptedCall::FailStream`); the
/// other reaches it through the first poll of the returned stream failing
/// (`TurnWithMidStreamError(vec![], …)`). The repair budget is request-wide,
/// so the branches use independent loop invocations.
#[tokio::test(start_paused = true)]
async fn capture_seam_reports_distinct_attempts_and_the_repair_build_path() {
    let poison = format!("bad{}value", '\u{0007}');
    let model = ScriptedModel::new_calls(vec![
        ScriptedCall::Turn(vec![
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-1".to_string(),
                "echo".to_string(),
                serde_json::json!({ "note": poison }),
            )),
            RawStreamingChoice::FinalResponse(()),
        ]),
        ScriptedCall::FailStream(parse_400_error("same")),
        ScriptedCall::FailStream(parse_400_error("same")),
        ScriptedCall::Turn(vec![
            RawStreamingChoice::Message("repaired".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ]),
    ]);

    let captures: Arc<Mutex<Vec<(usize, u32, AssemblyBuildPath, AssemblyTrace)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let captures_for_sink = captures.clone();
    let captured_requests: Arc<Mutex<Vec<CompletionRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_requests_for_sink = captured_requests.clone();
    let mut loop_config = config(4);
    loop_config.on_rendered_request = Some(Arc::new(move |turn_index, attempt, request, trace| {
        let captures = captures_for_sink.clone();
        let captured_requests = captured_requests_for_sink.clone();
        Box::pin(async move {
            captured_requests.lock().await.push(request);
            captures
                .lock()
                .await
                .push((turn_index, attempt, trace.build_path, trace));
            Ok(())
        })
    }));

    let stream = run_loop_stream(
        model.clone(),
        None,
        Message::user("use the echo tool"),
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        loop_config,
    );
    let collected = collect_scripted_stream(stream).await;
    assert_eq!(collected.error, None);
    assert_eq!(collected.final_text.as_deref(), Some("repaired"));

    let captures = captures.lock().await;
    let observed = captures
        .iter()
        .map(|(turn, attempt, path, _)| (*turn, *attempt, *path))
        .collect::<Vec<_>>();
    assert_eq!(
        observed,
        vec![
            (0, 0, AssemblyBuildPath::Budgeted),
            (1, 0, AssemblyBuildPath::Budgeted),
            (1, 1, AssemblyBuildPath::Budgeted),
            (1, 2, AssemblyBuildPath::Repair),
        ],
        "one capture per provider attempt, with the loop's own attempt counter"
    );
    assert_eq!(
        captures.len(),
        model.seen_histories().await.len(),
        "every provider request must have exactly one capture"
    );
    let captured_requests = captured_requests.lock().await.clone();
    let dispatched_requests = model.seen_requests().await;
    assert_eq!(captured_requests.len(), dispatched_requests.len());
    for (captured, dispatched) in captured_requests.iter().zip(dispatched_requests.iter()) {
        assert_eq!(
            format!("{captured:?}"),
            format!("{dispatched:?}"),
            "capture and transport must consume the same prepared request snapshot"
        );
    }

    // Leak 2: the exact tool-result content threaded to the model. Persistence
    // re-derives this text from `AgentToolCall.result` through a different
    // truncation mode and limit set, so the trace is the only place the bytes
    // the model actually saw survive.
    let repaired_trace = &captures[3].3;
    let threaded = repaired_trace
        .threaded_tool_results
        .iter()
        .find(|result| result.tool_call_id == "call-1")
        .expect("the echo call's threaded result");
    assert_eq!(
        threaded.content,
        vec![ToolResultContent::text("ECHOED")],
        "the trace must carry the threaded tool-result content verbatim"
    );
    assert!(
        repaired_trace.effective_message_count > threaded.message_index,
        "overlay positions must fit the reconstructible native list"
    );
    assert!(
        repaired_trace.effective_messages.is_some(),
        "a repaired attempt rewrote the message vectors in place, so the durable transcript no \
         longer reproduces them and the full native list is the only oracle"
    );

    let first_turn_trace = &captures.first().expect("a first capture").3;
    assert!(
        first_turn_trace.effective_messages.is_none(),
        "a turn before any repair must not duplicate the full transcript"
    );
    drop(captures);

    let poll_model = ScriptedModel::new_calls(vec![
        ScriptedCall::TurnWithMidStreamError(Vec::new(), parse_400_error("poll-branch")),
        ScriptedCall::TurnWithMidStreamError(Vec::new(), parse_400_error("poll-branch")),
        ScriptedCall::Turn(vec![
            RawStreamingChoice::Message("poll branch repaired".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ]),
    ]);
    let poll_captures: Arc<Mutex<Vec<(usize, u32, AssemblyBuildPath)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let poll_captures_for_sink = poll_captures.clone();
    let mut poll_config = config(0);
    poll_config.on_rendered_request = Some(Arc::new(move |turn, attempt, _, trace| {
        let captures = poll_captures_for_sink.clone();
        Box::pin(async move {
            captures.lock().await.push((turn, attempt, trace.build_path));
            Ok(())
        })
    }));
    let poll_result = collect_scripted_stream(run_loop_stream(
        poll_model,
        None,
        Message::user("repair the first poll"),
        Vec::new(),
        Arc::new(Vec::new()),
        poll_config,
    ))
    .await;
    assert_eq!(poll_result.error, None);
    assert_eq!(poll_result.final_text.as_deref(), Some("poll branch repaired"));
    assert_eq!(
        poll_captures.lock().await.as_slice(),
        &[
            (0, 0, AssemblyBuildPath::Budgeted),
            (0, 1, AssemblyBuildPath::Budgeted),
            (0, 2, AssemblyBuildPath::Repair),
        ]
    );
}

/// `repair_provider_input` rewrites `history` and `new_messages` in place, and
/// both outlive the turn loop, so every turn *after* a repair is assembled from
/// messages no `AgentMessage` row reproduces. `build_path` resets per turn and
/// would report `Budgeted` for those turns, so the ephemeral marker is what
/// stops a reconstructor trusting a list it cannot rebuild.
///
/// The repair occurs before another turn so carry-over is observable.
#[tokio::test(start_paused = true)]
async fn a_turn_after_a_repair_still_carries_the_effective_message_list() {
    let poison = format!("bad{}value", '\u{0007}');
    let model = ScriptedModel::new_calls(vec![
        // Turn 0 — a tool call carrying the argument that will need repair.
        ScriptedCall::Turn(vec![
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-1".to_string(),
                "echo".to_string(),
                serde_json::json!({ "note": poison }),
            )),
            RawStreamingChoice::FinalResponse(()),
        ]),
        // Turn 1 — rejected twice, then repaired and answered with another tool
        // call so the loop runs at least one more turn.
        ScriptedCall::FailStream(parse_400_error("same")),
        ScriptedCall::FailStream(parse_400_error("same")),
        ScriptedCall::Turn(vec![
            RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                "call-2".to_string(),
                "echo".to_string(),
                serde_json::json!({ "note": "clean" }),
            )),
            RawStreamingChoice::FinalResponse(()),
        ]),
        // Turn 2 — the turn after the repair. This is the one under test.
        ScriptedCall::Turn(vec![
            RawStreamingChoice::Message("after repair".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ]),
    ]);

    let captures: Arc<Mutex<Vec<(usize, u32, AssemblyBuildPath, AssemblyTrace)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let captures_for_sink = captures.clone();
    let mut loop_config = config(4);
    loop_config.on_rendered_request =
        Some(Arc::new(move |turn_index, attempt, _request, trace| {
            let captures = captures_for_sink.clone();
            Box::pin(async move {
                captures
                    .lock()
                    .await
                    .push((turn_index, attempt, trace.build_path, trace));
                Ok(())
            })
        }));

    let stream = run_loop_stream(
        model.clone(),
        None,
        Message::user("use the echo tool"),
        Vec::new(),
        Arc::new(vec![echo_tool()]),
        loop_config,
    );
    let collected = collect_scripted_stream(stream).await;
    assert_eq!(collected.error, None);
    assert_eq!(collected.final_text.as_deref(), Some("after repair"));

    let captures = captures.lock().await;
    let repair_position = captures
        .iter()
        .position(|(_, _, path, _)| *path == AssemblyBuildPath::Repair)
        .expect("the scripted 400s must have produced a repair");
    assert!(
        repair_position + 1 < captures.len(),
        "the script must run at least one turn after the repair; got {:?}",
        captures
            .iter()
            .map(|(turn, attempt, path, _)| (*turn, *attempt, *path))
            .collect::<Vec<_>>()
    );

    for (turn, attempt, _, trace) in captures.iter().skip(repair_position) {
        assert!(
            trace.effective_messages.is_some(),
            "turn {turn} attempt {attempt} was assembled from repaired vectors, so its effective \
             message list must be carried rather than left for a reconstructor to rebuild"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn capture_trace_retains_ephemeral_request_context() {
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("done".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let traces: Arc<Mutex<Vec<AssemblyTrace>>> = Arc::new(Mutex::new(Vec::new()));
    let traces_for_sink = Arc::clone(&traces);
    let mut loop_config = config(0);
    loop_config.context_message = Some(Message::user(
        "<context>\nrendered-at-2026-08-07T00:00:00Z\n</context>",
    ));
    loop_config.on_rendered_request = Some(Arc::new(move |_, _, _, trace| {
        let traces = Arc::clone(&traces_for_sink);
        Box::pin(async move {
            traces.lock().await.push(trace);
            Ok(())
        })
    }));

    let collected = collect_scripted_stream(run_loop_stream(
        model,
        None,
        Message::user("hi"),
        Vec::new(),
        Arc::new(Vec::new()),
        loop_config,
    ))
    .await;
    assert_eq!(collected.error, None);

    let traces = traces.lock().await;
    let effective = traces[0]
        .effective_messages
        .as_ref()
        .expect("dynamic request context requires the native oracle");
    assert!(effective.iter().any(is_request_context_message));
}

/// The mis-wired-transport backstop, which nothing else exercises.
///
/// The transport is what claims an armed capture and writes the row. A provider
/// stack assembled without `RenderedRequestCapturingHttpClient` — a new
/// `BackendProviderKind`, a wrapper inserted below the capture seam, a builder
/// that forgets it — still streams perfectly well; the only observable trace is
/// that the arm is still pending when the first stream item arrives (or at EOF
/// when the provider yields no item, so the item-level check is never reached
/// and the loop cannot misclassify an uncaptured send as an ordinary empty
/// completion). Deleting the check at that point would otherwise pass the
/// entire suite while every turn on that backend went uncaptured.
///
/// `ScriptedModel` stands in for exactly that mis-wiring: it answers the loop
/// without ever claiming the pending capture.
#[tokio::test(start_paused = true)]
async fn a_provider_response_with_the_capture_still_armed_fails_the_turn() {
    use crate::rendered_request::scope::{scope_request, test_scope, CaptureScopeKind};
    use crate::rendered_request::{RenderedRequestCaptureSink, RenderedRequestContext};

    for (request_id, session_id, choices) in [
        (
            "req-1",
            "session-1",
            vec![
                RawStreamingChoice::Message("uncaptured".to_string()),
                RawStreamingChoice::FinalResponse(()),
            ],
        ),
        // The same backstop must fire at EOF without any item; otherwise the
        // item-level check is never reached and the loop can misclassify an
        // uncaptured send as an ordinary empty completion.
        ("req-empty", "session-empty", Vec::new()),
    ] {
        let model = ScriptedModel::new(choices);
        let context = RenderedRequestContext {
            request_doc_id: format!("doc-{request_id}"),
            request_commit_cid: "bafy-request-commit".to_string(),
            request_id: request_id.to_string(),
            agent_did: "did:key:agent".to_string(),
            requester_did: String::new(),
            behavior_id: "general".to_string(),
            session_id: session_id.to_string(),
            model_name: "model".to_string(),
        };
        let sink: RenderedRequestCaptureSink = Arc::new(|_| Box::pin(async { Ok(()) }));
        let scope = test_scope(context, sink);

        let mut loop_config = config(0);
        // The production arming sink: it arms the ambient scope and leaves the
        // write to the transport, which in this stack does not exist.
        loop_config.on_rendered_request = Some(
            crate::rendered_request::scope::ambient_arming_sink(CaptureScopeKind::Inference),
        );

        let collected = scope_request(scope, async {
            let stream = run_loop_stream(
                model.clone(),
                None,
                Message::user("hi"),
                Vec::new(),
                Arc::new(Vec::new()),
                loop_config.clone(),
            );
            collect_scripted_stream(stream).await
        })
        .await;

        let error = collected
            .error
            .as_deref()
            .expect("an uncaptured provider response must terminate the turn");
        assert!(
            error.contains("missing its capturing transport"),
            "the failure must name the mis-wired stack: {error}"
        );
        assert_eq!(
            collected.final_text, None,
            "no turn may complete on a provider response nothing captured"
        );
    }
}

// Exercise fresh, idempotent, and conflicting captures at the durable sink.
#[tokio::test]
async fn generated_rendered_capture_cases_hold_against_the_real_defra_sink() {
    use crate::graphql::escape_graphql_string;
    use crate::rendered_request::capture_key as derive_capture_key;
    use crate::rendered_request::{
        DefraRenderedRequestSink, ProvenanceManifest, RenderedRequestSource, CAPTURE_VERSION,
    };

    let cases = crate::lean_vocab_test::lean_rendered_capture_cases();
    assert!(!cases.is_empty(), "Lean emitted no rendered-capture cases");

    async fn rendered_rows(
        node: &Arc<defra_node::EmbeddedNode>,
        capture_key: &str,
    ) -> Vec<serde_json::Value> {
        let query = format!(
            r#"{{ RenderedRequest(filter: {{ capture_key: {{ _eq: "{}" }} }}) {{ capture_key request_json }} }}"#,
            escape_graphql_string(capture_key),
        );
        let response = node.execute(&query).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        response.data.unwrap()["RenderedRequest"]
            .as_array()
            .cloned()
            .expect("RenderedRequest row array")
    }

    for case in cases {
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let sink = DefraRenderedRequestSink::new(node.clone());

        // The model's canonical request is opaque; equal values mean equal
        // canonical JSON. One witness value per modeled request keeps that
        // equality exact, including the case where a delivery must NOT be
        // idempotent (witness 100 vs 101 in the generated rows).
        let rendered_for_witness =
            |witness: u64| -> crate::rendered_request::RenderedCompletionRequest {
                let agent_did = format!("did:key:z6Mk-sink-{}", case.agent_did);
                let session_id = format!("session-{}", case.session_id);
                let request_doc_id = format!("bae-request-doc-{}", case.request_id);
                let assembly_trace =
                    crate::rendered_request::AssemblyTrace::from_effective_messages(
                        crate::rendered_request::AssemblyBuildPath::Budgeted,
                        Vec::new(),
                    );
                crate::rendered_request::RenderedCompletionRequest {
                    capture_key: derive_capture_key(
                        &agent_did,
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
                    agent_did,
                    requester_did: "did:key:z6Mk-requester".to_string(),
                    behavior_id: "behavior-sink".to_string(),
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
                    assembly_trace,
                }
            };

        // Seed the prior binding through the same sink: the model's store is
        // built by captures, never by fixture writes.
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
        let rows = rendered_rows(&node, &delivery_key).await;
        assert_eq!(
            rows.len(),
            usize::from(case.durable_after.is_some()),
            "{}",
            case.name
        );
        let stored = rows.first().map(|row| {
            serde_json::from_str::<serde_json::Value>(
                row["request_json"].as_str().expect("request_json string"),
            )
            .expect("stored request_json decodes")
        });
        assert_eq!(
            stored,
            case.durable_after
                .map(|witness| serde_json::json!({"witness": witness})),
            "{}: the durable binding must match the capture contract",
            case.name
        );
        node.shutdown().await;
    }
}

const CAPTURE_SCOPE_SINK: &str = "inference.1";
