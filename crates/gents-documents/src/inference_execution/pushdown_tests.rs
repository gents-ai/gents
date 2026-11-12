use super::InferenceExecution;

#[test]
fn execution_profiles_without_overrides_inherit_platform_defaults() {
    // An execution profile written with no overrides must stay all-None after
    // a round trip: the document never copies platform default values into
    // stored overrides, so later platform-default changes are inherited.
    let bare = serde_json::json!({
        "node_did": "did:key:zNODE",
        "execution_id": "profile-default"
    });
    let profile: InferenceExecution =
        serde_json::from_value(bare.clone()).expect("bare execution profile");
    assert_eq!(profile.max_turns, None);
    assert_eq!(profile.max_total_tokens, None);
    assert_eq!(profile.stream_batch_ms, None);
    assert_eq!(profile.stream_liveness_timeout_secs, None);
    assert_eq!(profile.provider_idle_timeout_secs, None);
    assert_eq!(profile.deadline_duration_secs, None);

    // Round trip through the serialized form keeps the omissions (no copied
    // defaults are materialized into the document).
    let reserialized = serde_json::to_value(&profile).expect("serialize bare profile");
    let round_tripped: InferenceExecution =
        serde_json::from_value(reserialized).expect("re-deserialize bare profile");
    assert_eq!(round_tripped, profile);
    assert_eq!(round_tripped.max_total_tokens, None);

    // An all-None profile is valid: unset fields preserve the owned-loop
    // defaults (stream_batch_ms 1s, liveness 120s, provider idle 300s,
    // deadline 86,400s) instead of failing validation.
    assert!(
        profile.validation_violations().is_empty(),
        "a profile with no overrides must validate against platform defaults"
    );

    // An explicit per-request token cap is honored and preserved, and stays
    // distinct from the goal-level budget contract.
    let explicit = serde_json::from_value::<InferenceExecution>(serde_json::json!({
        "node_did": "did:key:zNODE",
        "execution_id": "profile-explicit",
        "max_total_tokens": 50_000
    }))
    .expect("profile with explicit cap");
    assert_eq!(explicit.max_total_tokens, Some(50_000));
    assert!(explicit.validation_violations().is_empty());
}
