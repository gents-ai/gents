use gents_documents::observe_unattended_agent_in_txn;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::NodeIdentity;

async fn observe(access: &ConfigAccess, owner: &str, selected: &str) -> bool {
    access
        .transact_readonly("test.unattended_agent", |txn| {
            Box::pin(async move {
                observe_unattended_agent_in_txn(txn, owner, selected)
                    .await
                    .map_err(anyhow::Error::new)
            })
        })
        .await
        .is_ok()
}

#[tokio::test]
async fn enabled_and_native_invoke_are_independent_at_unattended_admission() -> anyhow::Result<()> {
    let cases = &gents_lean_vocab::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap()
        .unattended_agent_cases;
    let expected = |name: &str| {
        cases.iter().find(|case| case["name"] == name).unwrap()["allowed"]
            .as_bool()
            .unwrap()
    };
    let db = gents_inference::test_support::test_db(
        "enabled-and-native-invoke-are-independen",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = db.node.clone();
    let root = gents_store::test_support::held_runtime_identity(db.node_identity.did());
    let home = tempfile::tempdir()?;
    let user =
        gents_store::identity::KeyIdentity::load_or_create(home.path().join("user.key"), None)?;
    let agent_binding = gents_store::identity::provision_agent_identity(&root)?;
    let selected = gents_store::identity::resolve_agent_identity(&root, &agent_binding)?;
    crate::test_support::grant_operator_user(&node, &root, &user).await;
    let owner_access = db.client_access(&user);
    let agent_access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(selected.did())?,
        selected.as_ref(),
    )?;
    let root_did = root.did();
    let selected_did = selected.did();
    let nonce = &agent_binding.creation_nonce;
    let agent_doc = owner_access.transact_write("test.unattended.agent", |txn| {
        Box::pin(async move {
            let response = txn.execute_client_signed_create(
                "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
                &serde_json::json!({"input": {"node_did": root_did}}),
                gents_protocol::schemas::NODE,
            ).await?;
            let node_doc = gents_store::graphql::created_doc_id(&response, "Node")?;
            let response = txn.execute_client_signed_create(
                "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                &serde_json::json!({"input": {"node_did": root_did, "agent_did": selected_did,
                    "creation_nonce": nonce, "inference_profile_id": "profile", "enabled": true}}),
                gents_protocol::schemas::AGENT,
            ).await?;
            let doc = gents_store::graphql::created_doc_id(&response, "Agent")?;
            txn.add_resource_relationship("Agent", &doc, "parent", "nodes", &node_doc).await?;
            txn.add_actor_relationship("Agent", &doc, "executor", &identity::Did::new(selected_did)?).await?;
            Ok::<_, anyhow::Error>(doc)
        })
    }).await?;
    assert_eq!(
        observe(&agent_access, root.did(), selected.did()).await,
        expected("allowed")
    );
    assert_eq!(
        observe(&owner_access, root.did(), selected.did()).await,
        expected("wrong_actor")
    );
    owner_access
        .transact_write("test.unattended.disable", |txn| {
            let agent_doc = &agent_doc;
            Box::pin(async move {
                txn.execute_client_signed_update(
                    &format!(
                        "mutation($input: AgentMutationInputArg!) {{update_Agent(docID:\"{}\",input:$input){{_docID}}}}",
                        gents_store::graphql::escape_graphql_string(agent_doc)
                    ),
                    &serde_json::json!({"input": {"enabled": false}}),
                    gents_protocol::schemas::AGENT,
                )
                .await
            })
        })
        .await?;
    assert_eq!(
        observe(&agent_access, root.did(), selected.did()).await,
        expected("disabled")
    );
    owner_access
        .transact_write("test.unattended.enable", |txn| {
            let agent_doc = &agent_doc;
            Box::pin(async move {
                txn.execute_client_signed_update(
                    &format!(
                        "mutation($input: AgentMutationInputArg!) {{update_Agent(docID:\"{}\",input:$input){{_docID}}}}",
                        gents_store::graphql::escape_graphql_string(agent_doc)
                    ),
                    &serde_json::json!({"input": {"enabled": true}}),
                    gents_protocol::schemas::AGENT,
                )
                .await
            })
        })
        .await?;
    gents_store::test_support::relationships::actor_in_txn(
        &owner_access,
        "Agent",
        &agent_doc,
        "reader",
        &identity::Did::new(selected.did())?,
        false,
    )
    .await?;
    gents_store::test_support::relationships::actor_in_txn(
        &owner_access,
        "Agent",
        &agent_doc,
        "executor",
        &identity::Did::new(selected.did())?,
        true,
    )
    .await?;
    assert!(
        !agent_access.execute("{Agent {_docID}} ").await?["data"]["Agent"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        observe(&agent_access, root.did(), selected.did()).await,
        expected("reader_only")
    );
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn missing_native_node_authority_is_not_treated_as_invoke() -> anyhow::Result<()> {
    let case = gents_lean_vocab::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap()
        .unattended_agent_cases
        .iter()
        .find(|case| case["name"] == "unavailable")
        .unwrap();
    let db = gents_inference::test_support::test_db(
        "unattended-unavailable-without-native-in",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = db.node.clone();
    let root = gents_store::test_support::held_runtime_identity(db.node_identity.did());
    let binding = gents_store::identity::provision_agent_identity(&root)?;
    let agent = gents_store::identity::resolve_agent_identity(&root, &binding)?;
    let owner = ConfigAccess::local_as(node.clone(), identity::Did::new(root.did())?);
    let response = owner.write("test.unattended.no_nac", &format!(
        "mutation {{create_Agent(input:{{node_did:\"{}\",agent_did:\"{}\",creation_nonce:\"{}\",inference_profile_id:\"profile\",enabled:true}}){{_docID}}}}",
        gents_store::graphql::escape_graphql_string(root.did()), gents_store::graphql::escape_graphql_string(agent.did()),
        gents_store::graphql::escape_graphql_string(&"0".repeat(64)))).await?;
    let doc = gents_store::graphql::created_doc_id(&response, "Agent")?;
    owner
        .add_actor_relationship("Agent", &doc, "executor", &identity::Did::new(agent.did())?)
        .await?;
    let access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did())?,
        agent.as_ref(),
    )?;
    assert_eq!(
        observe(&access, root.did(), agent.did()).await,
        case["allowed"].as_bool().unwrap()
    );
    node.shutdown().await?;
    Ok(())
}
