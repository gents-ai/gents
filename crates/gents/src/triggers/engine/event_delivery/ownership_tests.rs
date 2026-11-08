use super::*;
use crate::store::graphql::escape_graphql_string;
use crate::store::identity::NodeIdentity;
use std::sync::Arc;

#[test]
fn modeled_event_execution_generations_match_runtime_projection() {
    let contract = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let project = |value: &serde_json::Value| {
        let task: crate::documents::Task = serde_json::from_value(json!({
            "node_did":"node", "task_id":value["task_id"], "agent_did":value["agent"],
            "display_name":value["name"], "prompt_template":value["prompt"],
            "goal_objective_template":value["objective"], "goal_token_budget":value["budget"],
            "output_schema_ref":value["output"], "hooks":serde_json::from_str::<serde_json::Value>(value["hooks"].as_str().unwrap()).unwrap(),
            "emit_outcome":value["emit"]
        })).unwrap();
        let trigger: crate::documents::Trigger = serde_json::from_value(json!({
            "node_did":"node", "trigger_id":"trigger", "task_id":value["task_id"],
            "source":{"kind":"event", "event_source_id":value["source_id"]},
            "session_id_template":value["session_template"], "concurrency":value["concurrency"]
        }))
        .unwrap();
        let resolved = crate::documents::automation::ResolvedEventTrigger {
            session_id_template: trigger.session_id_template.clone(),
            trigger_doc_id: value["trigger_doc"].as_str().unwrap().into(),
            trigger_id: trigger.trigger_id.clone(),
            task_id: task.task_id.clone(),
            task: crate::documents::automation::resolved_task_from(&task),
            event_source_id: value["source_id"].as_str().unwrap().into(),
            event_source_doc_id: value["source_doc"].as_str().unwrap().into(),
            task_doc_id: value["task_doc"].as_str().unwrap().into(),
            source_collection: "Records".into(),
            event_kind: "created".into(),
            filter: None,
            enabled: true,
            concurrency: trigger.concurrency.unwrap_or_default(),
            fire_mode: crate::documents::automation::EventTriggerFireMode::PerDocument,
            correlation_field: None,
            expected_count: None,
            expected_count_field: None,
            group_timeout_secs: None,
            group_min_count: 1,
            workspace_authority: None,
        };
        (
            crate::documents::automation::event_execution_generation(
                &trigger,
                &task,
                &crate::documents::automation::EventConfigDocIds {
                    trigger: value["trigger_doc"].as_str().unwrap(),
                    source: value["source_doc"].as_str().unwrap(),
                    task: value["task_doc"].as_str().unwrap(),
                },
            )
            .unwrap(),
            crate::documents::automation::resolved_event_execution_generation(&resolved),
        )
    };
    for case in &contract.event_execution_cases {
        let (observed, resolved) = project(&case["observed"]);
        let (current, _) = project(&case["current"]);
        assert_eq!(observed, case["observed"], "{}", case["name"]);
        assert_eq!(resolved, observed, "{}", case["name"]);
        assert_eq!(current, case["current"], "{}", case["name"]);
        assert_eq!(
            observed == current,
            case["allowed"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn modeled_event_source_generations_match_resolved_projection() {
    let contract = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let source = |generation: &serde_json::Value| -> EventSource {
        let expected = if generation["expected_count"].is_number() {
            generation["expected_count"].clone()
        } else if generation["expected_field"].is_string() {
            json!({"source_field": generation["expected_field"]})
        } else {
            serde_json::Value::Null
        };
        let group = if generation["grouped"] == true {
            json!({"expected_count":expected,"timeout_secs":generation["timeout"],"min_count":generation["minimum"]})
        } else {
            serde_json::Value::Null
        };
        serde_json::from_value(json!({
            "node_did":"node", "event_source_id":"source", "source_collection":generation["collection"],
            "event_kind":generation["event_kind"], "filter":generation["filter"],
            "correlation_field":generation["correlation"], "group":group,
            "workspace_authority":generation["workspace_authority"],
        })).unwrap()
    };
    let resolved =
        |generation: &serde_json::Value| crate::documents::automation::ResolvedEventTrigger {
            event_source_id: "source".into(),
            event_source_doc_id: "source-doc".into(),
            task_doc_id: "task-doc".into(),
            session_id_template: None,
            trigger_doc_id: "trigger-doc".into(),
            trigger_id: "trigger".into(),
            task_id: "task".into(),
            task: crate::documents::automation::ResolvedTask {
                emit_outcome: false,
                task_id: "task".into(),
                name: None,
                agent_did: "agent".into(),
                prompt_template: String::new(),
                goal_objective_template: None,
                goal_token_budget: None,
                output_schema_ref: None,
                hooks: Default::default(),
            },
            source_collection: generation["collection"].as_str().unwrap().into(),
            event_kind: generation["event_kind"].as_str().unwrap().into(),
            filter: generation["filter"].as_str().map(str::to_owned),
            enabled: true,
            concurrency: crate::documents::ConcurrencyMode::Parallel,
            fire_mode: if generation["grouped"] == true {
                crate::documents::automation::EventTriggerFireMode::PerGroup
            } else {
                crate::documents::automation::EventTriggerFireMode::PerDocument
            },
            correlation_field: generation["correlation"].as_str().map(str::to_owned),
            expected_count: generation["expected_count"]
                .as_u64()
                .map(|n| usize::try_from(n).unwrap()),
            expected_count_field: generation["expected_field"].as_str().map(str::to_owned),
            group_timeout_secs: generation["timeout"].as_u64(),
            group_min_count: usize::try_from(generation["minimum"].as_u64().unwrap()).unwrap(),
            workspace_authority: generation["workspace_authority"]
                .as_str()
                .map(str::to_owned),
        };
    for case in &contract.event_generation_cases {
        let observed =
            crate::documents::automation::event_source_generation(&source(&case["observed"]))
                .unwrap();
        let current =
            crate::documents::automation::event_source_generation(&source(&case["current"]))
                .unwrap();
        assert_eq!(observed, case["observed"], "{}", case["name"]);
        assert_eq!(current, case["current"], "{}", case["name"]);
        assert_eq!(
            crate::documents::automation::resolved_event_source_generation(&resolved(
                &case["observed"]
            )),
            observed,
            "{}",
            case["name"]
        );
        assert_eq!(
            observed == current,
            case["allowed"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
    }
}

#[tokio::test]
async fn modeled_group_clock_is_owned_by_selected_agent_and_linked_once() -> anyhow::Result<()> {
    let contract = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let modeled_user = &contract
        .objects
        .iter()
        .find(|row| row.id == "home")
        .unwrap()
        .owner;
    let modeled_agent = &contract
        .objects
        .iter()
        .find(|row| row.id == "agent-created")
        .unwrap()
        .owner;
    let modeled_runtime = contract
        .relations
        .iter()
        .find_map(|relation| match &relation.subject {
            crate::lean_vocab_test::LeanIdentitySubject::Actor { actor }
                if relation.resource == "nodes" && relation.relation == "runtime_reader" =>
            {
                Some(actor)
            }
            _ => None,
        })
        .unwrap();
    let directory = tempfile::tempdir()?;
    let root = crate::store::identity::KeyIdentity::load_or_create(
        directory.path().join("node.key"),
        None,
    )?;
    let user_binding = crate::store::identity::provision_agent_identity(&root)?;
    let agent_binding = crate::store::identity::provision_agent_identity(&root)?;
    let user = crate::store::identity::resolve_agent_identity(&root, &user_binding)?;
    let agent = crate::store::identity::resolve_agent_identity(&root, &agent_binding)?;
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(root.did())
            .build()
            .await?,
    );
    let owner = crate::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(user.did())?,
        user.as_ref(),
    )?;
    let worker = crate::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did())?,
        agent.as_ref(),
    )?;
    let runtime = crate::ConfigAccess::local_as(node.clone(), identity::Did::new(root.did())?);
    let ctx = defra_core::RequestContext::default().with_identity(Some(user.did().to_owned()));
    node.add_dac_policy_with_counter_as(&ctx, &contract.policy_yaml, 0)
        .await?;
    for schema in [
        gents_protocol::schemas::NODE,
        gents_protocol::schemas::AGENT,
        gents_protocol::schemas::EVENT_GROUP_STATE,
    ] {
        owner.add_collection(schema).await?;
    }
    let created = owner
        .write(
            "test.group.node",
            &format!(
                "mutation {{create_Node(input:{{node_did:\"{}\"}}){{_docID}}}}",
                escape_graphql_string(root.did())
            ),
        )
        .await?;
    let node_doc = crate::store::graphql::created_doc_id(&created, "Node")?;
    owner
        .add_actor_relationship(
            "Node",
            &node_doc,
            "runtime_reader",
            &identity::Did::new(root.did())?,
        )
        .await?;
    let created = owner.write("test.group.agent", &format!("mutation {{create_Agent(input:{{node_did:\"{}\",agent_did:\"{}\",creation_nonce:\"{}\",inference_profile_id:\"{}\"}}){{_docID}}}}", escape_graphql_string(root.did()), escape_graphql_string(agent.did()), escape_graphql_string(&agent_binding.creation_nonce), escape_graphql_string(&format!("{}:inference", agent.did())))).await?;
    let agent_doc = crate::store::graphql::created_doc_id(&created, "Agent")?;
    owner
        .add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node_doc)
        .await?;
    owner
        .add_actor_relationship(
            "Agent",
            &agent_doc,
            "executor",
            &identity::Did::new(agent.did())?,
        )
        .await?;
    let binding: CallbackBinding = serde_json::from_value(
        json!({"node_did":root.did(),"binding_id":"binding","agent_did":agent.did(),"callback_id":"callback","event_source_id":"source"}),
    )?;
    let source: EventSource = serde_json::from_value(
        json!({"node_did":root.did(),"event_source_id":"source","source_collection":"GroupMember","correlation_field":"batch","group":{"timeout_secs":1}}),
    )?;
    let delivery = Delivery::Callback {
        binding: &binding,
        source: &source,
    };
    let group = load_or_create_group(&worker, delivery, "batch").await?;
    let runtime_did = modeled_runtime.as_str();
    for observation in &contract.event_group_cases {
        let actor = observation["actor"].as_str().unwrap();
        let access = if actor == modeled_user {
            &owner
        } else if actor == modeled_agent {
            &worker
        } else if actor == runtime_did {
            &runtime
        } else {
            let sibling = crate::ConfigAccess::local_as(node.clone(), identity::Did::new(actor)?);
            let response = sibling.execute("{EventGroupState {_docID}}").await?;
            assert_eq!(
                !response["data"]["EventGroupState"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
                observation["expected"].as_bool().unwrap()
            );
            continue;
        };
        let response = access.execute("{EventGroupState {_docID}}").await?;
        assert_eq!(
            !response["data"]["EventGroupState"]
                .as_array()
                .unwrap()
                .is_empty(),
            observation["expected"].as_bool().unwrap()
        );
    }
    quiesce_group(&worker, &group, "done").await?;
    worker
        .delete_resource_relationship(
            "EventGroupState",
            &group.doc_id,
            "parent",
            "agents",
            &agent_doc,
        )
        .await?;
    let reloaded = load_or_create_group(&worker, delivery, "batch").await?;
    assert_eq!(reloaded.doc_id, group.doc_id);
    assert!(
        owner.execute("{EventGroupState {_docID}}").await?["data"]["EventGroupState"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    node.shutdown().await?;
    Ok(())
}
