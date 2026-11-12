use super::*;

#[tokio::test]
async fn quickstart_freezes_base_without_fabricating_requests_or_writer_receipts() {
    use gents_store::identity::NodeIdentity;

    let fx = tokio::task::spawn_blocking(Fixture::new).await.unwrap();
    let db = gents_inference::test_support::test_db(
        "quickstart-base-freeze",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = db.node.clone();
    let runtime = gents_store::test_support::held_runtime_identity(db.node_identity.did());
    let user =
        gents_store::identity::KeyIdentity::load_or_create(fx._root.path().join("user.key"), None)
            .unwrap();
    let outsider = gents_store::identity::KeyIdentity::load_or_create(
        fx._root.path().join("outsider.key"),
        None,
    )
    .unwrap();
    let binding = gents_store::identity::provision_agent_identity(&runtime).unwrap();
    let writer_binding = gents_store::identity::provision_agent_identity(&runtime).unwrap();
    let reader_binding = gents_store::identity::provision_agent_identity(&runtime).unwrap();
    let agent = gents_store::identity::resolve_agent_identity(&runtime, &binding).unwrap();
    let writer = gents_store::identity::resolve_agent_identity(&runtime, &writer_binding).unwrap();
    let reader = gents_store::identity::resolve_agent_identity(&runtime, &reader_binding).unwrap();
    let admin = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        runtime.did().parse().unwrap(),
    );
    crate::test_support::grant_operator_user(&node, &runtime, &user).await;
    for actor in [agent.did(), writer.did(), reader.did(), outsider.did()] {
        for permission in [
            gents_store::config_client::NodePermission::DocumentRead,
            gents_store::config_client::NodePermission::DocumentUpdate,
            gents_store::config_client::NodePermission::DacRelationAdd,
            gents_store::config_client::NodePermission::DacRelationDelete,
            gents_store::config_client::NodePermission::CollectionGet,
        ] {
            admin
                .grant_node_permission(&actor.parse().unwrap(), permission)
                .await
                .unwrap();
        }
    }
    let operator = db.client_access(&user);
    let outsider_access = db.client_access(&outsider);
    let (node_doc, agent_doc): (String, String) = operator.transact_write("test.quickstart.owner", |txn| Box::pin(async {
        let node_result = txn.execute_client_signed_create("mutation($input: NodeMutationInputArg!) { create_Node(input:$input) {_docID} }",
            &serde_json::json!({"input":{"node_did":runtime.did()}}), gents_protocol::schemas::NODE).await?;
        let node_doc = gents_store::graphql::created_doc_id(&node_result, "Node")?;
        let agent_result = txn.execute_client_signed_create("mutation($input: AgentMutationInputArg!) { create_Agent(input:$input) {_docID} }",
            &serde_json::json!({"input":{"node_did":runtime.did(),"agent_did":binding.agent_did,"creation_nonce":binding.creation_nonce,"enabled":true,"inference_profile_id":"profile"}}), gents_protocol::schemas::AGENT).await?;
        let agent_doc = gents_store::graphql::created_doc_id(&agent_result, "Agent")?;
        Ok::<_, anyhow::Error>((node_doc, agent_doc))
    })).await.unwrap();
    for target in [&writer_binding, &reader_binding] {
        operator.transact_write("test.quickstart.target_agent", |txn| Box::pin(async {
            let response = txn.execute_client_signed_create(
                "mutation($input: AgentMutationInputArg!) { create_Agent(input:$input) {_docID} }",
                &serde_json::json!({"input":{"node_did":runtime.did(), "agent_did":target.agent_did,
                    "creation_nonce":target.creation_nonce, "enabled":true, "inference_profile_id":"profile"}}), gents_protocol::schemas::AGENT).await?;
            let doc = gents_store::graphql::created_doc_id(&response, "Agent")?;
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
    let access = gents_store::config_client::ConfigAccess::local_managed_agent(
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
    let provisioned =
        provision_read_only_workspace(&operator, &access, &fx.repo, &fx.base_sha, &selection).await;
    let legal = provisioned.is_ok();
    let outcome = provisioned.unwrap();
    let target_query = format!(
        "{{Agent(filter:{{agent_did:{{_eq:\"{}\"}}}}) {{_docID}}}}",
        gents_store::graphql::escape_graphql_string(writer.did())
    );
    let target_doc = operator.execute(&target_query).await.unwrap()["data"]["Agent"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    let quickstart_cases =
        &gents_lean_vocab::lean_contract_snapshot().workspace_quickstart_selection_cases;
    for case in quickstart_cases {
        let actor = match case.actor.as_str() {
            "user" => &operator,
            "creator" => &access,
            "other" => &outsider_access,
            name => panic!("unknown modeled actor {name}"),
        };
        let visible = !actor.execute(&target_query).await.unwrap()["data"]["Agent"]
            .as_array()
            .unwrap()
            .is_empty();
        assert_eq!(visible, case.before, "{case:?}");
    }
    for case in gents_lean_vocab::families::contracts_workspace_quickstart_transition::cases() {
        assert_eq!(case.input.name, "provision_frozen_base");
        assert_eq!(case.input.from_state, "provisioning");
        assert_eq!(
            outcome.workspace.seal_hash.is_some(),
            case.input.seal_hash.is_some()
        );
        case.check(
            gents_lean_vocab::families::contracts_workspace_quickstart_transition::WorkspaceTransitionOutput {
                legal,
                state: outcome.workspace.lifecycle_state.clone(),
            },
        );
    }
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
    for case in &gents_lean_vocab::lean_contract_snapshot().workspace_quickstart_cases {
        let actor = match case.actor.as_str() {
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
            case.before,
            "{case:?}"
        );
    }
    // Commit history is read as the signing User; the node has no fallback.
    let history = operator
        .execute(&format!(
            r#"{{ _commits(docID: "{}") {{ cid height fieldName signature {{ identity type }} }} }}"#,
            gents_store::graphql::escape_graphql_string(repository_doc)
        ))
        .await
        .unwrap();
    let commits =
        gents_store::graphql::composite_commits_at(history["data"].get("_commits"), "_commits")
            .unwrap();
    let genesis = commits.iter().min_by_key(|commit| commit.height).unwrap();
    let user_signer = gents_store::identity::commit_signer_identity_for_did(user.did()).unwrap();
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
        let actor = match case.actor.as_str() {
            "user" => &operator,
            "creator" => &access,
            "other" => &outsider_access,
            name => panic!("unknown modeled actor {name}"),
        };
        let visible = !actor.execute(&target_query).await.unwrap()["data"]["Agent"]
            .as_array()
            .unwrap()
            .is_empty();
        assert_eq!(visible, case.after_revocation, "{case:?}");
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
    for case in &gents_lean_vocab::lean_contract_snapshot().workspace_quickstart_cases {
        let actor = match case.actor.as_str() {
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
            case.after_revoke,
            "{case:?}"
        );
    }
    let unpublished = fx._root.path().join("unpublished-repository");
    tokio::fs::create_dir(&unpublished).await.unwrap();
    let mut denied = selection.clone();
    denied.workspace_id = uuid::Uuid::new_v4().to_string();
    admin
        .revoke_node_permission(
            &user.did().parse().unwrap(),
            gents_store::config_client::NodePermission::DacRelationAdd,
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
        gents_lean_vocab::lean_contract_snapshot().workspace_quickstart_cases[0]
            .aborted_document_visible
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
