use std::sync::Arc;

use gents_store::config_client::{ConfigAccess, ConfigClientError};
use gents_store::identity::grant_sets::test_support::*;
use gents_store::identity::grant_sets::*;
use gents_store::identity::NodeIdentity;

fn recipient_sets(name: &str) -> Vec<GrantSet> {
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

pub(crate) async fn agent_step(
    access: &ConfigAccess,
    node_did: &str,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
    action: &str,
) -> anyhow::Result<()> {
    use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    if action == "delete" {
        let response = access
            .execute(&format!(
                r#"{{ Agent(filter: {{ agent_did: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                gents_store::graphql::escape_graphql_string(&binding.agent_did)
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
    let documents =
        gents_documents::test_support::test_agent_documents(node_did, "Baseline agent", binding)
            .into_iter()
            .filter(|(collection, _)| {
                action == "publish" || *collection == gents_store::collection::Collection::Agent
            })
            .map(|(collection, mut value)| {
                if collection == gents_store::collection::Collection::Agent {
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
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn agent_baseline_projection_follows_modeled_lifecycle() {
    let cases = &gents_lean_vocab::lean_contract_snapshot().identity_agent_baseline_cases;
    assert!(!cases.is_empty());
    for case in cases {
        let (db, owner, owner_access) =
            crate::test_support::root::signed_test_node_without_agent(&case.name).await;
        let runtime = db.node_identity.clone();
        let runtime_did = runtime.did().to_owned();
        let admin =
            ConfigAccess::local_as(db.node.clone(), identity::Did::new(runtime.did()).unwrap());
        let actor_name = format!("{}-actor", case.name);
        let actor = tokio::task::spawn_blocking(move || {
            gents_store::test_support::test_identity(&actor_name)
        })
        .await
        .unwrap();
        let actor_did = identity::Did::new(actor.did()).unwrap();
        for set in recipient_sets(&case.actor) {
            set.grant(&admin, &actor_did).await.unwrap();
        }
        let node_doc = owner_access.execute("{ Node { _docID } }").await.unwrap()["data"]["Node"]
            [0]["_docID"]
            .as_str()
            .expect("fixture Node")
            .to_owned();
        owner_access
            .add_actor_relationship("Node", &node_doc, "administrator", &actor_did)
            .await
            .unwrap();
        drop(owner);
        let actor_access = db.client_access(&actor);
        let binding = gents_store::identity::provision_agent_identity(runtime.as_ref()).unwrap();
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
