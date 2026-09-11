use super::*;
use crate::graph_pipeline::{activate_graph_revision, start_graph_run};
use crate::test_support::{install_test_graph_package, load_test_graph_package};
use defra_node::EmbeddedNode;
use serde_json::json;
use std::sync::Arc;

async fn fixture() -> (Arc<EmbeddedNode>, ConfigAccess, GraphPackageInstallBindings) {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let options = GraphPackageInstallBindings {
        agent_did: "did:key:package-owner".into(),
    };
    crate::document_config::ensure_agent_principal(&node, &options.agent_did)
        .await
        .unwrap();
    let access = ConfigAccess::Local(node.clone());
    (node, access, options)
}

#[test]
fn explicit_graph_selection_preserves_authored_identity_and_rejects_ambiguity() {
    let options = GraphPackageInstallBindings {
        agent_did: "did:key:owner".into(),
    };
    let mut package = load_test_graph_package("code_review", &options);
    assert_eq!(
        selected_intent(&package, None).unwrap().graph_id,
        "code-review"
    );
    let mut second = package.config.graph_intents[0].clone();
    second.graph_id = "second-review".into();
    package.config.graph_intents.push(second.clone());
    assert!(selected_intent(&package, None).is_err());
    assert_eq!(
        selected_intent(&package, Some("second-review"))
            .unwrap()
            .graph_id,
        "second-review"
    );
    assert!(selected_intent(&package, Some("absent")).is_err());
    package.config.graph_intents.push(second);
    assert!(selected_intent(&package, Some("second-review")).is_err());
}

#[tokio::test]
async fn missing_or_foreign_owner_does_not_register_package_schema() {
    let (node, access, options) = fixture().await;
    assert!(
        install_test_graph_package(&access, "did:key:foreign", "code_review", &options)
            .await
            .is_err()
    );
    let missing = GraphPackageInstallBindings {
        agent_did: "did:key:missing".into(),
    };
    assert!(
        install_test_graph_package(&access, &missing.agent_did, "code_review", &missing)
            .await
            .is_err()
    );
    assert!(node.get_collection("CodeReviewJob").unwrap().is_none());
}

#[tokio::test]
async fn invalid_retained_reference_fails_before_any_package_schema_write() {
    let (node, access, options) = fixture().await;
    let response = node
        .execute(
            r#"mutation { create_Task(input: {
        agent_did: "did:key:package-owner", task_id: "unrelated-broken-task",
        behavior_id: "missing-behavior", prompt_template: "Keep this reference visible"
    }) { _docID } }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let error = install_test_graph_package(&access, &options.agent_did, "code_review", &options)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("missing-behavior"),
        "{error:#}"
    );
    assert!(node.get_collection("CodeReviewJob").unwrap().is_none());
    let response = node
        .execute("{ AgentBehavior { behavior_id } GraphRevision { digest } }")
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let data = response.data.unwrap();
    assert!(data["AgentBehavior"].as_array().unwrap().is_empty());
    assert!(data["GraphRevision"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn existing_package_schema_must_match_types_indexes_and_immutability() {
    let (node, access, options) = fixture().await;
    let package = load_test_graph_package("code_review", &options);
    let expected = package.asset_text("schemas/review_job.graphql").unwrap();
    let incompatible = expected.replace(
        "run_id: String @index(unique: true) @immutable",
        "run_id: Int",
    );
    assert_ne!(incompatible, expected);
    node.add_schema(&incompatible).await.unwrap();
    let error = install_test_graph_package(&access, &options.agent_did, "code_review", &options)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("does not match bundled schema"),
        "{error:#}"
    );
    assert!(node.get_collection("CodeReviewArea").unwrap().is_none());
}

#[tokio::test]
async fn code_review_install_is_idempotent_shared_home_safe_and_runnable() {
    let (node, access, options) = fixture().await;
    let metadata = node
        .execute(
            r#"mutation { update_AgentPrincipal(filter: {
        agent_did: {_eq: "did:key:package-owner"}
    }, input: {display_name: "Keep owner metadata", tags: ["unrelated-owner-tag"]}) {_docID} }"#,
        )
        .await;
    assert!(!metadata.has_errors(), "{:?}", metadata.errors);
    let unrelated = node
        .execute(
            r#"mutation { create_Task(input: {
        agent_did: "did:key:package-owner", task_id: "unrelated-task",
        behavior_id: "review-recon", prompt_template: "Keep me", enabled: false
    }) {_docID} }"#,
        )
        .await;
    assert!(!unrelated.has_errors(), "{:?}", unrelated.errors);
    let first = install_test_graph_package(&access, &options.agent_did, "code_review", &options)
        .await
        .unwrap();
    let second = install_test_graph_package(&access, &options.agent_did, "code_review", &options)
        .await
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(first.desired_documents, 29);
    assert_eq!(first.graph_id, "code-review");
    let state = node
        .execute(
            r#"{
        AgentPrincipal {agent_did display_name tags}
        AgentBehavior {behavior_id agent_did context_id inference_profile_id}
        Task {task_id agent_did goal_objective_template goal_token_budget}
        GraphRevision {digest artifacts_complete plan_json}
        InferenceSampling {sampling_id temperature top_p}
        InferenceExecution {execution_id max_turns}
        InferenceProfile {profile_id sampling_id execution_id reasoning_effort}
        InferenceBackend {backend_id max_concurrent}
    }"#,
        )
        .await;
    assert!(!state.has_errors(), "{:?}", state.errors);
    let data = state.data.unwrap();
    assert_eq!(
        data["AgentPrincipal"][0]["display_name"],
        "Keep owner metadata"
    );
    assert_eq!(
        data["AgentPrincipal"][0]["tags"],
        json!(["unrelated-owner-tag"])
    );
    assert_eq!(data["GraphRevision"].as_array().unwrap().len(), 1);
    assert_eq!(data["GraphRevision"][0]["artifacts_complete"], true);
    assert!(data["Task"]
        .as_array()
        .unwrap()
        .iter()
        .any(|task| task["task_id"] == "unrelated-task"));
    for task in data["Task"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|task| task["task_id"] != "unrelated-task")
    {
        assert!(task["goal_objective_template"]
            .as_str()
            .is_some_and(|s| !s.is_empty()));
        assert!(task["goal_token_budget"].as_i64().is_some_and(|n| n > 0));
    }
    assert_eq!(data["InferenceSampling"][0]["temperature"], 1.0);
    assert_eq!(data["InferenceSampling"][0]["top_p"], 0.95);
    assert_eq!(data["InferenceExecution"][0]["max_turns"], 1000);
    assert_eq!(data["InferenceBackend"][0]["max_concurrent"], 8);
    for profile in data["InferenceProfile"].as_array().unwrap() {
        assert_eq!(profile["sampling_id"], "review-sampling");
        assert_eq!(profile["execution_id"], "review-execution");
        assert_eq!(profile["reasoning_effort"], "high");
    }
    let plan: GraphPlan =
        serde_json::from_str(data["GraphRevision"][0]["plan_json"].as_str().unwrap()).unwrap();
    let artifacts = &plan.package.as_ref().unwrap().artifacts;
    assert_eq!(artifacts.len(), 29);
    for collection in [
        Collection::Tools,
        Collection::AgentContext,
        Collection::InferenceProfile,
        Collection::InferenceBackend,
        Collection::InferenceSampling,
        Collection::InferenceExecution,
    ] {
        assert!(artifacts
            .iter()
            .any(|artifact| artifact.collection == collection));
    }
    assert!(!artifacts
        .iter()
        .any(|artifact| artifact.collection == Collection::AgentPrincipal));
    activate_graph_revision(
        &node,
        None,
        &options.agent_did,
        &first.graph_id,
        &first.revision_digest,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        first,
        install_test_graph_package(&access, &options.agent_did, "code_review", &options)
            .await
            .unwrap()
    );
    let run = start_graph_run(
        &node,
        None,
        &options.agent_did,
        &first.graph_id,
        None,
        "review",
        json!({
            "repository_path":"/tmp/repo", "base_ref":"base-sha", "head_ref":"head-sha",
            "lens_count":"4", "lens_min":"4", "lens_max":"4", "focus":"durability"
        }),
    )
    .await
    .unwrap();
    assert_eq!(run.revision_digest, first.revision_digest);
    // A metadata-only successor still pins the active predecessor exactly.
    let mut successor = load_test_graph_package("code_review", &options);
    successor.manifest.version.push_str("-successor");
    successor.package_digest = digest_bytes(b"successor distribution");
    let prepared = prepare_package(&access, &successor, &options, None)
        .await
        .unwrap();
    assert_ne!(prepared.plan.digest, first.revision_digest);
    assert_eq!(
        prepared
            .plan
            .package
            .unwrap()
            .predecessor_revision_digest
            .as_deref(),
        Some(first.revision_digest.as_str())
    );
    // Identical authored logical IDs may coexist under another selected DID.
    let foreign = GraphPackageInstallBindings {
        agent_did: "did:key:second-owner".into(),
    };
    crate::document_config::ensure_agent_principal(&node, &foreign.agent_did)
        .await
        .unwrap();
    let other = install_test_graph_package(&access, &foreign.agent_did, "code_review", &foreign)
        .await
        .unwrap();
    assert_eq!(other.graph_id, first.graph_id);
    assert_ne!(other.revision_digest, first.revision_digest);
    activate_graph_revision(
        &node,
        None,
        &foreign.agent_did,
        &other.graph_id,
        &other.revision_digest,
        None,
    )
    .await
    .unwrap();
    for (owner, receipt) in [(&options.agent_did, &first), (&foreign.agent_did, &other)] {
        let selected = load_installed_package_plan(&access, "code_review", owner)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(selected.digest, receipt.revision_digest);
        assert_eq!(selected.graph_id, receipt.graph_id);
    }
    assert!(
        load_installed_package_plan(&access, "code_review", "did:key:absent")
            .await
            .unwrap()
            .is_none()
    );
}
