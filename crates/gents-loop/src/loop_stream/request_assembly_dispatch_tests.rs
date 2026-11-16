use std::sync::Arc;

use gents_protocol::message::Message;
use rig::completion::CompletionRequest;

use super::dispatch::prepare_dispatch_attempt;
use crate::backend_provider::BackendProviderKind;
use crate::completion_retry::CompletionRetryPolicy;
use crate::loop_stream::LoopConfig;
use crate::openai_wire::OpenAiWireApi;
use crate::provider_input::budget::ContextCalibration;
use crate::provider_input::ProviderInputCounter;

fn request(text: &str, max_tokens: u64) -> CompletionRequest {
    CompletionRequest {
        model: None,
        preamble: None,
        chat_history: rig::OneOrMany::one(crate::rig_compat::to_rig_message(&Message::user(text))),
        documents: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_tokens: Some(max_tokens),
        tool_choice: None,
        additional_params: None,
        output_schema: None,
    }
}

fn claude_config_without_compactor(
    calibration: ContextCalibration,
    context_window: usize,
    max_tokens: u64,
) -> LoopConfig {
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
        max_tokens: Some(max_tokens),
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
        context_window,
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

fn sized_request(config: &LoopConfig, raw: usize, max_tokens: u64) -> CompletionRequest {
    let base = config
        .provider_input_counter
        .estimate_request(&request("x", max_tokens))
        .expect("estimate");
    let request = request(&"x".repeat(1 + (raw - base) * 4), max_tokens);
    assert_eq!(
        config
            .provider_input_counter
            .estimate_request(&request)
            .expect("estimate"),
        raw
    );
    request
}

#[test]
fn dispatch_capacity_matches_the_lean_model() {
    gents_lean_vocab::families::request_assembly_dispatch_capacity::replay(|i| {
        let config = claude_config_without_compactor(
            ContextCalibration::with_ratio(i.ratio),
            i.context_window as usize,
            i.max_output_tokens,
        );
        let request = sized_request(&config, i.raw as usize, i.max_output_tokens);
        match prepare_dispatch_attempt(&request, &config, None) {
            Ok(prepared) => Some({
                assert_eq!(prepared.context().raw, i.raw as usize, "{i:?}");
                gents_lean_vocab::families::request_assembly_dispatch_capacity::Dispatched {
                    max_output_tokens: prepared.request().max_tokens.expect("output cap"),
                    context_tokens: prepared.context().tokens as u64,
                }
            }),
            Err(error) => {
                assert!(
                    gents_protocol::error_chain::find_source::<
                        crate::provider_input::budget::ContextBudgetError,
                    >(&error)
                    .is_some(),
                    "{i:?}: {error:?}"
                );
                None
            }
        }
    });
}
