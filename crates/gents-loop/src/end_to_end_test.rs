use std::sync::{Arc, Mutex};

use gents_protocol::message::Message;
use rig::streaming::{RawStreamingChoice, RawStreamingToolCall};

use crate::backend_provider::BackendProviderKind;
use crate::loop_stream::{run_loop_to_text, LoopConfig, TaggedMessage};
use crate::openai_wire::OpenAiWireApi;
use crate::provider_input::ProviderInputCounter;
use crate::test_support::doubles::{
    AdmissionGate, EchoTool, GatedModel, PendingTool, ReadEchoTool, RecordedCall, RecordingHook,
    ResponsesTool, SettlementGate,
};
use crate::test_support::scripted_provider::{Reply, ScriptedProvider, Step};
use crate::tool::ToolDyn;

#[cfg(feature = "native")]
mod provider_finish_tests;

/// The scripted provider as these tests drive it: each call claims its
/// armed capture as a real send would.
fn scripted(turns: Vec<Vec<RawStreamingChoice<()>>>) -> ScriptedProvider {
    ScriptedProvider::turns(turns).claiming_capture()
}

/// Each call's chat history as it was sent.
fn sent_histories(model: &ScriptedProvider) -> Vec<String> {
    model
        .requests()
        .iter()
        .map(|request| serde_json::to_string(&request.chat_history).unwrap())
        .collect()
}

#[cfg(test)]
tokio::task_local! {
    /// A task-local the caller of the loop stream sets, standing in for the
    /// ones that remain (the workspace-approval context, the rendered-request
    /// capture scope): settlement the stream drives must observe the caller's.
    static CALLER_SCOPE: &'static str;
}

fn scripted_tool_call() -> RawStreamingChoice<()> {
    RawStreamingChoice::ToolCall(RawStreamingToolCall {
        id: "call_1".to_string(),
        internal_call_id: "call_1".to_string(),
        call_id: Some("call_1".to_string()),
        name: "echo".to_string(),
        arguments: serde_json::json!({"text": "hi"}),
        signature: None,
        additional_params: None,
    })
}

pub(crate) fn test_loop_config() -> LoopConfig {
    LoopConfig {
        provider_call: Default::default(),
        replay: crate::loop_stream::LoopReplayInput::default(),
        provider_input_counter: Arc::new(ProviderInputCounter::new(
            BackendProviderKind::OpenAiCompatible,
            OpenAiWireApi::ChatCompletions,
            "test-model",
        )),
        preamble: Some("You are a helpful assistant.".to_string()),
        context_message: None,
        temperature: None,
        max_tokens: None,
        aggregate_token_budget: None,
        additional_params: None,
        structured_output: None,
        tool_choice: None,
        configuration_guard: None,
        tool_rebinding: None,
        on_rendered_request: None,
        turn_compactor: None,
        active_reduction_keys: Vec::new(),
        reduction_chain_keys: Vec::new(),
        initial_turn_index: 0,
        resume_from_history: false,
        context_window: 128_000,
        compaction_threshold: 0.75,
        context_calibration: Default::default(),
        retry_policy: crate::completion_retry::CompletionRetryPolicy::interactive_default(),
        provider_idle_timeout: None,
        deadline: None,
        max_turns: 8,
        max_invalid_tool_calls: crate::loop_stream::DEFAULT_MAX_INVALID_TOOL_CALLS,
        repeated_tool_failure_limit: crate::loop_stream::DEFAULT_REPEATED_TOOL_FAILURE_LIMIT,
        output_obligation_gate: None,
        folded_prompts: Vec::new(),
        authored_input: None,
        tool_context: crate::tool::ToolCallEnvelope::default(),
    }
}

#[cfg(feature = "native")]
#[tokio::test]
async fn cancelling_a_pending_provider_stream_drops_it() {
    let model = ScriptedProvider::new([Reply::Stream(vec![Step::Pending])]);
    let mut completion = Box::pin(run_loop_to_text(
        model.clone(),
        Message::user("hello"),
        Vec::new(),
        Arc::new(Vec::new()),
        test_loop_config(),
    ));
    assert!(futures::poll!(&mut completion).is_pending());
    drop(completion);
    let records = model.records();
    assert_eq!(records.len(), 1);
    assert!(records[0].closed);
}

/// TLA+ `InterruptPartialOutput`: a chunk the provider has handed to the
/// loop reaches the consumer in the same poll, so an interrupt that wins the
/// consumer's next select cannot drop it with the stream. A spent cooperative
/// budget makes every tokio resource poll return `Pending`; the handoff must
/// not be one of them.
#[tokio::test]
async fn a_received_provider_chunk_is_yielded_when_the_task_budget_is_spent() {
    use std::sync::atomic::{AtomicBool, Ordering};

    use futures::StreamExt;

    use crate::loop_stream::LoopStreamItem;
    use crate::session_hook::NoopSessionHook;

    let model = GatedModel {
        call: Default::default(),
        open: Arc::new(AtomicBool::new(false)),
        emitted: Arc::new(AtomicBool::new(false)),
    };
    let stream = crate::loop_stream::run_loop_stream::<_, NoopSessionHook>(
        model.clone(),
        None,
        TaggedMessage::unassociated(Message::user("hello")),
        Vec::new(),
        Arc::new(Vec::new()),
        test_loop_config(),
    );
    futures::pin_mut!(stream);
    assert!(futures::poll!(stream.next()).is_pending());
    assert!(!model.emitted.load(Ordering::SeqCst));

    model.open.store(true, Ordering::SeqCst);
    while tokio::task::coop::has_budget_remaining() {
        tokio::task::consume_budget().await;
    }
    let polled = futures::poll!(stream.next());
    assert!(
        model.emitted.load(Ordering::SeqCst),
        "the provider was polled"
    );
    let std::task::Poll::Ready(Some(Ok(LoopStreamItem::Text(text)))) = polled else {
        panic!("a received provider chunk stayed inside the loop stream");
    };
    assert_eq!(text, "received partial");
}

#[tokio::test]
async fn cancelling_a_foreground_tool_keeps_its_request_and_drops_execution() {
    use std::sync::atomic::{AtomicBool, Ordering};

    use crate::tool_call_lifecycle::runtime::ToolRequestIdentity;

    let dropped = Arc::new(AtomicBool::new(false));
    let observed = Arc::new(Mutex::new(None));
    let model = scripted(vec![vec![
        scripted_tool_call(),
        RawStreamingChoice::FinalResponse(()),
    ]]);
    let tools: Arc<Vec<Box<dyn ToolDyn>>> = Arc::new(vec![Box::new(PendingTool {
        name: "echo",
        description: "pending",
        dropped: Some(dropped.clone()),
        observed: Some(observed.clone()),
    })]);
    let scope_call = crate::tool_call_lifecycle::runtime::ToolScopeEntry {
        identity: Some(ToolRequestIdentity::new(
            Some("requester".into()),
            Some("node".into()),
            Some("agent".into()),
            Some("request".into()),
        )),
        ..crate::tool_call_lifecycle::runtime::ToolScopeEntry::new(None, Default::default())
    }
    .call();
    let mut config = test_loop_config();
    config.tool_context = scope_call;
    let mut completion = Box::pin(run_loop_to_text(
        model,
        Message::user("echo"),
        Vec::new(),
        tools,
        config,
    ));
    assert!(futures::poll!(&mut completion).is_pending());
    drop(completion);
    assert_eq!(*observed.lock().unwrap(), Some("request".into()));
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn modeled_dispatch_permissions_gate_real_tool_invocation() {
    use futures::StreamExt;
    for case in gents_lean_vocab::families::dispatch_observation_contracts_loop_election::cases() {
        let input = &case.input;
        let model = scripted(vec![
            vec![scripted_tool_call(), RawStreamingChoice::FinalResponse(())],
            vec![
                RawStreamingChoice::Message("done".into()),
                RawStreamingChoice::FinalResponse(()),
            ],
        ]);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let tools: Arc<Vec<Box<dyn ToolDyn>>> = Arc::new(vec![Box::new(EchoTool {
            calls: calls.clone(),
            policy_allows: input.policy_allows,
        })]);
        let hook = RecordingHook {
            deny_dispatch: !input.hook_elects,
            ..Default::default()
        };
        let observed_hook = hook.clone();
        use crate::rendered_request::scope::{arming_sink, test_scope, CaptureScopeKind};
        let scope = test_scope(
            crate::rendered_request::RenderedRequestContext {
                request_doc_id: "dispatch-doc".into(),
                request_commit_cid: "dispatch-cid".into(),
                request_id: "dispatch-request".into(),
                node_did: "did:test:agent".into(),
                requester_did: "did:test:requester".into(),
                agent_did: "general".into(),
                session_id: "dispatch-session".into(),
                model_name: "scripted".into(),
                provider_family: None,
            },
            Arc::new(|_| Box::pin(async { Ok(()) })),
        );
        let mut config = test_loop_config();
        config.on_rendered_request = Some(arming_sink(CaptureScopeKind::Inference));
        let failure = {
            config.provider_call.capture = Some(scope);
            async {
                let stream = crate::loop_stream::run_loop_stream(
                    model,
                    Some(hook),
                    TaggedMessage::unassociated(Message::user("echo hi")),
                    Vec::new(),
                    tools,
                    config,
                );
                futures::pin_mut!(stream);
                let mut failure = None;
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(crate::loop_stream::LoopStreamItem::AuthoredInputReady {
                            reply,
                            ..
                        })
                        | Ok(crate::loop_stream::LoopStreamItem::SteeringBoundary { reply }) => {
                            reply.send(Ok(Vec::new())).unwrap();
                        }
                        Ok(crate::loop_stream::LoopStreamItem::FinishOrIntake {
                            reply, ..
                        }) => {
                            reply
                                .send(Ok(crate::loop_stream::FinishOrIntake::Finished))
                                .ok();
                        }
                        Err(error) => {
                            failure = Some(error);
                            break;
                        }
                        _ => {}
                    }
                }
                failure
            }
        }
        .await;
        let recorded = observed_hook.calls();
        let elections = recorded
            .iter()
            .filter(|call| matches!(call, RecordedCall::ToolCall { .. }))
            .count();
        let rejections = recorded
            .iter()
            .filter(|call| matches!(call, RecordedCall::AdmissionRejected { .. }))
            .count();
        let invocations = calls.lock().unwrap().len();
        case.check(Some(gents_lean_vocab::families::dispatch_observation_contracts_loop_election::LoopElectionOutput {
            invocations: invocations as u64,
            loop_failed: failure.is_some(),
            elections: elections as u64,
            rejections: rejections as u64,
        }));
    }
}

fn steering_capture_scope() -> Arc<crate::rendered_request::scope::RequestCaptureScope> {
    crate::rendered_request::scope::test_scope(
        crate::rendered_request::RenderedRequestContext {
            request_doc_id: "steering-doc".into(),
            request_commit_cid: "steering-cid".into(),
            request_id: "steering-request".into(),
            node_did: "did:test:node".into(),
            requester_did: "did:test:requester".into(),
            agent_did: "general".into(),
            session_id: "steering-session".into(),
            model_name: "scripted".into(),
            provider_family: None,
        },
        Arc::new(|_| Box::pin(async { Ok(()) })),
    )
}

fn steering_loop_config() -> LoopConfig {
    use crate::rendered_request::scope::{arming_sink, CaptureScopeKind};
    let mut config = test_loop_config();
    config.on_rendered_request = Some(arming_sink(CaptureScopeKind::Inference));
    config.provider_call.capture = Some(steering_capture_scope());
    config
}

#[tokio::test]
async fn cancelling_pending_settlement_drops_hook_in_the_callers_scope() {
    use futures::StreamExt;

    let gate = Arc::new(SettlementGate {
        read_scope: Some(|| CALLER_SCOPE.try_with(|scope| *scope).ok()),
        ..Default::default()
    });
    let hook = RecordingHook {
        settlement_gate: Some(gate.clone()),
        ..Default::default()
    };
    let model = scripted(vec![vec![
        scripted_tool_call(),
        RawStreamingChoice::FinalResponse(()),
    ]]);
    let tools: Arc<Vec<Box<dyn ToolDyn>>> = Arc::new(vec![Box::new(EchoTool {
        calls: Arc::default(),
        policy_allows: true,
    })]);
    CALLER_SCOPE
        .scope("request", async {
            let mut stream = Box::pin(crate::loop_stream::run_loop_stream(
                model,
                Some(hook),
                TaggedMessage::unassociated(Message::user("echo")),
                Vec::new(),
                tools,
                steering_loop_config(),
            ));
            loop {
                match stream.next().await.unwrap().unwrap() {
                    crate::loop_stream::LoopStreamItem::AuthoredInputReady { reply, .. }
                    | crate::loop_stream::LoopStreamItem::SteeringBoundary { reply } => {
                        reply.send(Ok(Vec::new())).unwrap();
                    }
                    crate::loop_stream::LoopStreamItem::ProviderTurnReady { .. } => break,
                    _ => {}
                }
            }
            let mut next = Box::pin(stream.next());
            tokio::select! {
                () = gate.entered.notified() => {},
                item = &mut next => panic!("settlement did not remain pending: {item:?}"),
            }
            drop(next);
            drop(stream);
        })
        .await;
    assert_eq!(*gate.caller.lock().unwrap(), Some("request"));
    assert!(gate.dropped.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn failed_input_publication_prevents_provider_dispatch() {
    use crate::loop_stream::{run_loop_stream, LoopStreamItem};
    use futures::StreamExt;
    let model = scripted(Vec::new());
    let stream = run_loop_stream(
        model.clone(),
        Some(RecordingHook::default()),
        TaggedMessage::unassociated(Message::user("must not dispatch")),
        Vec::new(),
        Arc::new(Vec::new()),
        test_loop_config(),
    );
    futures::pin_mut!(stream);
    let first = stream.next().await.unwrap().unwrap();
    let LoopStreamItem::AuthoredInputReady { reply, .. } = first else {
        panic!("input publication must precede provider dispatch");
    };
    reply
        .send(Err(crate::stream_processor::StreamProcessorError::Writer(
            "publication rejected".into(),
        )))
        .unwrap();
    let error = stream.next().await.unwrap().unwrap_err();
    assert!(error.to_string().contains("publication rejected"));
    assert_eq!(model.calls(), 0);
}

#[tokio::test]
async fn modeled_unsafe_stream_boundaries_never_intake_or_finish_before_provider_publication() {
    modeled_unsafe_stream_boundaries_never_intake_or_finish_before_provider_publication_scoped()
        .await;
}

async fn modeled_unsafe_stream_boundaries_never_intake_or_finish_before_provider_publication_scoped(
) {
    use crate::loop_stream::{run_loop_stream, FoldedPrompt, LoopStreamItem};
    use futures::StreamExt;
    for case in gents_lean_vocab::families::fold_queue_contracts_publication_unsafe_boundary::cases::<
        String,
    >() {
        let name = case.input.name.as_str();
        let model = scripted(vec![
            vec![
                RawStreamingChoice::Message("first ".into()),
                RawStreamingChoice::Message("answer".into()),
                RawStreamingChoice::FinalResponse(()),
            ],
            vec![
                RawStreamingChoice::Message("corrected answer".into()),
                RawStreamingChoice::FinalResponse(()),
            ],
        ]);
        let stream = run_loop_stream(
            model.clone(),
            Some(RecordingHook::default()),
            TaggedMessage::unassociated(Message::user("original request")),
            Vec::new(),
            Arc::new(Vec::new()),
            steering_loop_config(),
        );
        futures::pin_mut!(stream);
        let mut streaming = false;
        let mut pending = false;
        let mut publications = 0;
        let mut chunks = 0;
        let mut delivered = false;
        let mut final_text = None;
        let mut intake_while_streaming = false;
        while let Some(item) = stream.next().await {
            match item.unwrap() {
                LoopStreamItem::AuthoredInputReady { reply, .. } => {
                    reply.send(Ok(Vec::new())).unwrap();
                }
                LoopStreamItem::Text(_) => {
                    streaming = true;
                    if publications == 0 {
                        chunks += 1;
                        pending = true;
                        assert!(!delivered, "{name}: unsafe stream boundary consumed input");
                        assert_eq!(model.calls(), 1);
                    }
                }
                LoopStreamItem::ProviderTurnReady { .. } => {
                    streaming = false;
                    publications += 1;
                }
                LoopStreamItem::SteeringBoundary { reply } => {
                    intake_while_streaming |= streaming;
                    let inputs = if pending {
                        assert_eq!(publications, 1);
                        pending = false;
                        delivered = true;
                        vec![FoldedPrompt {
                            key: "late".into(),
                            message: Message::user("late correction"),
                        }]
                    } else {
                        Vec::new()
                    };
                    reply.send(Ok(inputs)).unwrap();
                }
                LoopStreamItem::FinishOrIntake { reply, .. } => {
                    intake_while_streaming |= streaming;
                    assert_eq!(publications, 2);
                    reply
                        .send(Ok(crate::loop_stream::FinishOrIntake::Finished))
                        .unwrap();
                }
                LoopStreamItem::Final { text } => final_text = Some(text),
                _ => {}
            }
        }
        assert_eq!(chunks, 2);
        assert!(delivered);
        assert!(!pending);
        assert_eq!(final_text.as_deref(), Some("corrected answer"));
        let requests = sent_histories(&model);
        assert_eq!(requests.len(), 2);
        assert!(!requests[0].contains("late correction"));
        assert!(requests[1].contains("first answer"));
        assert!(requests[1].contains("late correction"));
        case.check(
            gents_lean_vocab::families::fold_queue_contracts_publication_unsafe_boundary::UnsafeBoundaryObservation {
                accepted: intake_while_streaming,
            },
        );
    }
}

/// Answers with text and a final response reporting `request_id` as the
/// provider's response request id.
fn request_id_model(request_id: Option<&str>) -> ScriptedProvider {
    ScriptedProvider::new([Reply::Stream(vec![
        Step::Chunk(RawStreamingChoice::Message("answer".into())),
        Step::Chunk(RawStreamingChoice::MessageId("msg_1".into())),
        Step::End(crate::rig_compat::model::StreamEnd {
            request_id: request_id.map(str::to_owned),
            ..Default::default()
        }),
    ])])
    .claiming_capture()
}

#[tokio::test]
async fn the_accepted_assistant_message_records_the_final_responses_request_id() {
    async {
        for reported in [Some("req_abc"), None] {
            let stream = crate::loop_stream::run_loop_stream(
                request_id_model(reported),
                Some(RecordingHook::default()),
                TaggedMessage::unassociated(Message::user("hello")),
                Vec::new(),
                Arc::new(Vec::new()),
                steering_loop_config(),
            );
            futures::pin_mut!(stream);
            let mut accepted = Vec::new();
            while let Some(item) = futures::StreamExt::next(&mut stream).await {
                match item.expect("scripted loop completes") {
                    crate::loop_stream::LoopStreamItem::AuthoredInputReady { reply, .. } => {
                        reply.send(Ok(Vec::new())).unwrap();
                    }
                    crate::loop_stream::LoopStreamItem::ProviderTurnReady { message, .. } => {
                        accepted.push(message);
                    }
                    crate::loop_stream::LoopStreamItem::SteeringBoundary { reply } => {
                        reply.send(Ok(Vec::new())).unwrap();
                    }
                    crate::loop_stream::LoopStreamItem::FinishOrIntake { reply, .. } => {
                        reply
                            .send(Ok(crate::loop_stream::FinishOrIntake::Finished))
                            .ok();
                    }
                    _ => {}
                }
            }
            let [Message::Assistant { id, request_id, .. }] = accepted.as_slice() else {
                panic!("one assistant turn expected: {accepted:?}");
            };
            assert_eq!(id.as_deref(), Some("msg_1"));
            assert_eq!(request_id.as_deref(), reported);
        }
    }
    .await;
}

#[tokio::test]
async fn serial_and_parallel_admission_follow_the_accepted_turn_and_stop_without_invoking() {
    use crate::rendered_request::scope::{arming_sink, test_scope, CaptureScopeKind};
    use futures::StreamExt;

    for case in gents_lean_vocab::families::admission_handoff_owned_loop::cases() {
        let mut invocations = Vec::new();
        for count in [1, 2] {
            let mut choices = Vec::new();
            for index in 0..count {
                let RawStreamingChoice::ToolCall(mut call) = scripted_tool_call() else {
                    unreachable!()
                };
                call.id = format!("call-{index}");
                call.internal_call_id = format!("call-{index}");
                call.call_id = Some(format!("call-{index}"));
                call.arguments = serde_json::json!({"text": format!("input-{index}")});
                choices.push(RawStreamingChoice::ToolCall(call));
            }
            choices.push(RawStreamingChoice::FinalResponse(()));
            let model = scripted(vec![choices]);
            let invoked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let tools: Arc<Vec<Box<dyn ToolDyn>>> =
                Arc::new(vec![Box::new(ReadEchoTool(invoked.clone()))]);
            let gate = Arc::new(AdmissionGate::default());
            let hook = RecordingHook {
                admission_gate: Some(gate.clone()),
                ..Default::default()
            };
            let observed = hook.clone();
            let scope = test_scope(
                crate::rendered_request::RenderedRequestContext {
                    request_doc_id: "handoff-doc".into(),
                    request_commit_cid: "handoff-cid".into(),
                    request_id: "handoff-request".into(),
                    node_did: "did:test:node".into(),
                    requester_did: "did:test:user".into(),
                    agent_did: "did:test:agent".into(),
                    session_id: "handoff-session".into(),
                    model_name: "scripted".into(),
                    provider_family: None,
                },
                Arc::new(|_| Box::pin(async { Ok(()) })),
            );
            let mut config = test_loop_config();
            config.on_rendered_request = Some(arming_sink(CaptureScopeKind::Inference));
            {
                config.provider_call.capture = Some(scope);
                async {
                    let mut stream = Box::pin(crate::loop_stream::run_loop_stream(
                        model,
                        Some(hook),
                        TaggedMessage::unassociated(Message::user("work")),
                        Vec::new(),
                        tools,
                        config,
                    ));
                    loop {
                        match stream
                            .next()
                            .await
                            .expect("stream item")
                            .expect("stream succeeds")
                        {
                            crate::loop_stream::LoopStreamItem::AuthoredInputReady {
                                reply,
                                ..
                            }
                            | crate::loop_stream::LoopStreamItem::SteeringBoundary { reply } => {
                                reply.send(Ok(Vec::new())).unwrap();
                            }
                            crate::loop_stream::LoopStreamItem::ProviderTurnReady { .. } => break,
                            _ => {}
                        }
                    }
                    assert!(
                        !observed
                            .calls()
                            .iter()
                            .any(|call| matches!(call, RecordedCall::ToolCall { .. })),
                        "turn publication precedes admission"
                    );
                    let mut next = Box::pin(stream.next());
                    tokio::select! {
                        () = gate.entered.notified() => {},
                        result = &mut next => panic!("hook did not hold admission: {result:?}"),
                    }
                    drop(next);
                    drop(stream);
                    gate.release.notify_one();
                }
            }
            .await;
            invocations.push(invoked.load(std::sync::atomic::Ordering::SeqCst) != 0);
        }
        assert!(
            invocations.windows(2).all(|pair| pair[0] == pair[1]),
            "{invocations:?}"
        );
        case.check(invocations[0]);
    }
}

#[tokio::test]
async fn empty_output_is_retried_through_the_owned_loop() {
    let model = scripted(vec![
        vec![RawStreamingChoice::FinalResponse(())],
        vec![
            RawStreamingChoice::Message("recovered".into()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let mut config = test_loop_config();
    config.retry_policy.transport_backoff = vec![std::time::Duration::ZERO];
    let answer = run_loop_to_text(
        model.clone(),
        Message::user("hello"),
        Vec::new(),
        Arc::new(Vec::new()),
        config,
    )
    .await
    .expect("an empty provider response should be resampled");
    assert_eq!(answer, "recovered");
    assert_eq!(model.calls(), 2);
}

#[tokio::test]
async fn exhausted_backend_failure_retains_its_typed_availability_verdict() {
    use futures::StreamExt;
    let mut config = test_loop_config();
    config.retry_policy = crate::completion_retry::CompletionRetryPolicy::no_retry();
    let mut stream = Box::pin(crate::loop_stream::run_loop_stream::<
        _,
        crate::session_hook::NoopSessionHook,
    >(
        ScriptedProvider::responding(|_| Reply::provider_error("HTTP status 503: unavailable"))
            .claiming_capture(),
        None,
        TaggedMessage::unassociated(Message::user("hello")),
        Vec::new(),
        Arc::new(Vec::new()),
        config,
    ));
    while let Some(item) = stream.next().await {
        if let Err(error) = item {
            assert!(matches!(
                error.loop_verdict(),
                Some((crate::error::LoopVerdict::BackendUnavailable, _))
            ));
            return;
        }
    }
    panic!("unavailable provider must produce a typed failure");
}
