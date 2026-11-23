#[tokio::test]
async fn native_file_inventory_uses_document_ownership() -> anyhow::Result<()> {
    use crate::identity::{KeyIdentity, NodeIdentity};
    use crate::local::document_acp::{configure_fresh, DocumentAcp, VeraConnection};
    use crate::local::store_key::StoreKey;
    use crate::schema::catalog::{baseline, ensure, Catalog};
    use gents_protocol::schemas::{
        BuiltInPolicy, HomePolicyIds, TOOL_FILE_CHANGE_POLICY_RESOURCE, TOOL_FILE_INVENTORY_NAME,
    };
    use std::sync::Arc;

    let dir = tempfile::tempdir()?;
    let actor: Arc<dyn NodeIdentity> = Arc::new(KeyIdentity::load_or_create(
        dir.path().join("device.key"),
        None,
    )?);
    let node = configure_fresh(
        defra_node::Node::builder().with_node_identity_did(actor.did().to_owned()),
        dir.path(),
        &StoreKey::from_host(&[11; 32])?,
        DocumentAcp::Vera,
        &actor,
        &VeraConnection::Solo,
    )?
    .build()
    .await?;
    let ids = HomePolicyIds::embedded_vera();
    let entry = baseline(ids)
        .iter()
        .find(|entry| entry.name == TOOL_FILE_INVENTORY_NAME)
        .unwrap();
    ensure(
        &node,
        &Catalog {
            baseline: std::slice::from_ref(entry),
            ordinary_indexes: &[],
        },
    )
    .await?;
    let collection = node.get_collection(TOOL_FILE_INVENTORY_NAME)?.unwrap();
    let policy = ids.id(BuiltInPolicy::ToolFileChange);
    let resource = TOOL_FILE_CHANGE_POLICY_RESOURCE;
    let acp = node.document_acp();
    assert!(!collection.is_branchable);
    assert!(
        !acp.is_doc_registered(policy, resource, &collection.collection_id)
            .await?
    );
    let document = "inventory-observation";
    let device: identity::Did = actor.did().parse()?;
    acp.register_object(&device, policy, resource, document)
        .await?;
    let other = KeyIdentity::load_or_create(dir.path().join("other.key"), None)?;
    for (caller, allowed) in [(device, true), (other.did().parse()?, false)] {
        let identity = acp::Identity::Authenticated(caller);
        let checker = acp::DirectChecker {
            acp: acp.as_ref(),
            identity: &identity,
        };
        assert_eq!(
            acp::check_doc_read_access(
                &checker,
                policy,
                resource,
                &collection.collection_id,
                collection.is_branchable,
                document
            )
            .await?,
            allowed
        );
        assert!(
            !acp::check_doc_read_access(
                &checker,
                policy,
                resource,
                &collection.collection_id,
                collection.is_branchable,
                "missing"
            )
            .await?
        );
    }
    node.shutdown().await?;
    Ok(())
}
