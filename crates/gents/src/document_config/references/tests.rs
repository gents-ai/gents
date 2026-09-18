use super::*;
use serde_json::json;

fn behavior_scope_documents(scope: Option<&str>) -> Vec<(Collection, Value)> {
    vec![
        (
            Collection::AgentBehavior,
            json!({
                "behavior_id":"behavior", "agent_did":"owner",
                "context_id":"context", "inference_profile_id":"inference"
            }),
        ),
        (
            Collection::AgentContext,
            json!({
                "context_id":"context", "agent_did":"owner",
                "scope_behavior_id":scope, "tools_id":"tools",
                "compaction_id":"compaction", "skill_ids":["shared-skill"]
            }),
        ),
        (
            Collection::Tools,
            json!({
                "tools_id":"tools", "agent_did":"owner",
                "scope_behavior_id":scope
            }),
        ),
        (
            Collection::Compaction,
            json!({
                "compaction_id":"compaction", "agent_did":"owner",
                "scope_behavior_id":scope,
                "inference_profile_id":"compaction-inference"
            }),
        ),
        (
            Collection::InferenceProfile,
            json!({
                "profile_id":"inference", "agent_did":"owner",
                "scope_behavior_id":scope, "backend_id":"shared-backend",
                "model_name":"model", "sampling_id":"sampling",
                "execution_id":"execution"
            }),
        ),
        (
            Collection::InferenceProfile,
            json!({
                "profile_id":"compaction-inference", "agent_did":"owner",
                "scope_behavior_id":scope, "backend_id":"shared-backend",
                "model_name":"model", "sampling_id":"compaction-sampling",
                "execution_id":"compaction-execution"
            }),
        ),
        (
            Collection::InferenceSampling,
            json!({
                "sampling_id":"sampling", "agent_did":"owner",
                "scope_behavior_id":scope
            }),
        ),
        (
            Collection::InferenceSampling,
            json!({
                "sampling_id":"compaction-sampling", "agent_did":"owner",
                "scope_behavior_id":scope
            }),
        ),
        (
            Collection::InferenceExecution,
            json!({
                "execution_id":"execution", "agent_did":"owner",
                "scope_behavior_id":scope, "retry_policy_id":"retry"
            }),
        ),
        (
            Collection::InferenceExecution,
            json!({
                "execution_id":"compaction-execution", "agent_did":"owner",
                "scope_behavior_id":scope,
                "retry_policy_id":"compaction-retry"
            }),
        ),
        (
            Collection::InferenceRetryPolicy,
            json!({
                "retry_policy_id":"retry", "agent_did":"owner",
                "scope_behavior_id":scope
            }),
        ),
        (
            Collection::InferenceRetryPolicy,
            json!({
                "retry_policy_id":"compaction-retry", "agent_did":"owner",
                "scope_behavior_id":scope
            }),
        ),
        (
            Collection::InferenceBackend,
            json!({
                "backend_id":"shared-backend", "agent_did":"owner",
                "name":"Shared backend", "provider_kind":"OpenAiCompatible",
                "endpoint":"http://localhost:8000/v1",
                "auth":{"kind":"unauthenticated"}
            }),
        ),
        (
            Collection::Skill,
            json!({"skill_id":"shared-skill", "agent_did":"owner"}),
        ),
    ]
}

fn validate_behavior_scope_documents(documents: Vec<(Collection, Value)>) -> Result<()> {
    ConfigReferences::from_documents("owner", documents)?.validate()
}

#[test]
fn behavior_scope_graph_accepts_complete_scoped_and_legacy_closures() {
    validate_behavior_scope_documents(behavior_scope_documents(Some("behavior"))).unwrap();
    validate_behavior_scope_documents(behavior_scope_documents(None)).unwrap();
}

#[test]
fn behavior_scope_graph_rejects_mixed_closures_and_cross_behavior_aliases() {
    for (collection, id) in [
        (Collection::AgentContext, "context"),
        (Collection::Tools, "tools"),
        (Collection::Compaction, "compaction"),
        (Collection::InferenceSampling, "sampling"),
        (Collection::InferenceExecution, "execution"),
        (Collection::InferenceRetryPolicy, "retry"),
    ] {
        let mut documents = behavior_scope_documents(Some("behavior"));
        documents
            .iter_mut()
            .find(|(candidate, value)| {
                *candidate == collection && value[collection.unique_field()].as_str() == Some(id)
            })
            .unwrap()
            .1
            .as_object_mut()
            .unwrap()
            .remove("scope_behavior_id");
        assert!(
            validate_behavior_scope_documents(documents).is_err(),
            "mixed closure accepted unscoped {} {id}",
            collection.graphql_type()
        );
    }

    let mut documents = behavior_scope_documents(Some("behavior"));
    documents.push((
        Collection::AgentBehavior,
        json!({
            "behavior_id":"other", "agent_did":"owner",
            "inference_profile_id":"inference"
        }),
    ));
    let error = validate_behavior_scope_documents(documents)
        .unwrap_err()
        .to_string();
    assert!(error.contains("AgentBehavior other references InferenceProfile inference"));
}

#[test]
fn behavior_scope_graph_rejects_missing_behaviors_and_every_scoped_orphan_kind() {
    let mut missing = behavior_scope_documents(Some("behavior"));
    missing.push((
        Collection::AgentContext,
        json!({
            "context_id":"missing-behavior-context", "agent_did":"owner",
            "scope_behavior_id":"absent"
        }),
    ));
    let error = validate_behavior_scope_documents(missing)
        .unwrap_err()
        .to_string();
    assert!(error.contains("names a missing AgentBehavior"), "{error}");

    let orphans = [
        (
            Collection::AgentContext,
            json!({"context_id":"orphan-context", "agent_did":"owner", "scope_behavior_id":"behavior"}),
        ),
        (
            Collection::Tools,
            json!({"tools_id":"orphan-tools", "agent_did":"owner", "scope_behavior_id":"behavior"}),
        ),
        (
            Collection::Compaction,
            json!({"compaction_id":"orphan-compaction", "agent_did":"owner", "scope_behavior_id":"behavior"}),
        ),
        (
            Collection::InferenceProfile,
            json!({
                "profile_id":"orphan-profile", "agent_did":"owner",
                "scope_behavior_id":"behavior", "backend_id":"shared-backend",
                "model_name":"model"
            }),
        ),
        (
            Collection::InferenceSampling,
            json!({"sampling_id":"orphan-sampling", "agent_did":"owner", "scope_behavior_id":"behavior"}),
        ),
        (
            Collection::InferenceExecution,
            json!({"execution_id":"orphan-execution", "agent_did":"owner", "scope_behavior_id":"behavior"}),
        ),
        (
            Collection::InferenceRetryPolicy,
            json!({"retry_policy_id":"orphan-retry", "agent_did":"owner", "scope_behavior_id":"behavior"}),
        ),
    ];
    for (collection, orphan) in orphans {
        let mut documents = behavior_scope_documents(Some("behavior"));
        documents.push((collection, orphan));
        let error = validate_behavior_scope_documents(documents)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("is scoped to behavior \"behavior\" but is not reachable"),
            "{} orphan returned {error}",
            collection.graphql_type()
        );
    }
}

#[test]
fn ordinary_reference_errors_precede_behavior_scope_errors() {
    let mut documents = behavior_scope_documents(Some("behavior"));
    let behavior = documents
        .iter_mut()
        .find(|(collection, _)| *collection == Collection::AgentBehavior)
        .unwrap();
    behavior.1["inference_profile_id"] = json!("missing-profile");
    let error = validate_behavior_scope_documents(documents)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("references missing InferenceProfile \"missing-profile\""),
        "{error}"
    );
}

#[test]
fn candidate_tools_reuse_surface_expansion_and_collision_guards() {
    let tools = json!({"tools_id":"tools", "agent_did":"owner",
        "datastore":{"datastore_tool_surface_ids":["a", "b"]}});
    let surface = |id: &str, name: &str| {
        json!({"surface_id":id, "agent_did":"owner",
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
    let service = json!({"service_id":"service", "agent_did":"owner", "hostname":"localhost", "mcp_port":8000});
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
    let placement = json!({"repository_id":"repo", "agent_did":"owner", "host_path":"./checkout"});
    validate(Collection::RepositoryPlacement, placement.clone()).unwrap();
    let mut bad = placement;
    bad["host_path"] = json!(" ");
    assert!(validate(Collection::RepositoryPlacement, bad).is_err());
}

#[test]
fn callback_admission_reuses_projection_and_filter_secret_checks() {
    let callback = json!({"callback_id":"callback", "agent_did":"owner",
        "handler":{"kind":"built_in","emitter":"create_workspace"}});
    let source =
        json!({"event_source_id":"source", "agent_did":"owner", "source_collection":"Records"});
    let binding = json!({"binding_id":"binding", "agent_did":"owner",
        "callback_id":"callback", "event_source_id":"source", "input_fields":["name"]});
    let validate = |source, binding| {
        ConfigReferences::from_documents(
            "owner",
            [
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
    let base = json!({
        "agent_principal":{"agent_did":"owner"},
        "agent_behaviors":[{"agent_did":"owner","behavior_id":"behavior","inference_profile_id":"profile"}],
        "inference_profiles":[{"agent_did":"owner","profile_id":"profile","backend_id":"backend","model_name":"model"}],
        "inference_backends":[{"agent_did":"owner","backend_id":"backend","name":"Backend","provider_kind":"OpenAiCompatible",
            "endpoint":"http://localhost:8000/v1","auth":{"kind":"unauthenticated"}}],
        "tasks":[{"agent_did":"owner","task_id":"task","behavior_id":"behavior","prompt_template":"{{ ctx.now }}"}],
        "schedules":[{"agent_did":"owner","schedule_id":"schedule","cadence":{"kind":"interval","interval_secs":10}}],
        "event_sources":[{"agent_did":"owner","event_source_id":"event","source_collection":"Records"}],
        "triggers":[{"agent_did":"owner","trigger_id":"trigger","task_id":"task","source":{"kind":"schedule","schedule_id":"schedule"}}]
    });
    let validate = |value| {
        let config = serde_json::from_value::<super::super::PackConfig>(value).unwrap();
        let plan = crate::config_client::DesiredStateApplyPlan::from_pack_config(&config).unwrap();
        ConfigReferences::from_documents(
            "owner",
            plan.documents()
                .iter()
                .map(|doc| (doc.collection, doc.add.clone())),
        )?
        .validate()
    };
    validate(base.clone()).unwrap();
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
    use crate::config_client::{
        apply_desired_state_plan, read_desired_state_record_in_txn, ConfigAccess,
        DesiredStateApplyPlan,
    };
    let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let access = ConfigAccess::Local(node);
    let mut config = json!({
        "agent_principal":{"agent_did":"owner"},
        "agent_behaviors":[{"agent_did":"owner","behavior_id":"behavior","inference_profile_id":"profile"}],
        "inference_profiles":[{"agent_did":"owner","profile_id":"profile","backend_id":"backend","model_name":"model"}],
        "inference_backends":[{"agent_did":"owner","backend_id":"backend","name":"Backend","provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1","auth":{"kind":"unauthenticated"}}],
        "tasks":[{"agent_did":"owner","task_id":"task","behavior_id":"behavior","prompt_template":"{{ args.name }}"}],
        "event_sources":[{"agent_did":"owner","event_source_id":"event","source_collection":"Records"}]
    });
    let plan = DesiredStateApplyPlan::from_pack_config(
        &serde_json::from_value::<super::super::PackConfig>(config.clone()).unwrap(),
    )
    .unwrap();
    access
        .transact("test.manual_task", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap();
    config["tasks"][0]["prompt_template"] = json!("{{ args.other }}");
    config["triggers"] = json!([{"agent_did":"owner","trigger_id":"trigger","task_id":"task","source":{"kind":"event","event_source_id":"event"}}]);
    let plan = DesiredStateApplyPlan::from_pack_config(
        &serde_json::from_value::<super::super::PackConfig>(config.clone()).unwrap(),
    )
    .unwrap();
    assert!(access
        .transact("test.invalid_event_task", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await
        .is_err());
    access
        .transact("test.rollback", |txn| {
            Box::pin(async move {
                let (_, task) =
                    read_desired_state_record_in_txn(txn, Collection::Task, "owner", "task")
                        .await?
                        .unwrap();
                assert_eq!(task["prompt_template"], json!("{{ args.name }}"));
                assert!(read_desired_state_record_in_txn(
                    txn,
                    Collection::Trigger,
                    "owner",
                    "trigger"
                )
                .await?
                .is_none());
                Ok(())
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
        .transact("test.valid_event_task", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap();
}
