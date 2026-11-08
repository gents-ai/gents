use std::sync::Arc;

use super::*;
use crate::store::identity::NodeIdentity;

fn set_named(name: &str) -> GrantSet {
    GrantSet::ALL
        .into_iter()
        .find(|set| set.name() == name)
        .unwrap_or_else(|| panic!("Lean grant set {name:?} has no Rust owner"))
}

fn recipient_sets(name: &str) -> Vec<GrantSet> {
    let case = crate::lean_vocab_test::lean_contract_snapshot()
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

async fn nac_node() -> (tempfile::TempDir, crate::KeyIdentity, Arc<defra_node::Node>) {
    let directory = tempfile::tempdir().unwrap();
    let runtime =
        crate::KeyIdentity::load_or_create(directory.path().join("runtime.key"), None).unwrap();
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
fn agent_delegate(runtime: &crate::KeyIdentity) -> (identity::Did, Arc<dyn NodeIdentity>) {
    let binding = crate::store::identity::provision_agent_identity(runtime).unwrap();
    let agent = crate::store::identity::resolve_agent_identity(runtime, &binding).unwrap();
    let did = identity::Did::new(agent.did()).unwrap();
    (did, agent)
}

fn did(directory: &tempfile::TempDir, name: &str) -> identity::Did {
    let key = crate::KeyIdentity::load_or_create(directory.path().join(name), None).unwrap();
    identity::Did::new(key.did()).unwrap()
}

#[test]
fn sets_match_lean_owner() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().identity_grant_set_cases;
    assert_eq!(cases.len(), GrantSet::ALL.len());
    for case in cases {
        let set = set_named(&case.set);
        let permissions: Vec<_> = set.permissions().iter().map(|p| p.as_str()).collect();
        assert_eq!(permissions, case.permissions, "{}", case.set);
        assert_eq!(set.native_admin(), case.native_admin, "{}", case.set);
    }
}

#[tokio::test]
async fn native_delegation_follows_modeled_holders() {
    let (directory, runtime, node) = nac_node().await;
    let admin = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did()).unwrap());
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().identity_delegation_cases;
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

async fn agent_step(
    access: &ConfigAccess,
    node_did: &str,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
    action: &str,
) -> anyhow::Result<()> {
    use crate::store::config_client::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    if action == "delete" {
        let response = access
            .execute(&format!(
                r#"{{ Agent(filter: {{ agent_did: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                crate::store::graphql::escape_graphql_string(&binding.agent_did)
            ))
            .await?;
        let doc = response["data"]["Agent"][0]["_docID"]
            .as_str()
            .expect("published Agent is visible to its actor")
            .to_owned();
        access
            .transact_write("test.grant_sets.agent_delete", |txn| {
                let doc = &doc;
                Box::pin(async move { txn.delete_configuration_document("Agent", doc).await })
            })
            .await?;
        return Ok(());
    }
    let documents = crate::test_support::test_agent_documents(node_did, "Baseline agent", binding)
        .into_iter()
        .filter(|(collection, _)| action == "publish" || *collection == crate::Collection::Agent)
        .map(|(collection, mut value)| {
            if collection == crate::Collection::Agent {
                value["enabled"] = serde_json::json!(action != "disable");
            }
            DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            }
        })
        .collect();
    let plan = DesiredStateApplyPlan::new(documents).expect("Agent plan");
    access
        .transact_write("test.grant_sets.agent", |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::documents::apply_desired_state_plan(txn, plan).await
            })
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn agent_baseline_projection_follows_modeled_lifecycle() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().identity_agent_baseline_cases;
    assert!(!cases.is_empty());
    for case in cases {
        let (db, owner, owner_access) =
            crate::test_support::signed_test_node_without_agent(&case.name).await;
        let runtime = db.node_identity.clone();
        let runtime_did = runtime.did().to_owned();
        let admin = ConfigAccess::local_as(
            db.node.clone(),
            identity::Did::new(runtime.did()).unwrap(),
        );
        let actor_name = format!("{}-actor", case.name);
        let actor = tokio::task::spawn_blocking(move || {
            crate::support::fixtures::test_identity(&actor_name)
        })
        .await
        .unwrap();
        let actor_did = identity::Did::new(actor.did()).unwrap();
        for set in recipient_sets(&case.actor) {
            set.grant(&admin, &actor_did).await.unwrap();
        }
        let node_doc = owner_access
            .execute("{ Node { _docID } }")
            .await
            .unwrap()["data"]["Node"][0]["_docID"]
            .as_str()
            .expect("fixture Node")
            .to_owned();
        owner_access
            .add_actor_relationship("Node", &node_doc, "administrator", &actor_did)
            .await
            .unwrap();
        drop(owner);
        let actor_access = db.client_access(&actor);
        let binding = crate::store::identity::provision_agent_identity(runtime.as_ref()).unwrap();
        let agent = identity::Did::new(&binding.agent_did).unwrap();
        for step in &case.steps {
            let result = agent_step(&actor_access, &runtime_did, &binding, &step.action).await;
            assert_eq!(
                result.is_ok(),
                step.committed,
                "{} {}: {result:?}",
                case.name,
                step.action
            );
            if let Err(error) = &result {
                assert!(
                    gents_protocol::error_chain::find_map_source(error.as_ref(), |cause| {
                        cause
                            .downcast_ref::<ConfigClientError>()
                            .filter(|error| error.is_permission_denied())
                    })
                    .is_some(),
                    "{}: {error:#}",
                    case.name
                );
            }
            let mut installed = Vec::new();
            for permission in GrantSet::AgentBaseline.permissions() {
                if db
                    .node
                    .nac_manager()
                    .check_permission(&agent, *permission)
                    .await
                    .unwrap()
                {
                    installed.push(permission.as_str().to_owned());
                }
            }
            assert_eq!(installed, step.installed, "{} {}", case.name, step.action);
        }
        db.node.shutdown().await.unwrap();
    }
}
