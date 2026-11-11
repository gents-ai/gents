use std::sync::Arc;

use anyhow::Context;
use serde_json::json;

use super::{apply_selection_changes, selected_ids, Change};
use gents_lean_vocab::lean_identity_household_cases;
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::NodeIdentity;

pub(crate) use crate::test_support::desired_state_fixtures::canonical;

#[test]
fn modeled_tools_reference_sets_include_disabled_agent_targets() {
    let model = lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .expect("configuration model");
    for case in &model.configuration_tool_reference_cases {
        let services = case["services"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| json!({"mcp_service_id":id}))
            .collect::<Vec<_>>();
        let tools = canonical(
            Collection::Tools,
            "did:key:fixture",
            json!({"agents":{"enabled":case["agents_enabled"], "target_ids":case["targets"]},
            "remote":{"services":services}, "datastore":{"datastore_tool_surface_ids":case["surfaces"]}}),
        );
        let selected = selected_ids(Collection::Tools, "did:key:fixture", Some(&tools)).unwrap();
        for collection in [
            Collection::AgentTarget,
            Collection::ToolServiceRegistry,
            Collection::DatastoreToolSurface,
        ] {
            let expected = case["expected"][collection.graphql_type()]
                .as_array()
                .unwrap()
                .iter()
                .map(|id| id.as_str().unwrap().to_owned())
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(
                selected.get(&collection).cloned().unwrap_or_default(),
                expected,
                "modeled {}",
                case["name"]
            );
        }
    }
}

#[tokio::test]
async fn signed_skill_publication_and_self_configuration_use_native_grants() -> anyhow::Result<()> {
    use crate::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    use gents_store::config_client::GraphqlEndpoint;
    let directory = tempfile::tempdir()?;
    let runtime_identity =
        gents_store::identity::KeyIdentity::load_or_create(directory.path().join("runtime"), None)?;
    let agent_binding = gents_store::identity::provision_agent_identity(&runtime_identity)?;
    let managed_agent =
        gents_store::identity::resolve_agent_identity(&runtime_identity, &agent_binding)?;
    let user =
        gents_store::identity::KeyIdentity::load_or_create(directory.path().join("user"), None)?;
    let sibling =
        gents_store::identity::KeyIdentity::load_or_create(directory.path().join("sibling"), None)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime_identity.did())
            .with_node_acp_owner(user.did().to_owned())
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .build()
            .await?,
    );
    gents_store::schema::ensure_runtime_schemas(node.as_ref()).await?;
    let runtime = runtime_identity.did().to_owned();
    let context = defra_core::RequestContext::default().with_identity(Some(user.did().to_owned()));
    for actor in [agent_binding.agent_did.as_str(), sibling.did()] {
        for permission in [
            acp::nac::NodePermission::DocumentRead,
            acp::nac::NodePermission::DocumentUpdate,
            acp::nac::NodePermission::DocumentDelete,
            acp::nac::NodePermission::CollectionGet,
        ] {
            node.add_nac_actor_relationship_as(&context, permission.as_str(), actor)
                .await?;
        }
    }
    node.add_nac_actor_relationship_as(
        &context,
        acp::nac::NodePermission::DacRelationAdd.as_str(),
        &agent_binding.agent_did,
    )
    .await?;
    // The runtime observes the bound Node through its `runtime_reader`
    // relation and its node read permission; native ACP requires both.
    for permission in [
        acp::nac::NodePermission::DocumentRead,
        acp::nac::NodePermission::CollectionGet,
    ] {
        node.add_nac_actor_relationship_as(&context, permission.as_str(), &runtime)
            .await?;
    }
    let endpoint =
        GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), user.did())
            .with_client_signing_capability(user.client_signing_capability()?)?
            .with_client_signed_agent_creates();
    let access = ConfigAccess::Graphql(endpoint);
    let input = json!({"node_did":runtime});
    let node_response = access
        .transact_write("test.skill.node", |txn| {
            let input = &input;
            Box::pin(async move {
                txn.execute_desired_state_mutation(
            "mutation($input: NodeMutationInputArg!) { create_Node(input:$input) {_docID} }",
            &json!({"input":input}), Collection::Node, true).await
            })
        })
        .await?;
    let node_doc = gents_protocol::graphql::extract_mutation_doc_id(&node_response, "Node")?;
    access
        .add_actor_relationship(
            "Node",
            &node_doc,
            "runtime_reader",
            &identity::Did::new(&runtime)?,
        )
        .await?;
    let plan = DesiredStateApplyPlan::new([
        (Collection::Agent, json!({"node_did":runtime,"agent_did":agent_binding.agent_did,
            "creation_nonce":agent_binding.creation_nonce,
            "context_id":"context","inference_profile_id":"profile"})),
        (Collection::AgentContext, json!({"node_did":runtime,"context_id":"context","skill_ids":["skill"]})),
        (Collection::Skill, json!({"node_did":runtime,"skill_id":"skill","name":"skill",
            "description":"test","instructions":"private skill","enabled":true})),
        (Collection::InferenceProfile, json!({"node_did":runtime,"profile_id":"profile","backend_id":"backend","model_name":"test-model"})),
        (Collection::InferenceBackend, json!({"node_did":runtime,"backend_id":"backend","name":"test",
            "provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:1/v1","auth":{"kind":"unauthenticated"}})),
    ].into_iter().map(|(collection,value)| DesiredStateApplyDocument {collection,add:value.clone(),update:value}).collect())?;
    access
        .transact_write("test.skill.install", |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::apply_desired_state_plan(
                    txn,
                    plan,
                    &crate::test_support::AdmitAllProfiles,
                )
                .await
            })
        })
        .await?;
    let skill = access
        .execute("{ Skill(filter:{skill_id:{_eq:\"skill\"}}) {_docID} }")
        .await?;
    let skill_doc = skill["data"]["Skill"][0]["_docID"]
        .as_str()
        .context("Skill missing")?
        .to_owned();
    let context_response = access
        .execute("{ AgentContext(filter:{context_id:{_eq:\"context\"}}) {_docID} }")
        .await?;
    let context_doc = context_response["data"]["AgentContext"][0]["_docID"]
        .as_str()
        .context("AgentContext missing")?
        .to_owned();
    let agent_did = identity::Did::new(&agent_binding.agent_did)?;
    let agent_access =
        ConfigAccess::local_managed_agent(node.clone(), agent_did.clone(), managed_agent.as_ref())?;
    let query = "{ Skill(filter:{skill_id:{_eq:\"skill\"}}) {_docID description} }";
    let visible = |value: &serde_json::Value| {
        value["data"]["Skill"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    };
    assert!(visible(&agent_access.execute(query).await?));
    assert!(!visible(
        &ConfigAccess::local_as(node.clone(), identity::Did::new(sibling.did())?)
            .execute(query)
            .await?
    ));
    let history = access
        .execute(&format!(
            "{{ _commits(docID: \"{}\") {{cid height fieldName}} }}",
            gents_store::graphql::escape_graphql_string(&skill_doc)
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
        .context("Skill genesis missing")?;
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
    let update = format!("mutation {{ update_Skill(docID: \"{}\", input: {{description:\"agent edit\"}}) {{_docID}} }}",
        gents_store::graphql::escape_graphql_string(&skill_doc));
    match agent_access.write("test.skill.denied", &update).await {
        Ok(denied) => assert!(denied["data"]["update_Skill"]
            .as_array()
            .is_some_and(Vec::is_empty)),
        Err(error) => assert!(error.is_graphql_rejection(), "{error}"),
    }
    assert_eq!(
        access.execute(query).await?["data"]["Skill"][0]["description"],
        "test"
    );
    access
        .add_actor_relationship("Skill", &skill_doc, "writer", &agent_did)
        .await?;
    let allowed = agent_access.write("test.skill.allowed", &update).await?;
    assert_eq!(
        allowed["data"]["update_Skill"].as_array().map(Vec::len),
        Some(1)
    );
    access
        .delete_actor_relationship("Skill", &skill_doc, "writer", &agent_did)
        .await?;
    let revoked_update = update.replace("agent edit", "revoked edit");
    match agent_access
        .write("test.skill.writer_revoked", &revoked_update)
        .await
    {
        Ok(denied) => assert!(denied["data"]["update_Skill"]
            .as_array()
            .is_some_and(Vec::is_empty)),
        Err(error) => assert!(error.is_graphql_rejection(), "{error}"),
    }
    assert_eq!(
        access.execute(query).await?["data"]["Skill"][0]["description"],
        "agent edit"
    );
    access
        .transact_write("test.skill.revoke", |txn| {
            let (skill_doc, context_doc) = (&skill_doc, &context_doc);
            Box::pin(async move {
                txn.delete_entity_set_relationship(
                    "Skill",
                    skill_doc,
                    "selected_by",
                    "configuration",
                    context_doc,
                    "selected_by",
                )
                .await
            })
        })
        .await?;
    assert!(!visible(&agent_access.execute(query).await?));
    let unrelated = json!({"node_did":runtime,"context_id":"context","skill_ids":["skill"],"display_name":"renamed"});
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::AgentContext,
        add: unrelated.clone(),
        update: unrelated,
    }])?;
    access
        .transact_write("test.skill.unrelated", |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::apply_desired_state_plan(
                    txn,
                    plan,
                    &crate::test_support::AdmitAllProfiles,
                )
                .await
            })
        })
        .await?;
    assert!(!visible(&agent_access.execute(query).await?));
    access
        .add_actor_relationship("Skill", &skill_doc, "reader", &agent_did)
        .await?;
    assert!(visible(&agent_access.execute(query).await?));
    let created = json!({"node_did":runtime,"skill_id":"agent-skill","name":"agent-skill",
        "description":"agent owned","instructions":"agent created","enabled":true});
    let agent_plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Skill,
        add: created.clone(),
        update: created,
    }])?;
    let binding = crate::ConfigurationNodeBinding {
        node_did: identity::Did::new(&runtime)?,
        doc_id: node_doc,
        enabled: true,
        default_agent_did: None,
    };
    agent_access
        .transact_write("test.skill.agent_create", |txn| {
            let (plan, binding) = (&agent_plan, &binding);
            Box::pin(async move {
                crate::apply_desired_state_plan_with_node_binding(
                    txn,
                    plan,
                    Some(binding),
                    &crate::test_support::AdmitAllProfiles,
                )
                .await
            })
        })
        .await?;
    let created = agent_access
        .execute("{ Skill(filter:{skill_id:{_eq:\"agent-skill\"}}) {_docID} }")
        .await?;
    let created_doc = created["data"]["Skill"][0]["_docID"]
        .as_str()
        .context("agent-created Skill missing")?
        .to_owned();
    let owners = agent_access
        .transact_readonly("test.skill.owner", |txn| {
            let created_doc = &created_doc;
            Box::pin(async move {
                txn.document_owners("Skill", std::slice::from_ref(created_doc))
                    .await
            })
        })
        .await?;
    assert_eq!(owners[0].owner.as_ref(), Some(&agent_did));
    let history = agent_access
        .execute(&format!(
            "{{ _commits(docID: \"{}\") {{cid height fieldName}} }}",
            gents_store::graphql::escape_graphql_string(&created_doc)
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
        .context("agent-created Skill genesis missing")?;
    let (block, signature) = node
        .authorized_signed_block_bytes(cid, Some(&agent_binding.agent_did))
        .await?;
    let block = defra_core::block::Block::from_dag_cbor(&block)?;
    let signature = defra_core::block::Signature::from_dag_cbor(&signature)?;
    assert_eq!(
        db::block::verify::verified_signature_signer_did(&block, &signature)
            .map_err(anyhow::Error::msg)?,
        agent_binding.agent_did
    );
    let query_created = "{ Skill(filter:{skill_id:{_eq:\"agent-skill\"}}) {_docID} }";
    assert!(visible(&access.execute(query_created).await?));
    assert!(!visible(
        &ConfigAccess::local_as(node.clone(), identity::Did::new(sibling.did())?)
            .execute(query_created)
            .await?
    ));
    let removable = json!({"node_did":runtime,"skill_id":"user-skill","name":"user-skill",
        "description":"retained on denial","instructions":"private","enabled":true});
    let user_plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Skill,
        add: removable.clone(),
        update: removable,
    }])?;
    access
        .transact_write("test.skill.create_removal_target", |txn| {
            let plan = &user_plan;
            Box::pin(async move {
                crate::apply_desired_state_plan(
                    txn,
                    plan,
                    &crate::test_support::AdmitAllProfiles,
                )
                .await
            })
        })
        .await?;
    let query_removable = "{ Skill(filter:{skill_id:{_eq:\"user-skill\"}}) {_docID description} }";
    let before = access.execute(query_removable).await?;
    let removable_doc = before["data"]["Skill"][0]["_docID"]
        .as_str()
        .context("user Skill missing")?;
    access
        .add_actor_relationship("Skill", removable_doc, "reader", &agent_did)
        .await?;
    let removal = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
        Collection::Skill,
        runtime.clone(),
        "user-skill".into(),
    )])?;
    let result = agent_access
        .transact_write("test.skill.denied_removal", |txn| {
            let (plan, binding) = (&removal, &binding);
            Box::pin(async move {
                crate::apply_desired_state_plan_with_node_binding(
                    txn,
                    plan,
                    Some(binding),
                    &crate::test_support::AdmitAllProfiles,
                )
                .await
            })
        })
        .await;
    assert!(result.is_err(), "reader cannot delete the user's Skill");
    assert_eq!(access.execute(query_removable).await?, before);
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn modeled_inference_selection_changes_stage_shared_native_edges() -> anyhow::Result<()> {
    let model = lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .expect("configuration model");
    let dir = tempfile::tempdir()?;
    let owner = gents_store::identity::KeyIdentity::load_or_create(dir.path().join("node"), None)?;
    let selected =
        gents_store::identity::KeyIdentity::load_or_create(dir.path().join("selected"), None)?;
    let sibling =
        gents_store::identity::KeyIdentity::load_or_create(dir.path().join("sibling"), None)?;
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(owner.did())
            .with_node_acp_owner(owner.did().to_owned())
            .build()
            .await?,
    );
    let ctx = defra_core::RequestContext::default().with_identity(Some(owner.did().to_owned()));
    let policy = node
        .add_dac_policy_with_counter_as(&ctx, &model.policy_yaml, 0)
        .await?;
    for actor in [selected.did(), sibling.did()] {
        for permission in [
            acp::nac::NodePermission::DocumentRead,
            acp::nac::NodePermission::CollectionGet,
        ] {
            node.add_nac_actor_relationship_as(&ctx, permission.as_str(), actor)
                .await?;
        }
    }
    let previous_policy = gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID;
    node.add_collection(&gents_protocol::schemas::AGENT.replace(previous_policy, &policy))
        .await?;
    for schema in [
        gents_protocol::schemas::INFERENCE_PROFILE,
        gents_protocol::schemas::INFERENCE_SAMPLING,
        gents_protocol::schemas::INFERENCE_EXECUTION,
        gents_protocol::schemas::INFERENCE_RETRY_POLICY,
    ] {
        assert!(schema.contains(previous_policy));
        node.add_collection(&schema.replace(previous_policy, &policy))
            .await?;
    }
    let access = ConfigAccess::local_as(node.clone(), identity::Did::new(owner.did())?);
    let did = gents_store::graphql::escape_graphql_string(owner.did());
    let agent = gents_store::graphql::escape_graphql_string(selected.did());
    let create = |name: &str, fields: &str| {
        format!("mutation {{ create_{name}(input: {{{fields}}}) {{_docID}} }}")
    };
    let agent_doc = gents_store::graphql::created_doc_id(&access.write("test.inference.agent", &create("Agent",
        &format!("node_did: \"{did}\", agent_did: \"{agent}\", creation_nonce: \"nonce\", inference_profile_id: \"profile\""))).await?, "Agent")?;
    let profile_doc = gents_store::graphql::created_doc_id(&access.write("test.inference.profile", &create("InferenceProfile",
        &format!("node_did: \"{did}\", profile_id: \"profile\", backend_id: \"backend\", model_name: \"model\""))).await?, "InferenceProfile")?;
    let sampling_doc = gents_store::graphql::created_doc_id(
        &access
            .write(
                "test.inference.sampling",
                &create(
                    "InferenceSampling",
                    &format!("node_did: \"{did}\", sampling_id: \"sampling\""),
                ),
            )
            .await?,
        "InferenceSampling",
    )?;
    let execution_doc = gents_store::graphql::created_doc_id(
        &access
            .write(
                "test.inference.execution",
                &create(
                    "InferenceExecution",
                    &format!("node_did: \"{did}\", execution_id: \"execution\""),
                ),
            )
            .await?,
        "InferenceExecution",
    )?;
    let retry_doc = gents_store::graphql::created_doc_id(
        &access
            .write(
                "test.inference.retry",
                &create(
                    "InferenceRetryPolicy",
                    &format!("node_did: \"{did}\", retry_policy_id: \"retry\""),
                ),
            )
            .await?,
        "InferenceRetryPolicy",
    )?;
    access
        .add_actor_relationship(
            "Agent",
            &agent_doc,
            "executor",
            &identity::Did::new(selected.did())?,
        )
        .await?;
    let changes = vec![
        Change {
            collection: Collection::Agent,
            node_did: owner.did().into(),
            doc_id: agent_doc,
            before: None,
            after: canonical(
                Collection::Agent,
                owner.did(),
                json!({"inference_profile_id":"profile"}),
            ),
        },
        Change {
            collection: Collection::InferenceProfile,
            node_did: owner.did().into(),
            doc_id: profile_doc.clone(),
            before: None,
            after: canonical(
                Collection::InferenceProfile,
                owner.did(),
                json!({"sampling_id":"sampling", "execution_id":"execution"}),
            ),
        },
        Change {
            collection: Collection::InferenceExecution,
            node_did: owner.did().into(),
            doc_id: execution_doc,
            before: None,
            after: canonical(
                Collection::InferenceExecution,
                owner.did(),
                json!({"retry_policy_id":"retry"}),
            ),
        },
    ];
    access
        .transact_write("test.inference.select", |txn| {
            let changes = &changes;
            Box::pin(async move {
                apply_selection_changes(
                    txn,
                    changes,
                    |collection| collection != Collection::InferenceBackend,
                    false,
                )
                .await
            })
        })
        .await?;
    for case in model
        .configuration_userset_cases
        .iter()
        .filter(|case| case["document"] == "retry")
    {
        if case["revoked"] == "execution" {
            let change = vec![Change {
                collection: Collection::InferenceProfile,
                node_did: owner.did().into(),
                doc_id: profile_doc.clone(),
                before: Some(canonical(
                    Collection::InferenceProfile,
                    owner.did(),
                    json!({"sampling_id":"sampling", "execution_id":"execution"}),
                )),
                after: canonical(
                    Collection::InferenceProfile,
                    owner.did(),
                    json!({"sampling_id":"sampling", "execution_id":null}),
                ),
            }];
            access
                .transact_write("test.inference.remove_execution", |txn| {
                    let change = &change;
                    Box::pin(async move {
                        apply_selection_changes(
                            txn,
                            change,
                            |collection| collection != Collection::InferenceBackend,
                            false,
                        )
                        .await
                    })
                })
                .await?;
        }
        let actor = if case["actor"] == "sibling" {
            sibling.did()
        } else {
            selected.did()
        };
        let response = ConfigAccess::local_as(node.clone(), identity::Did::new(actor)?)
            .execute("{ InferenceRetryPolicy { _docID } }")
            .await?;
        let visible = response["data"]["InferenceRetryPolicy"]
            .as_array()
            .is_some_and(|rows| {
                rows.iter()
                    .any(|row| row["_docID"].as_str() == Some(retry_doc.as_str()))
            });
        assert_eq!(
            visible,
            case["expected"].as_bool().unwrap(),
            "modeled {}",
            case["name"]
        );
    }
    let response = ConfigAccess::local_as(node.clone(), identity::Did::new(selected.did())?)
        .execute("{ InferenceSampling { _docID } }")
        .await?;
    assert!(response["data"]["InferenceSampling"]
        .as_array()
        .is_some_and(|rows| rows
            .iter()
            .any(|row| row["_docID"].as_str() == Some(sampling_doc.as_str()))));
    node.shutdown().await?;
    Ok(())
}
