use std::sync::Arc;

use acp::{Subject, ZanzibarStore};
use storage::namespace::Namespace;

use gents_store::identity::NodeIdentity;
use gents_store::local::runtime_document_projection::RuntimeDocumentProjector;

#[tokio::test]
async fn installed_projector_links_pending_transport_and_preserves_revocation() -> anyhow::Result<()>
{
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("runtime");
    let runtime = gents_store::identity::KeyIdentity::load_or_create(&path, None)?;
    let key = gents_store::identity::load_file_identity(&path)?.private_key_bytes();
    let projector = RuntimeDocumentProjector::new(
        Arc::new(gents_store::identity::KeyIdentity::load_or_create(
            &path, None,
        )?),
        Arc::new(crate::surface::ToolCeiling::meta_only()),
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
    let access = gents_store::config_client::ConfigAccess::local_as(node.clone(), actor.clone());
    let did = gents_store::graphql::escape_graphql_string(runtime.did());
    let network = access
        .write(
            "test.projector.network",
            &format!(r#"mutation {{ create_Network(input: {{network_id: "network", admin_did: "{did}"}}) {{_docID}} }}"#),
        )
        .await?;
    let network = gents_store::graphql::created_doc_id(&network, "Network")?;
    let mut pending = std::collections::BTreeMap::new();
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
    pending.insert(
        "pending",
        relation(
            "pending_parent",
            Subject::entity(actor.clone()),
            network.clone(),
        )
        .await?,
    );
    let parent = access
        .write(
            "test.projector.node",
            &format!(r#"mutation {{ create_Node(input: {{node_did: "{did}"}}) {{_docID}} }}"#),
        )
        .await?;
    let parent = gents_store::graphql::created_doc_id(&parent, "Node")?;
    let parent_subject = Subject::entity_set("nodes", parent.clone(), "");
    assert!(relation("parent", parent_subject.clone(), network.clone()).await?);
    pending.insert(
        "linked",
        relation(
            "pending_parent",
            Subject::entity(actor.clone()),
            network.clone(),
        )
        .await?,
    );
    access
        .delete_resource_relationship("Network", &network, "parent", "nodes", &parent)
        .await?;
    access
        .write(
            "test.projector.network_update",
            &format!(r#"mutation {{ update_Network(docID: "{}", input: {{display_name: "updated"}}) {{_docID}} }}"#, gents_store::graphql::escape_graphql_string(&network)),
        )
        .await?;
    assert!(!relation("parent", parent_subject, network.clone()).await?);
    pending.insert(
        "revoked",
        relation("pending_parent", Subject::entity(actor.clone()), network).await?,
    );
    node.shutdown().await?;
    for case in gents_lean_vocab::families::identity_operations_replica_grant_pending::cases() {
        let pending = pending
            .remove(case.input.name.as_str())
            .unwrap_or_else(|| panic!("the projector scenario never reached {}", case.input.name));
        case.check(
            gents_lean_vocab::families::identity_operations_replica_grant_pending::ReplicaPending {
                pending,
            },
        );
    }
    assert!(pending.is_empty(), "unmodeled projector observations");
    Ok(())
}
