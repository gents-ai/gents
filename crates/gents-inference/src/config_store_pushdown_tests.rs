use anyhow::Context as _;
use gents_documents::apply_desired_state_plan;
use gents_documents::{
    read_desired_state_record_in_txn, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents_store::config_client::authored_document::*;
use gents_store::config_client::{ConfigAccess, ConfigClientError, NodePermission};
use gents_store::identity::grant_sets::test_support::*;
use gents_store::identity::grant_sets::*;
use gents_store::identity::NodeIdentity;
use serde_json::json;

#[tokio::test]
async fn authored_application_document_is_signed_by_the_held_user() -> anyhow::Result<()> {
    const SDL: &str = "type AuthoredDocumentFixture { name: String }";
    let (db, user, access, _) = crate::test_support::signed_test_node(
        "authored-document",
        gents_store::node_extension::Extensions::none(),
    )
    .await;
    // Schema installation is node administration; the held User authors documents only.
    let admin = ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(db.node_identity.did()).unwrap(),
    );
    let plan = gents_store::config_client::preview_schema_install(&admin, SDL).await?;
    gents_store::config_client::apply_schema_install(&admin, SDL, &plan.artifact_digest).await?;
    let doc = create_authored_document(
        &access,
        "AuthoredDocumentFixture",
        &json!({"name": "created"}),
        DocumentSchemaSource::UserAuthored,
        SDL,
    )
    .await?;
    let escaped = gents_store::graphql::escape_graphql_string(&doc);
    let response = access.execute(&format!(
        r#"{{ _commits(docID: "{escaped}", filter: {{fieldName: {{_eq: "_C"}}}}, order: {{height: ASC}}, limit: 1) {{cid}} }}"#,
    )).await?;
    let cid = response["data"]["_commits"][0]["cid"]
        .as_str()
        .expect("genesis CID");
    let (block, signature) = db
        .node
        .authorized_signed_block_bytes(cid, Some(user.did()))
        .await?;
    let block = defra_core::block::Block::from_dag_cbor(&block)?;
    let signature = defra_core::block::Signature::from_dag_cbor(&signature)?;
    let signer = db::block::verify::verified_signature_signer_did(&block, &signature)
        .map_err(anyhow::Error::msg)?;
    assert_eq!(signer, user.did());
    assert_ne!(user.did(), db.node_identity.did());
    let response = access
        .execute("{ AuthoredDocumentFixture { name } }")
        .await?;
    assert_eq!(
        response["data"]["AuthoredDocumentFixture"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    Ok(())
}

async fn set_node_hop_cap(owner: &ConfigAccess, node_did: &str, cap: u32) -> anyhow::Result<()> {
    owner
        .transact_write("test.runtime_hop_cap.bound", |txn| {
            Box::pin(async move {
                let (_, mut node) = read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Node,
                    node_did,
                    node_did,
                )
                .await?
                .context("owned Node is unavailable")?;
                node["max_request_hop"] = serde_json::json!(cap);
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: gents_store::collection::Collection::Node,
                    add: node.clone(),
                    update: node,
                }])?;
                apply_desired_state_plan(txn, &plan, &crate::resolved::ProviderProfilePolicy).await
            })
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn agent_transaction_reads_hidden_node_cap_as_runtime_and_fails_closed() -> anyhow::Result<()>
{
    let (db, user, user_access, binding) = crate::test_support::signed_test_node(
        "runtime-hop-cap",
        gents_store::node_extension::Extensions::none(),
    )
    .await;
    let runtime = db.node_identity.as_ref();
    let signer = gents_store::identity::resolve_agent_identity(runtime, &binding)?;
    let agent = ConfigAccess::local_managed_agent(
        db.node.clone(),
        identity::Did::new(&binding.agent_did)?,
        signer.as_ref(),
    )?;
    set_node_hop_cap(&user_access, runtime.did(), 1).await?;
    let observe = |node_did: String| {
        agent.transact_readonly("test.runtime_hop_cap.observe", move |txn| {
            let node_did = node_did.clone();
            Box::pin(async move {
                Ok::<_, anyhow::Error>(txn.runtime_max_request_hop(&node_did).await?)
            })
        })
    };
    let hidden = agent.execute("{ Node { _docID } }").await?;
    assert!(hidden["data"]["Node"].as_array().is_some_and(Vec::is_empty));
    assert_eq!(observe(runtime.did().to_owned()).await?, Some(Some(1)));
    assert!(observe("did:key:foreign".to_owned()).await.is_err());

    let admin = ConfigAccess::local_as(db.node.clone(), identity::Did::new(runtime.did())?);
    admin
        .grant_node_permission(
            &identity::Did::new(user.did())?,
            NodePermission::DacRelationDelete,
        )
        .await?;
    let node = user_access.execute("{ Node { _docID } }").await?;
    let node_doc = node["data"]["Node"][0]["_docID"].as_str().unwrap();
    user_access
        .delete_actor_relationship(
            "Node",
            node_doc,
            "runtime_reader",
            &identity::Did::new(runtime.did())?,
        )
        .await?;
    assert_eq!(observe(runtime.did().to_owned()).await?, None);
    Ok(())
}

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
                    &crate::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn agent_baseline_projection_follows_modeled_lifecycle() {
    use gents_lean_vocab::families::agent_baseline_lifecycle as wire;

    for (index, case) in gents_lean_vocab::families::agent_baseline_lifecycle::cases::<String>().into_iter().enumerate() {
        let case_name = format!("agent-baseline-{index}");
        let (db, owner, owner_access) = crate::test_support::signed_test_node_without_agent(
            &case_name,
            gents_store::node_extension::Extensions::none(),
        )
        .await;
        let runtime = db.node_identity.clone();
        let runtime_did = runtime.did().to_owned();
        let admin =
            ConfigAccess::local_as(db.node.clone(), identity::Did::new(runtime.did()).unwrap());
        let actor_name = format!("{case_name}-actor");
        let actor = tokio::task::spawn_blocking(move || {
            gents_store::test_support::test_identity(&actor_name)
        })
        .await
        .unwrap();
        let actor_did = identity::Did::new(actor.did()).unwrap();
        for set in recipient_sets(&case.input.actor) {
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
        let mut steps = Vec::new();
        for action in &case.input.actions {
            let result = agent_step(&actor_access, &runtime_did, &binding, action).await;
            if let Err(error) = &result {
                assert!(
                    gents_protocol::error_chain::find_map_source(error.as_ref(), |cause| {
                        cause
                            .downcast_ref::<ConfigClientError>()
                            .filter(|error| error.is_permission_denied())
                    })
                    .is_some(),
                    "{case_name} {action}: {error:#}"
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
            steps.push(wire::StepObservation {
                action: action.clone(),
                committed: result.is_ok(),
                installed,
            });
        }
        case.check(wire::LifecycleOutput { steps });
        db.node.shutdown().await.unwrap();
    }
}
