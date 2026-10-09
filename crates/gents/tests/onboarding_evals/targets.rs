use crate::support::live_inference::{parse_selection, targets_dir, InferenceTarget};

#[test]
fn checked_in_targets_decode_through_runtime_configuration() {
    let mut names = std::fs::read_dir(targets_dir())
        .expect("inference targets directory")
        .map(|entry| entry.expect("target entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .map(|path| path.file_stem().unwrap().to_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    names.sort();
    assert!(!names.is_empty(), "no checked-in inference targets");
    for name in names {
        let target = InferenceTarget::load(&name).unwrap_or_else(|error| panic!("{error:#}"));
        assert_eq!(target.name, name);
        assert!(
            !matches!(
                target.auth(),
                gents::document_config::BackendAuth::ApiKey { .. }
            ),
            "{name} stores a key"
        );
        let backend = target.backend("did:key:owner");
        assert_eq!(backend.node_did, "did:key:owner");
        gents::document_config::InferenceBackend::from_value(
            &serde_json::to_value(&backend).unwrap(),
        )
        .unwrap();
        assert_eq!(
            target.profile("did:key:owner").backend_id,
            backend.backend_id
        );
    }
}

#[test]
fn targets_reject_extra_documents_owners_and_dangling_profiles() {
    let backend = serde_json::json!({
        "backend_id": "b", "name": "b", "provider_kind": "OpenAiCompatible",
        "endpoint": "http://127.0.0.1:9/v1", "auth": {"kind": "unauthenticated"}
    });
    let profile = serde_json::json!({"profile_id": "p", "backend_id": "b", "model_name": "m"});
    let decode = |value| InferenceTarget::decode("t".into(), value);
    let target = decode(serde_json::json!({
        "node": {}, "inference_backends": [backend], "inference_profiles": [profile]
    }))
    .unwrap();
    assert_eq!((target.model(), target.backend_id()), ("m", "b"));
    for invalid in [
        serde_json::json!({"node": {}, "inference_backends": [backend]}),
        serde_json::json!({"node": {}, "inference_backends": [backend, backend], "inference_profiles": [profile]}),
        serde_json::json!({"node": {"display_name": "x"}, "inference_backends": [backend], "inference_profiles": [profile]}),
        serde_json::json!({"node": {"node_did": "did:key:other"}, "inference_backends": [backend], "inference_profiles": [profile]}),
        serde_json::json!({"node": {}, "inference_backends": [backend], "inference_profiles": [{"profile_id": "p", "backend_id": "missing", "model_name": "m"}]}),
        serde_json::json!({"node": {}, "inference_backends": [backend], "inference_profiles": [profile], "contexts": [{"context_id": "c"}]}),
        serde_json::json!({"node": {}, "inference_backends": [{"backend_id": "b", "name": "b", "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:9/v1", "auth": {"kind": "environment", "variable": " "}}], "inference_profiles": [profile]}),
    ] {
        assert!(decode(invalid.clone()).is_err(), "accepted {invalid}");
    }
}

#[test]
fn target_selection_dedupes_files_and_rejects_name_collisions() {
    let names = |value: &str| {
        parse_selection(value).map(|selected| {
            selected
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(
        names(" workstation-1, ,openrouter,,workstation-1 ").unwrap(),
        ["workstation-1", "openrouter"]
    );
    assert_eq!(
        names("workstation-1,scripts/evals/targets/workstation-1.json").unwrap(),
        ["workstation-1"]
    );
    let foreign = tempfile::tempdir().unwrap();
    let copy = foreign.path().join("workstation-1.json");
    std::fs::copy(targets_dir().join("workstation-1.json"), &copy).unwrap();
    let error = names(&format!("workstation-1,{}", copy.display())).unwrap_err();
    assert!(
        error.to_string().contains("share the name workstation-1"),
        "{error}"
    );
    assert!(names(" , ").is_err());
}

#[test]
fn targets_are_literal_and_never_hold_inline_keys() {
    let profile = serde_json::json!({"profile_id": "p", "backend_id": "b", "model_name": "m"});
    let backend = |endpoint: &str, auth: serde_json::Value| {
        serde_json::json!({
            "node": {},
            "inference_backends": [{
                "backend_id": "b", "name": "b", "provider_kind": "OpenAiCompatible",
                "endpoint": endpoint, "auth": auth
            }],
            "inference_profiles": [profile]
        })
    };
    assert!(InferenceTarget::decode(
        "t".into(),
        backend(
            "http://127.0.0.1:9/v1",
            serde_json::json!({"kind": "api_key", "key": "k"})
        )
    )
    .is_err());
    let error = InferenceTarget::decode(
        "t".into(),
        serde_json::json!({
            "node": {},
            "inference_backends": [{
                "backend_id": "b", "name": "b", "provider_kind": "ClaudeCliSubscription",
                "endpoint": "https://api.anthropic.com", "auth": {"kind": "node_oauth"}
            }],
            "inference_profiles": [profile]
        }),
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("NodeOAuth targets are not supported for fresh-node evals"),
        "{error:#}"
    );
    // `${PATH}` is set in every test process; a literal target must not expand it.
    assert!(InferenceTarget::decode(
        "t".into(),
        backend(
            "http://${PATH}/v1",
            serde_json::json!({"kind": "unauthenticated"})
        )
    )
    .is_err_and(|error| format!("{error:#}").contains("PATH")));
}
