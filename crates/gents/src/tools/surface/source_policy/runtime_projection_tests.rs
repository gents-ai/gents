use std::sync::Arc;

use acp::{Subject, ZanzibarStore};
use storage::namespace::Namespace;

use crate::store::identity::NodeIdentity;
use crate::store::local::runtime_document_projection::RuntimeDocumentProjector;

#[tokio::test]
async fn installed_projector_links_pending_transport_and_preserves_revocation() -> anyhow::Result<()>
{
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("runtime");
    let runtime = crate::KeyIdentity::load_or_create(&path, None)?;
    let key = crate::store::identity::load_file_identity(&path)?.private_key_bytes();
    let projector = RuntimeDocumentProjector::new(
        Arc::new(crate::KeyIdentity::load_or_create(&path, None)?),
        Arc::new(crate::tools::surface::ToolCeiling::meta_only()),
    )?;
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_signing_key(defra_node::SigningKey::Ed25519(key))
            .with_node_acp_enabled()
            .with_document_projector(Arc::new(projector))
            .build()
            .await?,
    );
    let actor = identity::Did::new(runtime.did())?;
    let context =
        defra_core::RequestContext::default().with_identity(Some(runtime.did().to_owned()));
    node.add_dac_policy_with_counter_as(
        &context,
        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
        0,
    )
    .await?;
    node.add_collection(gents_protocol::schemas::NETWORK)
        .await?;
    node.add_collection(gents_protocol::schemas::NODE).await?;
    let access = crate::ConfigAccess::local_as(node.clone(), actor.clone());
    let did = crate::store::graphql::escape_graphql_string(runtime.did());
    let network = access
        .write(
            "test.projector.network",
            &format!(r#"mutation {{ create_Network(input: {{network_id: "network", admin_did: "{did}"}}) {{_docID}} }}"#),
        )
        .await?;
    let network = crate::store::graphql::created_doc_id(&network, "Network")?;
    let cases = &gents_lean_vocab::lean_contract_snapshot().identity_replica_grant_cases;
    let modeled_pending = |name: &str| {
        cases
            .iter()
            .find(|case| case["name"] == name)
            .and_then(|case| case["pending"].as_bool())
            .expect("modeled transport publication case")
    };
    let relation = |name: &'static str, subject: Subject, network: String| {
        let node = node.clone();
        async move {
            let txn = node.database().new_txn(true).await?;
            let view = txn.datastore()?;
            let tuples = acp::zanzibar::store::TransactionZanzibarStore::new(
                view.sibling(Namespace::Acpstore),
            );
            let present = tuples
                .has_relationship(
                    gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
                    "enrollment_records",
                    &network,
                    name,
                    &subject,
                )
                .await?;
            drop(tuples);
            drop(view);
            txn.discard()?;
            Ok::<_, anyhow::Error>(present)
        }
    };
    assert_eq!(
        relation(
            "pending_parent",
            Subject::entity(actor.clone()),
            network.clone()
        )
        .await?,
        modeled_pending("pending")
    );
    let parent = access
        .write(
            "test.projector.node",
            &format!(r#"mutation {{ create_Node(input: {{node_did: "{did}"}}) {{_docID}} }}"#),
        )
        .await?;
    let parent = crate::store::graphql::created_doc_id(&parent, "Node")?;
    let parent_subject = Subject::entity_set("nodes", parent.clone(), "");
    assert!(relation("parent", parent_subject.clone(), network.clone()).await?);
    assert_eq!(
        relation(
            "pending_parent",
            Subject::entity(actor.clone()),
            network.clone()
        )
        .await?,
        modeled_pending("linked")
    );
    access
        .delete_resource_relationship("Network", &network, "parent", "nodes", &parent)
        .await?;
    access
        .write(
            "test.projector.network_update",
            &format!(r#"mutation {{ update_Network(docID: "{}", input: {{display_name: "updated"}}) {{_docID}} }}"#, crate::store::graphql::escape_graphql_string(&network)),
        )
        .await?;
    assert!(!relation("parent", parent_subject, network).await?);
    node.shutdown().await?;
    Ok(())
}
