//! Scripted summary models and reduction fixtures shared with the tests of the
//! layers above that drive a full request.
use std::sync::{Arc, Mutex};

use crate::llm::message::{AssistantContent, Message, Text, UserContent};
use rig::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, Usage,
};
use rig::streaming::{RawStreamingChoice, StreamingCompletionResponse};

use super::*;
use gents_loop::provider_input::ProviderInputProfile;

pub const GROUPED_PROFILE: ProviderInputProfile = ProviderInputProfile::OpenAiChatCompletions;

pub fn text_msg(role: &str, text: &str) -> Message {
    match role {
        "user" => Message::User {
            content: vec![UserContent::Text(Text {
                text: text.to_string(),
            })],
        },
        "assistant" => Message::Assistant {
            request_id: None,
            id: None,
            content: vec![AssistantContent::Text(Text {
                text: text.to_string(),
            })],
        },
        _ => panic!("unknown role"),
    }
}

pub fn test_reduction_admission() -> ReductionAdmission {
    ReductionAdmission::required(1, 0)
}

pub fn required_reduction(result: &ReductionResult) -> ExactReduction<'_> {
    result
        .exact_reduction()
        .expect("the test requires an exact reduction")
}

#[derive(Clone, Default)]
pub struct MockSummaryModel {
    pub response: String,
    pub last_request: Arc<Mutex<Option<CompletionRequest>>>,
    pub requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

impl MockSummaryModel {
    pub fn new(response: &str) -> Self {
        Self {
            response: response.to_string(),
            last_request: Arc::new(Mutex::new(None)),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[allow(refining_impl_trait)]
impl CompletionModel for MockSummaryModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &Self::Client, _: impl Into<String>) -> Self {
        Self::default()
    }

    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        *self.last_request.lock().unwrap() = Some(request.clone());
        self.requests.lock().unwrap().push(request);
        Ok(CompletionResponse {
            choice: rig::one_or_many::OneOrMany::one(rig::completion::AssistantContent::Text(
                rig::completion::message::Text {
                    text: self.response.clone(),
                },
            )),
            usage: Usage::new(),
            raw_response: (),
            message_id: None,
        })
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        *self.last_request.lock().unwrap() = Some(request.clone());
        self.requests.lock().unwrap().push(request);
        let items: Vec<Result<RawStreamingChoice<()>, CompletionError>> = vec![
            Ok(RawStreamingChoice::Message(self.response.clone())),
            Ok(RawStreamingChoice::FinalResponse(())),
        ];
        Ok(StreamingCompletionResponse::stream(Box::pin(
            futures::stream::iter(items),
        )))
    }
}

pub fn summary_worthy_messages() -> Vec<Message> {
    (0..12)
        .flat_map(|turn| {
            [
                text_msg("user", &"x".repeat(800)),
                text_msg("assistant", &format!("response {turn} {}", "y".repeat(400))),
            ]
        })
        .collect()
}

pub fn scheduled_origin_config() -> gents_loop::loop_stream::LoopConfig {
    gents_loop::loop_stream::LoopConfig {
        replay: gents_loop::loop_stream::LoopReplayInput::default(),
        provider_input_counter: std::sync::Arc::new(
            gents_loop::provider_input::ProviderInputCounter::new(
                crate::backends::provider::BackendProviderKind::OpenAiCompatible,
                gents_loop::openai_wire::OpenAiWireApi::ChatCompletions,
                "test-model",
            ),
        ),
        preamble: None,
        context_message: None,
        temperature: None,
        max_tokens: None,
        aggregate_token_budget: None,
        additional_params: None,
        structured_output: None,
        tool_choice: None,
        configuration_guard: None,
        on_rendered_request: None,
        turn_compactor: None,
        active_reduction_keys: Vec::new(),
        reduction_chain_keys: Vec::new(),
        initial_turn_index: 0,
        resume_from_history: false,
        context_window: crate::resolved::DEFAULT_CONTEXT_WINDOW,
        compaction_threshold: crate::resolved::DEFAULT_COMPACTION_THRESHOLD,
        context_calibration: Default::default(),
        retry_policy: gents_loop::completion_retry::CompletionRetryPolicy::scheduled_default(),
        provider_idle_timeout: None,
        deadline: None,
        max_turns: 0,
        output_obligation_gate: None,
        folded_prompts: Vec::new(),
        authored_input: None,
    }
}

#[derive(Clone)]
pub struct ScriptedSummaryModel {
    pub calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub script: Arc<Mutex<std::collections::VecDeque<Vec<RawStreamingChoice<()>>>>>,
    pub requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

impl ScriptedSummaryModel {
    pub fn new(script: Vec<Vec<RawStreamingChoice<()>>>) -> Self {
        Self {
            calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            script: Arc::new(Mutex::new(script.into_iter().collect())),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn empty_turn() -> Vec<RawStreamingChoice<()>> {
        vec![RawStreamingChoice::FinalResponse(())]
    }

    pub fn summary_turn() -> Vec<RawStreamingChoice<()>> {
        vec![
            RawStreamingChoice::Message(valid_summary_json()),
            RawStreamingChoice::FinalResponse(()),
        ]
    }

    pub fn malformed_summary_turn() -> Vec<RawStreamingChoice<()>> {
        vec![
            RawStreamingChoice::Message(
                r#"{"goal":"Continue the task.","key_decisions":["unfinished"#.to_string(),
            ),
            RawStreamingChoice::FinalResponse(()),
        ]
    }

    pub fn schema_invalid_summary_turn() -> Vec<RawStreamingChoice<()>> {
        vec![
            RawStreamingChoice::Message(
                serde_json::json!({
                    "key_decisions": [],
                    "files_read": ["hallucinated.rs"]
                })
                .to_string(),
            ),
            RawStreamingChoice::FinalResponse(()),
        ]
    }
}

#[allow(refining_impl_trait)]
impl CompletionModel for ScriptedSummaryModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &Self::Client, _: impl Into<String>) -> Self {
        Self::new(Vec::new())
    }

    async fn completion(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        unreachable!("compaction summarizes via the owned loop, which streams");
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.requests.lock().unwrap().push(request);
        // Stand in for the capturing transport: a real send claims the armed
        // capture, without which the owned loop's fence fires. No-op outside a
        // capture scope.
        let _ = crate::rendered_request::scope::claim_pending();
        let turn = self
            .script
            .lock()
            .unwrap()
            .pop_front()
            .expect("provider called past the scripted internal retry budget");
        let items: Vec<Result<RawStreamingChoice<()>, CompletionError>> =
            turn.into_iter().map(Ok).collect();
        Ok(StreamingCompletionResponse::stream(Box::pin(
            futures::stream::iter(items),
        )))
    }
}

pub fn valid_summary_json() -> String {
    serde_json::json!({
        "goal": "Continue the task from the compacted turns.",
        "next_actions": ["Run the pending verification command."]
    })
    .to_string()
}
