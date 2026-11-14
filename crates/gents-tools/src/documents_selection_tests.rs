use std::sync::Arc;

use serde_json::json;

use gents_documents::test_support::desired_state_fixtures::{
    apply_selection_changes, canonical, Change,
};
use gents_lean_vocab::lean_identity_household_cases;
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::NodeIdentity;

#[tokio::test]
async fn modeled_skill_selection_and_revocation_follow_native_source_edges() -> anyhow::Result<()> {
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
    for schema in [
        gents_protocol::schemas::AGENT,
        gents_protocol::schemas::AGENT_CONTEXT,
        gents_protocol::schemas::TOOLS,
    ] {
        node.add_collection(&schema.replace(previous_policy, &policy))
            .await?;
    }
    let skill_schema = gents_protocol::schemas::SKILL.replacen(
        "type Skill ",
        &format!("type Skill @policy(id: \"{policy}\", resource: \"configuration\") "),
        1,
    );
    node.add_collection(&skill_schema).await?;
    let access = ConfigAccess::local_as(node.clone(), identity::Did::new(owner.did())?);
    let did = gents_store::graphql::escape_graphql_string(owner.did());
    let agent = gents_store::graphql::escape_graphql_string(selected.did());
    let agent_response = access.write("test.skill.agent", &format!("mutation {{ create_Agent(input: {{node_did: \"{did}\", agent_did: \"{agent}\", creation_nonce: \"nonce\", inference_profile_id: \"profile\"}}) {{_docID}} }}")).await?;
    let agent_doc = gents_store::graphql::created_doc_id(&agent_response, "Agent")?;
    let context_response = access.write("test.skill.context", &format!("mutation {{ create_AgentContext(input: {{node_did: \"{did}\", context_id: \"context\"}}) {{_docID}} }}")).await?;
    let context_doc = gents_store::graphql::created_doc_id(&context_response, "AgentContext")?;
    let tools_response = access.write("test.skill.tools", &format!("mutation {{ create_Tools(input: {{node_did: \"{did}\", tools_id: \"tools\"}}) {{_docID}} }}")).await?;
    let tools_doc = gents_store::graphql::created_doc_id(&tools_response, "Tools")?;
    let skill_response = access.write("test.skill.create", &format!("mutation {{ create_Skill(input: {{node_did: \"{did}\", skill_id: \"skill\", name: \"skill\", description: \"test\", instructions: \"test\", enabled: true}}) {{_docID}} }}")).await?;
    let skill_doc = gents_store::graphql::created_doc_id(&skill_response, "Skill")?;
    let selected_did = identity::Did::new(selected.did())?;
    access
        .add_actor_relationship("Agent", &agent_doc, "executor", &selected_did)
        .await?;
    let changes = vec![
        Change {
            collection: Collection::Agent,
            node_did: owner.did().into(),
            doc_id: agent_doc.clone(),
            before: None,
            after: canonical(
                Collection::Agent,
                owner.did(),
                json!({"context_id":"context"}),
            ),
        },
        Change {
            collection: Collection::AgentContext,
            node_did: owner.did().into(),
            doc_id: context_doc.clone(),
            before: None,
            after: canonical(
                Collection::AgentContext,
                owner.did(),
                json!({"skill_ids":["skill"], "tools_id":"tools"}),
            ),
        },
    ];
    access
        .transact_write("test.skill.select", |txn| {
            let changes = &changes;
            Box::pin(async move {
                apply_selection_changes(
                    txn,
                    changes,
                    |collection| collection != Collection::InferenceProfile,
                    false,
                )
                .await
            })
        })
        .await?;
    for case in &model.configuration_tools_selection_cases {
        let revoked = case["revoked"].as_bool().unwrap();
        if revoked {
            access
                .transact_write("test.tools.revoke_context", |txn| {
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
        let actor = if case["actor"] == "sibling" {
            sibling.did()
        } else {
            selected.did()
        };
        let response = ConfigAccess::local_as(node.clone(), identity::Did::new(actor)?)
            .execute("{ Tools { _docID } }")
            .await?;
        let visible = response["data"]["Tools"].as_array().is_some_and(|rows| {
            rows.iter()
                .any(|row| row["_docID"].as_str() == Some(tools_doc.as_str()))
        });
        assert_eq!(
            visible,
            case["expected"].as_bool().unwrap(),
            "modeled {}",
            case["name"]
        );
        if revoked {
            access
                .transact_write("test.tools.restore_context", |txn| {
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
        }
    }
    for case in &model.configuration_skill_cases {
        let revoked = case["revoked"].as_bool().unwrap();
        if revoked {
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
        }
        let actor = if case["actor"] == "sibling" {
            sibling.did()
        } else {
            selected.did()
        };
        let response = ConfigAccess::local_as(node.clone(), identity::Did::new(actor)?)
            .execute("{ Skill { _docID } }")
            .await?;
        let visible = response["data"]["Skill"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["_docID"] == skill_doc));
        assert_eq!(
            visible,
            case["expected"].as_bool().unwrap(),
            "modeled {}",
            case["name"]
        );
        use gents_inference::resolved::{
            ResolvedConfiguration, SelectedConfiguration, SelectedConfigurationDocument,
        };
        use gents_loop::tool::Tool;
        let configuration = ResolvedConfiguration::Documents(SelectedConfiguration::new(
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
                    collection: Collection::Skill,
                    doc_id: skill_doc.clone(),
                    logical_id: Some("skill".into()),
                },
            ],
        ));
        let skill = gents_documents::skills::Skill {
            skill_id: "skill".into(),
            node_did: owner.did().into(),
            name: "skill".into(),
            description: "test".into(),
            instructions: "private skill".into(),
            source_directory: None,
            tool_refs: vec![],
            display_name: None,
            enabled: true,
        };
        let ceiling = gents_documents::skills::skill_tool_ceiling(Vec::<String>::new(), &[], false);
        let tool =
            crate::native::skills::LoadSkillTool::for_agent(vec![skill], ceiling, &configuration);
        let loaded = tool
            .call(crate::native::skills::LoadSkillArgs {
                name: "skill".into(),
            })
            .await;
        assert_eq!(
            loaded.is_ok(),
            case["tool_expected"].as_bool().unwrap(),
            "modeled tool {}",
            case["name"]
        );
        if let Ok(output) = loaded {
            assert!(output.contains("private skill"));
        }
        let catalog = tool
            .call(crate::native::skills::LoadSkillArgs {
                name: "missing".into(),
            })
            .await;
        assert_eq!(
            catalog.is_ok(),
            case["tool_expected"].as_bool().unwrap(),
            "modeled catalog {}",
            case["name"]
        );
    }
    let unrelated = vec![Change {
        collection: Collection::AgentContext,
        node_did: owner.did().into(),
        doc_id: context_doc.clone(),
        before: Some(canonical(
            Collection::AgentContext,
            owner.did(),
            json!({"skill_ids":["skill"], "display_name":"before"}),
        )),
        after: canonical(
            Collection::AgentContext,
            owner.did(),
            json!({"skill_ids":["skill"], "display_name":"after"}),
        ),
    }];
    access
        .transact_write("test.skill.unrelated", |txn| {
            let changes = &unrelated;
            Box::pin(async move {
                apply_selection_changes(
                    txn,
                    changes,
                    |collection| collection != Collection::InferenceProfile,
                    false,
                )
                .await
            })
        })
        .await?;
    let response = ConfigAccess::local_as(node.clone(), selected_did.clone())
        .execute("{ Skill { _docID } }")
        .await?;
    assert!(response["data"]["Skill"]
        .as_array()
        .is_some_and(Vec::is_empty));
    access
        .add_actor_relationship("Skill", &skill_doc, "reader", &selected_did)
        .await?;
    let removal = vec![Change {
        collection: Collection::AgentContext,
        node_did: owner.did().into(),
        doc_id: context_doc,
        before: Some(canonical(
            Collection::AgentContext,
            owner.did(),
            json!({"skill_ids":["skill"]}),
        )),
        after: canonical(
            Collection::AgentContext,
            owner.did(),
            json!({"skill_ids":null}),
        ),
    }];
    access
        .transact_write("test.skill.deselect", |txn| {
            let changes = &removal;
            Box::pin(async move {
                apply_selection_changes(
                    txn,
                    changes,
                    |collection| collection != Collection::InferenceProfile,
                    false,
                )
                .await
            })
        })
        .await?;
    let response = ConfigAccess::local_as(node.clone(), selected_did)
        .execute("{ Skill { _docID } }")
        .await?;
    assert_eq!(response["data"]["Skill"][0]["_docID"], skill_doc);
    node.shutdown().await?;
    Ok(())
}
