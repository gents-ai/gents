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
    assert_eq!(value["max_tokens"], 1024);
    assert!(value.get("temperature").is_none());
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
