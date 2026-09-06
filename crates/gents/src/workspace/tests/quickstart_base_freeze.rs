//! Actual quickstart + native DB persistence; Git setup comes from workspace tests.
use super::*;

#[tokio::test]
async fn quickstart_freezes_base_without_fabricating_requests_or_writer_receipts() {
    use crate::identity::AgentIdentity;
    use std::sync::Arc;

    let fx = Fixture::new();
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(fx._root.path().join("db"))
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let identity =
        crate::identity::KeyIdentity::load_or_create(fx._root.path().join("operator.key"), None)
            .unwrap();
    crate::document_config::ensure_agent_principal(&node, identity.did())
        .await
        .unwrap();
    let deployment = node.execute(r#"mutation {
        create_HostDeployment(input: {deployment_id: "freeze-local", display_name: "Freeze fixture"}) { _docID }
    }"#).await;
    assert!(!deployment.has_errors(), "{:?}", deployment.errors);
    let access = crate::ConfigAccess::Local(node.clone());
    let before_head = git(&fx.repo, &["rev-parse", "HEAD"]);
    let before_index = git(&fx.repo, &["write-tree"]);
    let base_tree = git(
        &fx.repo,
        &["rev-parse", &format!("{}^{{tree}}", fx.base_sha)],
    );
    let outcome = provision_read_only_workspace(
        &access,
        &fx.repo,
        &fx.base_sha,
        "freeze-local",
        identity.did(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.workspace.lifecycle_state, "sealed");
    assert_eq!(outcome.workspace.base_sha, fx.base_sha);
    assert_eq!(outcome.workspace.owner_deployment_id, "freeze-local");
    assert_eq!(
        outcome.workspace.path_capability,
        WorkspacePathCapability::exact_paths(Vec::new()).unwrap()
    );
    assert_eq!(
        outcome.workspace.seal_hash.as_deref(),
        Some(base_tree.as_str())
    );
    assert_eq!(outcome.placement.observed_tree_hash, base_tree);
    let placed = Path::new(&outcome.placement.host_path);
    assert_eq!(git(placed, &["rev-parse", "HEAD"]), fx.base_sha);
    assert_eq!(
        super::super::adapter::working_tree_hash(placed).unwrap(),
        base_tree
    );

    // Inspect persisted state independently of the returned in-memory result.
    let persisted = node.execute(r#"{
        IsolatedWorkspace { workspace_id owner_deployment_id base_sha lifecycle_state seal_hash path_capability }
        WorkspacePlacement { workspace_id deployment_id host_path observed_tree_hash }
        RepositoryPlacement { repository_id deployment_id }
        AgentRequest { _docID }
        AgentResponse { _docID }
        WorkspaceReceipt { _docID kind }
        WorkspaceBinding { _docID }
    }"#).await;
    assert!(!persisted.has_errors(), "{:?}", persisted.errors);
    let data = persisted.data.unwrap();
    assert_eq!(data["IsolatedWorkspace"].as_array().unwrap().len(), 1);
    let workspace = &data["IsolatedWorkspace"][0];
    assert_eq!(workspace["workspace_id"], outcome.workspace.workspace_id);
    assert_eq!(workspace["lifecycle_state"], "sealed");
    assert_eq!(workspace["owner_deployment_id"], "freeze-local");
    assert_eq!(workspace["base_sha"], fx.base_sha);
    assert_eq!(workspace["seal_hash"], base_tree);
    let capability: WorkspacePathCapability =
        serde_json::from_str(workspace["path_capability"].as_str().unwrap()).unwrap();
    assert_eq!(
        capability,
        WorkspacePathCapability::exact_paths(Vec::new()).unwrap()
    );
    assert_eq!(data["WorkspacePlacement"].as_array().unwrap().len(), 1);
    assert_eq!(
        data["WorkspacePlacement"][0]["workspace_id"],
        workspace["workspace_id"]
    );
    assert_eq!(
        data["WorkspacePlacement"][0]["deployment_id"],
        "freeze-local"
    );
    assert_eq!(
        data["WorkspacePlacement"][0]["host_path"],
        outcome.placement.host_path
    );
    assert_eq!(
        data["WorkspacePlacement"][0]["observed_tree_hash"],
        base_tree
    );
    assert_eq!(data["RepositoryPlacement"].as_array().unwrap().len(), 1);
    for collection in [
        "AgentRequest",
        "AgentResponse",
        "WorkspaceReceipt",
        "WorkspaceBinding",
    ] {
        assert!(
            data[collection].as_array().unwrap().is_empty(),
            "operator-only base freeze fabricated {collection}: {}",
            data[collection]
        );
    }
    assert_eq!(git(&fx.repo, &["rev-parse", "HEAD"]), before_head);
    assert_eq!(git(&fx.repo, &["write-tree"]), before_index);
    drop(access);
    node.shutdown().await;
}
