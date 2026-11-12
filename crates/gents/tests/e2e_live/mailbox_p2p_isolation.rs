use crate::support;

use gents_p2p::p2p_reconcile::{EmbeddedRemoteP2pAdmin, PairingFilters, RemoteP2pAdmin};
use gents_store::config_client::{ConfigAccess, NodePermission};
use gents_store::identity::{KeyIdentity, NodeIdentity};
use std::{sync::Arc, time::Duration};

async fn node(
    identity: Arc<KeyIdentity>,
    directory: &std::path::Path,
) -> Arc<gents::defra_node::Node> {
    let node = Arc::new(
        gents::defra_node::Node::builder()
            .enable_signing()
            .data_path(directory)
            .with_node_identity_did(identity.did())
            .with_node_acp_enabled()
            .with_document_projector(
                gents_store::local::document_projector(
                    identity.clone(),
                    std::sync::Arc::new(gents_tools::surface::ToolCeiling::meta_only()),
                )
                .unwrap(),
            )
            .with_p2p(support::test_p2p_config(&Default::default(), directory))
            .build()
            .await
            .unwrap(),
    );
    gents_store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    node
}

#[tokio::test]
async fn mailbox_recipient_access_and_unrelated_exact_peer_pull() {
    let keys = tempfile::tempdir().unwrap();
    let source_db = support::test_p2p_db_without_node("mailbox-live-p2p", Default::default()).await;
    let actors = crate::mailbox_live::configure(&source_db, false).await;
    let source = source_db.node.clone();
    let source_key = source_db.node_identity.clone();
    let recipient = &actors.fixture.user;
    let agent = actors.fixture.agent.clone();
    let outsider =
        Arc::new(KeyIdentity::load_or_create(keys.path().join("outsider.key"), None).unwrap());
    let receiver_dir = tempfile::tempdir().unwrap();
    let receiver = node(outsider.clone(), receiver_dir.path()).await;
    let runtime = support::live_inference::boot_live_agent(&source_db, source_key.clone())
        .await
        .unwrap();
    let admin = ConfigAccess::local_as(source.clone(), source_key.did().parse().unwrap());
    for actor in [agent.did(), recipient.did(), outsider.did()] {
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
            NodePermission::CollectionGet,
        ] {
            admin
                .grant_node_permission(&actor.parse().unwrap(), permission)
                .await
                .unwrap();
        }
    }
    let access = ConfigAccess::local_managed_agent(
        source.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    crate::mailbox_live::file(
        &actors,
        serde_json::json!({"title":"Private recipient notice"}),
    )
    .await;
    let item = gents_session::operations::mailbox::list(
        &actors.fixture.access,
        &gents_protocol::operations::mailbox::MailboxListInput {
            requester_did: Some(recipient.did().to_owned()),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .pop()
    .expect("live model filed the private item");
    let doc = gents_store::graphql::escape_graphql_string(&item.doc_id);
    let query = format!(
        "{{ MailboxItem(filter: {{_docID: {{_eq: \"{doc}\"}}}}) {{ _docID _version {{ cid }} }} }}"
    );
    let owned = access.execute(&query).await.unwrap();
    let cid = owned["data"]["MailboxItem"][0]["_version"][0]["cid"]
        .as_str()
        .unwrap();
    assert_eq!(
        support::snapshots::verified_block_signer_as(&source, agent.did(), cid).await,
        agent.did()
    );
    let visible = ConfigAccess::local_as(source.clone(), recipient.did().parse().unwrap())
        .execute(&query)
        .await
        .unwrap();
    assert_eq!(visible["data"]["MailboxItem"].as_array().unwrap().len(), 1);
    let hidden = ConfigAccess::local_as(source.clone(), outsider.did().parse().unwrap())
        .execute(&query)
        .await
        .unwrap();
    assert!(hidden["data"]["MailboxItem"].as_array().unwrap().is_empty());

    let public = admin.write("test.mailbox.public_probe", "mutation { create_PeerRegistry(input: {peer_id: \"mailbox-public-probe\", node_did: \"did:key:probe\", addresses: [\"probe\"]}) { _docID } }").await.unwrap();
    let public_id = gents_store::graphql::created_doc_id(&public, "PeerRegistry").unwrap();
    let source_admin = EmbeddedRemoteP2pAdmin::new(source.clone());
    let receiver_admin = EmbeddedRemoteP2pAdmin::new(receiver.clone());
    let (_, source_address) = support::enrollment::wait_for_peer_identity(&source).await;
    let (_, receiver_address) = support::enrollment::wait_for_peer_identity(&receiver).await;
    receiver_admin.connect(&[source_address]).await.unwrap();
    support::p2p_waits::wait_for_connected_peer(&receiver).await;
    source_admin
        .add_replicator(
            &[receiver_address],
            &["PeerRegistry".into()],
            &PairingFilters::default(),
        )
        .await
        .unwrap();
    gents_store::identity::node_p2p(&receiver)
        .unwrap()
        .sync_documents(
            "PeerRegistry",
            vec![public_id.clone()],
            Some(Duration::from_secs(5)),
        )
        .await
        .unwrap();
    let public_probe = ConfigAccess::local_as(receiver.clone(), outsider.did().parse().unwrap())
        .execute(&format!(
            "{{ PeerRegistry(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ peer_id }} }}",
            gents_store::graphql::escape_graphql_string(&public_id)
        ))
        .await
        .unwrap();
    assert_eq!(
        public_probe["data"]["PeerRegistry"][0]["peer_id"],
        "mailbox-public-probe"
    );
    let pull = gents_store::identity::node_p2p(&receiver)
        .unwrap()
        .sync_documents(
            "MailboxItem",
            vec![item.doc_id.clone()],
            Some(Duration::from_secs(5)),
        )
        .await;
    let copied = ConfigAccess::local_as(receiver.clone(), outsider.did().parse().unwrap())
        .execute(&query)
        .await
        .unwrap();
    assert!(
        copied["data"]["MailboxItem"].as_array().unwrap().is_empty(),
        "unrelated peer saw mailbox row: {pull:?}"
    );
    assert!(
        receiver
            .authorized_signed_block_bytes(cid, Some(agent.did()))
            .await
            .is_err(),
        "unrelated peer obtained mailbox block: {pull:?}"
    );
    runtime.shutdown().await;
    source.shutdown().await.unwrap();
    receiver.shutdown().await.unwrap();
}
