use gents_store::config_client::ConfigAccess;
use gents_store::identity::agent::*;
use gents_store::identity::NodeIdentity;

#[tokio::test]
async fn authorized_provisioning_grants_no_direct_node_permission() {
    assert!(gents_store::identity::GrantSet::for_recipient(
        gents_store::identity::Recipient::Agent
    )
    .direct
    .is_empty());
    let (db, owner, _) = gents_documents::test_support::SignedHome::enroll(
        crate::support::trial_store("provision-no-direct").await,
        "provision-no-direct",
    )
    .await
    .into_parts();
    let node = db.node.clone();
    let runtime = db.node_identity.clone();
    let runtime = runtime.as_ref();
    let outsider = tokio::task::spawn_blocking(|| {
        gents_store::test_support::test_identity("provision-outsider")
    })
    .await
    .unwrap();
    let admin = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did()).unwrap());
    gents_store::identity::GrantSet::OperatorUser
        .grant(&admin, &identity::Did::new(outsider.did()).unwrap())
        .await
        .unwrap();
    assert!(matches!(
        provision_agent_identity_authorized(
            &node,
            runtime,
            &identity::Did::new(outsider.did()).unwrap()
        )
        .await,
        Err(AgentProvisionError::Unauthorized)
    ));
    let binding = provision_agent_identity_authorized(
        &node,
        runtime,
        &identity::Did::new(owner.did()).unwrap(),
    )
    .await
    .unwrap();
    resolve_agent_identity(runtime, &binding).unwrap();
    let agent = identity::Did::new(&binding.agent_did).unwrap();
    for permission in gents_store::identity::GrantSet::AgentBaseline.permissions() {
        assert!(
            !node
                .nac_manager()
                .check_permission(&agent, *permission)
                .await
                .unwrap(),
            "provisioning granted {} before publication",
            permission.as_str()
        );
    }
    node.shutdown().await.unwrap();
}
