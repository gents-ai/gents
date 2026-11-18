use acp::{DocumentPermission, Identity};
use identity::Did;

#[tokio::test]
async fn modeled_configuration_grants_use_authenticated_native_relations() -> anyhow::Result<()> {
    use gents_documents::apply_desired_state_plan;
    use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    use gents_store::config_client::NodePermission;
    use gents_store::graphql::{created_doc_id, escape_graphql_string};
    use gents_store::identity::NodeIdentity;

    let traces = gents_lean_vocab::families::household_configuration_grant_trace::cases();
    let trace = traces.first().expect("modeled configuration trace");
    let role = |modeled: &str| -> anyhow::Result<&str> {
        trace
            .input
            .roles
            .iter()
            .find(|role| role.actor == modeled)
            .map(|role| role.role.as_str())
            .ok_or_else(|| anyhow::anyhow!("unknown modeled actor {modeled}"))
    };
    let db = crate::support::test_db("modeled-configuration-native").await;
    let fixture = crate::support::managed_agent_fixture(&db).await;
    let node = db.node.clone();
    let node_did = db.node_identity.did();
    let user_access = fixture.access.clone();
    let agent_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        Did::new(fixture.agent.did())?,
        fixture.agent.as_ref(),
    )?;
    let sibling_binding = gents_store::identity::provision_agent_identity_authorized(
        &node,
        db.node_identity.as_ref(),
        &Did::new(fixture.user.did())?,
    )
    .await?;
    let sibling_signer =
        gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &sibling_binding)?;
    let sibling_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        Did::new(sibling_signer.did())?,
        sibling_signer.as_ref(),
    )?;
    let runtime_access =
        gents_store::config_client::ConfigAccess::local_as(node.clone(), Did::new(node_did)?);
    for actual in [
        fixture.user.did(),
        fixture.agent.did(),
        sibling_signer.did(),
    ] {
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::CollectionGet,
            NodePermission::DacRelationAdd,
            NodePermission::DacRelationDelete,
        ] {
            runtime_access
                .grant_node_permission(&Did::new(actual)?, permission)
                .await?;
        }
    }
    let actual_actor = |modeled: &str| -> anyhow::Result<Did> {
        let actual = match role(modeled)? {
            "user" => fixture.user.did(),
            "agent" => fixture.agent.did(),
            "sibling" => sibling_signer.did(),
            "runtime" => node_did,
            other => anyhow::bail!("unmodeled configuration actor {other}"),
        };
        Ok(Did::new(actual)?)
    };
    let access_for = |modeled: &str| -> anyhow::Result<gents_store::config_client::ConfigAccess> {
        match role(modeled)? {
            "user" => Ok(user_access.clone()),
            "agent" => Ok(agent_access.clone()),
            "sibling" => Ok(sibling_access.clone()),
            "runtime" => Ok(runtime_access.clone()),
            other => anyhow::bail!("unmodeled configuration actor {other}"),
        }
    };
    let selected =
        serde_json::json!({"node_did":node_did,"tools_id":"selected", "display_name":"initial"});
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: gents_store::collection::Collection::Tools,
        add: selected.clone(),
        update: selected,
    }])?;
    user_access
        .transact_write("test.configuration.selected", |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await?;
    let query = |id: &str| {
        format!(
        "{{ Tools(filter: {{node_did: {{_eq: \"{}\"}}, tools_id: {{_eq: \"{}\"}}}}) {{_docID display_name}} }}",
        escape_graphql_string(node_did), escape_graphql_string(id),
    )
    };
    let selected_doc = user_access.execute(&query("selected")).await?["data"]["Tools"][0]["_docID"]
        .as_str()
        .expect("selected Tools physical ID")
        .to_owned();
    let created = agent_access.write("test.configuration.agent_owned", &format!(
        "mutation {{ create_Tools(input: {{node_did: \"{}\", tools_id: \"agent-created\", display_name: \"initial\"}}) {{_docID}} }}",
        escape_graphql_string(node_did),
    )).await?;
    let agent_doc = created_doc_id(&created, "Tools")?;
    let node_doc = user_access
        .transact_readonly("test.configuration.node", |txn| {
            Box::pin(async move {
                Ok::<_, anyhow::Error>(
                    gents_documents::read_desired_state_record_in_txn(
                        txn,
                        gents_store::collection::Collection::Node,
                        node_did,
                        node_did,
                    )
                    .await?
                    .expect("Node physical ID")
                    .0,
                )
            })
        })
        .await?;
    agent_access
        .add_resource_relationship("Tools", &agent_doc, "parent", "nodes", &node_doc)
        .await?;

    let mut outcomes = Vec::new();
    for (step, action) in trace.input.steps.iter().enumerate() {
        let outcome = match (action.action.as_str(), action.permission.as_deref()) {
            ("grant" | "revoke", _) => {
                let relation = action.relation.as_ref().expect("relationship step");
                let actor =
                    relation.subject.actor.as_deref().ok_or_else(|| {
                        anyhow::anyhow!("configuration relation must name an actor")
                    })?;
                let actual = actual_actor(actor)?;
                if action.action == "grant" {
                    user_access
                        .add_actor_relationship("Tools", &selected_doc, &relation.relation, &actual)
                        .await?;
                    if relation.relation == "reader" {
                        assert!(agent_access
                            .add_actor_relationship("Tools", &selected_doc, "writer", &actual)
                            .await
                            .is_err());
                    }
                } else {
                    user_access
                        .delete_actor_relationship(
                            "Tools",
                            &selected_doc,
                            &relation.relation,
                            &actual,
                        )
                        .await?;
                }
                None
            }
            ("check", Some("read")) => {
                let actor = action.actor.as_deref().expect("checking actor");
                let id = action.id.as_deref().expect("checked document");
                let response = access_for(actor)?.execute(&query(id)).await?;
                Some(!response["data"]["Tools"].as_array().unwrap().is_empty())
            }
            ("check", Some("update")) => {
                let actor = action.actor.as_deref().expect("checking actor");
                let id = action.id.as_deref().expect("checked document");
                let access = access_for(actor)?;
                let doc_id = if id == "selected" {
                    &selected_doc
                } else {
                    &agent_doc
                };
                let before = user_access.execute(&query(id)).await?["data"]["Tools"][0]
                    ["display_name"]
                    .clone();
                let value = format!("update-{step}");
                let updated = if role(actor)? == "user" {
                    let document =
                        serde_json::json!({"node_did":node_did,"tools_id":id,"display_name":value});
                    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                        collection: gents_store::collection::Collection::Tools,
                        add: document.clone(),
                        update: document,
                    }])?;
                    access
                        .transact_write("test.configuration.update", |txn| {
                            let plan = &plan;
                            Box::pin(async move {
                                apply_desired_state_plan(
                                    txn,
                                    plan,
                                    &gents_inference::resolved::ProviderProfilePolicy,
                                )
                                .await
                            })
                        })
                        .await
                        .is_ok()
                } else {
                    match access.write("test.configuration.update", &format!(
                        "mutation {{ update_Tools(docID: \"{}\", input: {{display_name: \"{}\"}}) {{_docID}} }}",
                        escape_graphql_string(doc_id), escape_graphql_string(&value),
                    )).await {
                        Ok(response) => !response["data"]["update_Tools"].as_array().expect("update rows").is_empty(),
                        Err(error) if error.is_graphql_rejection() => false,
                        Err(error) => return Err(error.into()),
                    }
                };
                let after = user_access.execute(&query(id)).await?["data"]["Tools"][0]
                    ["display_name"]
                    .clone();
                assert_eq!(
                    after,
                    if updated {
                        serde_json::json!(value)
                    } else {
                        before
                    },
                    "modeled {actor} update effect on {id}"
                );
                Some(updated)
            }
            ("check", Some("delete")) => {
                let actor = action.actor.as_deref().expect("checking actor");
                let id = action.id.as_deref().expect("checked document");
                let doc_id = if id == "selected" {
                    &selected_doc
                } else {
                    &agent_doc
                };
                Some(
                    node.document_acp()
                        .check_doc_access(
                            &Identity::Authenticated(actual_actor(actor)?),
                            DocumentPermission::Delete,
                            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
                            "configuration",
                            doc_id,
                        )
                        .await?,
                )
            }
            other => anyhow::bail!("unmodeled configuration step {other:?}"),
        };
        outcomes.push(outcome);
    }
    trace.check(outcomes);
    let actual_agent = Did::new(fixture.agent.did())?;
    for observation in gents_lean_vocab::families::household_configuration_atomic::cases() {
        let commit = observation.input.commit;
        let document = serde_json::json!({"node_did":node_did,"tools_id":"atomic"});
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: gents_store::collection::Collection::Tools,
            add: document.clone(),
            update: document,
        }])?;
        let result = user_access
            .transact_write("test.configuration.atomic", |txn| {
                let plan = &plan;
                let actual_agent = &actual_agent;
                Box::pin(async move {
                    apply_desired_state_plan(
                        txn,
                        plan,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await?;
                    let (doc, _) = gents_documents::read_desired_state_record_in_txn(
                        txn,
                        gents_store::collection::Collection::Tools,
                        node_did,
                        "atomic",
                    )
                    .await?
                    .expect("atomic Tools physical ID");
                    txn.add_actor_relationship("Tools", &doc, "reader", actual_agent)
                        .await?;
                    if !commit {
                        anyhow::bail!("modeled abort");
                    }
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await;
        assert_eq!(
            result.is_ok(),
            commit,
            "modeled transaction outcome: {result:?}"
        );
        let reads = |access: &gents_store::config_client::ConfigAccess| {
            let access = access.clone();
            let query = query("atomic");
            async move {
                let rows = access.execute(&query).await?;
                Ok::<_, anyhow::Error>(!rows["data"]["Tools"].as_array().unwrap().is_empty())
            }
        };
        observation.check(
            gents_lean_vocab::families::household_configuration_atomic::AtomicReads {
                owner_read: reads(&user_access).await?,
                agent_read: reads(&agent_access).await?,
                sibling_read: reads(&sibling_access).await?,
            },
        );
    }
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn modeled_self_config_grants_use_authenticated_native_relations() -> anyhow::Result<()> {
    use anyhow::Context;
    use gents_store::config_client::AgentSelfConfigPermission;
    use gents_store::identity::NodeIdentity;

    let traces = gents_lean_vocab::families::household_self_config_trace::cases();
    let trace = traces.first().expect("modeled self-config trace");
    let db = crate::support::test_db("modeled-self-config-native").await;
    let node = db.node.clone();
    let runtime = db.node_identity.clone();
    let fixture = crate::support::managed_agent_fixture(&db).await;
    let user = &fixture.user;
    let user_access = fixture.access.clone();
    let agent = &fixture.binding;
    let agent_signer = fixture.agent.clone();
    let sibling = gents_store::identity::provision_agent_identity_authorized(
        &node,
        runtime.as_ref(),
        &Did::new(user.did())?,
    )
    .await?;
    let sibling_signer = gents_store::identity::resolve_agent_identity(runtime.as_ref(), &sibling)?;
    let node_admin =
        gents_store::config_client::ConfigAccess::local_as(node.clone(), Did::new(runtime.did())?);
    for actor in [user.did(), agent_signer.did(), sibling_signer.did()] {
        for permission in [
            gents_store::config_client::NodePermission::DocumentRead,
            gents_store::config_client::NodePermission::DocumentUpdate,
            gents_store::config_client::NodePermission::CollectionGet,
            gents_store::config_client::NodePermission::DacRelationAdd,
            gents_store::config_client::NodePermission::DacRelationDelete,
        ] {
            node_admin
                .grant_node_permission(&Did::new(actor)?, permission)
                .await?;
        }
    }
    let agent_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        Did::new(agent_signer.did())?,
        agent_signer.as_ref(),
    )?;
    let sibling_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        Did::new(sibling_signer.did())?,
        sibling_signer.as_ref(),
    )?;
    let (agent_doc, agent_value) = user_access
        .transact_readonly("test.self_config.agent", |txn| {
            let runtime = &runtime;
            Box::pin(async move {
                gents_documents::read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Agent,
                    runtime.did(),
                    &agent.agent_did,
                )
                .await?
                .context("modeled Agent is unavailable")
            })
        })
        .await?;
    let access_for = |did: &str| -> anyhow::Result<gents_store::config_client::ConfigAccess> {
        if did == user.did() {
            Ok(user_access.clone())
        } else if did == agent_signer.did() {
            Ok(agent_access.clone())
        } else if did == sibling_signer.did() {
            Ok(sibling_access.clone())
        } else {
            anyhow::bail!("unknown native household actor {did}")
        }
    };
    let actor = |modeled: &str| -> anyhow::Result<Did> {
        let role = trace
            .input
            .roles
            .iter()
            .find(|role| role.actor == modeled)
            .map(|role| role.role.as_str());
        let did = match role {
            Some("user") => user.did(),
            Some("agent") => &agent.agent_did,
            Some("sibling") => &sibling.agent_did,
            _ => anyhow::bail!("unknown modeled actor {modeled}"),
        };
        Ok(Did::new(did)?)
    };
    for observation in gents_lean_vocab::families::household_self_config_lookup::cases() {
        let attempt = &observation.input;
        let (caller, requested): (&str, &str) = match (
            attempt.present,
            attempt.visible,
            attempt.same_generation,
            attempt.native_allows,
        ) {
            (false, false, _, _) => (user.did(), &sibling.agent_did),
            (true, false, _, _) => (&sibling.agent_did, &agent.agent_did),
            (true, true, true, false) => (&agent.agent_did, &agent.agent_did),
            (true, true, true, true) => (user.did(), &agent.agent_did),
            _ => anyhow::bail!("unrepresentable modeled lookup case {}", attempt.name),
        };
        let access = access_for(caller)?;
        let outcome = match access
            .set_agent_self_config_permission(
                runtime.did(),
                requested,
                AgentSelfConfigPermission::Grant,
            )
            .await
        {
            Ok(()) => "applied",
            Err(gents_store::config_client::AgentSelfConfigPermissionError::AgentUnavailable) => {
                "unavailable"
            }
            Err(gents_store::config_client::AgentSelfConfigPermissionError::Access(
                gents_store::config_client::ConfigClientError::DocumentRelationshipStatus {
                    status,
                    ..
                },
            )) if status.is_client_error() => "denied",
            Err(gents_store::config_client::AgentSelfConfigPermissionError::Access(error))
                if error.is_permission_denied() =>
            {
                "denied"
            }
            Err(error) => return Err(error.into()),
        };
        if outcome == "applied" {
            access
                .set_agent_self_config_permission(
                    runtime.did(),
                    requested,
                    AgentSelfConfigPermission::Revoke,
                )
                .await?;
        }
        observation.check(outcome.to_owned());
    }
    let mut outcomes = Vec::new();
    for action in &trace.input.steps {
        let modeled = action.actor.as_deref().unwrap_or_default();
        let outcome = match action.action.as_str() {
            "grant_as" | "revoke_as" => {
                let access = access_for(actor(modeled)?.as_str())?;
                let permission = if action.action == "grant_as" {
                    AgentSelfConfigPermission::Grant
                } else {
                    AgentSelfConfigPermission::Revoke
                };
                let result = access
                    .set_agent_self_config_permission(runtime.did(), &agent.agent_did, permission)
                    .await;
                result.is_ok()
            }
            "check" => {
                let allowed = node
                    .document_acp()
                    .check_doc_access(
                        &Identity::Authenticated(actor(modeled)?),
                        DocumentPermission::Update,
                        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
                        "agents",
                        &agent_doc,
                    )
                    .await?;
                let access = access_for(actor(modeled)?.as_str())?;
                let response = if actor(modeled)?.as_str() == user.did() {
                    let mut updated = agent_value.clone();
                    updated["description"] = serde_json::json!("configured");
                    let plan = gents_documents::DesiredStateApplyPlan::new(vec![
                        gents_documents::DesiredStateApplyDocument {
                            collection: gents_store::collection::Collection::Agent,
                            add: agent_value.clone(),
                            update: updated,
                        },
                    ])?;
                    access
                        .transact_write("test.self_config.update", |txn| {
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
                        .await
                        .map(|_| true)
                } else {
                    access.write("test.self_config.update", &format!(
                        "mutation {{ update_Agent(docID: \"{}\", input: {{description: \"configured\"}}) {{ _docID }} }}",
                        gents_protocol::graphql::escape_graphql_string(&agent_doc),
                    )).await.map(|value| {
                        value.pointer("/data/update_Agent").is_some_and(|rows| {
                            rows.as_array().is_some_and(|rows| !rows.is_empty()) || rows.is_object()
                        })
                    }).map_err(anyhow::Error::from)
                };
                let changed = response.as_ref().is_ok_and(|changed| *changed);
                assert_eq!(
                    changed, allowed,
                    "native update decision {action:?}: {response:?}"
                );
                allowed
            }
            _ => anyhow::bail!("unexpected self-config action {action:?}"),
        };
        outcomes.push(Some(outcome));
    }
    trace.check(outcomes);
    node.shutdown().await.expect("node shutdown");
    Ok(())
}
