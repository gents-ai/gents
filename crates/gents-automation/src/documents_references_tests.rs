use gents_documents::ConfigReferences;
use gents_store::collection::Collection;
use serde_json::json;

#[test]
fn callback_admission_reuses_projection_and_filter_secret_checks() {
    let callback = json!({"callback_id":"callback", "node_did":"owner",
        "handler":{"kind":"built_in","emitter":"create_workspace"}});
    let source =
        json!({"event_source_id":"source", "node_did":"owner", "source_collection":"Records"});
    let binding = json!({"binding_id":"binding", "node_did":"owner", "agent_did":crate::triggers::callback::test_agent_did(),
        "callback_id":"callback", "event_source_id":"source", "input_fields":["name"]});
    let validate = |source, binding| {
        ConfigReferences::from_documents(
            "owner",
            [
                (Collection::Agent, json!({"node_did":"owner", "agent_did":crate::triggers::callback::test_agent_did(), "creation_nonce":"nonce", "inference_profile_id":"profile"})),
                (Collection::InferenceProfile, json!({"node_did":"owner", "profile_id":"profile", "backend_id":"backend", "model_name":"model"})),
                (Collection::InferenceBackend, json!({"node_did":"owner", "backend_id":"backend", "name":"Backend", "provider_kind":"OpenAiCompatible", "endpoint":"http://localhost:8000/v1", "auth":{"kind":"unauthenticated"}})),
                (Collection::Callback, callback.clone()),
                (Collection::EventSource, source),
                (Collection::CallbackBinding, binding),
            ],
        )
        .and_then(|refs| refs.validate())
    };
    validate(source.clone(), binding.clone()).unwrap();
    for fields in [
        json!(["name", "name"]),
        json!(["api_key"]),
        json!(["bad field"]),
    ] {
        let mut bad = binding.clone();
        bad["input_fields"] = fields;
        assert!(validate(source.clone(), bad).is_err());
    }
    let mut bad_filter = source.clone();
    bad_filter["filter"] = json!("{api_key: {_eq: \"secret\"}}");
    assert!(validate(bad_filter, binding.clone()).is_err());
    let mut safe_value = source.clone();
    safe_value["filter"] = json!("{name: {_eq: \"api_key\"}}");
    validate(safe_value, binding.clone()).unwrap();
    let mut unsupported = source;
    unsupported["event_kind"] = json!("updated");
    assert!(validate(unsupported, binding).is_err());
}
