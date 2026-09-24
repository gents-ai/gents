//! Proves the loop is usable with no DefraDB and no network: a scripted
//! in-memory [`CompletionModel`] streams a tool call on turn one and a final
//! answer on turn two, an in-memory [`SessionHook`] records every
//! persistence call, and a plain [`ToolDyn`] tool is dispatched in between.
//! Nothing here touches a socket or a filesystem.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use gents_protocol::message::Message;
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};
use rig::streaming::{RawStreamingChoice, RawStreamingToolCall, StreamingCompletionResponse};

use crate::backend_provider::BackendProviderKind;
use crate::loop_stream::{run_loop_to_text, LoopConfig};
use crate::openai_wire::OpenAiWireApi;
use crate::provider_input::ProviderInputCounter;
use crate::session_hook::SessionHook;
use crate::tool::{Tool, ToolDefinition, ToolDyn};
use crate::tool_call_lifecycle::ToolOutcome;
use crate::{HookAction, ToolCallHookAction};

/// A `CompletionModel` that replays a fixed script of streamed responses, one
/// script entry per `stream` call: no network, no rig provider.
#[derive(Clone, Default)]
struct ScriptedModel {
    turns: Arc<Mutex<Vec<Vec<RawStreamingChoice<()>>>>>,
}

impl ScriptedModel {
    fn new(turns: Vec<Vec<RawStreamingChoice<()>>>) -> Self {
        Self {
            turns: Arc::new(Mutex::new(turns.into_iter().rev().collect())),
        }
    }
}

#[allow(refining_impl_trait)]
impl CompletionModel for ScriptedModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_client: &Self::Client, _model: impl Into<String>) -> Self {
        Self::default()
    }

    async fn completion(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        unreachable!("the owned loop only ever calls stream()")
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        // This scripted provider has no transport. Claim the modeled attempt
        // when scoped; this fixture does not establish durable input capture.
        let _ = crate::rendered_request::scope::claim_pending();
        let items = self
            .turns
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .pop()
            .expect("scripted model called more times than it was scripted for");
        let items: Vec<Result<RawStreamingChoice<()>, CompletionError>> =
            items.into_iter().map(Ok).collect();
        Ok(StreamingCompletionResponse::stream(Box::pin(
            futures::stream::iter(items),
        )))
    }
}

/// A tool with no filesystem, socket, or DefraDB access at all: it just
/// echoes its argument back, uppercased.
struct EchoTool {
    calls: Arc<Mutex<Vec<String>>>,
    policy_allows: bool,
}

#[derive(serde::Deserialize)]
struct EchoArgs {
    text: String,
}

impl Tool for EchoTool {
    const NAME: &'static str = "echo";
    type Error = crate::tool::ToolError;
    type Args = EchoArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Echoes its input, uppercased.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"],
            }),
        }
    }

    fn admit(&self, _args: &Self::Args) -> Result<(), Self::Error> {
        if self.policy_allows {
            Ok(())
        } else {
            Err(crate::tool::ToolError::ReportedFailure {
                class: crate::tool_call_lifecycle::FailureClass::PolicyDenied,
                text: "echo denied by tool policy".into(),
            })
        }
    }

    fn into_dyn_error(error: Self::Error) -> crate::tool::ToolError {
        error
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        self.calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(args.text.clone());
        Ok(args.text.to_uppercase())
    }
}

/// Every persistence call the loop made, in order, with no DefraDB behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RecordedCall {
    CompletionCall { prompt: String },
    ToolCall { tool_name: String },
    AdmissionRejected { tool_name: String, outcome: String },
    ToolResult { tool_name: String, outcome: String },
}

#[derive(Clone, Default)]
struct RecordingHook {
    log: Arc<Mutex<Vec<RecordedCall>>>,
    deny_dispatch: bool,
}

impl RecordingHook {
    fn log(&self, call: RecordedCall) {
        self.log
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(call);
    }

    fn calls(&self) -> Vec<RecordedCall> {
        self.log
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
}

#[async_trait]
impl SessionHook for RecordingHook {
    async fn on_completion_call_with_context(
        &self,
        prompt: &Message,
        _history: &[Message],
        _context: Option<&Message>,
    ) -> HookAction {
        self.log(RecordedCall::CompletionCall {
            prompt: prompt.rag_text().unwrap_or_default(),
        });
        HookAction::Continue
    }

    async fn on_tool_call(
        &self,
        tool_name: &str,
        _tool_call_id: Option<String>,
        _internal_call_id: &str,
        _args: &str,
    ) -> ToolCallHookAction {
        self.log(RecordedCall::ToolCall {
            tool_name: tool_name.to_string(),
        });
        if self.deny_dispatch {
            ToolCallHookAction::Terminate {
                reason: "dispatch not acknowledged".to_owned(),
            }
        } else {
            ToolCallHookAction::Continue
        }
    }

    async fn on_tool_admission_rejected(
        &self,
        tool_name: &str,
        _tool_call_id: Option<String>,
        _internal_call_id: &str,
        _args: &str,
        outcome: &ToolOutcome,
    ) -> HookAction {
        self.log(RecordedCall::AdmissionRejected {
            tool_name: tool_name.to_string(),
            outcome: outcome.model_facing_text().to_string(),
        });
        HookAction::Continue
    }

    async fn on_tool_result(
        &self,
        tool_name: &str,
        _tool_call_id: Option<String>,
        _internal_call_id: &str,
        _args: &str,
        outcome: &ToolOutcome,
    ) -> HookAction {
        self.log(RecordedCall::ToolResult {
            tool_name: tool_name.to_string(),
            outcome: outcome.model_facing_text().to_string(),
        });
        HookAction::Continue
    }

    async fn foreground_live_output_writer(
        &self,
        internal_call_id: &str,
    ) -> crate::live_output::LiveToolOutputWriter {
        crate::live_output::LiveToolOutputRegistry::default()
            .writer_for(internal_call_id.to_string())
            .await
    }

    async fn session_id(&self) -> Option<String> {
        Some("in-memory-session".to_string())
    }

    async fn register_stream_tool_call_identity(
        &self,
        _internal_call_id: &str,
        _result_id: &str,
        _call_id: Option<&str>,
    ) {
    }
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

fn test_loop_config() -> LoopConfig {
    LoopConfig {
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
        on_rendered_request: None,
        turn_compactor: None,
        active_reduction_keys: Vec::new(),
        reduction_chain_keys: Vec::new(),
        initial_turn_index: 0,
        context_window: 128_000,
        compaction_threshold: 0.75,
        retry_policy: crate::completion_retry::CompletionRetryPolicy::interactive_default(),
        provider_idle_timeout: None,
        deadline: None,
        max_turns: 8,
        output_obligation_gate: None,
    }
}

#[tokio::test]
async fn the_loop_dispatches_a_tool_and_threads_messages_with_no_defradb_and_no_network() {
    let model = ScriptedModel::new(vec![
        vec![scripted_tool_call(), RawStreamingChoice::FinalResponse(())],
        vec![
            RawStreamingChoice::Message("done".to_string()),
            RawStreamingChoice::FinalResponse(()),
        ],
    ]);
    let tool_calls = Arc::new(Mutex::new(Vec::new()));
    let tools: Arc<Vec<Box<dyn ToolDyn>>> = Arc::new(vec![Box::new(EchoTool {
        calls: tool_calls.clone(),
        policy_allows: true,
    })]);
    let final_text = run_loop_to_text(
        model,
        Message::user("please echo hi"),
        Vec::new(),
        tools,
        test_loop_config(),
    )
    .await
    .expect("the loop must complete with no DefraDB and no network");

    assert_eq!(final_text, "done");
    assert_eq!(
        tool_calls.lock().unwrap().as_slice(),
        &["hi".to_string()],
        "the echo tool must have been dispatched exactly once"
    );
}

/// This binds the loop's admission and hook-action boundaries, not database
/// receipt semantics or the native command-policy owner. The native hook tests
/// separately check actual receipt failures and policy settlement against the
/// same generated permission outcomes.
#[tokio::test]
async fn modeled_dispatch_permissions_gate_real_tool_invocation() {
    use futures::StreamExt;
    let contract: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = contract["canonical_dispatch_observation_cases"]
        .as_array()
        .unwrap();
    assert!(!cases.is_empty());
    assert!(cases.iter().any(|case| case["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|input| !input["policy_allows"].as_bool().unwrap())));
    for case in cases {
        let inputs = case["inputs"].as_array().unwrap();
        let expected_results = case["expected"].as_array().unwrap();
        assert_eq!(inputs.len(), expected_results.len());
        for (input, expected) in inputs.iter().zip(expected_results) {
            let may_invoke = expected["may_invoke"].as_bool().unwrap();
            let policy_allows = input["policy_allows"].as_bool().unwrap();
            let model = ScriptedModel::new(vec![
                vec![scripted_tool_call(), RawStreamingChoice::FinalResponse(())],
                vec![
                    RawStreamingChoice::Message("done".into()),
                    RawStreamingChoice::FinalResponse(()),
                ],
            ]);
            let calls = Arc::new(Mutex::new(Vec::new()));
            let tools: Arc<Vec<Box<dyn ToolDyn>>> = Arc::new(vec![Box::new(EchoTool {
                calls: calls.clone(),
                policy_allows,
            })]);
            let hook = RecordingHook {
                deny_dispatch: policy_allows && !may_invoke,
                ..Default::default()
            };
            let observed_hook = hook.clone();
            use crate::rendered_request::scope::{
                ambient_arming_sink, scope_request, test_scope, CaptureScopeKind,
            };
            let scope = test_scope(
                crate::rendered_request::RenderedRequestContext {
                    request_doc_id: "dispatch-doc".into(),
                    request_commit_cid: "dispatch-cid".into(),
                    request_id: "dispatch-request".into(),
                    agent_did: "did:test:agent".into(),
                    requester_did: "did:test:requester".into(),
                    behavior_id: "general".into(),
                    session_id: "dispatch-session".into(),
                    model_name: "scripted".into(),
                },
                Arc::new(|_| Box::pin(async { Ok(()) })),
            );
            let mut config = test_loop_config();
            config.on_rendered_request = Some(ambient_arming_sink(CaptureScopeKind::Inference));
            let failure = scope_request(scope, async {
                let stream = crate::loop_stream::run_loop_stream(
                    model,
                    Some(hook),
                    Message::user("echo hi"),
                    Vec::new(),
                    tools,
                    config,
                );
                futures::pin_mut!(stream);
                let mut failure = None;
                while let Some(item) = stream.next().await {
                    if let Err(error) = item {
                        failure = Some(error);
                        break;
                    }
                }
                failure
            })
            .await;
            assert_eq!(
                calls.lock().unwrap().len(),
                usize::from(may_invoke),
                "{}: {expected}",
                case["name"]
            );
            let recorded = observed_hook.calls();
            let elections = recorded
                .iter()
                .filter(|call| matches!(call, RecordedCall::ToolCall { .. }))
                .count();
            let rejections = recorded
                .iter()
                .filter(|call| matches!(call, RecordedCall::AdmissionRejected { .. }))
                .count();
            if policy_allows {
                assert_eq!(
                    failure.is_some(),
                    !may_invoke,
                    "{}: {expected}; {failure:?}",
                    case["name"]
                );
                assert_eq!(
                    (elections, rejections),
                    (1, 0),
                    "the test must reach the hook"
                );
            } else {
                assert!(!may_invoke && !expected["running"].as_bool().unwrap());
                assert!(
                    failure.is_none(),
                    "{}: a policy rejection is a tool result, not a loop failure: {failure:?}",
                    case["name"]
                );
                assert_eq!(
                    (elections, rejections),
                    (0, 1),
                    "{}: a rejected call must never reach the dispatch election",
                    case["name"]
                );
            }
        }
    }
}
