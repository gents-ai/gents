use super::evidence::home_usage;
use super::install::{install, read_pack};
use super::*;
use crate::eval::runner::executor::InferenceBinding;
use crate::eval::OutcomeKind;
use crate::graphql::escape_graphql_string;
use crate::pack::digest_declared_assets;
use serde_json::{json, Value};

pub(super) fn assert_failure_class(
    observed: Option<OutcomeKind>,
    kind: OutcomeKind,
    class: crate::eval::EvidenceClass,
) {
    assert_eq!(observed, Some(kind));
    let modeled = crate::lean_vocab_test::lean_eval_outcome_cases()
        .iter()
        .find(|case| case.kind == kind.as_str() && case.provider_reason.is_none())
        .expect("the Lean model classifies every outcome without a provider reason");
    assert_eq!(modeled.class, class.as_str());
    assert_eq!(crate::eval::classify(kind, None).as_str(), modeled.class);
}

#[test]
fn a_trial_ceiling_grants_bash_only_when_the_definition_asks_for_it() {
    let workspace = Path::new("/runs/r/trials/t/workspace");
    let without = trial_tool_ceiling(workspace, false);
    assert_eq!(
        (without.file_tools(), without.bash(), without.root()),
        (FileToolMode::ReadWrite, BashMode::Off, Some(workspace))
    );
    let with = trial_tool_ceiling(workspace, true);
    assert_eq!(
        (with.file_tools(), with.bash(), with.root()),
        (
            FileToolMode::ReadWrite,
            BashMode::Unrestricted,
            Some(workspace)
        )
    );
    assert_eq!(
        with.policy().bash.execution_mode,
        CommandExecutionMode::WorkspaceWrite
    );
}

#[tokio::test]
async fn a_declared_slot_the_binding_cannot_fill_is_infrastructure() {
    let dir = tempfile::tempdir().unwrap();
    let runs_dir = dir.path().join("runs");
    let pack_dir = runs_dir
        .join("run-1")
        .join("cells")
        .join("base")
        .join("pack");
    write_slot_pack(&pack_dir, "gents:inference-slot:primary");
    let executor = EmbeddedExecutor::new(
        DocumentRuntimeOptions::default(),
        runs_dir.clone(),
        crate::test_support::runtime_handle(),
    );
    let spec = TrialSpec {
        pack_digest: materialized_digest(&pack_dir),
        pack_dir,
        inference: frozen_binding(Value::Null),
        trial_dir: runs_dir.join("run-1").join("trials").join("t1"),
        ..TrialSpec::empty_for_tests("t1")
    };

    let home = EmbeddedHome::create_temp("unbindable-slot").await.unwrap();
    let error = install(&spec, &home, &dir.path().join("workspace"))
        .await
        .unwrap_err();
    let reason = format!("{error:#}");
    assert!(
        reason.contains("inference slots") && reason.contains("profile_id"),
        "{reason}"
    );
    home.node.shutdown().await;

    executor.provision(&spec).await;
    let evidence = executor.execute(&spec, CancellationToken::new()).await;
    assert!(evidence.stages.is_empty(), "{:?}", evidence.stages);
    assert_eq!(evidence.anchor.requests, 0);
}

pub(super) fn materialized_digest(pack_dir: &Path) -> String {
    let (manifest, assets) = read_pack(pack_dir).unwrap();
    digest_declared_assets(&manifest, |path| {
        assets.get(path).map(Vec::as_slice).context("missing asset")
    })
    .unwrap()
}

pub(super) fn frozen_binding(profile_id: Value) -> InferenceBinding {
    let mut profile = json!({
        "node_did": "did:key:frozen",
        "backend_id": "frozen-backend",
        "model_name": "frozen-model",
    });
    if !profile_id.is_null() {
        profile["profile_id"] = profile_id;
    }
    InferenceBinding {
        profile,
        backend: json!({
            "node_did": "did:key:frozen",
            "backend_id": "frozen-backend",
            "name": "Frozen backend",
            "provider_kind": "OpenAiCompatible",
            "endpoint": "http://127.0.0.1:1/v1",
            "auth": {"kind": "unauthenticated"},
        }),
        sampling: None,
        execution: None,
        retry_policy: None,
        seed: 7,
    }
}

pub(super) fn write_slot_pack(root: &Path, inference_profile_id: &str) {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(root.join("README.md"), "# slot fixture\n").unwrap();
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_vec(&json!({
            "manifest_version": 1,
            "name": "slot_fixture",
            "version": "1.0.0",
            "description": "A pack whose agent references an inference slot.",
            "authors": ["gents-ai contributors"],
            "kind": "documents",
            "assets": ["README.md", "pack_config.json"],
            "config": "pack_config.json",
            "inference_slots": [{
                "name": "primary",
                "description": "Runs the subject agent.",
                "agents": ["subject"],
            }],
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        root.join("pack_config.json"),
        serde_json::to_vec(&json!({
            "node": {},
            "agents": [{
                "agent_id": "subject",
                "display_name": "Subject",
                "inference_profile_id": inference_profile_id,
            }],
        }))
        .unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn provision_then_execute_on_a_bad_pack_digest_is_infrastructure() {
    let dir = tempfile::tempdir().unwrap();
    let runs_dir = dir.path().join("runs");
    let pack_dir = runs_dir
        .join("run-1")
        .join("cells")
        .join("base")
        .join("pack");
    write_minimal_pack(&pack_dir);
    let executor = EmbeddedExecutor::new(
        DocumentRuntimeOptions::default(),
        runs_dir.clone(),
        crate::test_support::runtime_handle(),
    );
    let spec = TrialSpec {
        pack_dir,
        pack_digest: "sha256:wrong".to_string(),
        trial_dir: runs_dir.join("run-1").join("trials").join("t1"),
        ..TrialSpec::empty_for_tests("t1")
    };

    let (manifest, assets) = read_pack(&spec.pack_dir).unwrap();
    let materialized = digest_declared_assets(&manifest, |path| {
        assets.get(path).map(Vec::as_slice).context("missing asset")
    })
    .unwrap();
    assert!(materialized.starts_with("sha256:"));
    assert_ne!(materialized, spec.pack_digest);

    let locator = executor.provision(&spec).await;
    assert!(locator.trial_node_did.starts_with("did:"));
    assert_eq!(locator.home_hint.as_deref(), Some("run-1/trials/t1"));
    assert!(spec.trial_dir.join("workspace").is_dir());

    let evidence = executor.execute(&spec, CancellationToken::new()).await;
    assert_eq!(evidence.locator.trial_node_did, locator.trial_node_did);
    assert!(evidence.stages.is_empty());
    assert_eq!(evidence.anchor.requests, 0);

    let reopened = EmbeddedHome::open_retained(&spec.trial_dir.join("home"))
        .await
        .expect("the trial home survives the close");
    assert_eq!(reopened.did(), locator.trial_node_did);
    reopened.node.shutdown().await;
}

#[tokio::test]
async fn trial_usage_includes_child_requests_and_preserves_unknown_tokens() {
    let home = EmbeddedHome::create_temp("trial-total-usage")
        .await
        .unwrap();
    for (id, input, output) in [("stage-call", 11, 7), ("automation-call", 101, 13)] {
        let id = escape_graphql_string(id);
        crate::config_client::ConfigAccess::write_local(
            &home.node,
            "eval.test.usage",
            &format!(r#"mutation {{ create_InferenceCall(input: {{ call_id: "{id}", request_id: "{id}", call_seq: 0, call_state: "completed", prompt_tokens: {input}, completion_tokens: {output} }}) {{ _docID }} }}"#),
        ).await.unwrap();
    }
    let total = home_usage(&home.node).await;
    assert_eq!(total.input_tokens, Some(112));
    assert_eq!(total.output_tokens, Some(20));
    crate::config_client::ConfigAccess::write_local(
        &home.node,
        "eval.test.usage_missing",
        r#"mutation { create_InferenceCall(input: { call_id: "unreported", request_id: "child", call_seq: 1, call_state: "failed", prompt_tokens: null, completion_tokens: 3 }) { _docID } }"#,
    ).await.unwrap();
    let total = home_usage(&home.node).await;
    assert_eq!(total.input_tokens, None);
    assert_eq!(total.output_tokens, Some(23));
    close(home).await;
}

#[tokio::test]
async fn recollect_preserves_missing_terminal_evidence_as_infrastructure() {
    let cases: Vec<Value> = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/eval_request_outcome.json"
    )))
    .unwrap();
    let case = cases
        .iter()
        .find(|case| {
            case["state"].is_null()
                && case["deadline"] == false
                && case["request_reason"].is_null()
                && case["inference_reason"].is_null()
                && case["inference_failed"] == false
                && case["tool_failed"] == false
        })
        .unwrap();
    let expected = OutcomeKind::parse(case["outcome"].as_str().unwrap()).unwrap();
    let runs = tempfile::tempdir().unwrap();
    let home = EmbeddedHome::create_retained(&runs.path().join("trial/home"))
        .await
        .unwrap();
    for state in ["pending", "processing"] {
        seed_request_with_state(&home, &format!("retained-{state}"), state).await;
    }
    let locator = TrialLocator {
        trial_node_did: home.did().to_owned(),
        session_id: "s-1".into(),
        home_hint: Some("trial".into()),
    };
    close(home).await;

    let executor = EmbeddedExecutor::new(
        DocumentRuntimeOptions::default(),
        runs.path().to_owned(),
        crate::test_support::runtime_handle(),
    );
    let evidence = executor.recollect(&locator, &[]).await.unwrap();
    assert_eq!(evidence.stages.len(), 2);
    assert!(evidence.anchor.terminal_states.is_empty());
    for stage in evidence.stages {
        assert_eq!(
            serde_json::to_value(stage.terminal_state).unwrap(),
            case["observed_terminal_state"]
        );
        assert_eq!(stage.failure_kind, Some(expected));
        assert_eq!(stage.provider_reason, None);
    }
}

#[tokio::test]
async fn recollect_refuses_a_home_hint_outside_the_runs_directory() {
    let dir = tempfile::tempdir().unwrap();
    let runs_dir = dir.path().join("runs");
    let outside = dir.path().join("outside");
    close(
        EmbeddedHome::create_retained(&outside.join("home"))
            .await
            .unwrap(),
    )
    .await;
    let executor = EmbeddedExecutor::new(
        DocumentRuntimeOptions::default(),
        runs_dir.clone(),
        crate::test_support::runtime_handle(),
    );
    let locator = |hint: &str| TrialLocator {
        trial_node_did: "did:key:zAny".to_string(),
        session_id: "s-1".to_string(),
        home_hint: Some(hint.to_string()),
    };
    for hint in ["../outside", outside.to_str().unwrap()] {
        assert!(
            executor.recollect(&locator(hint), &[]).await.is_none(),
            "{hint}"
        );
    }
    std::fs::create_dir_all(&runs_dir).unwrap();
    std::fs::rename(&outside, runs_dir.join("inside")).unwrap();
    assert!(executor.recollect(&locator("inside"), &[]).await.is_some());
}

#[cfg(unix)]
#[tokio::test]
async fn recollect_refuses_a_home_hint_that_resolves_outside_the_runs_directory() {
    let dir = tempfile::tempdir().unwrap();
    let runs_dir = dir.path().join("runs");
    let outside = dir.path().join("outside");
    close(
        EmbeddedHome::create_retained(&outside.join("home"))
            .await
            .unwrap(),
    )
    .await;
    std::fs::create_dir_all(runs_dir.join("real")).unwrap();
    std::os::unix::fs::symlink(&outside, runs_dir.join("linked")).unwrap();
    std::os::unix::fs::symlink(outside.join("home"), runs_dir.join("real").join("home")).unwrap();
    close(
        EmbeddedHome::create_retained(&runs_dir.join("ws").join("home"))
            .await
            .unwrap(),
    )
    .await;
    std::os::unix::fs::symlink(&outside, runs_dir.join("ws").join("workspace")).unwrap();
    let executor = EmbeddedExecutor::new(
        DocumentRuntimeOptions::default(),
        runs_dir,
        crate::test_support::runtime_handle(),
    );
    for hint in ["linked", "real", "ws"] {
        let locator = TrialLocator {
            trial_node_did: "did:key:zAny".to_string(),
            session_id: "s-1".to_string(),
            home_hint: Some(hint.to_string()),
        };
        assert!(executor.recollect(&locator, &[]).await.is_none(), "{hint}");
    }
}

#[tokio::test]
async fn discarding_is_a_send_future() {
    fn assert_send<T>(_: T)
    where
        T: Send,
    {
    }
    let executor = EmbeddedExecutor::new(
        DocumentRuntimeOptions::default(),
        PathBuf::new(),
        crate::test_support::runtime_handle(),
    );
    assert_send(executor.discard("t1"));
}

#[tokio::test]
async fn a_discarded_trial_is_forgotten_and_executes_as_unprovisioned() {
    let dir = tempfile::tempdir().unwrap();
    let runs_dir = dir.path().join("runs");
    let executor = EmbeddedExecutor::new(
        DocumentRuntimeOptions::default(),
        runs_dir.clone(),
        crate::test_support::runtime_handle(),
    );
    let spec = TrialSpec {
        trial_dir: runs_dir.join("run-1").join("trials").join("t1"),
        ..TrialSpec::empty_for_tests("t1")
    };

    let locator = executor.provision(&spec).await;
    assert!(locator.trial_node_did.starts_with("did:"));
    executor.discard(&spec.trial_id).await;

    let evidence = executor.execute(&spec, CancellationToken::new()).await;
    assert_eq!(evidence.locator.trial_node_did, "did:unprovisioned");
    assert!(evidence.stages.is_empty());
}

pub(super) async fn seed_trigger(
    home: &EmbeddedHome,
    trigger_id: &str,
    status: &str,
    error: Option<&str>,
    last_attempt_at: &str,
) {
    let node_did = escape_graphql_string(home.did());
    let trigger_id = escape_graphql_string(trigger_id);
    let status = escape_graphql_string(status);
    let last_attempt_at = escape_graphql_string(last_attempt_at);
    let last_error = error.map_or("null".to_string(), |error| {
        format!("\"{}\"", escape_graphql_string(error))
    });
    let mutation = format!(
        r#"mutation {{ create_Trigger(input: {{ trigger_id: "{trigger_id}", node_did: "{node_did}", task_id: "task", enabled: true, last_status: "{status}", last_error: {last_error}, last_attempt_at: "{last_attempt_at}" }}) {{ _docID }} }}"#
    );
    let response =
        crate::ConfigAccess::write_local_response(&home.node, "eval.test.trigger", &mutation)
            .await
            .unwrap();
    assert!(response.errors.is_empty(), "{:?}", response.errors);
}

pub(super) async fn seed_request_with_state(home: &EmbeddedHome, request_id: &str, state: &str) {
    let node_did = escape_graphql_string(home.did());
    let request_id = escape_graphql_string(request_id);
    let state = escape_graphql_string(state);
    let mutation = format!(
        r#"mutation {{ create_AgentRequest(input: {{ request_id: "{request_id}", purpose: "normal", node_did: "{node_did}", requester_did: "{node_did}", agent_id: "seeded-agent", session_id: "s-1", content: "x", execution_origin: "interactive", lifecycle_state: "{state}", created_at: "2026-01-01T00:00:00Z" }}) {{ _docID }} }}"#
    );
    let response = home.node.execute(&mutation).await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
}

fn write_minimal_pack(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_vec(&json!({
            "manifest_version": 1,
            "name": "digest_fixture",
            "version": "1.0.0",
            "description": "A pack that loads, so only the digest can disagree.",
            "authors": ["gents-ai contributors"],
            "kind": "documents",
            "assets": ["pack_config.json"],
            "config": "pack_config.json",
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        root.join("pack_config.json"),
        serde_json::to_vec(&json!({"node": {}})).unwrap(),
    )
    .unwrap();
}
