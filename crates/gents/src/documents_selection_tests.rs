mod agent_target_access {
    use std::collections::BTreeMap;

    use serde_json::{json, Value};

    use gents_documents::{
        read_desired_state_record_in_txn, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    use gents_store::collection::Collection;
    use gents_store::config_client::ConfigAccess;
    use gents_store::identity::NodeIdentity as _;

    async fn apply(
        access: &ConfigAccess,
        documents: Vec<(Collection, Value)>,
    ) -> Result<(), String> {
        let plan = DesiredStateApplyPlan::new(
            documents
                .into_iter()
                .map(|(collection, value)| DesiredStateApplyDocument {
                    collection,
                    add: value.clone(),
                    update: value,
                })
                .collect(),
        )
        .unwrap();
        access
            .transact_write("test.agent_target_access.apply", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    gents_documents::apply_desired_state_plan(
                        txn,
                        plan,
                        &gents_documents::test_support::AdmitAllProfiles,
                    )
                    .await
                })
            })
            .await
            .map(drop)
            .map_err(|error: anyhow::Error| format!("{error:#}"))
    }

    /// Lean `Identity.AgentTargetAccess`: each modeled configuration apply,
    /// published through the desired-state path by the modeled actor, commits
    /// exactly when the model applies it, and the native `invoke` decision
    /// behind `agent_new` and a plugin job's child spawn then matches the model
    /// for every caller and target.
    #[tokio::test]
    async fn configured_agent_targets_grant_exactly_session_writer() {
        let (db, user, user_access) =
            crate::test_support::root::signed_test_node_without_agent("agent-target-access").await;
        let runtime = db.node_identity.clone();
        let node_did = runtime.did().to_owned();
        let user_did = identity::Did::new(user.did()).unwrap();
        let remote_node = tokio::task::spawn_blocking(|| {
            gents_store::test_support::test_identity("agent-target-access-remote")
        })
        .await
        .unwrap();
        let mut bindings = BTreeMap::new();
        for name in ["lead", "reviewer", "other"] {
            let binding = gents_store::identity::provision_agent_identity_authorized(
                &db.node,
                runtime.as_ref(),
                &user_did,
            )
            .await
            .unwrap();
            bindings.insert(name, binding);
        }
        let did = |name: &str| bindings[name].agent_did.clone();
        let lead_tools =
            |input: &gents_lean_vocab::families::agent_target_access_access::AccessInput| {
                let mut agents = json!({ "enabled": input.enabled });
                if !input.target_ids.is_empty() {
                    agents["target_ids"] = json!(input.target_ids);
                }
                let mut tools = json!({"node_did": node_did, "tools_id": format!("{}:tools", did("lead")), "agents": agents});
                if !input.job_target_ids.is_empty() {
                    tools["integrations"] = json!({"plugins": [{
                        "plugin": "acme/jobs",
                        "job": {"agent_targets": input.job_target_ids},
                    }]});
                }
                (Collection::Tools, tools)
            };
        let mut setup: Vec<_> = bindings
            .iter()
            .flat_map(|(name, binding)| {
                gents_documents::test_support::test_agent_documents(&node_did, name, binding)
            })
            .collect();
        setup.extend([
            (
                Collection::AgentTarget,
                json!({"node_did": node_did, "target_id": "reviewer-target",
                       "target_node_did": node_did, "agent_did": did("reviewer"), "name": "reviewer"}),
            ),
            (
                Collection::AgentTarget,
                json!({"node_did": node_did, "target_id": "remote-target",
                       "target_node_did": remote_node.did(), "agent_did": did("other"), "name": "other"}),
            ),
        ]);
        let lead_key =
            gents_store::identity::resolve_agent_identity(runtime.as_ref(), &bindings["lead"])
                .unwrap();
        let lead_access = ConfigAccess::local_managed_agent(
            db.node.clone(),
            identity::Did::new(did("lead")).unwrap(),
            lead_key.as_ref(),
        )
        .unwrap();
        let observer = ConfigAccess::local_as(
            db.node.clone(),
            identity::Did::new(node_did.clone()).unwrap(),
        );
        let cases = gents_lean_vocab::families::agent_target_access_access::cases();
        let pairs = [
            ("lead", "reviewer"),
            ("lead", "other"),
            ("other", "reviewer"),
            ("reviewer", "lead"),
        ];
        for (index, case) in cases.iter().enumerate() {
            let input = &case.input;
            let access = match input.configurer.as_str() {
                "user" => &user_access,
                "lead" => &lead_access,
                other => panic!("unmodeled configurer {other}"),
            };
            let modeled = lead_tools(input);
            let mut documents = if index == 0 {
                setup.clone()
            } else {
                Vec::new()
            };
            // The modeled selection replaces the fixture's empty lead Tools.
            documents.retain(|(collection, value)| {
                !(*collection == Collection::Tools && value["tools_id"] == modeled.1["tools_id"])
            });
            documents.push(modeled);
            let outcome = apply(access, documents).await;
            let mut invokes = Vec::new();
            for (caller_name, target_name) in pairs {
                let caller = did(caller_name);
                let target = did(target_name);
                let doc = user_access
                    .transact_readonly("test.agent_target_access.target", |txn| {
                        let (node_did, target) = (&node_did, &target);
                        Box::pin(async move {
                            read_desired_state_record_in_txn(
                                txn,
                                Collection::Agent,
                                node_did,
                                target,
                            )
                            .await
                        })
                    })
                    .await
                    .unwrap()
                    .expect("target Agent")
                    .0;
                // The User's access observes over HTTP; native permission
                // observation runs on the embedded node.
                let decision = observer
                    .transact_readonly("test.agent_target_access.invoke", |txn| {
                        let (caller, doc) = (&caller, &doc);
                        Box::pin(async move {
                            txn.observe_document_access(
                                &identity::Did::new(caller)?,
                                "Agent",
                                std::slice::from_ref(doc),
                                defra_node::DocumentAccessMode::Named {
                                    permission: "invoke".into(),
                                    node_permission: acp::nac::NodePermission::DocumentUpdate,
                                },
                            )
                            .await
                            .map_err(anyhow::Error::from)
                        })
                    })
                    .await
                    .unwrap();
                invokes.push(
                    gents_lean_vocab::families::agent_target_access_access::InvokeRow {
                        caller: caller_name.to_owned(),
                        target: target_name.to_owned(),
                        invoke: matches!(
                            decision.as_slice(),
                            [defra_node::DocumentAccessDecision::Allowed]
                        ),
                    },
                );
            }
            case.check(
                gents_lean_vocab::families::agent_target_access_access::AccessOutput {
                    applied: outcome.is_ok(),
                    invokes,
                },
            );
        }
    }
}

mod self_config_inference {
    use anyhow::{Context, Result};
    use serde_json::Value;

    use gents_store::collection::Collection;
    use gents_store::config_client::ConfigAccess;
    use gents_store::identity::NodeIdentity;

    fn updated(response: &Value, field: &str, doc_id: &str) -> Result<bool> {
        let rows = gents_store::graphql::mutation_documents(response, field)?
            .context("mutation response missing update field")?;
        match rows {
            [] => Ok(false),
            [row] => {
                anyhow::ensure!(
                    row["_docID"] == doc_id,
                    "mutation returned a different document"
                );
                Ok(true)
            }
            _ => anyhow::bail!("mutation returned multiple documents"),
        }
    }

    #[tokio::test]
    async fn user_grants_selected_backend_manager_without_backend_write() -> Result<()> {
        let (db, user, owner, binding) =
            crate::test_support::root::signed_test_node("backend-selection-manager").await;
        let runtime = db.node_identity.did().to_owned();
        let agent_identity =
            gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)?;
        let actor = identity::Did::new(agent_identity.did())?;
        let agent = ConfigAccess::local_managed_agent(
            db.node.clone(),
            actor.clone(),
            agent_identity.as_ref(),
        )?;
        let backend_id = format!("{}:backend", binding.agent_did);
        let backend_query = format!(
            r#"{{ InferenceBackendMetadata(filter: {{backend_id:{{_eq:"{}"}}}}) {{_docID name}} }}"#,
            gents_store::graphql::escape_graphql_string(&backend_id)
        );
        let before = owner.execute(&backend_query).await?;
        let backend = before["data"]["InferenceBackendMetadata"][0]["_docID"]
            .as_str()
            .context("User-owned backend missing")?
            .to_owned();
        let owners = owner
            .transact_readonly("test.backend.owner", |txn| {
                let backend = &backend;
                Box::pin(async move {
                    txn.document_owners("InferenceBackendMetadata", std::slice::from_ref(backend))
                        .await
                })
            })
            .await?;
        assert_eq!(
            owners[0].owner.as_ref().map(|did| did.as_str()),
            Some(user.did())
        );
        let input = serde_json::json!({"node_did":runtime,"profile_id":"other-profile","backend_id":backend_id,"model_name":"test-model"});
        let created = owner.transact_write("test.backend.other_profile", |txn| {
            let input = &input;
            Box::pin(async move { txn.execute_client_signed_create(
                "mutation($input: InferenceProfileMutationInputArg!) { create_InferenceProfile(input:$input) {_docID} }",
                &serde_json::json!({"input":input}), gents_protocol::schemas::INFERENCE_PROFILE,
            ).await })
        }).await?;
        let profile = gents_store::graphql::created_doc_id(&created, "InferenceProfile")?;
        let mut linked = std::collections::BTreeMap::new();
        let link = || {
            agent.transact_write("test.backend.selected_by", |txn| {
                let backend = &backend;
                let profile = &profile;
                Box::pin(async move {
                    txn.add_entity_set_relationship(
                        "InferenceBackendMetadata",
                        backend,
                        "selected_by",
                        "configuration",
                        profile,
                        "selected_by",
                    )
                    .await
                })
            })
        };
        linked.insert("before", link().await.is_ok());
        owner
            .transact_write("test.backend.grant", |txn| {
                let runtime = &runtime;
                let agent_did = &binding.agent_did;
                Box::pin(async move {
                    gents_documents::grant_selected_self_configuration(txn, runtime, agent_did)
                        .await
                })
            })
            .await?;
        linked.insert("granted", link().await.is_ok());
        let node_rows = owner.execute("{ Node {_docID node_did} }").await?;
        let node_doc = node_rows["data"]["Node"]
            .as_array()
            .context("Node rows")?
            .iter()
            .find(|row| row["node_did"] == runtime)
            .and_then(|row| row["_docID"].as_str())
            .context("User-owned Node missing")?;
        let node_binding = gents_documents::ConfigurationNodeBinding {
            node_did: identity::Did::new(&runtime)?,
            doc_id: node_doc.to_owned(),
            enabled: true,
            default_agent_did: Some(binding.agent_did.clone()),
        };
        let value = serde_json::json!({"node_did":runtime,"profile_id":"fresh-profile",
            "backend_id":backend_id,"model_name":"test-model"});
        let plan = gents_documents::DesiredStateApplyPlan::new(vec![
            gents_documents::DesiredStateApplyDocument {
                collection: Collection::InferenceProfile,
                add: value.clone(),
                update: value,
            },
        ])?;
        agent
            .transact_write("test.backend.create_profile", |txn| {
                let (plan, node_binding) = (&plan, &node_binding);
                Box::pin(async move {
                    gents_documents::apply_desired_state_plan_with_node_binding(
                        txn,
                        plan,
                        Some(node_binding),
                        &gents_documents::test_support::AdmitAllProfiles,
                    )
                    .await
                })
            })
            .await?;
        let created = agent
            .execute(
                r#"{ InferenceProfile(filter: {profile_id: {_eq: "fresh-profile"}}) {_docID} }"#,
            )
            .await?;
        let created_doc = created["data"]["InferenceProfile"][0]["_docID"]
            .as_str()
            .context("Agent-created profile missing")?
            .to_owned();
        let created_owners = agent
            .transact_readonly("test.backend.profile_owner", |txn| {
                let created_doc = &created_doc;
                Box::pin(async move {
                    txn.document_owners("InferenceProfile", std::slice::from_ref(created_doc))
                        .await
                })
            })
            .await?;
        assert_eq!(created_owners[0].owner.as_ref(), Some(&actor));
        let mutation = format!(
            r#"mutation {{ update_InferenceBackendMetadata(docID:"{}",input:{{name:"forged"}}) {{_docID}} }}"#,
            gents_store::graphql::escape_graphql_string(&backend)
        );
        match agent.write("test.backend.write_denied", &mutation).await {
            Ok(response) => assert!(!updated(
                &response,
                "update_InferenceBackendMetadata",
                &backend
            )?),
            Err(error) => assert!(error.is_graphql_rejection(), "{error}"),
        }
        assert_eq!(owner.execute(&backend_query).await?, before);
        owner
            .set_agent_self_config_permission(
                &runtime,
                &binding.agent_did,
                gents_store::config_client::AgentSelfConfigPermission::Revoke,
            )
            .await?;
        linked.insert("revoked", link().await.is_ok());
        for case in
            gents_lean_vocab::families::household_inference_self_config_backend_manager::cases()
        {
            anyhow::ensure!(
                case.input.actor == "agent" && case.input.document == "backend-metadata",
                "unmodeled backend link {:?}",
                case.input
            );
            let observed = linked[case.input.phase.as_str()];
            case.check(observed);
        }
        db.node.shutdown().await?;
        Ok(())
    }
}
