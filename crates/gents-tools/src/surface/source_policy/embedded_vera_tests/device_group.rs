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
    assert_eq!(receiver.assigned_device_group(&home.user_did).await?, None);
    home.shutdown().await?;
    Ok(())
}
