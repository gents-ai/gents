mod support;

use gents::runtime::agent::p2p_reconcile::{
    EmbeddedRemoteP2pAdmin, PairingFilters, RemoteP2pAdmin,
};
use gents::session::mailbox::{
    FileMailboxItemArgs, MailboxAction, MailboxKind, MailboxSourceKind, MailboxStampContext,
};
use gents::store::config_client::{ConfigAccess, NodePermission};
use gents::{KeyIdentity, NodeIdentity};
use std::{sync::Arc, time::Duration};

async fn node(identity: &KeyIdentity, directory: &std::path::Path) -> Arc<gents::defra_node::Node> {
    let node = Arc::new(
        gents::defra_node::Node::builder()
            .enable_signing()
            .data_path(directory)
            .with_node_identity_did(identity.did())
            .with_p2p(support::test_p2p_config(&Default::default(), directory))
            .build()
            .await
            .unwrap(),
    );
    gents::ensure_runtime_schemas(&node).await.unwrap();
    node
}

#[tokio::test]
async fn mailbox_recipient_access_and_unrelated_exact_peer_pull() {
    let keys = tempfile::tempdir().unwrap();
    let source_key = KeyIdentity::load_or_create(keys.path().join("source.key"), None).unwrap();
    let recipient = KeyIdentity::load_or_create(keys.path().join("recipient.key"), None).unwrap();
    let outsider = KeyIdentity::load_or_create(keys.path().join("outsider.key"), None).unwrap();
    let source_dir = tempfile::tempdir().unwrap();
    let receiver_dir = tempfile::tempdir().unwrap();
    let source = node(&source_key, source_dir.path()).await;
    let receiver = node(&outsider, receiver_dir.path()).await;
    let binding = gents::store::identity::provision_agent_identity(&source_key).unwrap();
    let agent = gents::store::identity::resolve_agent_identity(&source_key, &binding).unwrap();
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
    let context = MailboxStampContext {
        requester_did: recipient.did().into(),
        node_did: source_key.did().into(),
        agent_did: agent.did().into(),
        session_id: None,
    };
    let args = FileMailboxItemArgs {
        kind: MailboxKind::Flag,
        action: MailboxAction::Ack,
        title: "Private recipient notice".into(),
        summary: None,
        payload: None,
        source_kind: MailboxSourceKind::Agent,
        source_id: "p2p-mailbox-notice".into(),
        session_id: None,
        request_id: None,
        cause_doc_id: None,
        expected_collection: None,
        parent_item_id: None,
        deadline_at: None,
    };
    let item =
        gents::session::mailbox::stamp_create_with_access(&source, &access, &context, args, &[])
            .await
            .unwrap();
    let doc = gents::store::graphql::escape_graphql_string(&item.doc_id);
    let query = format!(
        "{{ MailboxItem(filter: {{_docID: {{_eq: \"{doc}\"}}}}) {{ _docID _version {{ cid }} }} }}"
    );
    let owned = access.execute(&query).await.unwrap();
    let cid = owned["data"]["MailboxItem"][0]["_version"][0]["cid"]
        .as_str()
        .unwrap();
    assert_eq!(
        source.verified_block_signer_did(cid).await.unwrap(),
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
    let public_id = gents::store::graphql::created_doc_id(&public, "PeerRegistry").unwrap();
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
    receiver
        .p2p()
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
            gents::store::graphql::escape_graphql_string(&public_id)
        ))
        .await
        .unwrap();
    assert_eq!(
        public_probe["data"]["PeerRegistry"][0]["peer_id"],
        "mailbox-public-probe"
    );
    let pull = receiver
        .p2p()
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
    source.shutdown().await.unwrap();
    receiver.shutdown().await.unwrap();
}
