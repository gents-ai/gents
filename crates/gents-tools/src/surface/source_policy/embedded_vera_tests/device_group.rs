use acp::{DocumentPermission, Identity};
use gents_protocol::schemas::{BuiltInPolicy, ACCOUNT_DEVICE_GROUP_NAME};
use gents_store::config_client::ConfigClientError;
use gents_store::local::document_acp::{node_policy_ids, DocumentAcp};

use crate::test_support::vera_test_home::Household;

#[tokio::test]
async fn the_account_signs_device_admission_and_revocation() -> anyhow::Result<()> {
    let home = Household::open(DocumentAcp::Vera).await?;
    let account = home.as_user()?;
    let device = &home.node_did;
    assert!(matches!(
        home.as_node()
            .create_device_group(&home.user_did, device)
            .await,
        Err(ConfigClientError::TransactionActorMismatch)
    ));

    let group = account.create_device_group(&home.user_did, device).await?;
    home.settled(&home.user_did, "device group creation")
        .await?;
    drop(account);
    let home = home.reopen_for_replication().await?;
    let account = home.as_user()?;
    let device = &home.node_did;
    let network = gents_p2p::p2p_reconcile::ensure_enrollment_network(
        &home.node,
        home.home.node.as_ref(),
        "shared home",
    )
    .await?
    .context("admitted home did not publish its enrollment network")?;
    assert_eq!(network.admin_did, home.node_did.as_str());
    let policy = node_policy_ids(&home.node).id(BuiltInPolicy::Household);
    let provider = home.node.document_acp();
    let owner = provider
        .get_doc_owner(policy, "device_groups", &group)
        .await?;
    assert_eq!(owner.as_ref(), Some(&home.user_did));
    let receiver = home.as_node();
    assert_eq!(
        receiver
            .assigned_device_group(&home.user_did)
            .await?
            .as_ref(),
        Some(&group)
    );
    assert_eq!(receiver.assigned_device_group(&home.node_did).await?, None);
    use anyhow::Context;
    use gents_store::identity::NodeIdentity;
    let registration = home
        .node
        .database()
        .replica_registration()
        .context("admitted device has no replica policy")?;
    let network = home
        .node
        .get_collection("Network")?
        .context("Network missing")?;
    let rules = registration.ownership();
    let outsider = identity::Did::new(home.held_key("foreign").await?.did())?;
    assert!(registration.lists(&network));
    assert!(
        !rules
            .allows_foreign_registration(&network, &home.node_did, provider.as_ref())
            .await?
    );
    assert!(
        rules
            .allows_foreign_registration(&network, &outsider, provider.as_ref())
            .await?
    );
    assert_eq!(
        rules.creator_grants("Network", &outsider, "row"),
        vec![
            acp::ReplicaGrant::new(
                "reader_manager",
                acp::Subject::entity_set("device_groups", &group, "member")
            ),
            acp::ReplicaGrant::new("writer", acp::Subject::entity(outsider.clone())),
        ]
    );
    let device = Identity::Authenticated(device.clone());
    let can_read = || {
        provider.check_doc_access(
            &device,
            DocumentPermission::Read,
            policy,
            "device_groups",
            &group,
        )
    };
    assert!(can_read().await?);
    account
        .delete_actor_relationship(ACCOUNT_DEVICE_GROUP_NAME, &group, "member", &home.node_did)
        .await?;
    home.settled(&home.user_did, "device revocation").await?;
    assert!(!can_read().await?);
    assert!(gents_p2p::p2p_reconcile::ensure_enrollment_network(
        &home.node,
        home.home.node.as_ref(),
        "revoked"
    )
    .await
    .is_err());
    assert!(gents_store::local::document_acp::ensure_enrollment(
        &home.node,
        gents_store::local::document_acp::EnrollmentRequest::Offer
    )
    .await
    .is_err());
    assert!(rules
        .allows_foreign_registration(&network, &outsider, provider.as_ref())
        .await
        .is_err());
    assert_eq!(receiver.assigned_device_group(&home.user_did).await?, None);
    home.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn a_vera_home_registers_a_foreign_peer_registry_and_preserves_its_writer(
) -> anyhow::Result<()> {
    use crate::test_support::vera_test_home::SETTLE_CEILING;
    use anyhow::Context;

    if let Ok(filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_test_writer()
            .try_init();
    }
    async fn peer() -> anyhow::Result<Household> {
        let home = Household::open(DocumentAcp::Vera).await?;
        home.as_user()?
            .create_device_group(&home.user_did, &home.node_did)
            .await?;
        home.settled(&home.user_did, "account device admission")
            .await?;
        home.reopen_for_replication().await
    }
    let source = peer().await?;
    let receiver = peer().await?;
    let collection = "PeerRegistry";
    let admin = |home: &Household| -> anyhow::Result<_> {
        let caller =
            defra_core::RequestContext::default().with_identity(Some(home.user_did.to_string()));
        home.node
            .p2p_arc()
            .and_then(|p| p.bind_request(&caller))
            .context("home has no P2P administration")
    };
    let target = admin(&receiver)?;
    target.add_collections(vec![collection.into()]).await?;
    let address = target
        .shareable_address()
        .await?
        .context("missing receiver address")?;
    admin(&source)?
        .add_replicator(
            vec![collection.into()],
            Some(&address),
            Default::default(),
            Vec::new(),
            None,
        )
        .await?;

    let row = source
        .as_node()
        .transact_write("test.registry.create", |txn| {
            let mutation = format!(
                r#"mutation {{ create_PeerRegistry(input: {{peer_id:"foreign",node_did:"{}",addresses:["local"]}}) {{_docID}} }}"#,
                source.node_did
            );
            Box::pin(async move { txn.execute(&mutation).await })
        })
        .await?;
    let id = gents_store::graphql::created_doc_id(&row, collection)?;
    source
        .settled(&source.node_did, "origin registration")
        .await?;
    let mut changes =
        gents_store::subscription_source::subscribe_collections(&receiver.node, [collection])?;
    let policy = node_policy_ids(&receiver.node).id(BuiltInPolicy::Household);
    let provider = receiver.node.document_acp();
    tokio::time::timeout(SETTLE_CEILING, async {
        loop {
            let owner = provider.get_doc_owner(policy, "discovery", &id).await?;
            if owner.as_ref() == Some(&receiver.node_did)
                && receiver
                    .holds(&source.node_did, "update", "discovery", &id)
                    .await?
                && receiver
                    .holds(&source.node_did, "delete", "discovery", &id)
                    .await?
            {
                return Ok::<_, anyhow::Error>(());
            }
            if changes.recv().await.is_none() {
                anyhow::bail!("PeerRegistry subscription closed before replica grants arrived");
            }
        }
    })
    .await
    .context("foreign genesis and creator grants did not reach the receiver")??;
    receiver
        .settled(&receiver.node_did, "replica grants")
        .await?;
    check_group_manages_readers(&receiver, &id, &source.user_did).await?;
    check_group_manages_readers(&source, &id, &receiver.user_did).await?;
    assert!(
        !receiver
            .holds(&source.user_did, "update", "discovery", &id)
            .await?
    );
    let query = format!(r#"{{ PeerRegistry(docID:"{id}") {{_docID status}} }}"#);
    let origin_before = source.as_node().execute(&query).await?;
    anyhow::ensure!(
        gents_protocol::graphql::graphql_rows_from_response(&origin_before, collection).len() == 1,
        "origin registry missing: {origin_before}"
    );
    let receiver_before = receiver.as_node().execute(&query).await?;
    source.as_node().write("test.registry.update", &format!(
        r#"mutation {{ update_PeerRegistry(docID:"{id}",input:{{status:"updated"}}) {{_docID}} }}"#
    )).await?;
    let origin = source.as_node().execute(&query).await?;
    anyhow::ensure!(
        gents_protocol::graphql::graphql_rows_from_response(&origin, collection)
            .first()
            .and_then(|row| row["status"].as_str())
            == Some("updated"),
        "origin did not apply the update: {origin}"
    );
    let receiver_access = receiver.as_node();
    tokio::time::timeout(
        SETTLE_CEILING,
        gents_store::test_support::read_when_changed(
            &receiver_access,
            collection,
            "foreign creator update on receiver",
            async || {
                let row = receiver_access
                    .execute(&query)
                    .await
                    .map_err(|err| format!("{err:#}"))?;
                if gents_protocol::graphql::graphql_rows_from_response(&row, collection)
                    .first()
                    .and_then(|row| row["status"].as_str())
                    == Some("updated")
                {
                    Ok(())
                } else {
                    Err(row.to_string())
                }
            },
        ),
    )
    .await
    .map_err(|_| anyhow::anyhow!("foreign creator's update did not reach receiver, origin {origin}, receiver before update {receiver_before}"))?;
    let deletion = source
        .as_node()
        .write(
            "test.registry.delete",
            &format!(r#"mutation {{ delete_PeerRegistry(docID:"{id}") {{_docID _deleted}} }}"#),
        )
        .await?;
    let deleted =
        gents_protocol::graphql::graphql_rows_from_response(&deletion, "delete_PeerRegistry");
    anyhow::ensure!(
        deleted.len() == 1 && deleted[0]["_docID"] == id && deleted[0]["_deleted"] == true,
        "origin did not delete the registry row: {deletion}"
    );
    let tombstone =
        format!(r#"{{ PeerRegistry(docID:"{id}",showDeleted:true) {{_docID _deleted}} }}"#);
    tokio::time::timeout(
        SETTLE_CEILING,
        gents_store::test_support::read_when_changed(
            &receiver_access,
            collection,
            "foreign creator tombstone on receiver",
            async || {
                let response = receiver_access
                    .execute(&tombstone)
                    .await
                    .map_err(|err| format!("{err:#}"))?;
                let rows =
                    gents_protocol::graphql::graphql_rows_from_response(&response, collection);
                if rows.len() == 1 && rows[0]["_docID"] == id && rows[0]["_deleted"] == true {
                    Ok(())
                } else {
                    Err(response.to_string())
                }
            },
        ),
    )
    .await
    .context("foreign creator's tombstone did not reach the receiver")?;
    assert_eq!(
        provider
            .get_doc_owner(policy, "discovery", &id)
            .await?
            .as_ref(),
        Some(&receiver.node_did)
    );
    receiver.shutdown().await?;
    source.shutdown().await?;
    Ok(())
}

async fn check_group_manages_readers(
    home: &Household,
    id: &str,
    recipient: &identity::Did,
) -> anyhow::Result<()> {
    use anyhow::Context;
    let group = home
        .as_node()
        .assigned_device_group(&home.user_did)
        .await?
        .context("replica group revoked")?;
    let manager = home.client("reader-manager").await?;
    let manager_did = identity::Did::new(manager.did())?;
    let manager_access = home.as_client(&manager)?;
    let denied = manager_access
        .add_actor_relationship("PeerRegistry", id, "reader", recipient)
        .await
        .expect_err("unadmitted client managed registry readers");
    assert!(
        matches!(&denied, ConfigClientError::DocumentRelationshipStatus { status, .. } if status.is_client_error()),
        "{denied}"
    );
    assert!(
        !home
            .as_node()
            .observe_actor_relationship("PeerRegistry", id, "reader", recipient)
            .await?
    );
    home.as_user()?
        .add_actor_relationship(ACCOUNT_DEVICE_GROUP_NAME, &group, "member", &manager_did)
        .await?;
    home.settled(&home.user_did, "manager admission").await?;
    manager_access
        .add_actor_relationship("PeerRegistry", id, "reader", recipient)
        .await?;
    home.settled(&manager_did, "device group reader grant")
        .await?;
    assert!(
        home.as_node()
            .observe_actor_relationship("PeerRegistry", id, "reader", recipient)
            .await?
    );
    Ok(())
}
