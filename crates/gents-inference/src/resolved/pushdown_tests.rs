use super::{validate_advertised_context_override, SamplingConfig};
use gents_loop::backend_provider::BackendProviderKind;

#[test]
fn profile_seed_rejects_provider_paths_without_seed_support() {
    let sampling = SamplingConfig {
        seed: Some(1234),
        ..Default::default()
    };

    sampling
        .validate_for_provider(
            BackendProviderKind::OpenAiCompatible,
            gents_loop::openai_wire::OpenAiWireApi::ChatCompletions,
        )
        .unwrap();
    sampling
        .validate_for_provider(
            BackendProviderKind::OpenRouter,
            gents_loop::openai_wire::OpenAiWireApi::ChatCompletions,
        )
        .unwrap();
    assert!(sampling
        .validate_for_provider(
            BackendProviderKind::ChatGptCodex,
            gents_loop::openai_wire::OpenAiWireApi::Responses,
        )
        .is_err());
    assert!(sampling
        .validate_for_provider(
            BackendProviderKind::XaiGrokOAuth,
            gents_loop::openai_wire::OpenAiWireApi::ChatCompletions,
        )
        .is_err());
    assert!(sampling
        .validate_for_provider(
            BackendProviderKind::ClaudeCliSubscription,
            gents_loop::openai_wire::OpenAiWireApi::ChatCompletions,
        )
        .is_err());
}

#[test]
fn every_profile_sampling_knob_reaches_the_provider_body() {
    let sampling = SamplingConfig {
        temperature: Some(0.7),
        top_p: Some(0.95),
        top_k: Some(40),
        seed: Some(1234),
        min_p: Some(0.05),
        frequency_penalty: Some(0.5),
        presence_penalty: Some(-0.25),
        repetition_penalty: Some(1.1),
        max_tokens: Some(1024),
        reasoning_effort: None,
    };

    let value = sampling
        .additional_params()
        .expect("pinned sampling knobs must produce provider params");

    assert_eq!(value["top_p"], 0.95);
    assert_eq!(value["top_k"], 40);
    assert_eq!(value["seed"], 1234);
    assert_eq!(value["min_p"], 0.05);
    assert_eq!(value["frequency_penalty"], 0.5);
    assert_eq!(value["presence_penalty"], -0.25);
    assert_eq!(value["repetition_penalty"], 1.1);
    assert!(value.get("temperature").is_none());
    assert!(value.get("max_tokens").is_none());
}

#[test]
fn unpinned_sampling_knobs_emit_no_provider_params() {
    let sampling = SamplingConfig {
        temperature: Some(0.0),
        max_tokens: Some(256),
        ..Default::default()
    };
    assert!(
        sampling.additional_params().is_none(),
        "a profile that pins no body-param knob must not send any"
    );
}

#[test]
fn context_override_admission_matches_lean() {
    use gents_lean_vocab::families::configuration_scope_context_bounds as family;

    family::replay(|i| {
        let mut selected = profile(1);
        selected.context_window = i.selected;
        let model = advertised(i.model_default, i.maximum);
        validate_advertised_context_override(&selected, &model).is_ok()
    });
}

fn profile(context_window: i64) -> gents_documents::InferenceProfile {
    serde_json::from_value(serde_json::json!({
        "node_did": "did:test:owner",
        "profile_id": "profile",
        "backend_id": "backend",
        "model_name": "gpt-5.6-sol",
        "context_window": context_window
    }))
    .unwrap()
}

fn advertised(default: Option<i64>, maximum: Option<i64>) -> gents_documents::AdvertisedModel {
    gents_documents::AdvertisedModel {
        model_name: "gpt-5.6-sol".into(),
        display_name: None,
        context_window: default,
        max_context_window: maximum,
        max_output_tokens: None,
        reasoning_efforts: None,
    }
}
