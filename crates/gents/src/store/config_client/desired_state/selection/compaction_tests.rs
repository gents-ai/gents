use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::inference::resolved::{SelectedConfiguration, SelectedConfigurationDocument};
use crate::store::config_client::{
    apply_desired_state_plan, ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
    GraphqlEndpoint,
};
use crate::store::identity::NodeIdentity;
use crate::Collection;

async fn publish(access: &ConfigAccess, documents: Vec<(Collection, Value)>) -> Result<()> {
    let plan = DesiredStateApplyPlan::new(
        documents
            .into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
    )?;
    access
        .transact_write("test.compaction.publish", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
}

async fn document_id(
    access: &ConfigAccess,
    collection: &str,
    field: &str,
    id: &str,
) -> Result<String> {
    let response = access
        .execute(&format!(
            "{{{collection}(filter: {{{field}: {{_eq: \"{}\"}}}}) {{_docID}}}}",
            crate::store::graphql::escape_graphql_string(id),
        ))
        .await?;
    response["data"][collection][0]["_docID"]
        .as_str()
        .map(str::to_owned)
        .context("selected configuration document is missing")
}

#[tokio::test]
async fn signed_compaction_selection_and_revocation_follow_native_usersets() -> Result<()> {
    let cases = &crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .context("configuration model")?
        .configuration_compaction_cases;
    anyhow::ensure!(!cases.is_empty(), "Lean emitted no compaction cases");
    let home = tempfile::tempdir()?;
    let runtime = crate::KeyIdentity::load_or_create(home.path().join("runtime"), None)?;
    let user = crate::KeyIdentity::load_or_create(home.path().join("user"), None)?;
    let sibling = crate::KeyIdentity::load_or_create(home.path().join("sibling"), None)?;
    let binding = crate::store::identity::provision_agent_identity(&runtime)?;
    let selected = crate::store::identity::resolve_agent_identity(&runtime, &binding)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_node_acp_owner(user.did().to_owned())
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .build()
            .await?,
    );
    crate::ensure_runtime_schemas(&node).await?;
    let authority =
        defra_core::RequestContext::default().with_identity(Some(user.did().to_owned()));
    for actor in [selected.did(), sibling.did(), runtime.did()] {
        for permission in [
            acp::nac::NodePermission::DocumentRead,
            acp::nac::NodePermission::DocumentUpdate,
            acp::nac::NodePermission::CollectionGet,
        ] {
            node.add_nac_actor_relationship_as(&authority, permission.as_str(), actor)
                .await?;
        }
    }
    let endpoint =
        GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), user.did())
            .with_client_signing_capability(user.client_signing_capability()?)?
            .with_client_signed_agent_creates();
    let user_access = ConfigAccess::Graphql(endpoint);
    let input = json!({"node_did":runtime.did()});
    let node_response =
        user_access
            .transact_write("test.compaction.node", |txn| {
                let input = &input;
                Box::pin(async move {
                    txn.execute_desired_state_mutation(
                "mutation($input: NodeMutationInputArg!) {create_Node(input:$input){_docID}}",
                &json!({"input":input}), Collection::Node, true).await
                })
            })
            .await?;
    let node_doc = gents_protocol::graphql::extract_mutation_doc_id(&node_response, "Node")?;
    user_access
        .add_actor_relationship(
            "Node",
            &node_doc,
            "runtime_reader",
            &identity::Did::new(runtime.did())?,
        )
        .await?;
    let context_value =
        |id: &str| json!({"node_did":runtime.did(), "context_id":"context", "compaction_id":id});
    publish(&user_access, vec![
        (Collection::Agent, json!({"node_did":runtime.did(), "agent_did":binding.agent_did,
            "creation_nonce":binding.creation_nonce, "context_id":"context", "inference_profile_id":"profile"})),
        (Collection::AgentContext, context_value("compaction-a")),
        (Collection::Compaction, json!({"node_did":runtime.did(), "compaction_id":"compaction-a", "threshold":0.75,
            "inference_profile_id":"profile"})),
        (Collection::Compaction, json!({"node_did":runtime.did(), "compaction_id":"compaction-b", "threshold":0.5})),
        (Collection::InferenceProfile, json!({"node_did":runtime.did(), "profile_id":"profile", "backend_id":"backend", "model_name":"model"})),
        (Collection::InferenceBackend, json!({"node_did":runtime.did(), "backend_id":"backend", "name":"test",
            "provider_kind":"OpenAiCompatible", "endpoint":"http://127.0.0.1:1/v1", "auth":{"kind":"unauthenticated"}})),
    ]).await?;
    let agent_doc = document_id(&user_access, "Agent", "agent_did", &binding.agent_did).await?;
    let context_doc = document_id(&user_access, "AgentContext", "context_id", "context").await?;
    let a = document_id(
        &user_access,
        "CompactionConfig",
        "compaction_id",
        "compaction-a",
    )
    .await?;
    let b = document_id(
        &user_access,
        "CompactionConfig",
        "compaction_id",
        "compaction-b",
    )
    .await?;
    let agent_did = identity::Did::new(&binding.agent_did)?;
    let selected_access =
        ConfigAccess::local_managed_agent(node.clone(), agent_did.clone(), selected.as_ref())?;
    let sibling_access = ConfigAccess::local_as(node.clone(), identity::Did::new(sibling.did())?);
    let runtime_access = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
    let query = "{ CompactionConfig {_docID compaction_id threshold} }";
    let runtime_rows = runtime_access.execute(query).await?;
    anyhow::ensure!(
        runtime_rows["data"]["CompactionConfig"]
            .as_array()
            .is_some_and(|rows| rows.len() == 2),
        "runtime should observe configuration through its Node relationship"
    );
    let history = user_access
        .execute(&format!(
            "{{_commits(docID:\"{}\") {{cid height fieldName}}}}",
            crate::store::graphql::escape_graphql_string(&a)
        ))
        .await?;
    let cid = history["data"]["_commits"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .filter(|row| row["fieldName"] == "_C")
                .min_by_key(|row| row["height"].as_i64().unwrap_or_default())
        })
        .and_then(|row| row["cid"].as_str())
        .context("CompactionConfig genesis missing")?;
    let (block, signature) = node
        .authorized_signed_block_bytes(cid, Some(user.did()))
        .await?;
    let block = defra_core::block::Block::from_dag_cbor(&block)?;
    let signature = defra_core::block::Signature::from_dag_cbor(&signature)?;
    assert_eq!(
        db::block::verify::verified_signature_signer_did(&block, &signature)
            .map_err(anyhow::Error::msg)?,
        user.did()
    );
    let update = format!(
        "mutation {{update_CompactionConfig(docID:\"{}\", input:{{threshold:0.25}}) {{_docID}}}}",
        crate::store::graphql::escape_graphql_string(&a)
    );
    let initial_selected = cases
        .iter()
        .find(|case| {
            case["phase"] == "initial"
                && case["actor"] == "selected"
                && case["document"] == "compaction-a"
        })
        .context("modeled initial compaction")?;
    let updated_by_selected = match selected_access
        .write("test.compaction.denied", &update)
        .await
    {
        Ok(denied) => denied["data"]["update_CompactionConfig"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty()),
        Err(error) if error.is_graphql_rejection() => false,
        Err(error) => return Err(error.into()),
    };
    assert_eq!(
        updated_by_selected,
        initial_selected["update_expected"]
            .as_bool()
            .context("selected update expectation")?
    );
    assert_eq!(
        user_access.execute(query).await?["data"]["CompactionConfig"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["_docID"] == a)
            .unwrap()["threshold"],
        json!(0.75)
    );
    publish(
        &user_access,
        vec![(
            Collection::Compaction,
            json!({"node_did":runtime.did(), "compaction_id":"compaction-a", "threshold":0.6,
                "inference_profile_id":"profile"}),
        )],
    )
    .await?;
    let updated = user_access.execute(query).await?;
    let updated_by_owner = updated["data"]["CompactionConfig"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["_docID"] == a)
        .is_some_and(|row| row["threshold"] == json!(0.6));
    assert_eq!(
        updated_by_owner,
        initial_selected["owner_update_expected"]
            .as_bool()
            .context("owner update expectation")?
    );
    let history = user_access
        .execute(&format!(
            "{{_commits(docID:\"{}\") {{cid height fieldName}}}}",
            crate::store::graphql::escape_graphql_string(&a)
        ))
        .await?;
    let cid = history["data"]["_commits"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .filter(|row| row["fieldName"] == "_C")
                .max_by_key(|row| row["height"].as_i64().unwrap_or_default())
        })
        .and_then(|row| row["cid"].as_str())
        .context("CompactionConfig threshold update missing")?;
    let (block, signature) = node
        .authorized_signed_block_bytes(cid, Some(user.did()))
        .await?;
    let block = defra_core::block::Block::from_dag_cbor(&block)?;
    let signature = defra_core::block::Signature::from_dag_cbor(&signature)?;
    assert_eq!(
        db::block::verify::verified_signature_signer_did(&block, &signature)
            .map_err(anyhow::Error::msg)?,
        user.did()
    );

    for phase in [
        "initial",
        "revoked",
        "unchanged",
        "rebound",
        "direct-rebound",
        "context-revoked",
        "executor-revoked",
    ] {
        match phase {
            "revoked" => {
                user_access
                    .transact_write("test.compaction.revoke", |txn| {
                        let (a, context_doc) = (&a, &context_doc);
                        Box::pin(async move {
                            txn.delete_entity_set_relationship(
                                "CompactionConfig",
                                a,
                                "selected_by",
                                "configuration",
                                context_doc,
                                "selected_by",
                            )
                            .await
                        })
                    })
                    .await?
            }
            "unchanged" => {
                publish(
                    &user_access,
                    vec![(Collection::AgentContext, context_value("compaction-a"))],
                )
                .await?
            }
            "rebound" => {
                user_access
                    .transact_write("test.compaction.restore", |txn| {
                        let (a, context_doc) = (&a, &context_doc);
                        Box::pin(async move {
                            txn.add_entity_set_relationship(
                                "CompactionConfig",
                                a,
                                "selected_by",
                                "configuration",
                                context_doc,
                                "selected_by",
                            )
                            .await
                        })
                    })
                    .await?;
                publish(
                    &user_access,
                    vec![(Collection::AgentContext, context_value("compaction-b"))],
                )
                .await?;
            }
            "direct-rebound" => {
                user_access
                    .add_actor_relationship("CompactionConfig", &a, "reader", &agent_did)
                    .await?
            }
            "context-revoked" => {
                user_access
                    .delete_actor_relationship("CompactionConfig", &a, "reader", &agent_did)
                    .await?;
                user_access
                    .transact_write("test.compaction.revoke_context", |txn| {
                        let (context_doc, agent_doc) = (&context_doc, &agent_doc);
                        Box::pin(async move {
                            txn.delete_entity_set_relationship(
                                "AgentContext",
                                context_doc,
                                "selected_by",
                                "agents",
                                agent_doc,
                                "executor",
                            )
                            .await
                        })
                    })
                    .await?;
            }
            "executor-revoked" => {
                user_access
                    .transact_write("test.compaction.restore_context", |txn| {
                        let (context_doc, agent_doc) = (&context_doc, &agent_doc);
                        Box::pin(async move {
                            txn.add_entity_set_relationship(
                                "AgentContext",
                                context_doc,
                                "selected_by",
                                "agents",
                                agent_doc,
                                "executor",
                            )
                            .await
                        })
                    })
                    .await?;
                user_access
                    .delete_actor_relationship("Agent", &agent_doc, "executor", &agent_did)
                    .await?;
            }
            _ => {}
        }
        let phase_cases = cases
            .iter()
            .filter(|case| case["phase"] == phase)
            .collect::<Vec<_>>();
        anyhow::ensure!(phase_cases.len() == 4, "incomplete Lean cases for {phase}");
        for case in phase_cases {
            let (actor, access) = if case["actor"] == "selected" {
                (selected.did(), &selected_access)
            } else {
                (sibling.did(), &sibling_access)
            };
            let doc = if case["document"] == "compaction-a" {
                &a
            } else {
                &b
            };
            let rows = access.execute(query).await?;
            let visible = rows["data"]["CompactionConfig"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["_docID"] == *doc));
            assert_eq!(
                visible,
                case["expected"]
                    .as_bool()
                    .context("visibility expectation")?,
                "{case}"
            );
            let guard = SelectedConfiguration::new(
                node.clone(),
                identity::Did::new(actor)?,
                vec![
                    SelectedConfigurationDocument {
                        collection: Collection::Agent,
                        doc_id: agent_doc.clone(),
                        logical_id: None,
                    },
                    SelectedConfigurationDocument {
                        collection: Collection::AgentContext,
                        doc_id: context_doc.clone(),
                        logical_id: None,
                    },
                    SelectedConfigurationDocument {
                        collection: Collection::Compaction,
                        doc_id: doc.clone(),
                        logical_id: None,
                    },
                ],
            )
            .check()
            .await;
            assert_eq!(
                guard.is_ok(),
                case["guard_expected"]
                    .as_bool()
                    .context("guard expectation")?,
                "{case}: {guard:?}"
            );
        }
    }
    node.shutdown().await?;
    Ok(())
}
