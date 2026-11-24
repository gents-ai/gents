//! Scripted summary models and reduction fixtures shared with the tests of the
//! layers above that drive a full request.
use crate::llm::message::{AssistantContent, Message, Text, UserContent};
use gents_loop::test_support::scripted_provider::{Reply, ScriptedProvider};
use rig::streaming::RawStreamingChoice;

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

/// A summarizer answering every request with `response`.
pub fn mock_summary_model(response: &str) -> ScriptedProvider {
    let response = response.to_owned();
    ScriptedProvider::responding(move |_| Reply::text(response.clone()))
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
        provider_call: Default::default(),
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
        tool_rebinding: None,
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
        max_invalid_tool_calls: gents_loop::loop_stream::DEFAULT_MAX_INVALID_TOOL_CALLS,
        repeated_tool_failure_limit: gents_loop::loop_stream::DEFAULT_REPEATED_TOOL_FAILURE_LIMIT,
        output_obligation_gate: None,
        folded_prompts: Vec::new(),
        authored_input: None,
        tool_context: gents_loop::tool::ToolCallEnvelope::default(),
    }
}

/// A summarizer answering its calls with `script`, claiming each call's
/// armed capture as the capturing transport would; a call past the script
/// (past the internal retry budget) panics.
pub fn scripted_summary_model(script: Vec<Vec<RawStreamingChoice<()>>>) -> ScriptedProvider {
    ScriptedProvider::turns(script).claiming_capture()
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

pub fn valid_summary_json() -> String {
    serde_json::json!({
        "goal": "Continue the task from the compacted turns.",
        "next_actions": ["Run the pending verification command."]
    })
    .to_string()
}
