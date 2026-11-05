use super::*;
use crate::lean_vocab_test::{lean_identity_household_cases, LeanIdentityAction};
use crate::store::config_client::ConfigAccess;
use crate::store::graphql::{created_doc_id, escape_graphql_string};
use crate::store::identity::NodeIdentity;
use serde_json::Value;

#[tokio::test]
async fn modeled_provider_inputs_require_the_selected_agent_to_read_exact_documents(
) -> anyhow::Result<()> {
    let case = lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .expect("configuration model");
    let modeled_user = &case
        .objects
        .iter()
        .find(|object| object.id == "home")
        .unwrap()
        .owner;
    let agent = &case
        .objects
        .iter()
        .find(|object| object.id == "agent-created")
        .unwrap()
        .owner;
    let sibling = case
        .actions
        .iter()
        .find_map(|action| match action {
            LeanIdentityAction::Check { actor, .. }
                if actor != modeled_user
                    && actor != agent
                    && case.configuration_provider_cases.iter().any(|c| {
                        c["name"] == "sibling" && c["actor"].as_str() == Some(actor.as_str())
                    }) =>
            {
                Some(actor)
            }
            _ => None,
        })
        .expect("modeled sibling");
    let directory = tempfile::tempdir()?;
    let owner = crate::KeyIdentity::load_or_create(directory.path().join("node"), None)?;
    let user = owner.did();
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(user)
            .build()
            .await?,
    );
    let user_access = ConfigAccess::local_as(node.clone(), identity::Did::new(user)?);
    let actor = identity::Did::new(agent)?;
    let context = defra_core::RequestContext::default().with_identity(Some(user.to_owned()));
    let policy = node
        .add_dac_policy_with_counter_as(&context, &case.policy_yaml, 0)
        .await?;
    user_access.add_collection(&format!(
        "type Agent @policy(id: \"{}\", resource: \"agents\") {{ agent_did: String }}\ntype AgentContext @policy(id: \"{}\", resource: \"configuration\") {{ context_id: String }}\ntype Tools @policy(id: \"{}\", resource: \"configuration\") {{ tools_id: String }}",
        escape_graphql_string(&policy), escape_graphql_string(&policy), escape_graphql_string(&policy)
    )).await?;
    let create = |name: &str, field: &str, value: &str| {
        format!(
            "mutation {{ create_{name}(input: {{{field}: \"{}\"}}) {{_docID}} }}",
            escape_graphql_string(value)
        )
    };
    let mut docs = Vec::new();
    for (collection, field, value) in [
        (crate::Collection::Agent, "agent_did", "agent"),
        (crate::Collection::AgentContext, "context_id", "context"),
        (crate::Collection::Tools, "tools_id", "tools"),
        (crate::Collection::AgentContext, "context_id", "replacement"),
    ] {
        let name = collection.graphql_type();
        let response = user_access
            .write("test.provider_config.create", &create(name, field, value))
            .await?;
        docs.push(created_doc_id(&response, name)?);
    }
    user_access
        .add_actor_relationship("Agent", &docs[0], "executor", &actor)
        .await?;
    user_access
        .add_actor_relationship("AgentContext", &docs[1], "reader", &actor)
        .await?;
    user_access
        .add_actor_relationship("Tools", &docs[2], "reader", &actor)
        .await?;
    for item in &case.configuration_provider_cases {
        let revoked = item["revoke_tools"].as_bool().unwrap();
        if revoked {
            user_access
                .delete_actor_relationship("Tools", &docs[2], "reader", &actor)
                .await?;
        } else {
            user_access
                .add_actor_relationship("Tools", &docs[2], "reader", &actor)
                .await?;
        }
        let who = if item["actor"] == Value::String(sibling.clone()) {
            sibling
        } else {
            agent
        };
        let selected = vec![
            SelectedConfigurationDocument {
                collection: crate::Collection::Agent,
                doc_id: docs[0].clone(),
                logical_id: None,
            },
            SelectedConfigurationDocument {
                collection: crate::Collection::AgentContext,
                doc_id: if item["missing_context"] == true {
                    docs[3].clone()
                } else {
                    docs[1].clone()
                },
                logical_id: None,
            },
            SelectedConfigurationDocument {
                collection: crate::Collection::Tools,
                doc_id: docs[2].clone(),
                logical_id: None,
            },
        ];
        let binding = SelectedConfiguration::new(node.clone(), identity::Did::new(who)?, selected);
        let result = binding.check().await;
        assert_eq!(
            result.is_ok(),
            item["expected"].as_bool().unwrap(),
            "modeled {}: {result:?}",
            item["name"]
        );
    }
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn modeled_inference_inputs_require_each_selected_native_document() -> anyhow::Result<()> {
    let case = lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .expect("configuration model");
    let modeled_user = &case
        .objects
        .iter()
        .find(|object| object.id == "home")
        .unwrap()
        .owner;
    let agent = &case
        .objects
        .iter()
        .find(|object| object.id == "agent-created")
        .unwrap()
        .owner;
    let sibling = case
        .actions
        .iter()
        .find_map(|action| match action {
            LeanIdentityAction::Check { actor, .. } if actor != modeled_user && actor != agent => {
                Some(actor)
            }
            _ => None,
        })
        .expect("modeled sibling");
    let directory = tempfile::tempdir()?;
    let owner = crate::KeyIdentity::load_or_create(directory.path().join("node"), None)?;
    let user = owner.did();
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(user)
            .build()
            .await?,
    );
    let access = ConfigAccess::local_as(node.clone(), identity::Did::new(user)?);
    let context = defra_core::RequestContext::default().with_identity(Some(user.to_owned()));
    let policy = node
        .add_dac_policy_with_counter_as(&context, &case.policy_yaml, 0)
        .await?;
    let names = [
        "Agent",
        "InferenceProfile",
        "InferenceSampling",
        "InferenceExecution",
        "InferenceRetryPolicy",
        "CompactionConfig",
    ];
    let sdl = names
        .iter()
        .map(|name| {
            format!(
                "type {name} @policy(id: \"{}\", resource: \"{}\") {{ label: String }}",
                escape_graphql_string(&policy),
                if *name == "Agent" {
                    "agents"
                } else {
                    "configuration"
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    access.add_collection(&sdl).await?;
    let create = |name: &str, label: &str| {
        format!(
            "mutation {{ create_{name}(input: {{label: \"{}\"}}) {{_docID}} }}",
            escape_graphql_string(label)
        )
    };
    let response = access
        .write("test.inference.agent", &create("Agent", "agent"))
        .await?;
    let agent_doc = created_doc_id(&response, "Agent")?;
    access
        .add_actor_relationship("Agent", &agent_doc, "executor", &identity::Did::new(agent)?)
        .await?;
    let documents = [
        ("profile", crate::Collection::InferenceProfile),
        ("sampling", crate::Collection::InferenceSampling),
        ("execution", crate::Collection::InferenceExecution),
        ("retry", crate::Collection::InferenceRetryPolicy),
        ("compaction", crate::Collection::Compaction),
        ("summary-profile", crate::Collection::InferenceProfile),
    ];
    let modeled_documents = case.configuration_inference_provider_cases[0]["documents"]
        .as_array()
        .expect("modeled inference documents")
        .iter()
        .map(|value| value.as_str().expect("modeled document ID"))
        .collect::<Vec<_>>();
    assert_eq!(
        documents.map(|(label, _)| label).as_slice(),
        modeled_documents.as_slice()
    );
    let mut ids = Vec::new();
    for (label, collection) in documents {
        let name = collection.graphql_type();
        let response = access
            .write("test.inference.create", &create(name, label))
            .await?;
        let doc_id = created_doc_id(&response, name)?;
        access
            .transact_write("test.inference.select", |txn| {
                let (doc_id, agent_doc) = (&doc_id, &agent_doc);
                Box::pin(async move {
                    txn.add_entity_set_relationship(
                        name,
                        doc_id,
                        "selected_by",
                        "agents",
                        agent_doc,
                        "executor",
                    )
                    .await
                })
            })
            .await?;
        ids.push((label, collection, doc_id));
    }
    for item in &case.configuration_inference_provider_cases {
        let revoked = item["revoked"].as_str().unwrap();
        if let Some((_, collection, doc_id)) = ids.iter().find(|(label, _, _)| *label == revoked) {
            access
                .transact_write("test.inference.revoke", |txn| {
                    let agent_doc = &agent_doc;
                    Box::pin(async move {
                        txn.delete_entity_set_relationship(
                            collection.graphql_type(),
                            doc_id,
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
        let mut selected = vec![SelectedConfigurationDocument {
            collection: crate::Collection::Agent,
            doc_id: agent_doc.clone(),
            logical_id: None,
        }];
        selected.extend(
            ids.iter()
                .map(|(_, collection, doc_id)| SelectedConfigurationDocument {
                    collection: *collection,
                    doc_id: doc_id.clone(),
                    logical_id: None,
                }),
        );
        let who = if item["actor"] == "sibling" {
            sibling
        } else {
            agent
        };
        let result = SelectedConfiguration::new(node.clone(), identity::Did::new(who)?, selected)
            .check()
            .await;
        assert_eq!(
            result.is_ok(),
            item["expected"].as_bool().unwrap(),
            "modeled {}: {result:?}",
            item["name"]
        );
        if let Some((_, collection, doc_id)) = ids.iter().find(|(label, _, _)| *label == revoked) {
            access
                .transact_write("test.inference.restore", |txn| {
                    let agent_doc = &agent_doc;
                    Box::pin(async move {
                        txn.add_entity_set_relationship(
                            collection.graphql_type(),
                            doc_id,
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
    }
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn modeled_provider_inputs_recheck_backend_metadata_as_agent() -> anyhow::Result<()> {
    let case = lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let user = &case
        .objects
        .iter()
        .find(|object| object.id == "home")
        .unwrap()
        .owner;
    let agent = &case
        .objects
        .iter()
        .find(|object| object.id == "agent-created")
        .unwrap()
        .owner;
    let node = Arc::new(defra_node::Node::builder().build().await?);
    let access = ConfigAccess::local_as(node.clone(), identity::Did::new(user)?);
    let actor = identity::Did::new(agent)?;
    let context = defra_core::RequestContext::default().with_identity(Some(user.clone()));
    let policy = node
        .add_dac_policy_with_counter_as(&context, &case.policy_yaml, 0)
        .await?;
    access.add_collection(&format!(
        "type Agent @policy(id: \"{policy}\", resource: \"agents\") {{ agent_did: String }}\ntype InferenceProfile @policy(id: \"{policy}\", resource: \"configuration\") {{ profile_id: String }}\ntype InferenceBackendMetadata @policy(id: \"{policy}\", resource: \"configuration\") {{ backend_id: String }}"
    )).await?;
    let mut docs = Vec::new();
    for (collection, field) in [
        (crate::Collection::Agent, "agent_did"),
        (crate::Collection::InferenceProfile, "profile_id"),
        (crate::Collection::InferenceBackend, "backend_id"),
    ] {
        let name = collection.graphql_type();
        let response = access
            .write(
                "test.provider_metadata.create",
                &format!("mutation {{ create_{name}(input: {{{field}: \"value\"}}) {{_docID}} }}"),
            )
            .await?;
        docs.push(created_doc_id(&response, name)?);
    }
    access
        .add_actor_relationship("Agent", &docs[0], "executor", &actor)
        .await?;
    access
        .add_actor_relationship("InferenceProfile", &docs[1], "reader", &actor)
        .await?;
    access
        .add_actor_relationship("InferenceBackendMetadata", &docs[2], "reader", &actor)
        .await?;
    for item in &case.configuration_plugin_backend_cases {
        if item["actor"] != "agent" {
            continue;
        }
        let revoked = item["revoked"].as_str().unwrap();
        let target = match revoked {
            "profile" => Some(("InferenceProfile", &docs[1])),
            "backend" => Some(("InferenceBackendMetadata", &docs[2])),
            "" => None,
            _ => anyhow::bail!("unknown modeled revocation"),
        };
        if let Some((collection, doc)) = target {
            access
                .delete_actor_relationship(collection, doc, "reader", &actor)
                .await?;
        }
        let selection = SelectedConfiguration::new(
            node.clone(),
            actor.clone(),
            vec![
                SelectedConfigurationDocument {
                    collection: crate::Collection::Agent,
                    doc_id: docs[0].clone(),
                    logical_id: None,
                },
                SelectedConfigurationDocument {
                    collection: crate::Collection::InferenceProfile,
                    doc_id: docs[1].clone(),
                    logical_id: None,
                },
                SelectedConfigurationDocument {
                    collection: crate::Collection::InferenceBackend,
                    doc_id: docs[2].clone(),
                    logical_id: None,
                },
            ],
        );
        assert_eq!(
            selection.check().await.is_ok(),
            item["expected"].as_bool().unwrap()
        );
        if let Some((collection, doc)) = target {
            access
                .add_actor_relationship(collection, doc, "reader", &actor)
                .await?;
        }
    }
    node.shutdown().await?;
    Ok(())
}
