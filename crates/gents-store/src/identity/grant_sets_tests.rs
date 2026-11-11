use std::sync::Arc;

use super::*;
use crate::identity::NodeIdentity;

pub(crate) fn set_named(name: &str) -> GrantSet {
    GrantSet::ALL
        .into_iter()
        .find(|set| set.name() == name)
        .unwrap_or_else(|| panic!("Lean grant set {name:?} has no Rust owner"))
}

pub(crate) fn recipient_sets(name: &str) -> Vec<GrantSet> {
    let case = gents_lean_vocab::lean_contract_snapshot()
        .identity_grant_recipient_cases
        .iter()
        .find(|case| case.recipient == name)
        .unwrap_or_else(|| panic!("Lean recipient {name:?}"));
    case.direct
        .iter()
        .chain(&case.projected)
        .map(|set| set_named(set))
        .collect()
}

pub(crate) async fn nac_node() -> (
    tempfile::TempDir,
    crate::identity::KeyIdentity,
    Arc<defra_node::Node>,
) {
    let directory = tempfile::tempdir().unwrap();
    let runtime =
        crate::identity::KeyIdentity::load_or_create(directory.path().join("runtime.key"), None)
            .unwrap();
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_node_acp_enabled()
            .build()
            .await
            .unwrap(),
    );
    (directory, runtime, node)
}

/// A delegate acts as a managed Agent: native relation changes by a non-node actor
/// require its validated Agent signer.
pub(crate) fn agent_delegate(
    runtime: &crate::identity::KeyIdentity,
) -> (identity::Did, Arc<dyn NodeIdentity>) {
    let binding = crate::identity::provision_agent_identity(runtime).unwrap();
    let agent = crate::identity::resolve_agent_identity(runtime, &binding).unwrap();
    let did = identity::Did::new(agent.did()).unwrap();
    (did, agent)
}

pub(crate) fn did(directory: &tempfile::TempDir, name: &str) -> identity::Did {
    let key =
        crate::identity::KeyIdentity::load_or_create(directory.path().join(name), None).unwrap();
    identity::Did::new(key.did()).unwrap()
}

#[test]
fn sets_match_lean_owner() {
    use gents_lean_vocab::families;
    families::identity_grant_sets_grant_set_vocabulary::replay(&GrantSet::ALL);
    families::identity_grant_sets_grant_set_native_admin::replay(|i| {
        GrantSet::native_admin(i.grant_set)
    });
    for case in &gents_lean_vocab::lean_contract_snapshot().identity_grant_set_cases {
        let set = set_named(&case.set);
        let permissions: Vec<_> = set.permissions().iter().map(|p| p.as_str()).collect();
        assert_eq!(permissions, case.permissions, "{}", case.set);
    }
}

#[tokio::test]
async fn native_delegation_follows_modeled_holders() {
    let (directory, runtime, node) = nac_node().await;
    let admin = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did()).unwrap());
    let cases = &gents_lean_vocab::lean_contract_snapshot().identity_delegation_cases;
    assert!(!cases.is_empty());
    for (index, case) in cases.iter().enumerate() {
        let (holder, holder_identity) = agent_delegate(&runtime);
        let target = did(&directory, &format!("target-{index}"));
        for set in recipient_sets(&case.holder) {
            set.grant(&admin, &holder).await.unwrap();
        }
        let permission = (case.relation != "admin")
            .then(|| NodePermission::parse(&case.relation).expect("modeled permission"));
        if case.delete {
            admin
                .grant_node_permission(&target, permission.expect("revocable permission"))
                .await
                .unwrap();
        }
        let access =
            ConfigAccess::local_managed_agent(node.clone(), holder, holder_identity.as_ref())
                .unwrap();
        let result = match (permission, case.delete) {
            (None, false) => access.grant_node_admin(&target).await,
            (None, true) => access.revoke_node_admin(&target).await,
            (Some(permission), false) => access.grant_node_permission(&target, permission).await,
            (Some(permission), true) => access.revoke_node_permission(&target, permission).await,
        };
        assert_eq!(result.is_ok(), case.expected, "{}: {result:?}", case.name);
        if let (Some(permission), true) = (permission, case.expected) {
            assert_eq!(
                node.nac_manager()
                    .check_permission(&target, permission)
                    .await
                    .unwrap(),
                !case.delete,
                "{}",
                case.name
            );
        }
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
