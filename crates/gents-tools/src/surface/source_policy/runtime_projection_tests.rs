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
    let open = || async {
        let projector = RuntimeDocumentProjector::new(
            Arc::new(gents_store::identity::KeyIdentity::load_or_create(
                &path, None,
            )?),
            Arc::new(crate::surface::ToolCeiling::meta_only()),
            gents_protocol::schemas::HomePolicyIds::node_store(),
        )?;
        Ok::<_, anyhow::Error>(Arc::new(
            defra_node::Node::builder()
                .data_path(directory.path().join("store"))
                .with_node_identity_did(runtime.did())
                .with_signing_key(defra_node::SigningKey::Ed25519(key.clone()))
                .with_node_acp_enabled()
                .with_document_projector(Arc::new(projector))
                .build()
                .await?,
        ))
    };
    let node = open().await?;
    let actor = identity::Did::new(runtime.did())?;
    let context =
        defra_core::RequestContext::default().with_identity(Some(runtime.did().to_owned()));
    node.add_dac_policy_with_counter_as(
        &context,
        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
        0,
    )
    .await?;
    node.add_collection(
        &node.node_context(),
        &gents_protocol::schemas::HomePolicyIds::node_store()
            .render(gents_protocol::schemas::NETWORK),
    )
    .await?;
    node.add_collection(
        &node.node_context(),
        &gents_protocol::schemas::HomePolicyIds::node_store().render(gents_protocol::schemas::NODE),
    )
    .await?;
    let access = gents_store::config_client::ConfigAccess::local_as(node.clone(), actor.clone());
    let did = gents_store::graphql::escape_graphql_string(runtime.did());
    let network = access
        .write(
            "test.projector.network",
            &format!(r#"mutation {{ create_Network(input: {{network_id: "network", admin_did: "{did}"}}) {{_docID}} }}"#),
        )
        .await?;
    let network = gents_store::graphql::created_doc_id(&network, "Network")?;
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
                    gents_protocol::schemas::HomePolicyIds::node_store()
                        .id(gents_protocol::schemas::BuiltInPolicy::Household),
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
    let is_pending = || async {
        use gents_store::config_client::transport_publication::{
            publication_state, PublicationState,
        };
        let txn = node.database().new_txn(true).await?;
        let state =
            publication_state(&txn.datastore()?, "Network", &network.parse()?, &actor).await?;
        txn.discard()?;
        Ok::<_, anyhow::Error>(state == Some(PublicationState::PendingParent))
    };
    assert!(is_pending().await?);
    let parent = access
        .write(
            "test.projector.node",
            &format!(r#"mutation {{ create_Node(input: {{node_did: "{did}"}}) {{_docID}} }}"#),
        )
        .await?;
    let parent = gents_store::graphql::created_doc_id(&parent, "Node")?;
    let parent_subject = Subject::entity_set("nodes", parent.clone(), "");
    assert!(relation("parent", parent_subject.clone(), network.clone()).await?);
    assert!(!is_pending().await?);
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
    assert!(!is_pending().await?);
    node.shutdown().await?;
    drop(access);
    drop(node);
    let reopened = open().await?;
    reopened.database().reconcile_document_projector().await?;
    let txn = reopened.database().new_txn(true).await?;
    let view = txn.datastore()?;
    use gents_store::config_client::transport_publication::{publication_state, PublicationState};
    assert_eq!(
        publication_state(&view, "Network", &network.parse()?, &actor).await?,
        Some(PublicationState::Linked)
    );
    let tuples =
        acp::zanzibar::store::TransactionZanzibarStore::new(view.sibling(Namespace::Acpstore));
    assert!(
        !tuples
            .has_relationship(
                gents_protocol::schemas::HomePolicyIds::node_store()
                    .id(gents_protocol::schemas::BuiltInPolicy::Household),
                "enrollment_records",
                &network,
                "parent",
                &Subject::entity_set("nodes", parent, "")
            )
            .await?
    );
    drop(tuples);
    drop(view);
    txn.discard()?;
    reopened.shutdown().await?;
    Ok(())
}
