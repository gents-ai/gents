use super::{InferenceExecution, InferenceRetryPolicy};

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

#[test]
fn retry_backoff_steps_fit_under_the_rate_limit_wait() {
    let policy = |backoff_ms: Vec<i64>, max_rate_limit_wait_ms: Option<i64>| InferenceRetryPolicy {
        node_did: "did:key:zNODE".into(),
        retry_policy_id: "retry".into(),
        backoff_ms: Some(backoff_ms),
        max_rate_limit_wait_ms,
        ..InferenceRetryPolicy::default()
    };
    let default_wait = gents_loop::provider_limit::LONG_RATE_LIMIT_WAIT.as_millis() as i64;

    assert!(policy(vec![1_000, default_wait], None)
        .validation_violations()
        .is_empty());
    assert_eq!(
        policy(vec![1_000, default_wait + 1], None).validation_violations(),
        vec![format!(
            "InferenceRetryPolicy retry backoff_ms entries must not exceed max_rate_limit_wait_ms ({default_wait})"
        )]
    );
    assert!(policy(vec![600_000], Some(900_000))
        .validation_violations()
        .is_empty());
    assert_eq!(
        policy(vec![5_000, 30_000], Some(10_000)).validation_violations(),
        vec![
            "InferenceRetryPolicy retry backoff_ms entries must not exceed max_rate_limit_wait_ms (10000)"
                .to_string()
        ]
    );
}
