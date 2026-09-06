use super::super::*;

#[test]
fn validate_rejects_non_positive_stream_liveness_timeout() {
    let mut manifest = empty_manifest("did:test:test");
    let mut profile = profile("fast");
    profile.stream_liveness_timeout_secs = Some(0);
    manifest.inference_profiles.push(profile);

    let errors = validation_errors(&manifest);

    assert!(
        errors
            .iter()
            .any(|message| message.contains("stream_liveness_timeout_secs must be positive")),
        "expected stream_liveness_timeout_secs validation error, got {errors:?}"
    );
}

#[test]
fn validate_rejects_non_positive_deadline_without_relationship_error() {
    for deadline in [0, -1] {
        let mut manifest = empty_manifest("did:test:test");
        let mut inference_profile = profile(&format!("deadline-{deadline}"));
        inference_profile.stream_liveness_timeout_secs = Some(300);
        inference_profile.deadline_duration_secs = Some(deadline);
        manifest.inference_profiles.push(inference_profile);

        let errors = validation_errors(&manifest);

        assert!(
            errors
                .iter()
                .any(|message| message.contains("deadline_duration_secs must be positive")),
            "expected deadline validation error for {deadline}, got {errors:?}"
        );
        assert!(
            !errors
                .iter()
                .any(|message| message.contains("must be less than deadline_duration_secs")),
            "expected no relationship error for invalid deadline {deadline}, got {errors:?}"
        );
    }
}

#[test]
fn validate_reports_each_invalid_timeout_without_relationship_error() {
    // `InferenceProfile::validate` (#1331, the single owner) reports every
    // violated rule at once (fix round 1) — both timeout bounds surface
    // when both are invalid, and the relationship check is still skipped
    // (neither individual bound is valid enough to compare).
    let mut manifest = empty_manifest("did:test:test");
    let mut profile = profile("invalid-timeouts");
    profile.stream_liveness_timeout_secs = Some(0);
    profile.deadline_duration_secs = Some(0);
    manifest.inference_profiles.push(profile);

    let errors = validation_errors(&manifest);

    assert!(
        errors
            .iter()
            .any(|message| message.contains("stream_liveness_timeout_secs must be positive")),
        "expected liveness validation error, got {errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|message| message.contains("deadline_duration_secs must be positive")),
        "expected deadline validation error, got {errors:?}"
    );
    assert!(
        !errors
            .iter()
            .any(|message| message.contains("must be less than deadline_duration_secs")),
        "expected no relationship error for invalid timeouts, got {errors:?}"
    );
    assert_eq!(
        errors
            .iter()
            .filter(|message| message.contains("InferenceProfile invalid-timeouts"))
            .count(),
        2,
        "each invalid profile bound must be a separate error-list entry: {errors:?}"
    );
}

#[test]
fn validate_rejects_stream_liveness_timeout_equal_to_deadline() {
    let mut manifest = empty_manifest("did:test:test");
    let mut profile = profile("equal-timeouts");
    profile.stream_liveness_timeout_secs = Some(300);
    profile.deadline_duration_secs = Some(300);
    manifest.inference_profiles.push(profile);

    let errors = validation_errors(&manifest);

    assert!(
        errors.iter().any(|message| {
            message.contains(
                "stream_liveness_timeout_secs (300) must be less than deadline_duration_secs (300)",
            )
        }),
        "expected ineffective liveness timeout validation error, got {errors:?}"
    );
}

#[test]
fn validate_rejects_stream_liveness_timeout_greater_than_deadline() {
    let mut manifest = empty_manifest("did:test:test");
    let mut profile = profile("late-liveness");
    profile.stream_liveness_timeout_secs = Some(600);
    profile.deadline_duration_secs = Some(300);
    manifest.inference_profiles.push(profile);

    let errors = validation_errors(&manifest);

    assert!(
        errors.iter().any(|message| {
            message.contains(
                "stream_liveness_timeout_secs (600) must be less than deadline_duration_secs (300)",
            )
        }),
        "expected late liveness timeout validation error, got {errors:?}"
    );
}

#[test]
fn validate_accepts_stream_liveness_timeout_shorter_than_deadline() {
    let mut manifest = empty_manifest("did:test:test");
    let mut profile = profile("early-liveness");
    profile.stream_liveness_timeout_secs = Some(300);
    profile.deadline_duration_secs = Some(600);
    manifest.inference_profiles.push(profile);

    let errors = validation_errors(&manifest);

    assert!(
        !errors
            .iter()
            .any(|message| message.contains("stream_liveness_timeout_secs")),
        "expected valid timeout relationship, got {errors:?}"
    );
}

#[test]
fn validate_accepts_default_inference_timeouts() {
    let mut manifest = empty_manifest("did:test:test");
    manifest
        .inference_profiles
        .push(profile("default-timeouts"));

    let errors = validation_errors(&manifest);

    assert!(
        !errors
            .iter()
            .any(|message| message.contains("stream_liveness_timeout_secs")),
        "expected shipped default timeout relationship to remain valid, got {errors:?}"
    );
}

#[test]
fn shipped_long_running_demo_manifests_validate() {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");

    for relative_root in [
        "packs/security_scan",
        "packs/defending_code",
        "packs/repo_maintenance",
    ] {
        let root = workspace_root.join(relative_root);
        let (_, report) = load::load_manifest_root(&root);
        assert!(
            report.ok,
            "expected {relative_root} to validate, got {:?}",
            report.errors
        );
    }
}

#[test]
fn validate_rejects_default_liveness_when_explicit_deadline_is_too_short() {
    let mut manifest = empty_manifest("did:test:test");
    let mut profile = profile("implicit-liveness");
    profile.deadline_duration_secs = Some(gents::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS as i64);
    manifest.inference_profiles.push(profile);

    let errors = validation_errors(&manifest);

    assert!(
        errors.iter().any(|message| {
            message.contains("stream_liveness_timeout_secs (1800)")
                && message.contains("deadline_duration_secs (1800)")
        }),
        "expected default liveness relationship validation error, got {errors:?}"
    );
}

#[test]
fn validate_rejects_negative_sampling_seed() {
    let mut manifest = empty_manifest("did:test:test");
    let mut profile = profile("seeded");
    profile.seed = Some(-1);
    manifest.inference_profiles.push(profile);

    let errors = validation_errors(&manifest);

    assert!(
        errors
            .iter()
            .any(|message| message.contains("seed must be non-negative")),
        "expected seed validation error, got {errors:?}"
    );
}

fn template_manifest(
    system_prompt: Option<&str>,
    task_prompt: Option<&str>,
) -> DesiredStateManifest {
    DesiredStateManifest {
        agent_principal: DesiredAgentPrincipal {
            agent_did: "did:key:test-template-validation".to_string(),
            display_name: None,
            default_behavior_id: Some("default".to_string()),
            enabled: true,
        },
        agent_behaviors: vec![DesiredAgentBehavior {
            behavior_id: "default".to_string(),
            agent_did: "did:key:test-template-validation".to_string(),
            display_name: None,
            description: None,
            summary: None,
            system_prompt: system_prompt.map(str::to_string),
            request_context_template: None,
            backend_id: None,
            model_name: None,
            tool_selection_id: None,
            inference_profile_id: None,
            compaction_strategy: None,
            compaction_threshold: None,
            enabled: true,
            skill_refs: Vec::new(),
            skill_excludes: Vec::new(),
        }],
        skills: Vec::new(),
        datastore_tool_surfaces: Vec::new(),
        chain_key_bindings: Vec::new(),
        eth_tools: Vec::new(),
        tool_selections: Vec::new(),
        inference_backends: Vec::new(),
        inference_profiles: Vec::new(),
        tool_service_registries: Vec::new(),
        projection_acp_bindings: Vec::new(),
        tasks: task_prompt
            .map(|prompt| {
                vec![DesiredTask {
                    task_id: "task".to_string(),
                    name: "Task".to_string(),
                    description: None,
                    behavior_id: "default".to_string(),
                    prompt_template: prompt.to_string(),
                    goal_objective_template: None,
                    goal_token_budget: None,
                    enabled: true,
                    output_schema_ref: None,
                }]
            })
            .unwrap_or_default(),
        schedules: Vec::new(),
        event_triggers: Vec::new(),
        callback_bindings: Vec::new(),
        repository_placements: Vec::new(),
    }
}

#[test]
fn behavior_model_must_be_advertised_by_its_backend() {
    let mut manifest = template_manifest(None, None);
    manifest
        .inference_backends
        .push(super::super::DesiredInferenceBackend {
            backend_id: "reviewers".to_string(),
            name: "Reviewers".to_string(),
            provider_kind: Default::default(),
            openai_wire_api: None,
            endpoint: "http://127.0.0.1:8000/v1".to_string(),
            api_key: None,
            api_key_env_var: None,
            max_concurrent: 4,
            max_queue_depth: 8,
            enabled: true,
            models: vec!["d4f".to_string()],
        });
    manifest.agent_behaviors[0].backend_id = Some("reviewers".to_string());
    manifest.agent_behaviors[0].model_name = Some("GLM-5.2".to_string());

    let errors = validation_errors(&manifest);
    assert!(errors.iter().any(|error| {
        error.contains(
            "behavior default selects model GLM-5.2 which backend reviewers does not advertise",
        )
    }));
}

#[test]
fn behavior_validation_reports_each_missing_reference_separately() {
    let mut manifest = template_manifest(None, None);
    let behavior = &mut manifest.agent_behaviors[0];
    behavior.backend_id = Some("missing-backend".to_string());
    behavior.tool_selection_id = Some("missing-tools".to_string());
    behavior.inference_profile_id = Some("missing-profile".to_string());
    behavior.skill_refs = vec!["missing-skill-a".to_string(), "missing-skill-b".to_string()];
    behavior.skill_excludes = vec!["missing-skill-c".to_string()];

    let errors = validation_errors(&manifest);
    let expected = [
        "missing backend_id missing-backend",
        "missing tool_selection_id missing-tools",
        "missing inference_profile_id missing-profile",
        "missing skill_ref missing-skill-a",
        "missing skill_ref missing-skill-b",
        "missing skill_exclude missing-skill-c",
    ];

    for expected_message in expected {
        assert_eq!(
            errors
                .iter()
                .filter(|error| error.contains(expected_message))
                .count(),
            1,
            "expected one separate {expected_message:?} error, got {errors:?}"
        );
    }

    assert_eq!(
        errors.len(),
        expected.len(),
        "expected one error-list entry per missing reference, got {errors:?}"
    );
}

#[test]
fn backend_validation_reports_each_violation_separately() {
    let mut manifest = template_manifest(None, None);
    let mut invalid = backend("invalid-backend");
    invalid.endpoint = "  ".to_string();
    invalid.max_concurrent = 0;
    invalid.max_queue_depth = -1;
    manifest.inference_backends.push(invalid);

    let errors = validation_errors(&manifest);
    assert_eq!(errors.len(), 3, "{errors:?}");
    assert!(errors
        .iter()
        .any(|error| error.contains("endpoint must not be empty")));
    assert!(errors
        .iter()
        .any(|error| error.contains("max_concurrent must be positive")));
    assert!(errors
        .iter()
        .any(|error| error.contains("max_queue_depth must be positive")));
}

fn manifest_with_reasoning_effort(reasoning_effort: Option<&str>) -> DesiredStateManifest {
    let mut manifest = template_manifest(None, None);
    manifest
        .inference_profiles
        .push(super::super::DesiredInferenceProfile {
            profile_id: "default-profile".to_string(),
            display_name: None,
            context_window: None,
            max_output_tokens: None,
            max_turns: None,
            temperature: None,
            top_p: None,
            top_k: None,
            seed: None,
            min_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            repetition_penalty: None,
            reasoning_effort: reasoning_effort.map(str::to_string),
            stream_batch_ms: None,
            stream_liveness_timeout_secs: None,
            deadline_duration_secs: None,
            retry_max_transport: None,
            retry_backoff_ms: None,
            retry_max_resample: None,
            retry_allow_repair: None,
            retry_interactive_max: None,
        });
    manifest
}

#[test]
fn an_unset_reasoning_effort_round_trips_through_export_and_apply() {
    for unset in [Some(""), Some("   "), None] {
        let errors = validation_errors(&manifest_with_reasoning_effort(unset));
        assert!(
            errors.is_empty(),
            "an unset reasoning_effort ({unset:?}) must validate: {errors:?}"
        );
    }
}

#[test]
fn a_reasoning_effort_outside_the_vocabulary_is_still_rejected() {
    let errors = validation_errors(&manifest_with_reasoning_effort(Some("extreme")));
    assert!(
        errors
            .iter()
            .any(|error| error.contains("reasoning_effort must be one of")),
        "expected the vocabulary rejection, got {errors:?}"
    );
}

#[test]
fn system_prompt_rejects_per_request_ref() {
    let errors = validation_errors(&template_manifest(Some("now {{ ctx.now }}"), None));

    assert!(
        errors
            .iter()
            .any(|error| error.contains("per-request variable `ctx.now`")),
        "expected ctx.now rejection, got {errors:?}"
    );
}

#[test]
fn system_prompt_accepts_literal_raw_and_node_refs() {
    for prompt in [
        "literal text with no MiniJinja markers",
        "{% raw %}{{ ctx.now }}{% endraw %}",
        "node {{ node.node_did }} / {{ node.behavior_id }}",
    ] {
        let errors = validation_errors(&template_manifest(Some(prompt), None));
        assert!(errors.is_empty(), "prompt {prompt:?} failed: {errors:?}");
    }
}

fn manifest_with_request_context(template: &str) -> DesiredStateManifest {
    let mut manifest = template_manifest(None, None);
    manifest.agent_behaviors[0].request_context_template = Some(template.to_string());
    manifest
}

#[test]
fn request_context_template_validated_at_apply() {
    let ok = validation_errors(&manifest_with_request_context(
        "seat at {{ ctx.now }} on {{ node.node_did }}",
    ));
    assert!(
        ok.is_empty(),
        "valid request-context template failed: {ok:?}"
    );

    let bad = validation_errors(&manifest_with_request_context("{{ ctx.bogus_unknown }}"));
    assert!(
        bad.iter()
            .any(|e| e.contains("request_context_template") && e.contains("ctx.bogus_unknown")),
        "expected unknown ctx ref rejection at apply, got {bad:?}"
    );

    let hidden = validation_errors(&manifest_with_request_context(
        "{% set x = ctx.bogus_unknown %}{{ x }}",
    ));
    assert!(
        hidden
            .iter()
            .any(|e| e.contains("request_context_template") && e.contains("ctx.bogus_unknown")),
        "expected unknown ctx ref inside set to be rejected at apply, got {hidden:?}"
    );
}

#[test]
fn task_template_raw_block_is_not_scope_checked() {
    let errors = validation_errors(&template_manifest(
        None,
        Some("{% raw %}{{ ctx.collection_summary }}{% endraw %} at {{ ctx.now }}"),
    ));
    assert!(
        errors.is_empty(),
        "raw-wrapped task-unavailable var must not be scope-rejected: {errors:?}"
    );
}

#[test]
fn task_template_rejects_task_unavailable_ctx_ref() {
    let errors = validation_errors(&template_manifest(
        None,
        Some("{{ ctx.collection_summary }}"),
    ));
    assert!(
        errors.iter().any(|e| e.contains("ctx.collection_summary")),
        "expected task-unavailable ctx ref rejection, got {errors:?}"
    );
}

#[test]
fn task_prompt_accepts_task_catalog_refs() {
    let errors = validation_errors(&template_manifest(
        None,
        Some("run {{ node.node_did }} {{ node.behavior_id }} {{ ctx.now }}"),
    ));

    assert!(errors.is_empty(), "task refs should pass: {errors:?}");
}

#[test]
fn task_prompt_rejects_request_context_only_ref() {
    let errors = validation_errors(&template_manifest(
        None,
        Some("state {{ ctx.collection_summary }}"),
    ));

    assert!(
        errors
            .iter()
            .any(|error| error.contains("unavailable template variable ctx.collection_summary")),
        "expected ctx.collection_summary rejection, got {errors:?}"
    );
}
