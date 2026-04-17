use super::*;
use crate::config::SamplingConfig;

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
    use crate::watcher::AgentRequest;

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
        created_at: String::new(),
    };

    let sampling = sampling_for_request(defaults, &request);

    assert_eq!(sampling.temperature, Some(0.0));
    assert_eq!(sampling.top_p, Some(0.9));
    assert_eq!(sampling.top_k, Some(40));
    assert_eq!(sampling.max_tokens, Some(512));
}
