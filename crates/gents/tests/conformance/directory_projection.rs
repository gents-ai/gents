use std::collections::{BTreeMap, BTreeSet};

use gents::agent::directory_projection::{derive_directory_entries, BehaviorInfo, CatalogOptions};

fn principal(did: &str, name: &str) -> (String, String, String) {
    (did.to_string(), name.to_string(), String::new())
}

fn principal_with_default(
    did: &str,
    name: &str,
    default_behavior_id: &str,
) -> (String, String, String) {
    (
        did.to_string(),
        name.to_string(),
        default_behavior_id.to_string(),
    )
}

#[test]
fn derivation_projects_exactly_the_principals() {
    let coder = BehaviorInfo {
        behavior_id: "did:key:a:coder".to_string(),
        display_name: "Coder".to_string(),
        backend_id: "openai".to_string(),
        model_name: "gpt-5".to_string(),
        host_root: "/repo/a".to_string(),
        preset: "readonly".to_string(),
        inference_profile_id: "profile-fast".to_string(),
    };
    let artist = BehaviorInfo {
        behavior_id: "did:key:a:artist".to_string(),
        display_name: "Artist".to_string(),
        backend_id: "anthropic".to_string(),
        model_name: "claude".to_string(),
        host_root: String::new(),
        preset: String::new(),
        inference_profile_id: "".to_string(),
    };
    let options = CatalogOptions {
        available_models: vec!["anthropic|claude".to_string(), "openai|gpt-5".to_string()],
        allowed_roots: vec!["/repo/a".to_string()],
        permission_presets: vec!["readonly".to_string(), "write".to_string()],
        available_profiles: vec!["profile-fast|Fast".to_string()],
        // Index-aligned with `available_profiles` (#1050): one entry per
        // profile, `params[i]` describing `profiles[i]`.
        available_profile_params: vec![r#"{"context_window":128000,"temperature":0.2}"#.to_string()],
    };

    let derived = derive_directory_entries(
        "did:key:home",
        &[
            principal_with_default("did:key:a", "Amy", "did:key:a:coder"),
            principal("did:key:b", "Bob"),
        ],
        &BTreeMap::from([("did:key:a".to_string(), vec![coder.clone(), artist.clone()])]),
        &BTreeMap::from([(
            "did:key:a".to_string(),
            ("running".to_string(), "2026-07-23T00:00:00Z".to_string()),
        )]),
        &BTreeMap::from([("did:key:a".to_string(), options.clone())]),
    );
    assert_eq!(
        derived.keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from(["did:key:a".to_string(), "did:key:b".to_string()])
    );
    let a = &derived["did:key:a"];
    assert_eq!(a.source_did, "did:key:home");
    assert_eq!(a.display_name, "Amy");
    assert_eq!(a.behaviors, vec!["Artist".to_string(), "Coder".to_string()]);
    assert_eq!(
        a.behavior_ids,
        vec![
            "did:key:a:artist".to_string(),
            "did:key:a:coder".to_string()
        ],
        "ids must stay index-aligned with display names"
    );
    assert_eq!(
        a.default_behavior_id, "did:key:a:coder",
        "default_behavior_id must copy through from the principal"
    );
    assert_eq!(a.runtime_state, "running");

    // The four `behavior_*` arrays are index-aligned with the sorted
    // `behavior_ids` (artist first, coder second).
    assert_eq!(
        a.behavior_models,
        vec!["anthropic|claude".to_string(), "openai|gpt-5".to_string()],
        "behavior_models must be backend_id|model_name, aligned with behavior_ids"
    );
    assert_eq!(
        a.behavior_roots,
        vec![String::new(), "/repo/a".to_string()],
        "behavior_roots must copy the resolved host root, aligned"
    );
    assert_eq!(
        a.behavior_presets,
        vec![String::new(), "readonly".to_string()],
        "resolved preset names must remain aligned with behavior IDs"
    );
    assert_eq!(
        a.behavior_profiles,
        vec![String::new(), "profile-fast".to_string()],
        "behavior_profiles must copy inference_profile_id, aligned"
    );

    assert_eq!(
        a.options, options,
        "the five option lists must pass through for the matching principal"
    );
    assert_eq!(
        a.options.available_profile_params.len(),
        a.options.available_profiles.len(),
        "available_profile_params must stay index-aligned with available_profiles"
    );
    assert_eq!(
        a.options.available_profile_params[0], r#"{"context_window":128000,"temperature":0.2}"#,
        "available_profile_params[0] must describe available_profiles[0] (profile-fast)"
    );

    let b = &derived["did:key:b"];
    assert!(b.behaviors.is_empty() && b.behavior_ids.is_empty());
    assert_eq!(
        b.default_behavior_id, "",
        "a principal with no default behavior must derive an empty default_behavior_id"
    );
    assert_eq!(b.runtime_state, "");
    assert!(
        b.behavior_models.is_empty()
            && b.behavior_roots.is_empty()
            && b.behavior_presets.is_empty()
            && b.behavior_profiles.is_empty(),
        "a principal with no behaviors derives empty dimension arrays"
    );
    assert_eq!(
        b.options,
        CatalogOptions::default(),
        "a principal without a catalog must not inherit another principal's options"
    );
}

#[test]
fn derivation_preserves_empty_resolved_dimensions() {
    let coder = BehaviorInfo {
        behavior_id: "did:key:a:coder".to_string(),
        display_name: "Coder".to_string(),
        backend_id: String::new(),
        model_name: String::new(),
        host_root: String::new(),
        preset: String::new(),
        inference_profile_id: String::new(),
    };
    let derived = derive_directory_entries(
        "did:key:home",
        &[principal("did:key:a", "Amy")],
        &BTreeMap::from([("did:key:a".to_string(), vec![coder])]),
        &BTreeMap::new(),
        &BTreeMap::new(),
    );
    let a = &derived["did:key:a"];
    assert_eq!(a.behavior_models, vec![String::new()]);
    assert_eq!(a.behavior_roots, vec![String::new()]);
    assert_eq!(a.behavior_presets, vec![String::new()]);
    assert_eq!(a.behavior_profiles, vec![String::new()]);
}
