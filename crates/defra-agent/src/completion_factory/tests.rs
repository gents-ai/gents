use super::*;
use crate::config::SamplingConfig;
use crate::watcher::AgentRequest;

fn request() -> AgentRequest {
    AgentRequest {
        doc_id: String::new(),
        request_id: "request-123".to_string(),
        agent_did: String::new(),
        behavior_id: None,
        session_id: "session-456".to_string(),
        content: String::new(),
        temperature: None,
        top_p: None,
        top_k: None,
        max_tokens: None,
        metadata: None,
        execution_origin: None,
        created_at: String::new(),
        deadline: None,
        subagent_depth: 0,
        caused_by_parent_request_id: None,
        caused_by_parent_tool_call_id: None,
    }
}

#[test]
fn thinking_default_enables_reasoning_capture() {
    // The default outbound request must carry the kwarg that flips vLLM's
    // reasoning trace ON, serialized so it lands as top-level
    // `chat_template_kwargs.enable_thinking=true` in the completion body.
    let value = thinking_default_params().expect("thinking default must be present");

    assert_eq!(value["chat_template_kwargs"]["enable_thinking"], true);
}

#[test]
fn thinking_default_merges_under_provider_and_sampling_params() {
    // Reproduces the merge wired into `loop_config`: thinking default is the
    // leftmost base, with provider params then sampling params merged on top.
    // The result must STILL carry `enable_thinking=true` (nothing clobbers it)
    // while preserving the provider/sampling params alongside it.
    let sampling = SamplingConfig {
        temperature: Some(0.1),
        top_p: Some(0.95),
        top_k: Some(40),
        max_tokens: Some(1024),
    };

    let value = merge_optional_params(
        merge_optional_params(
            thinking_default_params(),
            provider_additional_params(BackendProviderKind::OpenRouter),
        ),
        sampling.additional_params(),
    )
    .expect("merged params should be present");

    assert_eq!(value["chat_template_kwargs"]["enable_thinking"], true);
    assert_eq!(value["provider"]["require_parameters"], true);
    assert_eq!(value["top_p"], 0.95);
    assert_eq!(value["top_k"], 40);
}

#[test]
fn caller_can_override_thinking_default_off() {
    // A caller that explicitly disables thinking deep-merges on top of the
    // default base and wins, proving the default stays overridable.
    let caller_override =
        Some(serde_json::json!({ "chat_template_kwargs": { "enable_thinking": false } }));

    let value = merge_optional_params(thinking_default_params(), caller_override)
        .expect("merged params should be present");

    assert_eq!(value["chat_template_kwargs"]["enable_thinking"], false);
}

/// End-to-end wire-shape proof: the `additional_params` we attach by default in
/// [`loop_config`] must serialize into the OpenAI-compatible completion body as
/// a TOP-LEVEL `chat_template_kwargs` object — that is exactly where vLLM's
/// `--reasoning-parser` reads `enable_thinking` to turn the reasoning trace on.
/// This runs the real rig OpenAI request conversion (the same path the live
/// `CompletionsClient` uses for the d4f backend) and asserts the flattened body,
/// so it proves the kwarg reaches the server without needing a live endpoint.
#[test]
fn thinking_default_serializes_top_level_into_openai_body() {
    use rig::completion::message::{Message, Text, UserContent};
    use rig::one_or_many::OneOrMany;

    // The default additional_params loop_config attaches for an OpenAI-compatible
    // backend with no sampling overrides: just the thinking-on base.
    let additional_params = merge_optional_params(
        merge_optional_params(
            thinking_default_params(),
            provider_additional_params(BackendProviderKind::OpenAiCompatible),
        ),
        SamplingConfig::default().additional_params(),
    );

    let core_req = rig::completion::CompletionRequest {
        model: None,
        preamble: None,
        chat_history: OneOrMany::one(Message::User {
            content: OneOrMany::one(UserContent::Text(Text {
                text: "hi".to_string(),
            })),
        }),
        documents: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_tokens: None,
        tool_choice: None,
        additional_params,
        output_schema: None,
    };

    // Same conversion the live OpenAI CompletionsClient performs before POSTing
    // to `/chat/completions`.
    let openai_req =
        rig::providers::openai::CompletionRequest::try_from(("d4f".to_string(), core_req))
            .expect("openai request conversion should succeed");
    let body = serde_json::to_value(&openai_req).expect("serializing openai request");

    // Flattened to the top level of the request body — NOT nested under any
    // wrapper — which is where vLLM expects it.
    assert_eq!(
        body["chat_template_kwargs"]["enable_thinking"], true,
        "request body must carry top-level chat_template_kwargs.enable_thinking=true; body was {body}"
    );
}

#[test]
fn openrouter_additional_params_require_parameters() {
    let value = provider_additional_params(BackendProviderKind::OpenRouter)
        .expect("OpenRouter should contribute additional params");

    assert_eq!(value["provider"]["require_parameters"], true);
}

#[test]
fn openai_compatible_has_no_provider_specific_additional_params() {
    assert!(provider_additional_params(BackendProviderKind::OpenAiCompatible).is_none());
}

#[test]
fn sampling_additional_params_merge_with_provider_params() {
    let sampling = SamplingConfig {
        temperature: Some(0.1),
        top_p: Some(0.95),
        top_k: Some(40),
        max_tokens: Some(1024),
    };

    let value = merge_optional_params(
        provider_additional_params(BackendProviderKind::OpenRouter),
        sampling.additional_params(),
    )
    .expect("sampling params should be present");

    assert_eq!(value["provider"]["require_parameters"], true);
    assert_eq!(value["top_p"], 0.95);
    assert_eq!(value["top_k"], 40);
    assert!(value.get("max_tokens").is_none());
    assert!(value.get("temperature").is_none());
}

#[test]
fn sampling_additional_params_omit_dedicated_completion_fields() {
    let sampling = SamplingConfig {
        temperature: Some(0.1),
        top_p: None,
        top_k: None,
        max_tokens: Some(1024),
    };

    assert!(sampling.additional_params().is_none());
}

#[test]
fn request_sampling_overrides_behavior_defaults() {
    let defaults = SamplingConfig {
        temperature: Some(0.7),
        top_p: Some(0.9),
        top_k: Some(20),
        max_tokens: Some(2048),
    };
    let request = AgentRequest {
        doc_id: String::new(),
        request_id: String::new(),
        agent_did: String::new(),
        behavior_id: None,
        session_id: String::new(),
        content: String::new(),
        temperature: Some(0.0),
        top_p: None,
        top_k: Some(40),
        max_tokens: Some(512),
        metadata: Some(r#"{"run_id":"foo"}"#.to_string()),
        execution_origin: None,
        created_at: String::new(),
        deadline: None,
        subagent_depth: 0,
        caused_by_parent_request_id: None,
        caused_by_parent_tool_call_id: None,
    };

    let sampling = sampling_for_request(defaults, &request);

    assert_eq!(sampling.temperature, Some(0.0));
    assert_eq!(sampling.top_p, Some(0.9));
    assert_eq!(sampling.top_k, Some(40));
    assert_eq!(sampling.max_tokens, Some(512));
}

#[test]
fn effective_max_tokens_falls_back_to_behavior_budget() {
    assert_eq!(effective_max_tokens(4096, None), Some(4096));
}

#[test]
fn effective_max_tokens_prefers_sampling_override() {
    assert_eq!(effective_max_tokens(4096, Some(512)), Some(512));
}

#[test]
fn openai_cache_scope_prefers_session_id() {
    let value = openai_cache_scope_params(&request()).expect("scope should be present");

    assert_eq!(value["user"], "session-456");
}

#[test]
fn openai_cache_scope_falls_back_to_request_id() {
    let mut request = request();
    request.session_id.clear();

    let value = openai_cache_scope_params(&request).expect("fallback scope should be present");

    assert_eq!(value["user"], "request-123");
}
