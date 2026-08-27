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

/// The durable `RenderedRequest` table, as `Proofs.RenderedCapture` models it:
/// a partial map from the five-component capture key to the opaque canonical
/// request stored under it.
type CaptureKey = (u64, u64, u64, usize, u32);

/// Mirror the capture contract: write a missing key, accept an identical
/// binding, and reject a conflicting canonical request.
fn mirror_capture(
    store: &mut std::collections::HashMap<CaptureKey, u64>,
    key: CaptureKey,
    request: u64,
) -> &'static str {
    match store.get(&key).copied() {
        None => {
            store.insert(key, request);
            "fresh"
        }
        Some(stored) if stored == request => "idempotent",
        Some(_) => "rejected",
    }
}

/// Persist-before-send, driven end to end through the real owned loop.
///
/// The Lean model (`Proofs/RenderedCapture.lean`) proves that `sent` is
/// unreachable from `assembled` without an intervening successful capture of
/// the same `(key, canonical request)`, and that a rejected capture makes
/// `sent` unreachable permanently. This test is the fence that keeps
/// `run_loop_stream` honest about it: for every generated row, a sink that
/// answers exactly as `RenderedCapture.capture` does must let the provider
/// observe exactly `provider_requests_observed` requests — one when the fact is
/// durable, zero when it is not.
///
/// `on_rendered_request` must complete immediately before `model.stream`;
/// capture rejection therefore permits no provider request.
#[tokio::test(start_paused = true)]
async fn generated_rendered_capture_cases_fence_persist_before_send() {
    let cases = crate::lean_vocab_test::lean_rendered_capture_cases();
    assert!(!cases.is_empty(), "Lean emitted no rendered-capture cases");

    for case in cases {
        let key: CaptureKey = (
            case.agent_did,
            case.session_id,
            case.request_id,
            case.turn_index,
            case.attempt,
        );
        let mut seeded = std::collections::HashMap::new();
        if let Some(prior) = case.prior_binding {
            seeded.insert(key, prior);
        }
        let store = Arc::new(Mutex::new(seeded));
        let outcomes = Arc::new(Mutex::new(Vec::<&'static str>::new()));

        let store_for_sink = store.clone();
        let outcomes_for_sink = outcomes.clone();
        let request_value = case.request;
        let mut loop_config = config(0);
        loop_config.on_rendered_request =
            Some(Arc::new(move |_turn_index, _attempt, _request, _trace| {
                let store = store_for_sink.clone();
                let outcomes = outcomes_for_sink.clone();
                Box::pin(async move {
                    let outcome = mirror_capture(&mut *store.lock().await, key, request_value);
                    outcomes.lock().await.push(outcome);
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
            "{}: the sink decision drifted from RenderedCapture.capture",
            case.name
        );
        assert_eq!(
            store.lock().await.get(&key).copied(),
            case.durable_after,
            "{}: the durable binding drifted from the Lean model",
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
            assert_eq!(case.final_stage, "sent");
            assert!(
                collected.error.is_none(),
                "{}: a durable capture must not fail the turn: {:?}",
                case.name,
                collected.error
            );
        } else {
            assert_eq!(case.final_stage, "assembled");
            assert!(
                !case.capture_durable,
                "{}: a row may not refuse the send while claiming durability",
                case.name
            );
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
///   `PreStreamDirective::Repair` branch rebuilt with `build_request`. That
///   attempt skips `clamp_request_output_budget`, so a reconstructor that
///   assumes the budgeted path would produce a different `max_tokens` and a
///   false mismatch.
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

    // Leak 2: the exact tool-result content threaded to the model. Persistence
    // re-derives this text from `AgentToolCall.result` through a different
    // truncation mode and limit set, so the trace is the only place the bytes
    // the model actually saw survive.
    let repaired_trace = &captures.last().expect("a repaired capture").3;
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

/// `PreStreamDirective::Repair` is handled in two places: once where
/// `model.stream` itself returns `Err`, and once where the first poll of the
/// returned stream fails. Both rebuild with `build_request` and both must
/// report `Repair`. `ScriptedCall::FailStream` only reaches the first;
/// `TurnWithMidStreamError(vec![], …)` reaches the second.
#[tokio::test(start_paused = true)]
async fn capture_seam_reports_the_repair_build_path_from_the_first_poll_branch() {
    let model = ScriptedModel::new_calls(vec![
        ScriptedCall::TurnWithMidStreamError(Vec::new(), parse_400_error("same")),
        ScriptedCall::TurnWithMidStreamError(Vec::new(), parse_400_error("same")),
        ScriptedCall::Turn(vec![
            RawStreamingChoice::Message("repaired".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ]),
    ]);

    let captures: Arc<Mutex<Vec<(usize, u32, AssemblyBuildPath)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let captures_for_sink = captures.clone();
    let mut loop_config = config(0);
    loop_config.on_rendered_request =
        Some(Arc::new(move |turn_index, attempt, _request, trace| {
            let captures = captures_for_sink.clone();
            Box::pin(async move {
                captures
                    .lock()
                    .await
                    .push((turn_index, attempt, trace.build_path));
                Ok(())
            })
        }));

    let stream = run_loop_stream(
        model.clone(),
        None,
        Message::user("hi"),
        Vec::new(),
        Arc::new(Vec::new()),
        loop_config,
    );
    let collected = collect_scripted_stream(stream).await;
    assert_eq!(collected.error, None);
    assert_eq!(collected.final_text.as_deref(), Some("repaired"));

    assert_eq!(
        captures.lock().await.as_slice(),
        &[
            (0, 0, AssemblyBuildPath::Budgeted),
            (0, 1, AssemblyBuildPath::Budgeted),
            (0, 2, AssemblyBuildPath::Repair),
        ]
    );
}

/// The mis-wired-transport backstop, which nothing else exercises.
///
/// The transport is what claims an armed capture and writes the row. A provider
/// stack assembled without `RenderedRequestCapturingHttpClient` — a new
/// `BackendProviderKind`, a wrapper inserted below the capture seam, a builder
/// that forgets it — still streams perfectly well; the only observable trace is
/// that the arm is still pending when the first stream item arrives. Deleting
/// the check at that point would otherwise pass the entire suite while every
/// turn on that backend went uncaptured.
///
/// `ScriptedModel` stands in for exactly that mis-wiring: it answers the loop
/// without ever claiming the pending capture.
#[tokio::test(start_paused = true)]
async fn a_provider_response_with_the_capture_still_armed_fails_the_turn() {
    use crate::rendered_request::scope::{scope_request, test_scope, CaptureScopeKind};
    use crate::rendered_request::{RenderedRequestCaptureSink, RenderedRequestContext};

    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("uncaptured".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);

    let context = RenderedRequestContext {
        request_doc_id: "doc-1".to_string(),
        request_commit_cid: "bafy-request-commit".to_string(),
        request_id: "req-1".to_string(),
        agent_did: "did:key:agent".to_string(),
        requester_did: String::new(),
        behavior_id: "general".to_string(),
        session_id: "session-1".to_string(),
        model_name: "model".to_string(),
    };
    let sink: RenderedRequestCaptureSink = Arc::new(|_| Box::pin(async { Ok(()) }));
    let scope = test_scope(context, sink);

    let mut loop_config = config(0);
    // The production arming sink: it arms the ambient scope and leaves the
    // write to the transport, which in this stack does not exist.
    loop_config.on_rendered_request = Some(crate::rendered_request::scope::ambient_arming_sink(
        CaptureScopeKind::Inference,
    ));

    let collected = scope_request(scope, async {
        let stream = run_loop_stream(
            model.clone(),
            None,
            Message::user("hi"),
            Vec::new(),
            Arc::new(Vec::new()),
            loop_config,
        );
        collect_scripted_stream(stream).await
    })
    .await;

    let error = collected
        .error
        .as_deref()
        .expect("a response with no durable capture must terminate the turn");
    assert!(
        error.contains("missing its capturing transport"),
        "the failure must name the mis-wired stack: {error}"
    );
    assert_eq!(
        collected.final_text, None,
        "no turn may complete on a provider response nothing captured"
    );
}

/// The same backstop must fire when the provider returns EOF without yielding
/// an item; otherwise the item-level check is never reached and the loop can
/// misclassify an uncaptured send as an ordinary empty completion.
#[tokio::test(start_paused = true)]
async fn an_empty_provider_stream_with_the_capture_still_armed_fails_the_turn() {
    use crate::rendered_request::scope::{scope_request, test_scope, CaptureScopeKind};
    use crate::rendered_request::{RenderedRequestCaptureSink, RenderedRequestContext};

    let model = ScriptedModel::new(Vec::new());
    let context = RenderedRequestContext {
        request_doc_id: "doc-empty".to_string(),
        request_commit_cid: "bafy-request-commit".to_string(),
        request_id: "req-empty".to_string(),
        agent_did: "did:key:agent".to_string(),
        requester_did: String::new(),
        behavior_id: "general".to_string(),
        session_id: "session-empty".to_string(),
        model_name: "model".to_string(),
    };
    let sink: RenderedRequestCaptureSink = Arc::new(|_| Box::pin(async { Ok(()) }));
    let scope = test_scope(context, sink);
    let mut loop_config = config(0);
    loop_config.on_rendered_request = Some(crate::rendered_request::scope::ambient_arming_sink(
        CaptureScopeKind::Inference,
    ));

    let collected = scope_request(scope, async {
        collect_scripted_stream(run_loop_stream(
            model,
            None,
            Message::user("hi"),
            Vec::new(),
            Arc::new(Vec::new()),
            loop_config,
        ))
        .await
    })
    .await;

    let error = collected
        .error
        .as_deref()
        .expect("an empty uncaptured response must terminate the turn");
    assert!(error.contains("missing its capturing transport"), "{error}");
    assert_eq!(collected.final_text, None);
}
