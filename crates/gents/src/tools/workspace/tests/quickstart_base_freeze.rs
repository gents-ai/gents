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
    let writer_binding = crate::store::identity::provision_agent_identity(&runtime).unwrap();
    let reader_binding = crate::store::identity::provision_agent_identity(&runtime).unwrap();
    let agent = crate::store::identity::resolve_agent_identity(&runtime, &binding).unwrap();
    let writer = crate::store::identity::resolve_agent_identity(&runtime, &writer_binding).unwrap();
    let reader = crate::store::identity::resolve_agent_identity(&runtime, &reader_binding).unwrap();
    let admin = crate::ConfigAccess::local_as(node.clone(), runtime.did().parse().unwrap());
    for actor in [
        user.did(),
        agent.did(),
        writer.did(),
        reader.did(),
        outsider.did(),
    ] {
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
            &serde_json::json!({"input":{"node_did":runtime.did(),"agent_did":binding.agent_did,"creation_nonce":binding.creation_nonce,"enabled":true,"inference_profile_id":"profile"}}), gents_protocol::schemas::AGENT).await?;
        let agent_doc = crate::store::graphql::created_doc_id(&agent_result, "Agent")?;
        Ok::<_, anyhow::Error>((node_doc, agent_doc))
    })).await.unwrap();
    for target in [&writer_binding, &reader_binding] {
        operator.transact_write("test.quickstart.target_agent", |txn| Box::pin(async {
            let response = txn.execute_client_signed_create(
                "mutation($input: AgentMutationInputArg!) { create_Agent(input:$input) {_docID} }",
                &serde_json::json!({"input":{"node_did":runtime.did(), "agent_did":target.agent_did,
                    "creation_nonce":target.creation_nonce, "enabled":true, "inference_profile_id":"profile"}}), gents_protocol::schemas::AGENT).await?;
            let doc = crate::store::graphql::created_doc_id(&response, "Agent")?;
            txn.add_resource_relationship("Agent", &doc, "parent", "nodes", &node_doc).await?;
            txn.add_actor_relationship("Agent", &doc, "executor", &target.agent_did.parse()?).await?;
            Ok::<_, anyhow::Error>(())
        })).await.unwrap();
    }
    operator
        .add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node_doc)
        .await
        .unwrap();
    operator
        .add_actor_relationship(
            "Agent",
            &agent_doc,
            "executor",
            &agent.did().parse().unwrap(),
        )
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
        writer_agent_did: writer.did().into(),
        integrator_agent_did: writer.did().into(),
        reader_agent_dids: vec![reader.did().into()],
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
    let target_query = format!(
        "{{Agent(filter:{{agent_did:{{_eq:\"{}\"}}}}) {{_docID}}}}",
        crate::store::graphql::escape_graphql_string(writer.did())
    );
    let target_doc = operator.execute(&target_query).await.unwrap()["data"]["Agent"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    let quickstart_cases =
        &crate::lean_vocab_test::lean_contract_snapshot().workspace_quickstart_selection_cases;
    for case in quickstart_cases {
        let actor = match case["actor"].as_str().unwrap() {
            "user" => &operator,
            "creator" => &access,
            "other" => &outsider_access,
            name => panic!("unknown modeled actor {name}"),
        };
        let visible = !actor.execute(&target_query).await.unwrap()["data"]["Agent"]
            .as_array()
            .unwrap()
            .is_empty();
        assert_eq!(visible, case["before"].as_bool().unwrap(), "{case}");
    }
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
    for case in &crate::lean_vocab_test::lean_contract_snapshot().workspace_quickstart_cases {
        let actor = match case["actor"].as_str().unwrap() {
            "user" => &operator,
            "agent" => &access,
            "other" => &outsider_access,
            name => panic!("unknown modeled actor {name}"),
        };
        let rows = actor
            .execute("{RepositoryPlacement {_docID}}")
            .await
            .unwrap();
        assert_eq!(
            !rows["data"]["RepositoryPlacement"]
                .as_array()
                .unwrap()
                .is_empty(),
            case["before"].as_bool().unwrap(),
            "{case}"
        );
    }
    // Commit history is read as the signing User; the node has no fallback.
    let history = operator
        .execute(&format!(
            r#"{{ _commits(docID: "{}") {{ cid height fieldName signature {{ identity type }} }} }}"#,
            crate::store::graphql::escape_graphql_string(repository_doc)
        ))
        .await
        .unwrap();
    let commits =
        crate::store::graphql::composite_commits_at(history["data"].get("_commits"), "_commits")
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
        .transact_write("test.quickstart.revoke_target", |txn| {
            let (target_doc, agent_doc) = (&target_doc, &agent_doc);
            Box::pin(async move {
                txn.delete_entity_set_relationship(
                    "Agent",
                    target_doc,
                    "workspace_selected_by",
                    "agents",
                    agent_doc,
                    "executor",
                )
                .await
            })
        })
        .await
        .unwrap();
    provision_read_only_workspace(&operator, &access, &fx.repo, &fx.base_sha, &selection)
        .await
        .unwrap();
    for case in quickstart_cases {
        let actor = match case["actor"].as_str().unwrap() {
            "user" => &operator,
            "creator" => &access,
            "other" => &outsider_access,
            name => panic!("unknown modeled actor {name}"),
        };
        let visible = !actor.execute(&target_query).await.unwrap()["data"]["Agent"]
            .as_array()
            .unwrap()
            .is_empty();
        assert_eq!(
            visible,
            case["after_revocation"].as_bool().unwrap(),
            "{case}"
        );
    }
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
    for case in &crate::lean_vocab_test::lean_contract_snapshot().workspace_quickstart_cases {
        let actor = match case["actor"].as_str().unwrap() {
            "user" => &operator,
            "agent" => &access,
            "other" => &outsider_access,
            name => panic!("unknown modeled actor {name}"),
        };
        let rows = actor
            .execute("{RepositoryPlacement {_docID}}")
            .await
            .unwrap();
        assert_eq!(
            !rows["data"]["RepositoryPlacement"]
                .as_array()
                .unwrap()
                .is_empty(),
            case["after_revoke"].as_bool().unwrap(),
            "{case}"
        );
    }
    let unpublished = fx._root.path().join("unpublished-repository");
    tokio::fs::create_dir(&unpublished).await.unwrap();
    let mut denied = selection.clone();
    denied.workspace_id = uuid::Uuid::new_v4().to_string();
    admin
        .revoke_node_permission(
            &user.did().parse().unwrap(),
            crate::store::config_client::NodePermission::DacRelationAdd,
        )
        .await
        .unwrap();
    assert!(
        provision_read_only_workspace(&operator, &access, &unpublished, &fx.base_sha, &denied)
            .await
            .is_err()
    );
    let retained = operator
        .execute("{RepositoryPlacement {_docID}}")
        .await
        .unwrap();
    let created_after_abort = retained["data"]["RepositoryPlacement"]
        .as_array()
        .unwrap()
        .len()
        > 1;
    assert_eq!(
        created_after_abort,
        crate::lean_vocab_test::lean_contract_snapshot().workspace_quickstart_cases[0]
            ["aborted_document_visible"]
            .as_bool()
            .unwrap()
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
