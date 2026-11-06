use super::{schedule_visibility, ScheduleSource, TriggerSource};
use crate::lean_vocab_test::{lean_identity_household_cases, LeanIdentitySubject};
use crate::store::config_client::{ConfigAccess, GraphqlEndpoint};
use crate::store::identity::NodeIdentity;
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn modeled_cached_schedule_requires_runtime_visibility() -> anyhow::Result<()> {
    let case = lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .expect("configuration contract");
    let modeled_runtime = case
        .relations
        .iter()
        .find_map(|relation| match &relation.subject {
            LeanIdentitySubject::Actor { actor }
                if relation.resource == "nodes" && relation.relation == "runtime_reader" =>
            {
                Some(actor)
            }
            _ => None,
        })
        .expect("modeled runtime");
    let directory = tempfile::tempdir()?;
    let user = crate::KeyIdentity::load_or_create(directory.path().join("user.key"), None)?;
    let agent = crate::triggers::engine::tests::integration_test_agent("schedule-policy");
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(agent.node_did())
            .with_node_acp_owner(user.did().to_owned())
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .build()
            .await?,
    );
    let runtime = node.node_identity_did().unwrap().to_owned();
    let context = defra_core::RequestContext::default().with_identity(Some(user.did().to_owned()));
    let policy = node
        .add_dac_policy_with_counter_as(&context, &case.policy_yaml, 0)
        .await?;
    assert_eq!(policy, gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID);
    node.add_nac_actor_relationship_as(
        &context,
        acp::nac::NodePermission::DocumentRead.as_str(),
        &runtime,
    )
    .await?;
    node.add_nac_actor_relationship_as(
        &context,
        acp::nac::NodePermission::DocumentRead.as_str(),
        &agent.agent_did,
    )
    .await?;
    node.add_nac_actor_relationship_as(
        &context,
        acp::nac::NodePermission::DocumentUpdate.as_str(),
        &agent.agent_did,
    )
    .await?;
    node.add_collection(gents_protocol::schemas::NODE).await?;
    node.add_collection(gents_protocol::schemas::SCHEDULE)
        .await?;
    node.add_collection(gents_protocol::schemas::TRIGGER)
        .await?;
    for (name, schema) in gents_protocol::schemas::RUNTIME_COLLECTION_NAMES
        .iter()
        .zip(gents_protocol::schemas::RUNTIME_ALL)
        .chain(
            gents_protocol::schemas::ALL_COLLECTION_NAMES
                .iter()
                .zip(gents_protocol::schemas::ALL),
        )
    {
        if !["Node", "Schedule", "Trigger"].contains(name)
            && crate::Collection::ALL
                .iter()
                .any(|collection| collection.graphql_type() == *name)
        {
            node.add_collection(schema).await?;
        }
    }
    let endpoint =
        GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), user.did())
            .with_client_signing_capability(user.client_signing_capability()?)?
            .with_client_signed_agent_creates();
    let user_access = ConfigAccess::Graphql(endpoint);
    let node_input = json!({"node_did":runtime});
    let node_response = user_access
        .transact_write("test.schedule.node", |txn| {
            let input = &node_input;
            Box::pin(async move {
                txn.execute_desired_state_mutation(
            "mutation($input: NodeMutationInputArg!) { create_Node(input:$input) { _docID } }",
            &json!({"input":input}), crate::Collection::Node, true).await
            })
        })
        .await?;
    let node_doc = gents_protocol::graphql::extract_mutation_doc_id(&node_response, "Node")?;
    user_access
        .add_actor_relationship(
            "Node",
            &node_doc,
            "runtime_reader",
            &identity::Did::new(&runtime)?,
        )
        .await?;
    let plan_value = json!({"node_did":runtime, "schedule_id":"planned",
        "cadence":{"kind":"interval", "interval_secs":60}});
    let plan = crate::store::config_client::DesiredStateApplyPlan::new(vec![
        crate::store::config_client::DesiredStateApplyDocument {
            collection: crate::Collection::Schedule,
            add: plan_value.clone(),
            update: plan_value,
        },
    ])?;
    user_access
        .transact_write("test.schedule.plan", |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::store::config_client::apply_desired_state_plan(txn, plan).await
            })
        })
        .await?;
    let planned = user_access
        .execute("{ Schedule(filter: {schedule_id: {_eq: \"planned\"}}) { _docID } }")
        .await?;
    let planned_doc = planned["data"]["Schedule"][0]["_docID"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("planned Schedule is missing"))?
        .to_owned();
    assert_eq!(
        schedule_visibility(&node, &runtime, std::slice::from_ref(&planned_doc)).await?,
        vec![true]
    );
    let updated = json!({"node_did":runtime, "schedule_id":"planned", "display_name":"edited by user",
        "cadence":{"kind":"interval", "interval_secs":120}});
    let update_plan = crate::store::config_client::DesiredStateApplyPlan::new(vec![
        crate::store::config_client::DesiredStateApplyDocument {
            collection: crate::Collection::Schedule,
            add: updated.clone(),
            update: updated,
        },
    ])?;
    user_access
        .transact_write("test.schedule.update", |txn| {
            let plan = &update_plan;
            Box::pin(async move {
                crate::store::config_client::apply_desired_state_plan(txn, plan).await
            })
        })
        .await?;
    let edited = user_access
        .execute("{ Schedule(filter: {schedule_id: {_eq: \"planned\"}}) { display_name cadence } }")
        .await?;
    assert_eq!(
        edited["data"]["Schedule"][0]["display_name"],
        "edited by user"
    );
    assert_eq!(
        edited["data"]["Schedule"][0]["cadence"]["interval_secs"],
        120
    );
    let history = user_access
        .execute(&format!(
            "{{ _commits(docID: \"{}\") {{cid height fieldName}} }}",
            crate::store::graphql::escape_graphql_string(&planned_doc),
        ))
        .await?;
    let head = history["data"]["_commits"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .filter(|row| row["fieldName"] == "_C")
                .max_by_key(|row| row["height"].as_i64().unwrap_or_default())
        })
        .ok_or_else(|| anyhow::anyhow!("updated Schedule history is missing"))?;
    let cid = head["cid"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("updated Schedule CID is missing"))?;
    let (block, signature) = node
        .authorized_signed_block_bytes(cid, Some(user.did()))
        .await?;
    let block = defra_core::block::Block::from_dag_cbor(&block)?;
    let signature = defra_core::block::Signature::from_dag_cbor(&signature)?;
    let signer = db::block::verify::verified_signature_signer_did(&block, &signature)
        .map_err(anyhow::Error::msg)?;
    assert_eq!(signer, user.did());
    async fn create_schedule(
        access: &ConfigAccess,
        node: &defra_node::Node,
        user: &str,
        runtime: &str,
        id: &str,
        parent: Option<&str>,
    ) -> anyhow::Result<String> {
        let input = json!({"node_did":runtime, "schedule_id":id,
            "cadence":{"kind":"interval", "interval_secs":60}});
        let response = access.transact_write("test.schedule.create", |txn| {
            let input = &input;
            Box::pin(async move { let response = txn.execute_desired_state_mutation(
                "mutation($input: ScheduleMutationInputArg!) { create_Schedule(input:$input) { _docID _version { cid } } }",
                &json!({"input":input}), crate::Collection::Schedule, true).await?;
                if let Some(parent) = parent {
                    let doc = gents_protocol::graphql::extract_mutation_doc_id(&response, "Schedule")?;
                    txn.add_resource_relationship("Schedule", &doc, "parent", "nodes", parent).await?;
                }
                Ok::<_, anyhow::Error>(response)
            })
        }).await?;
        let cid = response["data"]["add_Schedule"][0]["_version"][0]["cid"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Schedule genesis CID is missing"))?;
        let (block, signature) = node.authorized_signed_block_bytes(cid, Some(user)).await?;
        let block = defra_core::block::Block::from_dag_cbor(&block)?;
        let signature = defra_core::block::Signature::from_dag_cbor(&signature)?;
        let signer = db::block::verify::verified_signature_signer_did(&block, &signature)
            .map_err(anyhow::Error::msg)?;
        assert_eq!(signer, user);
        Ok(gents_protocol::graphql::extract_mutation_doc_id(
            &response, "Schedule",
        )?)
    }
    let schedule_doc = create_schedule(
        &user_access,
        &node,
        user.did(),
        &runtime,
        "schedule",
        Some(&node_doc),
    )
    .await?;
    let agent_read = ConfigAccess::local_as(node.clone(), identity::Did::new(&agent.agent_did)?)
        .execute(&format!(
            "{{ Schedule(filter: {{_docID: {{_eq: \"{}\"}}}}) {{_docID}} }}",
            crate::store::graphql::escape_graphql_string(&schedule_doc)
        ))
        .await?;
    assert!(agent_read["data"]["Schedule"]
        .as_array()
        .unwrap()
        .is_empty());
    let agent_did = identity::Did::new(&agent.agent_did)?;
    let agent_access = ConfigAccess::local_managed_agent(
        node.clone(),
        agent_did.clone(),
        agent.agent_identity.as_ref(),
    )?;
    let mut last_name = serde_json::Value::Null;
    for observation in &case.configuration_schedule_write_cases {
        if observation["grant"] == true && observation["revoke"] != true {
            user_access
                .add_actor_relationship("Schedule", &schedule_doc, "writer", &agent_did)
                .await?;
        }
        if observation["revoke"] == true {
            user_access
                .delete_actor_relationship("Schedule", &schedule_doc, "writer", &agent_did)
                .await?;
        }
        let name = observation["name"].as_str().unwrap();
        let mutation = format!(
            "mutation {{ update_Schedule(docID: \"{}\", input: {{display_name: \"{}\"}}) {{_docID _version {{cid}}}} }}",
            crate::store::graphql::escape_graphql_string(&schedule_doc),
            crate::store::graphql::escape_graphql_string(name),
        );
        let result = agent_access
            .write("test.schedule.agent_update", &mutation)
            .await;
        let expected = observation["expected"].as_bool().unwrap();
        let changed = match &result {
            Ok(response) => {
                let rows = response["data"]["update_Schedule"]
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("Schedule update rows are missing"))?;
                anyhow::ensure!(rows.len() <= 1, "Schedule update returned multiple rows");
                !rows.is_empty()
            }
            Err(_) => false,
        };
        assert_eq!(changed, expected, "{name}: {result:?}");
        if changed {
            let response = result?;
            let cid = response["data"]["update_Schedule"][0]["_version"][0]["cid"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("updated Schedule CID is missing"))?;
            let (block, signature) = node
                .authorized_signed_block_bytes(cid, Some(user.did()))
                .await?;
            let block = defra_core::block::Block::from_dag_cbor(&block)?;
            let signature = defra_core::block::Signature::from_dag_cbor(&signature)?;
            let signer = db::block::verify::verified_signature_signer_did(&block, &signature)
                .map_err(anyhow::Error::msg)?;
            assert_eq!(signer, agent.agent_did);
            last_name = json!(name);
        }
        let observed = user_access
            .execute(&format!(
                "{{ Schedule(filter: {{_docID: {{_eq: \"{}\"}}}}) {{display_name}} }}",
                crate::store::graphql::escape_graphql_string(&schedule_doc),
            ))
            .await?;
        assert_eq!(observed["data"]["Schedule"][0]["display_name"], last_name);
    }
    let replacement = create_schedule(
        &user_access,
        &node,
        user.did(),
        &runtime,
        "replacement",
        None,
    )
    .await?;
    for observation in &case.configuration_schedule_cases {
        if observation["revoke_parent"] == true {
            user_access
                .delete_resource_relationship(
                    "Schedule",
                    &schedule_doc,
                    "parent",
                    "nodes",
                    &node_doc,
                )
                .await?;
        } else {
            user_access
                .add_resource_relationship("Schedule", &schedule_doc, "parent", "nodes", &node_doc)
                .await?;
        }
        let doc = if observation["missing"] == true {
            &replacement
        } else {
            &schedule_doc
        };
        let actor = if observation["actor"].as_str() == Some(modeled_runtime) {
            &runtime
        } else {
            observation["actor"].as_str().unwrap()
        };
        let observed = schedule_visibility(&node, actor, std::slice::from_ref(doc)).await;
        let allowed = match observed {
            Ok(rows) => matches!(rows.as_slice(), [true]),
            Err(crate::store::config_client::ConfigClientError::TransactionActorMismatch) => false,
            Err(error) => return Err(error.into()),
        };
        assert_eq!(
            allowed,
            observation["expected"].as_bool().unwrap(),
            "{}",
            observation["name"]
        );
    }

    let trigger_input = json!({"node_did":runtime, "trigger_id":"trigger", "task_id":"task",
        "source":{"kind":"schedule", "schedule_id":"schedule"}, "enabled":true,
        "next_run_at":(chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339()});
    let trigger_response = user_access.transact_write("test.schedule.trigger", |txn| {
        let input = &trigger_input;
        Box::pin(async move { txn.execute_desired_state_mutation(
            "mutation($input: TriggerMutationInputArg!) { create_Trigger(input:$input) { _docID } }",
            &json!({"input":input}), crate::Collection::Trigger, true).await })
    }).await?;
    let trigger_doc =
        gents_protocol::graphql::extract_mutation_doc_id(&trigger_response, "Trigger")?;
    let task = crate::runtime::snapshot::ResolvedTask {
        emit_outcome: false,
        task_id: "task".into(),
        name: None,
        agent_did: agent.agent_did.clone(),
        prompt_template: "work".into(),
        goal_objective_template: None,
        goal_token_budget: None,
        output_schema_ref: None,
        hooks: Vec::new(),
    };
    let schedule = crate::runtime::snapshot::ResolvedSchedule {
        session_id_template: None,
        trigger_doc_id: trigger_doc,
        schedule_doc_id: schedule_doc.clone(),
        schedule_id: "schedule".into(),
        task_id: "task".into(),
        task,
        cadence: crate::documents::ScheduleCadence::Interval { interval_secs: 60 },
        enabled: true,
        concurrency: crate::runtime::snapshot::ConcurrencyMode::Serial,
    };
    let snapshot = crate::triggers::engine::tests::snapshot_with_agent_and_schedules(
        agent,
        rapidhash::RapidHashMap::from_iter([("trigger".to_owned(), schedule)]),
    );
    let (_sender, receiver) = tokio::sync::watch::channel(snapshot);
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut source = ScheduleSource::new(receiver, node.clone(), cancel.clone())
        .with_tick_every(std::time::Duration::from_millis(10));
    user_access
        .delete_resource_relationship("Schedule", &schedule_doc, "parent", "nodes", &node_doc)
        .await?;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(150), source.next_fire())
            .await
            .is_err(),
        "revoked cached cadence must not fire"
    );
    user_access
        .add_resource_relationship("Schedule", &schedule_doc, "parent", "nodes", &node_doc)
        .await?;
    let intent = tokio::time::timeout(std::time::Duration::from_secs(2), source.next_fire())
        .await?
        .ok_or_else(|| anyhow::anyhow!("schedule source stopped"))?;
    assert_eq!(intent.trigger_id.as_deref(), Some("trigger"));
    cancel.cancel();
    node.shutdown().await?;
    Ok(())
}
