use super::*;
use serde_json::json;

#[test]
fn modeled_affected_validation_preserves_visible_dependencies() {
    let case = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .expect("configuration contract");
    let document = |key: &str| match key {
        "agent" => (
            Collection::Agent,
            json!({"node_did":"owner", "agent_did":"agent",
            "creation_nonce":"nonce", "inference_profile_id":"profile"}),
        ),
        "task" => (
            Collection::Task,
            json!({"node_did":"owner", "task_id":"task",
            "agent_did":"agent", "prompt_template":"work"}),
        ),
        "trigger" => (
            Collection::Trigger,
            json!({"node_did":"owner", "trigger_id":"trigger",
            "task_id":"task", "source":{"kind":"schedule", "schedule_id":"schedule"}}),
        ),
        "schedule" => (
            Collection::Schedule,
            json!({"node_did":"owner", "schedule_id":"schedule",
            "cadence":{"kind":"interval", "interval_secs":60}}),
        ),
        "tools" => (
            Collection::Tools,
            json!({"node_did":"owner", "tools_id":"tools"}),
        ),
        "tools-collision" => (
            Collection::Tools,
            json!({"node_did":"owner", "tools_id":"collision",
            "datastore":{"datastore_tool_surface_ids":["a", "b"]}}),
        ),
        "surface-a" | "surface-b" => (
            Collection::DatastoreToolSurface,
            json!({"node_did":"owner",
            "surface_id": if key == "surface-a" { "a" } else { "b" },
            "entries":[{"kind":"query", "tool_name":"duplicate", "collection":"Records",
                "fields":["name"], "description":"Read records"}]}),
        ),
        "default-node" => (
            Collection::Node,
            json!({"node_did":"owner", "default_agent_did":"disabled-agent"}),
        ),
        "disabled-agent" => (
            Collection::Agent,
            json!({"node_did":"owner", "agent_did":"disabled-agent",
            "creation_nonce":"nonce", "inference_profile_id":"profile", "enabled":false}),
        ),
        "profile" => (
            Collection::InferenceProfile,
            json!({"node_did":"owner", "profile_id":"profile",
            "backend_id":"backend", "model_name":"model"}),
        ),
        _ => panic!("unknown modeled document {key}"),
    };
    for modeled in &case.configuration_affected_cases {
        let documents = modeled["documents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|key| document(key.as_str().unwrap()))
            .collect::<Vec<_>>();
        let changed = modeled["changed"]
            .as_array()
            .unwrap()
            .iter()
            .map(|key| {
                let (collection, value) = document(key.as_str().unwrap());
                (
                    collection,
                    value[collection.unique_field()]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                )
            })
            .collect();
        let snapshot = ConfigReferences::from_documents("owner", documents).unwrap();
        let actual = snapshot.validate_affected(&changed);
        assert_eq!(
            actual.is_ok(),
            modeled["expected"].as_bool().unwrap(),
            "{}: {actual:?}",
            modeled["name"]
        );
    }
}

#[test]
fn candidate_tools_reuse_surface_expansion_and_collision_guards() {
    let tools = json!({"tools_id":"tools", "node_did":"owner",
        "datastore":{"datastore_tool_surface_ids":["a", "b"]}});
    let surface = |id: &str, name: &str| {
        json!({"surface_id":id, "node_did":"owner",
        "entries":[{"kind":"query", "tool_name":name, "collection":"Records",
            "fields":["name"], "description":"Read records"}]})
    };
    let validate = |tools, a, b| {
        ConfigReferences::from_documents(
            "owner",
            [
                (Collection::Tools, tools),
                (Collection::DatastoreToolSurface, a),
                (Collection::DatastoreToolSurface, b),
            ],
        )
        .and_then(|refs| refs.validate())
    };
    validate(
        tools.clone(),
        surface("a", "inspect_a"),
        surface("b", "inspect_b"),
    )
    .unwrap();
    assert!(validate(
        tools.clone(),
        surface("a", "inspect"),
        surface("b", "inspect")
    )
    .is_err());
    let mut disabled = surface("b", "inspect_b");
    disabled["enabled"] = json!(false);
    assert!(validate(tools.clone(), surface("a", "inspect_a"), disabled).is_err());
    let mut collision = tools.clone();
    collision["host"] = json!({"cli":[{"name":"inspect_a"}]});
    assert!(validate(
        collision,
        surface("a", "inspect_a"),
        surface("b", "inspect_b")
    )
    .is_err());
    let mut duplicate = tools;
    duplicate["datastore"]["datastore_tool_surface_ids"] = json!(["a", "a"]);
    assert!(validate(
        duplicate,
        surface("a", "inspect_a"),
        surface("b", "inspect_b")
    )
    .is_err());
}

#[test]
fn candidate_installation_uses_existing_endpoint_and_path_requirements() {
    let validate = |collection, value| {
        ConfigReferences::from_documents("owner", [(collection, value)])
            .and_then(|refs| refs.validate())
    };
    let service = json!({"service_id":"service", "node_did":"owner", "hostname":"localhost", "mcp_port":8000});
    validate(Collection::ToolServiceRegistry, service.clone()).unwrap();
    for (field, value) in [
        ("mcp_port", json!(null)),
        ("mcp_port", json!(0)),
        ("hostname", json!(" ")),
    ] {
        let mut bad = service.clone();
        bad[field] = value;
        assert!(validate(Collection::ToolServiceRegistry, bad).is_err());
    }
    for address in ["hostname", "tailscale_ip", "lan_ip"] {
        let mut valid = service.clone();
        valid["hostname"] = json!(null);
        valid[address] = json!("host");
        validate(Collection::ToolServiceRegistry, valid).unwrap();
    }
    let placement = json!({"repository_id":"repo", "node_did":"owner", "host_path":"./checkout"});
    validate(Collection::RepositoryPlacement, placement.clone()).unwrap();
    let mut bad = placement;
    bad["host_path"] = json!(" ");
    assert!(validate(Collection::RepositoryPlacement, bad).is_err());
}

#[test]
fn callback_admission_reuses_projection_and_filter_secret_checks() {
    let callback = json!({"callback_id":"callback", "node_did":"owner",
        "handler":{"kind":"built_in","emitter":"create_workspace"}});
    let source =
        json!({"event_source_id":"source", "node_did":"owner", "source_collection":"Records"});
    let binding = json!({"binding_id":"binding", "node_did":"owner", "agent_did":crate::triggers::callback::test_agent_did(),
        "callback_id":"callback", "event_source_id":"source", "input_fields":["name"]});
    let validate = |source, binding| {
        ConfigReferences::from_documents(
            "owner",
            [
                (Collection::Agent, json!({"node_did":"owner", "agent_did":crate::triggers::callback::test_agent_did(), "creation_nonce":"nonce", "inference_profile_id":"profile"})),
                (Collection::InferenceProfile, json!({"node_did":"owner", "profile_id":"profile", "backend_id":"backend", "model_name":"model"})),
                (Collection::InferenceBackend, json!({"node_did":"owner", "backend_id":"backend", "name":"Backend", "provider_kind":"OpenAiCompatible", "endpoint":"http://localhost:8000/v1", "auth":{"kind":"unauthenticated"}})),
                (Collection::Callback, callback.clone()),
                (Collection::EventSource, source),
                (Collection::CallbackBinding, binding),
            ],
        )
        .and_then(|refs| refs.validate())
    };
    validate(source.clone(), binding.clone()).unwrap();
    for fields in [
        json!(["name", "name"]),
        json!(["api_key"]),
        json!(["bad field"]),
    ] {
        let mut bad = binding.clone();
        bad["input_fields"] = fields;
        assert!(validate(source.clone(), bad).is_err());
    }
    let mut bad_filter = source.clone();
    bad_filter["filter"] = json!("{api_key: {_eq: \"secret\"}}");
    assert!(validate(bad_filter, binding.clone()).is_err());
    let mut safe_value = source.clone();
    safe_value["filter"] = json!("{name: {_eq: \"api_key\"}}");
    validate(safe_value, binding.clone()).unwrap();
    let mut unsupported = source;
    unsupported["event_kind"] = json!("updated");
    assert!(validate(unsupported, binding).is_err());
}

#[test]
fn trigger_scope_checks_use_bound_task_and_source_for_both_templates() {
    use crate::NodeIdentity;
    let directory = tempfile::tempdir().unwrap();
    let runtime =
        crate::KeyIdentity::load_or_create(directory.path().join("runtime"), None).unwrap();
    let binding = crate::store::identity::provision_agent_identity(&runtime).unwrap();
    let owner = runtime.did();
    let base = json!({
        "node":{"node_did":owner},
        "agents":[{"node_did":owner,"agent_did":binding.agent_did,"creation_nonce":binding.creation_nonce,"inference_profile_id":"profile"}],
        "inference_profiles":[{"node_did":owner,"profile_id":"profile","backend_id":"backend","model_name":"model"}],
        "inference_backends":[{"node_did":owner,"backend_id":"backend","name":"Backend","provider_kind":"OpenAiCompatible",
            "endpoint":"http://localhost:8000/v1","auth":{"kind":"unauthenticated"}}],
        "tasks":[{"node_did":owner,"task_id":"task","agent_did":binding.agent_did,"prompt_template":"{{ ctx.now }}"}],
        "schedules":[{"node_did":owner,"schedule_id":"schedule","cadence":{"kind":"interval","interval_secs":10}}],
        "event_sources":[{"node_did":owner,"event_source_id":"event","source_collection":"Records"}],
        "triggers":[{"node_did":owner,"trigger_id":"trigger","task_id":"task","source":{"kind":"schedule","schedule_id":"schedule"}}]
    });
    let validate = |value| {
        let config = serde_json::from_value::<super::super::PackConfig>(value).unwrap();
        let plan =
            crate::store::config_client::DesiredStateApplyPlan::from_pack_config(&config).unwrap();
        ConfigReferences::from_documents(
            owner,
            plan.documents()
                .iter()
                .map(|doc| (doc.collection, doc.add.clone())),
        )?
        .validate()
    };
    validate(base.clone()).unwrap();
    let mut outcome = base.clone();
    outcome["triggers"][0]["source"] = json!({"kind":"event","event_source_id":"event"});
    outcome["event_sources"][0]["source_collection"] = json!("FireOutcome");
    validate(outcome.clone()).unwrap();
    outcome["tasks"][0]["emit_outcome"] = json!(true);
    let error = validate(outcome.clone()).unwrap_err();
    assert!(error.to_string().contains("sources FireOutcome"));
    outcome["event_sources"][0]["source_collection"] = json!("Records");
    validate(outcome).unwrap();

    for field in ["prompt_template", "goal_objective_template"] {
        for template in ["{{ doc.name }}", "{{ args.name }}", "{{ group.documents }}"] {
            let mut invalid = base.clone();
            invalid["tasks"][0][field] = json!(template);
            assert!(
                validate(invalid).is_err(),
                "schedule accepted {field} {template}"
            );
        }
        let mut event = base.clone();
        event["triggers"][0]["source"] = json!({"kind":"event","event_source_id":"event"});
        event["tasks"][0][field] = json!("{{ doc.name }}");
        validate(event.clone()).unwrap();
        event["tasks"][0][field] = json!("{{ group.documents }}");
        assert!(validate(event.clone()).is_err());
        event["event_sources"][0]["correlation_field"] = json!("job");
        event["event_sources"][0]["group"] = json!({"expected_count":2});
        validate(event.clone()).unwrap();
        for grouped in [false, true] {
            let mut invalid = event.clone();
            if !grouped {
                invalid["event_sources"][0]["group"] = json!(null);
            }
            invalid["tasks"][0][field] = json!("{{ args.name }}");
            assert!(
                validate(invalid.clone()).is_err(),
                "event accepted caller args"
            );
            invalid["triggers"] = json!([]);
            validate(invalid).unwrap();
        }
    }
}

#[tokio::test]
async fn event_args_rejection_rolls_back_the_whole_configuration_plan() {
    use crate::documents::apply_desired_state_plan;
    use crate::store::config_client::{read_desired_state_record_in_txn, DesiredStateApplyPlan};
    use crate::NodeIdentity;
    let (db, user, access, default) =
        crate::test_support::signed_test_node("event-args-rollback").await;
    let owner = db.node_identity.did().to_owned();
    let binding = crate::store::identity::provision_agent_identity_authorized(
        &db.node,
        db.node_identity.as_ref(),
        &identity::Did::new(user.did()).unwrap(),
    )
    .await
    .unwrap();
    let mut config = json!({
        "node":{"node_did":owner,"default_agent_did":default.agent_did},
        "agents":[{"node_did":owner,"agent_did":binding.agent_did,"creation_nonce":binding.creation_nonce,"inference_profile_id":"profile"}],
        "inference_profiles":[{"node_did":owner,"profile_id":"profile","backend_id":"backend","model_name":"model"}],
        "inference_backends":[{"node_did":owner,"backend_id":"backend","name":"Backend","provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1","auth":{"kind":"unauthenticated"}}],
        "tasks":[{"node_did":owner,"task_id":"task","agent_did":binding.agent_did,"prompt_template":"{{ args.name }}"}],
        "event_sources":[{"node_did":owner,"event_source_id":"event","source_collection":"Records"}]
    });
    let plan = DesiredStateApplyPlan::from_pack_config(
        &serde_json::from_value::<super::super::PackConfig>(config.clone()).unwrap(),
    )
    .unwrap();
    access
        .transact_write("test.manual_task", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap();
    config["tasks"][0]["prompt_template"] = json!("{{ args.other }}");
    config["triggers"] = json!([{"node_did":owner,"trigger_id":"trigger","task_id":"task","source":{"kind":"event","event_source_id":"event"}}]);
    let plan = DesiredStateApplyPlan::from_pack_config(
        &serde_json::from_value::<super::super::PackConfig>(config.clone()).unwrap(),
    )
    .unwrap();
    assert!(access
        .transact_write("test.invalid_event_task", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await
        .is_err());
    access
        .transact_write("test.rollback", |txn| {
            let owner = &owner;
            Box::pin(async move {
                let (_, task) =
                    read_desired_state_record_in_txn(txn, Collection::Task, owner, "task")
                        .await?
                        .unwrap();
                assert_eq!(task["prompt_template"], json!("{{ args.name }}"));
                assert!(read_desired_state_record_in_txn(
                    txn,
                    Collection::Trigger,
                    owner,
                    "trigger"
                )
                .await?
                .is_none());
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    config["tasks"][0]["prompt_template"] = json!("{{ doc.name }}");
    let plan = DesiredStateApplyPlan::from_pack_config(
        &serde_json::from_value::<super::super::PackConfig>(config).unwrap(),
    )
    .unwrap();
    access
        .transact_write("test.valid_event_task", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap();
}

/// Identifier-shaped fields that do not name a same-owner configuration
/// document, so no reference consumer selects or grants through them.
const NOT_REFERENCES: &[(Collection, &str)] = &[
    // Removed EthTool collection; read and dropped from rc0 documents.
    (Collection::Tools, "integrations.eth_tool_ids"),
    (Collection::AgentTarget, "target_node_did"),
    (Collection::InferenceBackend, "auth.access_key_id"),
    (Collection::InferenceBackend, "claude_platform.workspace_id"),
    (Collection::ToolServiceRegistry, "send_node_did"),
    (Collection::ProjectionAcpBinding, "policy_id"),
    (Collection::ProjectionAcpBinding, "previous_policy_id"),
    (Collection::ProjectionAcpBinding, "projection_id"),
    (Collection::ProjectionAcpBinding, "staged_policy_id"),
    (Collection::Task, "hooks.hook_id"),
    (Collection::CallbackModule, "signer_did"),
    (Collection::EvalDefinition, "cases.case_id"),
    (Collection::EvalDefinition, "cases.stages.stage_id"),
];

fn identifier_paths(
    schema: &Value,
    definitions: &serde_json::Map<String, Value>,
    prefix: &str,
    visiting: &mut Vec<String>,
    paths: &mut BTreeSet<String>,
) {
    if let Some(name) = schema
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix("#/$defs/"))
    {
        if !visiting.iter().any(|seen| seen == name) {
            visiting.push(name.to_owned());
            if let Some(definition) = definitions.get(name) {
                identifier_paths(definition, definitions, prefix, visiting, paths);
            }
            visiting.pop();
        }
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        for branch in schema
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            identifier_paths(branch, definitions, prefix, visiting, paths);
        }
    }
    if let Some(items) = schema.get("items") {
        identifier_paths(items, definitions, prefix, visiting, paths);
    }
    for (name, property) in schema
        .get("properties")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        if ["_id", "_ids", "_did"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
        {
            paths.insert(path.clone());
        }
        identifier_paths(property, definitions, &path, visiting, paths);
    }
}

#[test]
fn every_canonical_reference_field_reaches_reference_consumers() {
    let samples = [
        (
            Collection::Node,
            json!({"node_did":"owner", "default_agent_did":"agent"}),
        ),
        (
            Collection::Agent,
            json!({"node_did":"owner", "agent_did":"agent", "creation_nonce":"nonce",
                "context_id":"context", "inference_profile_id":"profile"}),
        ),
        (
            Collection::AgentContext,
            json!({"node_did":"owner", "context_id":"context", "tools_id":"tools",
                "compaction_id":"compaction", "skill_ids":["skill"]}),
        ),
        (
            Collection::Compaction,
            json!({"node_did":"owner", "compaction_id":"compaction",
                "inference_profile_id":"profile"}),
        ),
        (
            Collection::Tools,
            json!({"node_did":"owner", "tools_id":"tools",
                "remote":{"services":[{"mcp_service_id":"service"}]},
                "agents":{"target_ids":["target"]},
                "datastore":{"datastore_tool_surface_ids":["surface"]}}),
        ),
        (
            Collection::AgentTarget,
            json!({"node_did":"owner", "target_id":"target", "target_node_did":"owner",
                "agent_did":"agent", "name":"target"}),
        ),
        (
            Collection::InferenceProfile,
            json!({"node_did":"owner", "profile_id":"profile", "backend_id":"backend",
                "model_name":"model", "sampling_id":"sampling", "execution_id":"execution"}),
        ),
        (
            Collection::InferenceExecution,
            json!({"node_did":"owner", "execution_id":"execution", "retry_policy_id":"retry"}),
        ),
        (
            Collection::ProjectionAcpBinding,
            json!({"node_did":"owner", "binding_id":"binding", "agent_did":"agent",
                "policy_id":"policy"}),
        ),
        (
            Collection::Task,
            json!({"node_did":"owner", "task_id":"task", "agent_did":"agent",
                "prompt_template":"work"}),
        ),
        (
            Collection::Trigger,
            json!({"node_did":"owner", "trigger_id":"scheduled", "task_id":"task",
                "source":{"kind":"schedule", "schedule_id":"schedule"}}),
        ),
        (
            Collection::Trigger,
            json!({"node_did":"owner", "trigger_id":"evented", "task_id":"task",
                "source":{"kind":"event", "event_source_id":"source"}}),
        ),
        (
            Collection::Callback,
            json!({"node_did":"owner", "callback_id":"callback",
                "handler":{"kind":"module", "module_id":"module", "workspace_agents":{
                    "writer_agent_did":"writer", "integrator_agent_did":"integrator",
                    "readers":[{"agent_did":"reader"}]}}}),
        ),
        (
            Collection::CallbackBinding,
            json!({"node_did":"owner", "binding_id":"binding", "agent_did":"agent",
                "event_source_id":"source", "callback_id":"callback", "input_fields":["name"]}),
        ),
        (
            Collection::Task,
            json!({"node_did":"owner", "task_id":"task", "agent_did":"agent",
                "prompt_template":"work", "emit_outcome":true,
                "outcome_readers":[{"agent_did":"reader"}]}),
        ),
        (
            Collection::RepositoryPlacement,
            json!({"node_did":"owner", "repository_id":"repository", "host_path":"/repo",
                "readers":[{"agent_did":"reader"}]}),
        ),
    ];
    let mut reported = BTreeSet::new();
    for (collection, sample) in samples {
        let value = config_projection(collection, Some(&sample))
            .unwrap()
            .1
            .unwrap();
        for reference in references_of(collection, &value, "owner").unwrap() {
            assert!(
                !reference.id.is_empty(),
                "{collection:?} {}",
                reference.field
            );
            reported.insert((collection, reference.field.to_owned()));
        }
    }
    let mut declared = BTreeSet::new();
    for collection in Collection::ALL {
        let schema =
            serde_json::to_value(crate::pack::authored_document_schema(collection)).unwrap();
        let definitions = schema
            .get("$defs")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut paths = BTreeSet::new();
        identifier_paths(&schema, &definitions, "", &mut Vec::new(), &mut paths);
        for path in paths {
            if path != collection.unique_field()
                && path != "node_did"
                && !NOT_REFERENCES.contains(&(collection, path.as_str()))
            {
                declared.insert((collection, path));
            }
        }
    }
    assert_eq!(
        declared, reported,
        "classify each identifier field as a reference in references_of or in NOT_REFERENCES"
    );
}
