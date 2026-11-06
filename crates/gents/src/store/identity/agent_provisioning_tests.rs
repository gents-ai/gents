use super::*;
use crate::store::config_client::ConfigAccess;

#[tokio::test]
async fn authorized_provisioning_grants_no_direct_node_permission() {
    let modeled = crate::lean_vocab_test::lean_contract_snapshot()
        .identity_grant_recipient_cases
        .iter()
        .find(|case| case.recipient == "agent")
        .expect("modeled Agent recipient");
    assert!(modeled.direct.is_empty());
    let (db, owner, _) =
        crate::test_support::signed_test_node_without_agent("provision-no-direct").await;
    let node = db.node.clone();
    let runtime = db.node_identity.clone();
    let runtime = runtime.as_ref();
    let outsider = tokio::task::spawn_blocking(|| {
        crate::support::fixtures::test_identity("provision-outsider")
    })
    .await
    .unwrap();
    let admin = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did()).unwrap());
    super::super::GrantSet::OperatorUser
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
    for permission in super::super::GrantSet::AgentBaseline.permissions() {
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

#[tokio::test]
async fn modeled_agent_creator_and_node_owner_control_native_agent() -> anyhow::Result<()> {
    use acp::policy_yaml::{build_policy, parse_policy_yaml};
    use acp::{
        DocumentACP, DocumentPermission, Identity, MemoryZanzibarStore, ZanzibarDocumentACP,
        ZanzibarStore,
    };
    let cases = tokio::task::spawn_blocking(|| {
        crate::lean_vocab_test::lean_contract_snapshot()
            .identity_created_agent_access_cases
            .clone()
    })
    .await?;
    let dir = tempfile::tempdir()?;
    let user = KeyIdentity::load_or_create(dir.path().join("user"), None)?;
    let creator = KeyIdentity::load_or_create(dir.path().join("creator"), None)?;
    let sibling = KeyIdentity::load_or_create(dir.path().join("sibling"), None)?;
    let policy = build_policy(
        &parse_policy_yaml(gents_protocol::schemas::AGENT_OWNERSHIP_POLICY)
            .map_err(anyhow::Error::msg)?,
        0,
    )?;
    let store = Arc::new(MemoryZanzibarStore::new());
    store.store_policy(&policy).await?;
    let acp = ZanzibarDocumentACP::new(store);
    let user_did = identity::Did::new(user.did())?;
    let creator_did = identity::Did::new(creator.did())?;
    let sibling_did = identity::Did::new(sibling.did())?;
    acp.register_doc_object(&user_did, &policy.id, "nodes", "node")
        .await?;
    acp.register_doc_object(&creator_did, &policy.id, "agents", "child")
        .await?;
    assert!(
        acp.add_relationship(
            &creator_did,
            acp::Subject::entity_set("nodes", "node", ""),
            &policy.id,
            "agents",
            "child",
            "parent",
            &[]
        )
        .await?
    );
    for case in &cases {
        let actor = match case["actor"].as_str().unwrap() {
            "user" => &user_did,
            "creator" => &creator_did,
            "sibling" => &sibling_did,
            actor => panic!("unknown modeled actor {actor}"),
        };
        let permission = match case["permission"].as_str().unwrap() {
            "read" => DocumentPermission::Read,
            "update" => DocumentPermission::Update,
            permission => panic!("unknown modeled permission {permission}"),
        };
        let actual = acp
            .check_doc_access(
                &Identity::Authenticated(actor.clone()),
                permission,
                &policy.id,
                "agents",
                "child",
            )
            .await?;
        assert_eq!(actual, case["allowed"].as_bool().unwrap(), "{case}");
    }
    Ok(())
}
