use gents::config_client::GraphqlEndpoint;
use std::collections::BTreeSet;

use super::*;
use super::{
    background::*, load::*, manifest::*, provenance::*, setup::*, stages::*, tool_calls::*,
    validate::*, wait::*,
};

#[test]
fn source_pack_paths_require_normalized_snake_case_directories() {
    for path in ["pipeline", "packs/subject_pack", "/tmp/my_pack"] {
        validate_source_pack_path(Path::new(path)).unwrap();
    }
    for path in [
        "packs/../pipeline",
        "./packs/subject_pack",
        "packs/CodeReview",
        "packs/code-review",
    ] {
        assert!(
            validate_source_pack_path(Path::new(path)).is_err(),
            "{path}"
        );
    }
}

#[test]
fn pack_sidecar_reference_cannot_escape_the_pack_directory() {
    let root = tempfile::tempdir().unwrap();
    let pack = root.path().join("code_review");
    std::fs::create_dir(&pack).unwrap();
    std::fs::write(pack.join("system_prompt.md"), "inside").unwrap();
    std::fs::write(root.path().join("outside.md"), "outside").unwrap();

    assert_eq!(
        read_pack_sidecar(&pack, "./system_prompt.md").unwrap(),
        "inside"
    );
    for reference in [
        "./../outside.md",
        "./code_review/../../outside.md",
        "../outside.md",
        "./",
    ] {
        assert!(read_pack_sidecar(&pack, reference).is_err(), "{reference}");
    }
}

fn load_manifest_defaults(pack: &Path) -> Result<ScenarioManifest> {
    let distribution = read_distribution_manifest(pack)?;
    load_manifest_with(pack, &distribution, &|_| None)
}

#[test]
fn scenario_manifest_reads_the_dependency_environment_key() {
    let manifest: ScenarioManifest = serde_json::from_value(serde_json::json!({
        "name": "t",
        "init": {"inference_url": "http://x", "model_name": "m"},
        "seed": {"collection": "J", "job_id_field": "run_id", "prompt_field": "focus"},
        "expect": {"trigger_ids": []},
        "dependency_environment": {"dep": {"TOKEN": "value"}}
    }))
    .unwrap();
    assert_eq!(manifest.dependency_environment["dep"]["TOKEN"], "value");
}

#[test]
fn resolve_prepare_within_uses_tool_root_independent_of_the_tool_package() {
    let pack = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let manifest: ScenarioManifest = serde_json::from_value(serde_json::json!({
        "name": "t",
        "init": {
            "inference_url": "http://x", "model_name": "m",
            "tool_root": root.path().to_str().unwrap()
        },
        "seed": {"collection": "J", "job_id_field": "run_id", "prompt_field": "focus"},
        "expect": {"trigger_ids": []}
    }))
    .unwrap();

    assert_eq!(
        resolve_manifest_tool_root(pack.path(), &manifest).unwrap(),
        None
    );
    let within = resolve_prepare_within(pack.path(), &manifest)
        .unwrap()
        .expect("a declared tool_root must bound prepare steps under a minimal tool package too");
    assert_eq!(within, root.path().canonicalize().unwrap());

    let unset: ScenarioManifest = serde_json::from_value(serde_json::json!({
        "name": "t", "init": {"inference_url": "http://x", "model_name": "m"},
        "seed": {"collection": "J", "job_id_field": "run_id", "prompt_field": "focus"},
        "expect": {"trigger_ids": []}
    }))
    .unwrap();
    assert_eq!(resolve_prepare_within(pack.path(), &unset).unwrap(), None);
}

#[test]
fn scenario_allowed_folders_are_written_inside_the_tool_root() {
    let pack = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let manifest = |folders: serde_json::Value, tool_root: bool| -> ScenarioManifest {
        let mut init = serde_json::json!({"inference_url": "http://x", "model_name": "m"});
        if tool_root {
            init["tool_root"] = serde_json::json!(root.path());
        }
        serde_json::from_value(serde_json::json!({
            "name": "t", "init": init,
            "seed": {"collection": "J", "job_id_field": "run_id", "prompt_field": "focus"},
            "expect": {"trigger_ids": []},
            "allowed_folders": folders,
        }))
        .unwrap()
    };

    let allowed = allow_scenario_folders(
        pack.path(),
        &manifest(
            serde_json::json!([{"path": "out", "access": "read_write"}, {"path": "data"}]),
            true,
        ),
        home.path(),
    )
    .unwrap();
    let root = root.path().canonicalize().unwrap();
    assert!(root.join("out").is_dir(), "a missing folder is made");
    assert_eq!(
        gents::plugin::allowed::list(home.path()).unwrap(),
        vec![
            gents::plugin::allowed::AllowedDir {
                path: root.join("data"),
                access: gents::pack::BindAccess::Read,
            },
            gents::plugin::allowed::AllowedDir {
                path: root.join("out"),
                access: gents::pack::BindAccess::ReadWrite,
            },
        ]
    );
    assert_eq!(allowed.len(), 2);

    for (folders, tool_root, expected) in [
        (
            serde_json::json!([{"path": outside.path()}]),
            true,
            "outside init.tool_root",
        ),
        (
            serde_json::json!([{"path": "../escape"}]),
            true,
            "must not contain ..",
        ),
        (
            serde_json::json!([{"path": "out"}]),
            false,
            "needs init.tool_root",
        ),
        (
            serde_json::json!([{"path": outside.path()}]),
            false,
            "needs init.tool_root",
        ),
    ] {
        let error = allow_scenario_folders(pack.path(), &manifest(folders, tool_root), home.path())
            .unwrap_err();
        assert!(format!("{error:#}").contains(expected), "{error:#}");
    }
    let twice = manifest(serde_json::json!([{"path": "a"}, {"path": "a"}]), true);
    assert!(format!("{:#}", validate_manifest(&twice).unwrap_err()).contains("twice"));
}

#[test]
fn scenario_staging_explicitly_binds_the_initialized_profile() {
    let pack = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../gents/tests/fixtures/packs/documents_fixture");
    let distribution = read_distribution_manifest(&pack).unwrap();
    let staged = stage_scenario_pack(
        &pack,
        &distribution,
        "did:key:scenario-owner",
        "default-profile",
        "did:key:scenario-owner:default",
    )
    .unwrap();
    let (config, report) = crate::desired_state::load_manifest_root(staged.path());
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let config = config.unwrap();
    assert_eq!(
        config.node.default_agent_id.as_deref(),
        Some("did:key:scenario-owner:default"),
    );
    assert!(config
        .agents
        .iter()
        .all(|agent| agent.inference_profile_id == "default-profile"));
    assert!(config.agents.iter().all(|agent| agent
        .tags
        .contains(&"gents:pack:documents_fixture".to_owned())));
    assert!(config.inference_profiles.is_empty());
    assert!(config.inference_backends.is_empty());
}

#[test]
fn scenario_refuses_a_pack_that_selects_the_default_agent() {
    let pack = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../gents/tests/fixtures/packs/documents_fixture");
    let distribution = read_distribution_manifest(&pack).unwrap();
    let authored_pack = tempfile::tempdir().unwrap();
    for asset in distribution
        .metadata
        .assets
        .iter()
        .map(String::as_str)
        .chain(["manifest.json"])
    {
        let destination = authored_pack.path().join(asset);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::copy(pack.join(asset), destination).unwrap();
    }
    validate_scenario_defaults(authored_pack.path()).unwrap();
    let config_path = authored_pack.path().join("pack_config.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["node"]["default_agent_id"] = json!("fixture-worker");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();

    let error = format!(
        "{:#}",
        validate_scenario_defaults(authored_pack.path()).unwrap_err()
    );
    assert!(
        error.contains("must not choose the default agent"),
        "{error}"
    );
}

#[tokio::test]
async fn documents_fixture_staging_installs_its_declared_schemas() {
    let pack = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../gents/tests/fixtures/packs/documents_fixture");
    let distribution = read_distribution_manifest(&pack).unwrap();
    let schema_assets = distribution
        .metadata
        .assets
        .iter()
        .filter(|asset| asset.starts_with("schemas/") && asset.ends_with(".graphql"))
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        distribution
            .schemas
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>(),
        schema_assets
    );

    let staged = stage_scenario_pack(
        &pack,
        &distribution,
        "did:key:scenario-owner",
        "default-profile",
        "did:key:scenario-owner:default",
    )
    .unwrap();
    for schema in &distribution.schemas {
        assert_eq!(
            std::fs::read(staged.path().join(schema)).unwrap(),
            std::fs::read(pack.join(schema)).unwrap(),
            "staged schema {schema}"
        );
    }

    let node = std::sync::Arc::new(
        crate::shared::test_support::node_builder()
            .build()
            .await
            .unwrap(),
    );
    gents::ensure_runtime_schemas(&node).await.unwrap();
    let access = ConfigAccess::local(node.clone());
    let phase = crate::commands::schema::apply_pack_schemas_if_present(&access, staged.path())
        .await
        .unwrap()
        .expect("staged pack must have schemas");
    assert_eq!(phase.schema_files.len(), schema_assets.len());

    let (config, report) = crate::desired_state::load_manifest_root(staged.path());
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let errors =
        crate::desired_state::validate::validate_manifest_against_live(&config.unwrap(), &access)
            .await
            .unwrap();
    assert!(errors.is_empty(), "{errors:?}");
    node.shutdown().await;
}

const COLOURED: &str = "\u{1b}[32m INFO\u{1b}[0m \u{1b}[2mgents::trigger_engine::event_source\u{1b}[0m\u{1b}[2m:\u{1b}[0m event source now observing source collection \u{1b}[3msource_collection\u{1b}[0m\u{1b}[2m=\u{1b}[0mExperimentJob \u{1b}[3mgeneration\u{1b}[0m\u{1b}[2m=\u{1b}[0m3";

#[test]
fn matches_the_observe_line_through_ansi_colouring() {
    assert!(observes_collection(&strip_ansi(COLOURED), "ExperimentJob"));
}

#[test]
fn observe_line_alone_is_not_ready_to_seed() {
    assert!(!event_source_ready(COLOURED, "ExperimentJob"));
}

#[test]
fn post_apply_server_marker_is_required_before_pack_mutations() {
    assert!(!server_ready_marker(
        "gents ready runnable_agents=1 unavailable_agents=0"
    ));
    assert!(server_ready_marker(
        "gents server is running local-only. Press Ctrl-C to stop."
    ));
    assert!(server_ready_marker(
        "\u{1b}[32mgents server is running with IROH P2P. Press Ctrl-C to stop.\u{1b}[0m"
    ));
}

#[test]
fn event_backed_trigger_ready_requires_enabled_matching_collection() {
    let response = json!({
        "data": {
            "Trigger": [
                {"trigger_id": "other", "source": {"kind": "event", "event_source_id": "other-source"}, "enabled": true},
                {"trigger_id": "recon", "source": {"kind": "event", "event_source_id": "review-source"}, "enabled": false}
            ],
            "EventSource": [
                {"event_source_id": "other-source", "source_collection": "Other"},
                {"event_source_id": "review-source", "source_collection": "ReviewJob"}
            ]
        }
    });
    assert!(!event_backed_trigger_ready(&response, "ReviewJob"));
    let response = json!({
        "data": {
            "Trigger": [
                {"trigger_id": "recon", "source": {"kind": "event", "event_source_id": "review-source"}, "enabled": true}
            ],
            "EventSource": [
                {"event_source_id": "review-source", "source_collection": "ReviewJob"}
            ]
        }
    });
    assert!(event_backed_trigger_ready(&response, "ReviewJob"));
}

#[test]
fn global_update_subscription_completes_seed_readiness() {
    let log = format!(
        "{COLOURED}\n INFO event source opened global Update subscription collections=1 generation=3"
    );
    assert!(event_source_ready(&log, "ExperimentJob"));
}

#[test]
fn every_checked_in_scenario_loads() {
    let catalog_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../gents/tests/fixtures/packs");
    let mut packs = std::fs::read_dir(&catalog_root)
        .expect("read pack directory")
        .map(|entry| entry.expect("read pack entry").path())
        .filter(|path| path.join("experiment.json").is_file())
        .collect::<Vec<_>>();
    packs.sort();
    assert!(!packs.is_empty(), "expected checked-in scenario packs");
    for pack in packs {
        load_manifest_defaults(&pack)
            .unwrap_or_else(|error| panic!("{} should load: {error:#}", pack.display()));
    }
}

fn copy_fixture_pack(name: &str) -> tempfile::TempDir {
    let copy = tempfile::tempdir().unwrap();
    super::super::test_support::copy_tree(
        &super::super::test_support::fixture_dir(name),
        copy.path(),
    )
    .unwrap();
    copy
}

#[test]
fn expected_trigger_without_a_correlation_field_is_refused_at_load() {
    let dir = copy_fixture_pack("documents_fixture");
    let config_path = dir.path().join("pack_config.json");
    let mut config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    config["event_sources"][0]["event_source_id"] = "fixture-worker-source".into();
    config["event_sources"][0]
        .as_object_mut()
        .unwrap()
        .remove("correlation_field");
    config["triggers"][0]["source"]["event_source_id"] = "fixture-worker-source".into();
    std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    let error = load_manifest_defaults(dir.path()).expect_err("pack must refuse to load: run");
    let message = format!("{error:#}");
    assert!(
        message.contains(
            "expect.trigger_ids entry fixture-worker uses EventSource fixture-worker-source \
             without a correlation_field, so its requests can never match the scenario run"
        ),
        "{message}"
    );
}

#[test]
fn expected_trigger_with_a_missing_event_source_is_refused_at_load() {
    let dir = copy_fixture_pack("documents_fixture");
    let config_path = dir.path().join("pack_config.json");
    let mut config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    config["triggers"][0]["source"]["event_source_id"] = "absent-source".into();
    std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    let error = load_manifest_defaults(dir.path()).expect_err("pack must refuse to load: run");
    let message = format!("{error:#}");
    assert!(
        message.contains("missing EventSource absent-source"),
        "{message}"
    );
}

#[test]
fn signed_fact_gate_requires_every_composite_commit_to_have_the_node_signer() {
    let signer = "did:key:zNode";
    let signed = json!({
        "cid": "bafy-signed",
        "signature": { "identity": signer, "type": "ES256K" }
    });
    let unsigned = json!({ "cid": "bafy-unsigned", "signature": null });

    assert!(require_signed_commits(
        "AgentMessage",
        "doc-1",
        std::slice::from_ref(&signed),
        signer
    )
    .is_ok());
    assert!(require_signed_commits("AgentMessage", "doc-1", &[], signer).is_err());
    let error = require_signed_commits("AgentMessage", "doc-1", &[signed, unsigned], signer)
        .unwrap_err()
        .to_string();
    assert!(error.contains("bafy-unsigned"), "{error}");
}

#[test]
fn stage_request_query_scopes_correlated_triggers_to_the_current_run() {
    let query = stage_requests_query("review-\"scan", "run-\"42");
    assert!(query.contains(r#"caused_by_trigger_id: { _eq: "review-\"scan" }"#));
    assert!(query.contains(r#"caused_by_correlation: { _eq: "run-\"42" }"#));
}

#[test]
fn stage_tool_sequence_is_fail_closed_at_the_write_boundary() {
    let stage = StageResult {
        trigger_id: "recon".into(),
        request_id: "request-1".into(),
        lifecycle_state: RequestLifecycleState::Completed,
        caused_by_source_doc_id: None,
    };
    let expected = StageToolSequenceExpectation {
        trigger_id: "recon".into(),
        boundary_tool_name: "write_row".into(),
        max_calls_before_boundary: 2,
        max_calls_per_message_before_boundary: 2,
        exact_boundary_calls: 2,
        allowed_at_or_after_boundary: vec![
            "write_row".into(),
            "get_goal".into(),
            "update_goal".into(),
        ],
    };
    let mut call_ordinal = 0usize;
    let mut row = |sequence, name| {
        call_ordinal += 1;
        json!({
            "message_sequence": sequence,
            "tool_name": name,
            "args": format!(r#"{{"call":{call_ordinal}}}"#),
            "lifecycle_state": "completed"
        })
    };
    let valid = vec![
        row(2, "read_file"),
        row(2, "grep"),
        row(4, "write_row"),
        row(4, "write_row"),
        row(6, "get_goal"),
        row(6, "update_goal"),
    ];
    verify_stage_tool_sequence_rows(&stage, &expected, &valid)
        .expect("bounded consecutive writes followed by goal completion should pass");

    let over_budget = vec![
        row(2, "read_file"),
        row(2, "grep"),
        row(3, "glob"),
        row(4, "write_row"),
        row(4, "write_row"),
    ];
    let error = verify_stage_tool_sequence_rows(&stage, &expected, &over_budget)
        .expect_err("the pre-write budget must be enforced");
    assert!(error.to_string().contains("maximum is 2"));

    let oversized_batch = vec![
        row(2, "read_file"),
        row(2, "grep"),
        row(2, "glob"),
        row(4, "write_row"),
        row(4, "write_row"),
    ];
    let mut batch_expected = StageToolSequenceExpectation {
        trigger_id: "recon".into(),
        boundary_tool_name: "write_row".into(),
        max_calls_before_boundary: 3,
        max_calls_per_message_before_boundary: 2,
        exact_boundary_calls: 2,
        allowed_at_or_after_boundary: vec!["write_row".into()],
    };
    let error = verify_stage_tool_sequence_rows(&stage, &batch_expected, &oversized_batch)
        .expect_err("the per-message discovery ceiling must be enforced");
    assert!(error.to_string().contains("pre-boundary message"));
    batch_expected.max_calls_per_message_before_boundary = 3;
    verify_stage_tool_sequence_rows(&stage, &batch_expected, &oversized_batch)
        .expect("the configured per-message discovery ceiling should pass");

    let first_write = json!({
        "message_sequence": 4,
        "tool_name": "write_row",
        "args": r#"{"row_id":"one","value":"first"}"#,
        "lifecycle_state": "completed"
    });
    let second_write = json!({
        "message_sequence": 4,
        "tool_name": "write_row",
        "args": r#"{"row_id":"two","value":"second"}"#,
        "lifecycle_state": "completed"
    });
    let exact_retry = json!({
        "message_sequence": 6,
        "tool_name": "write_row",
        "args": r#"{"row_id":"two","value":"second"}"#,
        "lifecycle_state": "failed"
    });
    verify_stage_tool_sequence_rows(
        &stage,
        &expected,
        &[first_write.clone(), second_write.clone(), exact_retry],
    )
    .expect("a byte-identical deterministic retry is one logical write");

    let conflicting_retry = json!({
        "message_sequence": 6,
        "tool_name": "write_row",
        "args": r#"{"row_id":"two","value":"changed"}"#,
        "lifecycle_state": "failed"
    });
    let error = verify_stage_tool_sequence_rows(
        &stage,
        &expected,
        &[first_write, second_write, conflicting_retry],
    )
    .expect_err("a conflicting deterministic retry must remain an extra logical write");
    assert!(error.to_string().contains("3 distinct write_row calls"));

    let interleaved = vec![
        row(2, "read_file"),
        row(3, "write_row"),
        row(3, "grep"),
        row(3, "write_row"),
    ];
    let error = verify_stage_tool_sequence_rows(&stage, &expected, &interleaved)
        .expect_err("discovery in the first write batch must be rejected");
    assert!(error.to_string().contains("disallowed tools"));
}

#[test]
fn workspace_receipt_paths_reject_unowned_build_artifacts() {
    let expected = WorkspaceReceiptPathExpectation {
        work_unit_collection: "PortWorkUnit".into(),
        correlation_field: "run_id".into(),
        work_unit_id_field: "work_unit_id".into(),
        owned_paths_field: "owned_paths".into(),
    };
    let units = vec![json!({
        "work_unit_id": "run:unit-08:attempt-1",
        "owned_paths": "[\"crates/gents-cli/src/commands/serve.rs\"]"
    })];
    let clean = vec![json!({
        "work_unit_id": "run:unit-08:attempt-1",
        "changed_files": "[\"crates/gents-cli/src/commands/serve.rs\"]"
    })];
    validate_workspace_receipt_path_rows(&expected, &units, &clean)
        .expect("an exact owned path should pass");

    let contaminated = vec![json!({
        "work_unit_id": "run:unit-08:attempt-1",
        "changed_files": "[\".tmp-build/test-build.log\"]"
    })];
    let error = validate_workspace_receipt_path_rows(&expected, &units, &contaminated)
        .expect_err("an unowned build log must fail the run independently of model review");
    assert!(error.to_string().contains("out-of-scope path"));
    assert!(error.to_string().contains(".tmp-build/test-build.log"));

    let traversal = vec![json!({
        "work_unit_id": "run:unit-08:attempt-1",
        "owned_paths": "[\"../outside.rs\"]"
    })];
    let error = validate_workspace_receipt_path_rows(&expected, &traversal, &[])
        .expect_err("ownership paths must be canonical repository-relative names");
    assert!(error.to_string().contains("invalid, non-canonical"));
}

#[test]
fn background_completion_expectation_requires_the_whole_delivery_path() {
    let expected = BackgroundCompletionExpectation {
        min_completed_caused_requests: 2,
        min_completed_wakes: 1,
        min_acknowledged_notifications: 2,
        max_pending_notifications: 0,
        max_stranded_notifications: 0,
    };
    let complete = BackgroundCompletionEvidence {
        completed_caused_request_ids: vec!["child-1".into(), "child-2".into()],
        failed_caused_request_ids: Vec::new(),
        completed_wake_request_ids: vec!["wake-1".into()],
        pending_notifications: 0,
        acknowledged_notifications: 2,
        stranded_notifications: 0,
        diagnostics: Value::Null,
    };
    assert!(complete.satisfies(&expected));

    let mut pending = complete.clone();
    pending.pending_notifications = 1;
    assert!(!pending.satisfies(&expected));

    let mut missing_wake = complete;
    missing_wake.completed_wake_request_ids.clear();
    assert!(!missing_wake.satisfies(&expected));
}

#[test]
fn pack_tool_root_requires_declared_markers() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
    let markers = vec!["Cargo.toml".to_string(), "crates/gents".to_string()];
    let error = resolve_pack_tool_root(root.path(), Some(root.path().to_str().unwrap()), &markers)
        .unwrap_err();
    assert!(error.to_string().contains("crates/gents"), "{error}");

    std::fs::create_dir_all(root.path().join("crates/gents")).unwrap();
    let resolved =
        resolve_pack_tool_root(root.path(), Some(root.path().to_str().unwrap()), &markers).unwrap();
    assert_eq!(resolved, std::fs::canonicalize(root.path()).unwrap());
}

#[test]
fn readonly_pack_requires_tool_root() {
    let mut manifest = ScenarioManifest {
        name: "lsp".into(),
        description: String::new(),
        init: PackInit {
            inference_url: "http://127.0.0.1:8080".into(),
            model_name: "test".into(),
            api_key_env_var: None,
            backend_preset: None,
            openai_wire_api: None,
            max_concurrent: None,
            tool_package: "readonly".into(),
            tool_root: None,
            tool_root_env_var: None,
            tool_root_markers: Vec::new(),
        },
        seed: PackSeed {
            collection: "Job".into(),
            job_id_field: "job_id".into(),
            prompt_field: "prompt".into(),
            fields: BTreeMap::new(),
        },
        default_prompt: String::new(),
        expect: PackExpect {
            trigger_ids: Vec::new(),
            trigger_request_counts: BTreeMap::new(),
            trigger_request_count_sources: BTreeMap::new(),
            collection_counts: BTreeMap::new(),
            projections: Vec::new(),
            signed_provenance: false,
            required_tool_call_trigger_ids: Vec::new(),
            source_edges: Vec::new(),
            fan_in: None,
            prompt_tool_contracts: Vec::new(),
            background_completion: None,
            tool_calls: Vec::new(),
            stage_tool_sequences: Vec::new(),
            workspace_receipt_paths: None,
            result_documents: Vec::new(),
        },
        await_timeout_secs: 1,
        prepare: Vec::new(),
        allowed_folders: Vec::new(),
        dependency_environment: BTreeMap::new(),
        dependencies: Vec::new(),
        plugins: Vec::new(),
        config: None,
    };
    let error = validate_manifest(&manifest).expect_err("readonly needs tool_root");
    assert!(error.to_string().contains("tool_root"), "{error}");
    manifest.init.tool_root = Some(".".into());
    validate_manifest(&manifest).expect("declared tool_root is enough");
}

#[cfg(unix)]
#[tokio::test]
async fn projection_traces_read_as_the_served_home() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let calls = temp.path().join("calls");
    let bin = temp.path().join("gents");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\necho '{{}}'\n",
            calls.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let graphql = GraphqlEndpoint::anonymous("http://127.0.0.1:9/api/v0/graphql");
    let stages = [StageResult {
        trigger_id: "stage".to_string(),
        request_id: "req-1".to_string(),
        lifecycle_state: RequestLifecycleState::Completed,
        caused_by_source_doc_id: None,
    }];

    let home = temp.path().join("home");
    render_projection_artifacts(
        &bin,
        &home,
        &graphql,
        temp.path(),
        &stages,
        &["langfuse".to_string()],
    )
    .await
    .unwrap();

    let calls = std::fs::read_to_string(&calls).unwrap();
    assert_eq!(calls.lines().count(), 2, "{calls}");
    for call in calls.lines() {
        assert!(
            call.contains(&format!("--home {}", home.display())),
            "{call}"
        );
    }
}
