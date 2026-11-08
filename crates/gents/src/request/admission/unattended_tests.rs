use super::*;
use crate::documents::observe_unattended_agent_in_txn;
use crate::store::config_client::{ConfigAccess, NodePermission};
use crate::store::identity::NodeIdentity;

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
    let cases = &crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap()
        .unattended_agent_cases;
    let expected = |name: &str| {
        cases.iter().find(|case| case["name"] == name).unwrap()["allowed"]
            .as_bool()
            .unwrap()
    };
    let home = tempfile::tempdir()?;
    let root = crate::KeyIdentity::load_or_create(home.path().join("node.key"), None)?;
    let owner_binding = crate::store::identity::provision_agent_identity(&root)?;
    let agent_binding = crate::store::identity::provision_agent_identity(&root)?;
    let owner = crate::store::identity::resolve_agent_identity(&root, &owner_binding)?;
    let selected = crate::store::identity::resolve_agent_identity(&root, &agent_binding)?;
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(root.did())
            .with_node_acp_enabled()
            .build()
            .await?,
    );
    crate::ensure_runtime_schemas(&node).await?;
    let runtime = ConfigAccess::local_as(node.clone(), identity::Did::new(root.did())?);
    for did in [owner.did(), selected.did()] {
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
            NodePermission::DacRelationDelete,
        ] {
            runtime
                .grant_node_permission(&identity::Did::new(did)?, permission)
                .await?;
        }
    }
    let owner_access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(owner.did())?,
        owner.as_ref(),
    )?;
    let agent_access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(selected.did())?,
        selected.as_ref(),
    )?;
    let root_did = root.did();
    let selected_did = selected.did();
    let nonce = &agent_binding.creation_nonce;
    let node_doc = owner_access
        .transact_write("test.unattended.node", |txn| {
            Box::pin(async move {
                let response = txn
                    .execute(&format!(
                        "mutation {{create_Node(input:{{node_did:\"{}\"}}){{_docID}}}}",
                        crate::store::graphql::escape_graphql_string(root_did)
                    ))
                    .await?;
                let doc = crate::store::graphql::created_doc_id(&response, "Node")?;
                Ok::<_, anyhow::Error>(doc)
            })
        })
        .await?;
    let agent_doc = owner_access.transact_write("test.unattended.agent", |txn| {
        let node_doc = &node_doc;
        Box::pin(async move {
            let response = txn.execute(&format!("mutation {{create_Agent(input:{{node_did:\"{}\",agent_did:\"{}\",creation_nonce:\"{}\",inference_profile_id:\"profile\",enabled:true}}){{_docID}}}}",
                crate::store::graphql::escape_graphql_string(root_did), crate::store::graphql::escape_graphql_string(selected_did),
                crate::store::graphql::escape_graphql_string(nonce))).await?;
            let doc = crate::store::graphql::created_doc_id(&response, "Agent")?;
            txn.add_resource_relationship("Agent", &doc, "parent", "nodes", node_doc).await?;
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
        .write(
            "test.unattended.disable",
            &format!(
                "mutation {{update_Agent(docID:\"{}\",input:{{enabled:false}}){{_docID}}}}",
                crate::store::graphql::escape_graphql_string(&agent_doc)
            ),
        )
        .await?;
    assert_eq!(
        observe(&agent_access, root.did(), selected.did()).await,
        expected("disabled")
    );
    owner_access
        .write(
            "test.unattended.enable",
            &format!(
                "mutation {{update_Agent(docID:\"{}\",input:{{enabled:true}}){{_docID}}}}",
                crate::store::graphql::escape_graphql_string(&agent_doc)
            ),
        )
        .await?;
    owner_access
        .add_actor_relationship(
            "Agent",
            &agent_doc,
            "reader",
            &identity::Did::new(selected.did())?,
        )
        .await?;
    owner_access
        .delete_actor_relationship(
            "Agent",
            &agent_doc,
            "executor",
            &identity::Did::new(selected.did())?,
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
    let case = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap()
        .unattended_agent_cases
        .iter()
        .find(|case| case["name"] == "unavailable")
        .unwrap();
    let home = tempfile::tempdir()?;
    let root = crate::KeyIdentity::load_or_create(home.path().join("node.key"), None)?;
    let binding = crate::store::identity::provision_agent_identity(&root)?;
    let agent = crate::store::identity::resolve_agent_identity(&root, &binding)?;
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(root.did())
            .build()
            .await?,
    );
    crate::ensure_runtime_schemas(&node).await?;
    let owner = ConfigAccess::local_as(node.clone(), identity::Did::new(root.did())?);
    let response = owner.write("test.unattended.no_nac", &format!(
        "mutation {{create_Agent(input:{{node_did:\"{}\",agent_did:\"{}\",creation_nonce:\"{}\",inference_profile_id:\"profile\",enabled:true}}){{_docID}}}}",
        crate::store::graphql::escape_graphql_string(root.did()), crate::store::graphql::escape_graphql_string(agent.did()),
        crate::store::graphql::escape_graphql_string(&binding.creation_nonce))).await?;
    let doc = crate::store::graphql::created_doc_id(&response, "Agent")?;
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
