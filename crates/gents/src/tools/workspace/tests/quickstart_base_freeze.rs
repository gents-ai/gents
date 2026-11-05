use super::*;

#[tokio::test]
async fn quickstart_freezes_base_without_fabricating_requests_or_writer_receipts() {
    use crate::store::identity::NodeIdentity;
    use std::sync::Arc;

    let fx = tokio::task::spawn_blocking(Fixture::new).await.unwrap();
    let runtime =
        crate::KeyIdentity::load_or_create(fx._root.path().join("runtime.key"), None).unwrap();
    let user = crate::KeyIdentity::load_or_create(fx._root.path().join("user.key"), None).unwrap();
    let outsider =
        crate::KeyIdentity::load_or_create(fx._root.path().join("outsider.key"), None).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let node = Arc::new(
        defra_node::Node::builder()
            .enable_signing()
            .with_node_identity_did(runtime.did())
            .with_node_acp_enabled()
            .with_http(defra_node::HttpConfig::with_listener(listener).unwrap())
            .data_path(fx._root.path().join("db"))
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let binding = crate::store::identity::provision_agent_identity(&runtime).unwrap();
    let agent = crate::store::identity::resolve_agent_identity(&runtime, &binding).unwrap();
    let admin = crate::ConfigAccess::local_as(node.clone(), runtime.did().parse().unwrap());
    for actor in [user.did(), agent.did(), outsider.did()] {
        for permission in [
            crate::store::config_client::NodePermission::DocumentRead,
            crate::store::config_client::NodePermission::DocumentUpdate,
            crate::store::config_client::NodePermission::DacRelationAdd,
            crate::store::config_client::NodePermission::DacRelationDelete,
            crate::store::config_client::NodePermission::CollectionGet,
        ] {
            admin
                .grant_node_permission(&actor.parse().unwrap(), permission)
                .await
                .unwrap();
        }
    }
    let endpoint = format!("http://{address}/api/v0/graphql");
    let operator = crate::ConfigAccess::Graphql(
        crate::store::config_client::GraphqlEndpoint::as_principal(&endpoint, user.did())
            .with_client_signing_capability(user.client_signing_capability().unwrap())
            .unwrap()
            .with_client_signed_agent_creates(),
    );
    let outsider_access = crate::ConfigAccess::Graphql(
        crate::store::config_client::GraphqlEndpoint::as_principal(&endpoint, outsider.did())
            .with_client_signing_capability(outsider.client_signing_capability().unwrap())
            .unwrap(),
    );
    let (node_doc, agent_doc): (String, String) = operator.transact_write("test.quickstart.owner", |txn| Box::pin(async {
        let node_result = txn.execute_client_signed_create("mutation($input: NodeMutationInputArg!) { create_Node(input:$input) {_docID} }",
            &serde_json::json!({"input":{"node_did":runtime.did()}}), gents_protocol::schemas::NODE).await?;
        let node_doc = crate::store::graphql::created_doc_id(&node_result, "Node")?;
        let agent_result = txn.execute_client_signed_create("mutation($input: AgentMutationInputArg!) { create_Agent(input:$input) {_docID} }",
            &serde_json::json!({"input":{"node_did":runtime.did(),"agent_did":binding.agent_did,"creation_nonce":binding.creation_nonce,"enabled":true}}), gents_protocol::schemas::AGENT).await?;
        let agent_doc = crate::store::graphql::created_doc_id(&agent_result, "Agent")?;
        Ok::<_, anyhow::Error>((node_doc, agent_doc))
    })).await.unwrap();
    operator
        .add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node_doc)
        .await
        .unwrap();
    operator
        .add_actor_relationship("Agent", &agent_doc, "reader", &agent.did().parse().unwrap())
        .await
        .unwrap();
    let access = crate::ConfigAccess::local_managed_agent(
        node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let selection = WorkspaceProvisioningSelection {
        workspace_id: uuid::Uuid::new_v4().to_string(),
        node_did: runtime.did().into(),
        creator_agent_did: agent.did().into(),
        writer_agent_did: agent.did().into(),
        integrator_agent_did: agent.did().into(),
    };
    let before_head = git(&fx.repo, &["rev-parse", "HEAD"]);
    let before_index = git(&fx.repo, &["write-tree"]);
    let base_tree = git(
        &fx.repo,
        &["rev-parse", &format!("{}^{{tree}}", fx.base_sha)],
    );
    let outcome =
        provision_read_only_workspace(&operator, &access, &fx.repo, &fx.base_sha, &selection)
            .await
            .unwrap();
    let modeled = crate::lean_vocab_test::lean_workspace_cases()
        .iter()
        .find(|case| case.name == "provision_frozen_base")
        .unwrap();
    assert!(modeled.legal);
    assert_eq!(modeled.from, "provisioning");
    assert_eq!(outcome.workspace.lifecycle_state, modeled.to);
    assert_eq!(
        outcome.workspace.seal_hash.is_some(),
        modeled.seal_hash.is_some()
    );
    assert_eq!(outcome.workspace.base_sha, fx.base_sha);
    assert_eq!(outcome.workspace.owner_node_did, runtime.did());
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
        super::super::adapter::working_tree_hash(placed)
            .await
            .unwrap(),
        base_tree
    );

    let persisted = access.execute(r#"{
        IsolatedWorkspace { workspace_id owner_node_did base_sha lifecycle_state seal_hash path_capability }
        WorkspacePlacement { workspace_id owner_node_did host_path observed_tree_hash }
        RepositoryPlacement { repository_id node_did }
        AgentRequest { _docID }
        AgentMessage { _docID }
        AgentOutputSegment { _docID }
        WorkspaceReceipt { _docID kind }
        WorkspaceBinding { _docID }
    }"#).await;
    let data = persisted.unwrap()["data"].clone();
    assert_eq!(data["IsolatedWorkspace"].as_array().unwrap().len(), 1);
    let workspace = &data["IsolatedWorkspace"][0];
    assert_eq!(workspace["workspace_id"], outcome.workspace.workspace_id);
    assert_eq!(workspace["lifecycle_state"], "sealed");
    assert_eq!(workspace["owner_node_did"], runtime.did());
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
        data["WorkspacePlacement"][0]["owner_node_did"],
        runtime.did()
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
    let outside = outsider_access
        .execute(
            "{RepositoryPlacement {_docID} IsolatedWorkspace {_docID} WorkspacePlacement {_docID}}",
        )
        .await
        .unwrap();
    for collection in [
        "RepositoryPlacement",
        "IsolatedWorkspace",
        "WorkspacePlacement",
    ] {
        assert!(
            outside["data"][collection].as_array().unwrap().is_empty(),
            "outsider sees {collection}"
        );
    }
    let repository = operator
        .execute("{RepositoryPlacement {_docID}}")
        .await
        .unwrap();
    let repository_doc = repository["data"]["RepositoryPlacement"][0]["_docID"]
        .as_str()
        .unwrap();
    let commits = crate::store::graphql::composite_commits(
        &node,
        repository_doc,
        "test quickstart repository signature",
    )
    .await
    .unwrap();
    let genesis = commits.iter().min_by_key(|commit| commit.height).unwrap();
    let user_signer = crate::store::identity::commit_signer_identity_for_did(user.did()).unwrap();
    assert_eq!(
        genesis
            .signature
            .as_ref()
            .map(|signature| signature.identity.as_str()),
        Some(user_signer.as_str())
    );
    let replayed =
        provision_read_only_workspace(&operator, &access, &fx.repo, &fx.base_sha, &selection)
            .await
            .unwrap();
    assert_eq!(replayed, outcome);
    operator
        .delete_actor_relationship(
            "RepositoryPlacement",
            repository_doc,
            "reader",
            &agent.did().parse().unwrap(),
        )
        .await
        .unwrap();
    assert!(
        provision_read_only_workspace(&operator, &access, &fx.repo, &fx.base_sha, &selection)
            .await
            .is_err()
    );
    assert!(
        access
            .execute("{RepositoryPlacement {_docID}}")
            .await
            .unwrap()["data"]["RepositoryPlacement"]
            .as_array()
            .unwrap()
            .is_empty(),
        "quickstart replay restored the revoked repository reader"
    );
    for collection in [
        "AgentRequest",
        "AgentMessage",
        "AgentOutputSegment",
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
    node.shutdown().await.expect("shut down test node");
}
