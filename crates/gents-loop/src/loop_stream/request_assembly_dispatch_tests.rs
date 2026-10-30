use std::sync::Arc;

use gents_protocol::message::Message;
use rig::completion::CompletionRequest;

use super::prepare_dispatch_attempt;
use crate::backend_provider::BackendProviderKind;
use crate::completion_retry::CompletionRetryPolicy;
use crate::loop_stream::LoopConfig;
use crate::openai_wire::OpenAiWireApi;
use crate::provider_input::budget::ContextCalibration;
use crate::provider_input::ProviderInputCounter;

const WINDOW: usize = 4_000;

fn request(text: &str) -> CompletionRequest {
    CompletionRequest {
        model: None,
        preamble: None,
        chat_history: rig::OneOrMany::one(crate::rig_compat::to_rig_message(&Message::user(text))),
        documents: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_tokens: Some(2_048),
        tool_choice: None,
        additional_params: None,
        output_schema: None,
    }
}

fn claude_config_without_compactor(calibration: ContextCalibration) -> LoopConfig {
    LoopConfig {
        replay: crate::loop_stream::LoopReplayInput::default(),
        provider_input_counter: Arc::new(ProviderInputCounter::new(
            BackendProviderKind::AnthropicApiKey,
            OpenAiWireApi::ChatCompletions,
            "claude-test",
        )),
        preamble: None,
        context_message: None,
        temperature: None,
        max_tokens: Some(2_048),
        aggregate_token_budget: None,
        additional_params: None,
        structured_output: None,
        tool_choice: None,
        on_rendered_request: None,
        turn_compactor: None,
        active_reduction_keys: Vec::new(),
        reduction_chain_keys: Vec::new(),
        initial_turn_index: 0,
        resume_from_history: false,
        context_window: WINDOW,
        compaction_threshold: 0.75,
        context_calibration: calibration,
        retry_policy: CompletionRetryPolicy::no_retry(),
        provider_idle_timeout: None,
        deadline: None,
        max_turns: 1,
        output_obligation_gate: None,
        folded_prompts: Vec::new(),
        authored_input: None,
    }
}

#[test]
fn a_calibrated_overestimate_never_refuses_a_request_the_raw_count_fits() {
    let request = request(&"tool output ".repeat(400));
    let config = claude_config_without_compactor(ContextCalibration::with_ratio(40_000));
    let raw = config
        .provider_input_counter
        .estimate_request(&request)
        .expect("estimate");
    assert!(raw < WINDOW, "raw {raw} must fit the window");
    assert!(
        config.context_calibration.calibrated(raw) > WINDOW,
        "the calibrated count must exceed the window"
    );

    let prepared = prepare_dispatch_attempt(&request, &config, None).expect("dispatches");
    let max_tokens = usize::try_from(prepared.request().max_tokens.expect("output cap"))
        .expect("usize output cap");
    assert!(max_tokens > 0);
    assert!(raw + max_tokens <= WINDOW);
    assert_eq!(prepared.context().raw, raw);
}

#[test]
fn the_raw_count_still_refuses_a_request_that_cannot_fit() {
    let request = request(&"tool output ".repeat(4_000));
    let config = claude_config_without_compactor(ContextCalibration::with_ratio(10_000));
    assert!(prepare_dispatch_attempt(&request, &config, None).is_err());
}
