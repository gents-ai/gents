use crate::store::config_client::ConfigAccess;
use crate::store::identity::agent::*;

#[tokio::test]
async fn authorized_provisioning_grants_no_direct_node_permission() {
    let modeled = gents_lean_vocab::lean_contract_snapshot()
        .identity_grant_recipient_cases
        .iter()
        .find(|case| case.recipient == "agent")
        .expect("modeled Agent recipient");
    assert!(modeled.direct.is_empty());
    let (db, owner, _) =
        crate::test_support::root::signed_test_node_without_agent("provision-no-direct").await;
    let node = db.node.clone();
    let runtime = db.node_identity.clone();
    let runtime = runtime.as_ref();
    let outsider = tokio::task::spawn_blocking(|| {
        crate::store::test_support::test_identity("provision-outsider")
    })
    .await
    .unwrap();
    let admin = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did()).unwrap());
    crate::store::identity::GrantSet::OperatorUser
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
    for permission in crate::store::identity::GrantSet::AgentBaseline.permissions() {
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
