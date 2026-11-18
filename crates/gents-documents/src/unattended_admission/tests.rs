use super::*;
use gents_store::config_client::{ConfigAccess, NodePermission};
use gents_store::identity::NodeIdentity;
use std::sync::Arc;

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
    let home = tempfile::tempdir()?;
    let root =
        gents_store::identity::KeyIdentity::load_or_create(home.path().join("node.key"), None)?;
    let owner_binding = gents_store::identity::provision_agent_identity(&root)?;
    let agent_binding = gents_store::identity::provision_agent_identity(&root)?;
    let owner = gents_store::identity::resolve_agent_identity(&root, &owner_binding)?;
    let selected = gents_store::identity::resolve_agent_identity(&root, &agent_binding)?;
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(root.did())
            .with_node_acp_enabled()
            .build()
            .await?,
    );
    gents_store::schema::ensure_runtime_schemas(&node).await?;
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
                        gents_store::graphql::escape_graphql_string(root_did)
                    ))
                    .await?;
                let doc = gents_store::graphql::created_doc_id(&response, "Node")?;
                Ok::<_, anyhow::Error>(doc)
            })
        })
        .await?;
    let agent_doc = owner_access.transact_write("test.unattended.agent", |txn| {
        let node_doc = &node_doc;
        Box::pin(async move {
            let response = txn.execute(&format!("mutation {{create_Agent(input:{{node_did:\"{}\",agent_did:\"{}\",creation_nonce:\"{}\",inference_profile_id:\"profile\",enabled:true}}){{_docID}}}}",
                gents_store::graphql::escape_graphql_string(root_did), gents_store::graphql::escape_graphql_string(selected_did),
                gents_store::graphql::escape_graphql_string(nonce))).await?;
            let doc = gents_store::graphql::created_doc_id(&response, "Agent")?;
            txn.add_resource_relationship("Agent", &doc, "parent", "nodes", node_doc).await?;
            txn.add_actor_relationship("Agent", &doc, "executor", &identity::Did::new(selected_did)?).await?;
            Ok::<_, anyhow::Error>(doc)
        })
    }).await?;
    let selected_identity = identity::Did::new(selected.did())?;
    let mut enabled = true;
    let mut executor = true;
    let mut reader = false;
    for case in gents_lean_vocab::families::household_unattended_documents_admission::cases() {
        if case.input.enabled != enabled {
            owner_access
                .write(
                    "test.unattended.enabled",
                    &format!(
                        "mutation {{update_Agent(docID:\"{}\",input:{{enabled:{}}}){{_docID}}}}",
                        gents_store::graphql::escape_graphql_string(&agent_doc),
                        case.input.enabled
                    ),
                )
                .await?;
            enabled = case.input.enabled;
        }
        let invoke = case.input.invoke == "allowed";
        if !invoke && !reader {
            gents_store::test_support::relationships::actor_in_txn(
                &owner_access,
                "Agent",
                &agent_doc,
                "reader",
                &selected_identity,
                false,
            )
            .await?;
            reader = true;
        }
        if invoke != executor {
            gents_store::test_support::relationships::actor_in_txn(
                &owner_access,
                "Agent",
                &agent_doc,
                "executor",
                &selected_identity,
                !invoke,
            )
            .await?;
            executor = invoke;
        }
        if !invoke {
            assert!(
                !agent_access.execute("{Agent {_docID}} ").await?["data"]["Agent"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
        let observer = if case.input.actor_matches {
            &agent_access
        } else {
            &owner_access
        };
        case.check(observe(observer, root.did(), selected.did()).await);
    }
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn missing_native_node_authority_is_not_treated_as_invoke() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let root =
        gents_store::identity::KeyIdentity::load_or_create(home.path().join("node.key"), None)?;
    let binding = gents_store::identity::provision_agent_identity(&root)?;
    let agent = gents_store::identity::resolve_agent_identity(&root, &binding)?;
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(root.did())
            .build()
            .await?,
    );
    gents_store::schema::ensure_runtime_schemas(&node).await?;
    let owner = ConfigAccess::local_as(node.clone(), identity::Did::new(root.did())?);
    let response = owner.write("test.unattended.no_nac", &format!(
        "mutation {{create_Agent(input:{{node_did:\"{}\",agent_did:\"{}\",creation_nonce:\"{}\",inference_profile_id:\"profile\",enabled:true}}){{_docID}}}}",
        gents_store::graphql::escape_graphql_string(root.did()), gents_store::graphql::escape_graphql_string(agent.did()),
        gents_store::graphql::escape_graphql_string(&binding.creation_nonce))).await?;
    let doc = gents_store::graphql::created_doc_id(&response, "Agent")?;
    owner
        .add_actor_relationship("Agent", &doc, "executor", &identity::Did::new(agent.did())?)
        .await?;
    let access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did())?,
        agent.as_ref(),
    )?;
    for case in gents_lean_vocab::families::household_unattended_documents_unavailable::cases() {
        case.check(observe(&access, root.did(), agent.did()).await);
    }
    node.shutdown().await?;
    Ok(())
}
