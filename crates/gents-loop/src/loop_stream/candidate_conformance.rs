use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use gents_lean_vocab::families::completion_loop_stream as family;
use gents_protocol::message::{AssistantContent, Message, ToolResultContent};
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};
use rig::streaming::{RawStreamingChoice, RawStreamingToolCall, StreamingCompletionResponse};

use super::*;
use crate::backend_provider::BackendProviderKind;
use crate::openai_wire::OpenAiWireApi;
use crate::provider_input::ProviderInputCounter;
use crate::session_hook::NoopSessionHook;
use crate::tool::{BoxFuture, ToolDefinition, ToolError};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Response {
    usage: Option<rig::completion::Usage>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct TypedReply {
    #[expect(dead_code, reason = "the field exists for the structured-output schema")]
    answer: String,
}
impl rig::completion::GetTokenUsage for Response {
    fn token_usage(&self) -> Option<rig::completion::Usage> {
        self.usage
    }
}

#[derive(Clone)]
struct Model {
    calls: Arc<Mutex<VecDeque<family::Call>>>,
    observations: Arc<Mutex<Vec<(bool, bool, bool)>>>,
    requests: Arc<Mutex<Vec<String>>>,
}

fn model(calls: &[family::Call]) -> Model {
    Model {
        calls: Arc::new(Mutex::new(calls.iter().cloned().collect())),
        observations: Default::default(),
        requests: Default::default(),
    }
}

fn has_control(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(text) => text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')),
        serde_json::Value::Array(values) => values.iter().any(has_control),
        serde_json::Value::Object(fields) => fields.values().any(has_control),
        _ => false,
    }
}

fn error(kind: &str) -> CompletionError {
    CompletionError::ProviderError(match kind {
        "parse" => "BadRequestError: Expecting value [same]: line 1 column 28 (char 27)",
        "reasoning" => "400 invalid_request_error: messages.1.content.0: Invalid `signature` in `thinking` block",
        "permanent" => "status code 400: duplicate field max_tokens",
        "rate_limit" => "status code 429: overloaded",
        "connection" => "connection refused before response",
        "overflow" => r#"Claude Messages HTTP 400 Bad Request (request-id -) body={"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 1001685 tokens > 1000000 maximum"}}"#,
        _ => "status code 503: unavailable",
    }.into())
}

#[allow(refining_impl_trait)]
impl CompletionModel for Model {
    type Response = Response;
    type StreamingResponse = Response;
    type Client = ();

    fn make(_: &(), _: impl Into<String>) -> Self {
        model(&[])
    }

    async fn completion(
        &self,
        _: CompletionRequest,
    ) -> Result<CompletionResponse<Response>, CompletionError> {
        unreachable!("streaming owner")
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Response>, CompletionError> {
        self.requests.lock().unwrap().push(format!("{request:?}"));
        let index = self.requests.lock().unwrap().len();
        let messages = request
            .chat_history
            .iter()
            .map(crate::rig_compat::from_rig_message)
            .collect::<Vec<_>>();
        let dirty = messages.iter().any(|message| match message {
            Message::Assistant { content, .. } => content.iter().any(|content| match content {
                AssistantContent::ToolCall(call) => has_control(&call.function.arguments),
                _ => false,
            }),
            _ => false,
        });
        let context = messages.iter().any(is_request_context_message);
        let offered = request.tools.iter().any(|tool| tool.name == "echo");
        self.observations
            .lock()
            .unwrap()
            .push((dirty, context, offered));
        let call = self
            .calls
            .lock()
            .unwrap()
            .pop_front()
            .expect("model supplied every provider attempt");
        let mut chunks = Vec::new();
        chunks.extend(
            call.text
                .iter()
                .cloned()
                .map(RawStreamingChoice::Message)
                .map(Ok),
        );
        chunks.extend(call.reasoning.iter().cloned().map(|reasoning| {
            Ok(RawStreamingChoice::ReasoningDelta {
                id: None,
                reasoning,
            })
        }));
        if call.tool {
            chunks.push(Ok(RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                format!("call-{index}"),
                "echo".into(),
                serde_json::json!({"note":if call.poison { "bad\u{7}value" } else { "clean" }}),
            ))));
        }
        if let Some(kind) = &call.failure {
            if chunks.is_empty() && !call.first_poll {
                return Err(error(kind));
            }
            chunks.push(Err(error(kind)));
        } else {
            chunks.push(Ok(RawStreamingChoice::FinalResponse(Response {
                usage: call.usage.map(|usage| rig::completion::Usage {
                    input_tokens: usage.input,
                    output_tokens: usage.output,
                    total_tokens: usage.total,
                    cached_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                }),
            })));
        }
        Ok(StreamingCompletionResponse::stream(Box::pin(
            futures::stream::iter(chunks),
        )))
    }
}

struct Echo;
impl ToolDyn for Echo {
    fn name(&self) -> String {
        "echo".into()
    }
    fn definition<'a>(&'a self, _: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "echo".into(),
                description: "echo".into(),
                parameters: serde_json::json!({"type":"object"}),
            }
        })
    }
    fn call<'a>(&'a self, _: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async { Ok("ECHOED".into()) })
    }
}

fn config(
    input: &family::Input,
    captures: Arc<Mutex<Vec<(family::Capture, String)>>>,
    allowances: Arc<Mutex<Vec<u64>>>,
) -> LoopConfig {
    let has_limit = input.limit.is_some();
    let arming = input.missing_capture.then(|| {
        crate::rendered_request::scope::ambient_arming_sink(
            gents_protocol::rendered_request::CaptureScopeKind::Inference,
        )
    });
    LoopConfig {
        replay: Default::default(),
        provider_input_counter: Arc::new(ProviderInputCounter::new(
            BackendProviderKind::OpenAiCompatible,
            OpenAiWireApi::ChatCompletions,
            "test",
        )),
        preamble: None,
        context_message: input
            .context
            .then(|| Message::user("<context>\nrepair-test\n</context>")),
        temperature: None,
        max_tokens: input.limit.map(|_| {
            if input.nested_usage.is_some() {
                6000
            } else {
                50_000
            }
        }),
        aggregate_token_budget: input.limit.map(AggregateTokenBudget::new),
        additional_params: None,
        structured_output: input
            .structured
            .then(StructuredOutputConfig::for_type::<TypedReply>),
        tool_choice: None,
        configuration_guard: None,
        on_rendered_request: Some(Arc::new(move |turn, attempt, request, trace| {
            let captures = captures.clone();
            let arming = arming.clone();
            let allowances = allowances.clone();
            Box::pin(async move {
                let request_debug = format!("{request:?}");
                if has_limit {
                    let accounting = trace
                        .context_accounting
                        .as_ref()
                        .expect("accounted request");
                    allowances.lock().unwrap().push(
                        accounting.estimated_input_tokens as u64 + request.max_tokens.unwrap(),
                    );
                }
                let capture = family::Capture {
                    turn: turn as u64,
                    attempt: u64::from(attempt),
                    path: match trace.build_path {
                        crate::rendered_request::AssemblyBuildPath::Budgeted => "budgeted",
                        crate::rendered_request::AssemblyBuildPath::Repair => "repair",
                    }
                    .into(),
                    effective: trace.effective_messages.is_some(),
                    results: trace
                        .threaded_tool_results
                        .iter()
                        .map(|result| {
                            result
                                .content
                                .iter()
                                .filter_map(|content| match content {
                                    ToolResultContent::Text(text) => Some(text.text.as_str()),
                                    _ => None,
                                })
                                .collect::<String>()
                        })
                        .collect(),
                    matches_transport: false,
                };
                captures.lock().unwrap().push((capture, request_debug));
                if let Some(arming) = arming {
                    arming(turn, attempt, request, trace).await?;
                }
                Ok(())
            })
        })),
        turn_compactor: None,
        active_reduction_keys: Vec::new(),
        reduction_chain_keys: Vec::new(),
        initial_turn_index: 0,
        resume_from_history: false,
        context_window: if input.nested_usage.is_some() {
            6500
        } else {
            1_000_000
        },
        compaction_threshold: if input.nested_usage.is_some() {
            0.25
        } else {
            0.75
        },
        context_calibration: Default::default(),
        retry_policy: CompletionRetryPolicy {
            transport_backoff: vec![Duration::from_millis(5); input.transport as usize],
            max_resample: input.resample as u32,
            max_empty_resample: 1,
            allow_repair: input.repair,
            account_hops: 0,
        },
        provider_idle_timeout: None,
        deadline: (!input.deadline_fits).then(chrono::Utc::now),
        max_turns: input.max_turns as usize,
        output_obligation_gate: None,
        folded_prompts: Vec::new(),
        authored_input: None,
    }
}

fn history(input: &family::Input) -> Vec<TaggedMessage> {
    if input.recovered_pair {
        return vec![TaggedMessage::unassociated(Message::Assistant {
            request_id: None,
            id: None,
            content: vec![AssistantContent::ToolCall(
                gents_protocol::message::ToolCall {
                    raw_arguments: None,
                    id: "restored".into(),
                    call_id: Some("restored".into()),
                    function: gents_protocol::message::ToolFunction {
                        name: "read".into(),
                        arguments: serde_json::json!({}),
                    },
                    signature: None,
                    additional_params: None,
                },
            )],
        })];
    }
    if !input.initial_dirty {
        return Vec::new();
    }
    vec![
        Message::user("earlier"),
        Message::Assistant {
            request_id: None,
            id: None,
            content: vec![AssistantContent::ToolCall(
                gents_protocol::message::ToolCall {
                    raw_arguments: None,
                    id: "historic".into(),
                    call_id: Some("historic".into()),
                    function: gents_protocol::message::ToolFunction {
                        name: "echo".into(),
                        arguments: serde_json::json!({"note":"bad\u{7}value"}),
                    },
                    signature: None,
                    additional_params: None,
                },
            )],
        },
        Message::User {
            content: vec![gents_protocol::message::UserContent::tool_result(
                "historic",
                vec![ToolResultContent::text("ok")],
            )],
        },
    ]
    .into_iter()
    .map(TaggedMessage::unassociated)
    .collect()
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Failure {
    TurnLimit,
    BudgetExhausted,
    BudgetOverrun,
    UsageMissing,
    CaptureMissing,
    RetryExhausted,
    Refused,
    ContextOverflow,
}

/// Requests persist a failure as its rendered reason, and readers classify it by
/// the reason's leading token; no typed cause separates these completion failures.
fn failure(error: &crate::error::LoopStreamError) -> Failure {
    use crate::error::{CompletionFailure, LoopFailureCause, LoopVerdict};
    let reason = error.to_string();
    let says = |token: &str| reason.contains(token);
    match error.cause() {
        LoopFailureCause::MaxTurns => Failure::TurnLimit,
        LoopFailureCause::Completion {
            failure:
                CompletionFailure::Loop {
                    verdict: LoopVerdict::AggregateTokenBudgetExhausted,
                    ..
                },
            ..
        } => Failure::BudgetExhausted,
        _ if says(AGGREGATE_TOKEN_BUDGET_EXHAUSTED_PREFIX) => Failure::BudgetExhausted,
        _ if says("aggregate_token_budget_overrun: ") => Failure::BudgetOverrun,
        _ if says("aggregate_token_usage_missing: ") => Failure::UsageMissing,
        _ if says("missing its capturing transport") => Failure::CaptureMissing,
        _ if says("retry budget exhausted") => Failure::RetryExhausted,
        _ if says("has no per-turn compactor") => Failure::ContextOverflow,
        _ => Failure::Refused,
    }
}

async fn observe(input: &family::Input) -> family::Observation<Failure> {
    let model = model(&input.calls);
    let captures = Arc::new(Mutex::new(Vec::new()));
    let allowances = Arc::new(Mutex::new(Vec::new()));
    let mut config = config(input, captures.clone(), allowances.clone());
    let budget = config.aggregate_token_budget.clone();
    if let Some(usage) = input.nested_usage.clone() {
        let budget = budget.clone().unwrap();
        config.turn_compactor = Some(Arc::new(move |_| {
            let usage = usage.clone();
            let budget = budget.clone();
            Box::pin(async move {
                let child: family::Call = serde_json::from_value(serde_json::json!({
                    "failure": null, "text": ["summary"], "reasoning": [], "tool": false,
                    "first_poll": false, "poison": false,
                    "usage": {"input":usage.input,"output":usage.output,"total":usage.total},
                }))
                .unwrap();
                let input: family::Input = serde_json::from_value(serde_json::json!({
                    "calls": [], "transport": 0, "resample": 0, "repair": false,
                    "deadline_fits": true, "max_turns": 0, "context": false,
                    "initial_dirty": false, "missing_capture": false, "limit": null, "nested_usage": null, "structured": false, "recovered_pair": false, "compactor": false,
                })).unwrap();
                let mut child_config = self::config(&input, Default::default(), Default::default());
                child_config.aggregate_token_budget = Some(budget);
                run_loop_to_text(
                    self::model(&[child]),
                    Message::user("summarize"),
                    Vec::new(),
                    Arc::new(Vec::new()),
                    child_config,
                )
                .await
                .map_err(crate::compaction::CompactionError::GuidedBudgetExhausted)?;
                Ok(TurnCompactionOutcome::Reduced {
                    messages: vec![TaggedMessage::unassociated(Message::user(
                        "compacted prompt",
                    ))],
                    reduction_key: "reduction-1".into(),
                    commit: TurnReductionCommit::new(|| async { Ok(()) }),
                })
            })
        }));
    }
    if input.compactor {
        let reductions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        config.turn_compactor = Some(Arc::new(move |_| {
            let key = reductions.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move {
                Ok(TurnCompactionOutcome::Reduced {
                    messages: vec![TaggedMessage::unassociated(Message::user(
                        "compacted prompt",
                    ))],
                    reduction_key: format!("overflow-reduction-{key}"),
                    commit: TurnReductionCommit::new(|| async { Ok(()) }),
                })
            })
        }));
    }
    let prompt = if input.nested_usage.is_some() {
        "x".repeat(8000)
    } else {
        "hi".to_owned()
    };
    let stream = run_loop_stream(
        model.clone(),
        None::<NoopSessionHook>,
        TaggedMessage::unassociated(if input.recovered_pair {
            Message::User {
                content: vec![gents_protocol::message::UserContent::tool_result(
                    "restored",
                    vec![ToolResultContent::text("restored")],
                )],
            }
        } else {
            Message::user(prompt)
        }),
        history(input),
        Arc::new(vec![Box::new(Echo) as Box<dyn ToolDyn>]),
        config,
    );
    futures::pin_mut!(stream);
    let mut observed = family::Observation {
        text: Vec::new(),
        reasoning: Vec::new(),
        tools: Vec::new(),
        retractions: Vec::new(),
        retries: Vec::new(),
        final_text: None,
        failure: None,
        dirty_inputs: Vec::new(),
        context_inputs: Vec::new(),
        captures: Vec::new(),
        allowances: Vec::new(),
        ledger_used: None,
        offered: Vec::new(),
    };
    while let Some(item) = stream.next().await {
        match item {
            Ok(LoopStreamItem::Text(text)) => observed.text.push(text),
            Ok(LoopStreamItem::ReasoningDelta { reasoning, .. }) => {
                observed.reasoning.push(reasoning)
            }
            Ok(LoopStreamItem::ToolResult { tool_result, .. }) => {
                for content in tool_result.content {
                    if let ToolResultContent::Text(text) = content {
                        observed.tools.push(text.text);
                    }
                }
            }
            Ok(LoopStreamItem::TurnRetracted { turn, attempt, .. }) => {
                observed.retractions.push(family::Coordinate {
                    turn: turn as u64,
                    attempt: u64::from(attempt),
                })
            }
            Ok(LoopStreamItem::AttemptFailed {
                turn,
                attempt,
                will_retry,
                backoff,
                ..
            }) => observed.retries.push(family::Retry {
                turn: turn as u64,
                attempt: u64::from(attempt),
                retry: will_retry,
                delayed: !backoff.is_zero(),
            }),
            Ok(LoopStreamItem::Final { text }) => observed.final_text = Some(text),
            Ok(_) => {}
            Err(error) => {
                observed.failure = Some(failure(&error));
                break;
            }
        }
    }
    let inputs = model.observations.lock().unwrap();
    observed.dirty_inputs = inputs.iter().map(|i| i.0).collect();
    observed.context_inputs = inputs.iter().map(|i| i.1).collect();
    observed.offered = inputs.iter().map(|i| i.2).collect();
    let requests = model.requests.lock().unwrap();
    observed.captures = captures
        .lock()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(index, (capture, request))| {
            let mut capture = capture.clone();
            capture.matches_transport = requests.get(index) == Some(request);
            capture
        })
        .collect();
    observed.allowances = allowances.lock().unwrap().clone();
    observed.ledger_used =
        budget.and_then(|budget| budget.snapshot().ok().map(|ledger| ledger.used));
    observed
}

#[tokio::test(start_paused = true)]
async fn generated_completion_stream_traces_replay_retry_publication_and_repair() {
    for case in family::cases::<Failure>() {
        let result = if case.input.missing_capture {
            use crate::rendered_request::scope::{scope_request, test_scope};
            use crate::rendered_request::{RenderedRequestCaptureSink, RenderedRequestContext};
            let context = RenderedRequestContext {
                request_doc_id: "doc".into(),
                request_commit_cid: "bafy-commit".into(),
                request_id: "request".into(),
                node_did: "did:test:node".into(),
                requester_did: String::new(),
                agent_did: "agent".into(),
                session_id: "session".into(),
                model_name: "model".into(),
                provider_family: None,
            };
            let sink: RenderedRequestCaptureSink = Arc::new(|_| Box::pin(async { Ok(()) }));
            scope_request(test_scope(context, sink), observe(&case.input)).await
        } else {
            observe(&case.input).await
        };
        case.check(result);
    }
}

#[tokio::test(start_paused = true)]
async fn generated_completion_text_discards_retracted_previews() {
    use gents_lean_vocab::families::completion_loop_text as family;
    for case in family::cases() {
        let wire = serde_json::to_value(&case.input).unwrap();
        let input: gents_lean_vocab::families::completion_loop_stream::Input =
            serde_json::from_value(wire).unwrap();
        let result = run_loop_to_text(
            model(&input.calls),
            Message::user("hi"),
            Vec::new(),
            Arc::new(vec![Box::new(Echo) as Box<dyn ToolDyn>]),
            config(&input, Default::default(), Default::default()),
        )
        .await;
        case.check(family::TextObservation {
            failed: result.is_err(),
            text: result.ok(),
        });
    }
}
