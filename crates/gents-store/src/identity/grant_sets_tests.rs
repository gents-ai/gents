use super::test_support::*;
use super::*;
use crate::identity::NodeIdentity;

#[test]
fn sets_match_lean_owner() {
    use gents_lean_vocab::families;
    families::identity_grant_sets_grant_set_vocabulary::replay(&GrantSet::ALL);
    families::identity_grant_sets_grant_set_native_admin::replay(|i| {
        GrantSet::native_admin(i.grant_set)
    });
    families::identity_grant_sets_grant_set_permissions::replay(|i| {
        GrantSet::permissions(i.grant_set)
            .iter()
            .map(|permission| permission.as_str().to_owned())
            .collect()
    });
}

#[tokio::test]
async fn native_delegation_follows_modeled_holders() {
    let (directory, runtime, node) = nac_node().await;
    let admin = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did()).unwrap());
    for (index, case) in gents_lean_vocab::families::identity_delegation_native::cases()
        .into_iter()
        .enumerate()
    {
        let input = &case.input;
        let (holder, holder_identity) = agent_delegate(&runtime);
        let target = did(&directory, &format!("target-{index}"));
        for set in recipient_sets(&input.holder) {
            set.grant(&admin, &holder).await.unwrap();
        }
        let permission = (input.relation != "admin")
            .then(|| NodePermission::parse(&input.relation).expect("modeled permission"));
        if input.delete {
            admin
                .grant_node_permission(&target, permission.expect("revocable permission"))
                .await
                .unwrap();
        }
        let access =
            ConfigAccess::local_managed_agent(node.clone(), holder, holder_identity.as_ref())
                .unwrap();
        let result = match (permission, input.delete) {
            (None, false) => access.grant_node_admin(&target).await,
            (None, true) => access.revoke_node_admin(&target).await,
            (Some(permission), false) => access.grant_node_permission(&target, permission).await,
            (Some(permission), true) => access.revoke_node_permission(&target, permission).await,
        };
        if let (Some(permission), true) = (permission, result.is_ok()) {
            assert_eq!(
                node.nac_manager()
                    .check_permission(&target, permission)
                    .await
                    .unwrap(),
                !input.delete,
                "{}",
                input.name
            );
        }
        case.check(result.is_ok());
    }
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn refused_grant_rolls_back_the_grants_it_issued() {
    let (directory, runtime, node) = nac_node().await;
    let admin = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did()).unwrap());
    let (delegate, delegate_identity) = agent_delegate(&runtime);
    let subject = did(&directory, "subject");
    for permission in [
        NodePermission::NacRelationAdd,
        NodePermission::NacRelationDelete,
        NodePermission::CollectionGet,
        NodePermission::DocumentRead,
    ] {
        admin
            .grant_node_permission(&delegate, permission)
            .await
            .unwrap();
    }
    let error = GrantSet::OperatorUser
        .grant(
            &ConfigAccess::local_managed_agent(node.clone(), delegate, delegate_identity.as_ref())
                .unwrap(),
            &subject,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            GrantSetError::Grant {
                relation: "update-document",
                ..
            }
        ),
        "{error}"
    );
    for permission in GrantSet::OperatorUser.permissions() {
        assert!(
            !node
                .nac_manager()
                .check_permission(&subject, *permission)
                .await
                .unwrap(),
            "{} survived the rollback",
            permission.as_str()
        );
    }

    GrantSet::HouseholdOwner
        .grant(&admin, &subject)
        .await
        .unwrap();
    assert!(node.nac_manager().is_admin(&subject).await.unwrap());
    GrantSet::HouseholdOwner
        .revoke(&admin, &subject)
        .await
        .unwrap();
    assert!(!node.nac_manager().is_admin(&subject).await.unwrap());
    node.shutdown().await.unwrap();
}
