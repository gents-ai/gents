use super::*;

use crate::config_client::{
    apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::identity::{AgentIdentity, RuntimePrincipal};
use crate::lean_vocab_test::{LeanDelegatedChildChoice, LeanDelegatedChildResolutionCase};
use crate::tool_call_lifecycle::IllegalToolCallTransition;
use crate::{Collection, KeyIdentity};
use gents_protocol::output::{DelegatedToolInput, DelegatedWorkspace, PayloadRef};
use serde_json::{json, Value};

pub(super) fn receiver_snapshot(
    identity: Arc<dyn AgentIdentity>,
    behavior_id: &str,
) -> ActiveRuntimeSnapshot {
    let did = identity.did().to_owned();
    let principal = Arc::new(RuntimePrincipal {
        agent_did: did.clone(),
        identity,
        default_behavior_id: behavior_id.to_owned(),
        display_name: None,
        enabled: true,
    });
    let behavior = crate::config::ResolvedBehavior {
        behavior_id: behavior_id.to_owned(),
        principal,
        backend_id: None,
        backend_provider_kind: crate::BackendProviderKind::OpenAiCompatible,
        openai_wire_api: crate::OpenAiWireApi::ChatCompletions,
        backend_endpoint: "http://127.0.0.1:1/v1".into(),
        backend_auth: crate::document_config::BackendAuth::Unauthenticated,
        model_name: crate::config::DEFAULT_MODEL_NAME.into(),
        resolved_reasoning_efforts: None,
        context_window: crate::config::DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: crate::config::DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: crate::config::DEFAULT_MAX_TURNS,
        max_turns_provenance: crate::config::MaxTurnsProvenance::Default,
        system_prompt: String::new(),
        tools: crate::BehaviorToolConfig::default(),
        compaction: None,
        compaction_inference: None,
        max_total_tokens: None,
        stream_batch_ms: crate::config::DEFAULT_STREAM_BATCH_MS,
        stream_liveness_timeout: Duration::from_secs(
            crate::config::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
        ),
        deadline_duration: Duration::from_secs(crate::config::DEFAULT_DEADLINE_DURATION_SECS),
        provider_idle_timeout: Duration::from_secs(
            crate::config::DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS,
        ),
        completion_retry: crate::agent::completion_retry::CompletionRetryProfileFields::default(),
        sampling: crate::config::SamplingConfig::default(),
        skills: Vec::new(),
    };
    ActiveRuntimeSnapshot {
        generation: 1,
        principal: None,
        local_did: did,
        default_behavior_id: behavior_id.to_owned(),
        behaviors: HashMap::from([(behavior_id.to_owned(), Arc::new(behavior))]),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: HashMap::new(),
        active_schedules: HashMap::new(),
        unavailable_schedules: HashSet::new(),
        active_event_triggers: HashMap::new(),
        unavailable_event_triggers: HashSet::new(),
        active_tasks: HashMap::new(),
        dispatchers: Default::default(),
        behavior_executor_capacities: HashMap::new(),
        behavior_executor_queue_capacities: HashMap::new(),
    }
}

pub(super) async fn install_cross_deployment_behavior(
    node: &EmbeddedNode,
    did: &str,
    behavior_id: &str,
) {
    crate::test_support::install_test_behavior(node, did, behavior_id).await;
    let tools = json!({
        "agent_did": did,
        "tools_id": format!("{behavior_id}:tools"),
        "subagents": {"spawn_enabled": true, "background_enabled": true,
            "allow_cross_principal": true}
    });
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Tools,
        add: tools.clone(),
        update: tools,
    }])
    .unwrap();
    ConfigAccess::transact_local(node, None, "test.delegated_child_behavior", |txn| {
        let plan = &plan;
        Box::pin(async move { apply_desired_state_plan(txn, plan).await })
    })
    .await
    .unwrap();
}

async fn seed_observed_workspace(
    node: &EmbeddedNode,
    case: &LeanDelegatedChildResolutionCase,
    owner: &str,
) -> tempfile::TempDir {
    let stamp = case.parent_workspace.as_ref().unwrap();
    assert_eq!(stamp.workspace_owner_agent_did, case.child_agent);
    let observed = match &case.choice {
        LeanDelegatedChildChoice::Inherit { workspace }
        | LeanDelegatedChildChoice::Bind { workspace, .. } => workspace,
        _ => panic!("receiver fixture only binds inherit and bind"),
    };
    assert_eq!(stamp.workspace_id, observed.workspace_id);
    assert_eq!(
        stamp.workspace_owner_agent_did,
        observed.workspace_owner_agent_did
    );
    assert_eq!(stamp.workspace_seal_hash, observed.workspace_seal_hash);
    assert!(observed.available);
    let guard = tempfile::tempdir().unwrap();
    let host_path = guard.path().join("workspace");
    std::fs::create_dir(&host_path).unwrap();
    let workspace_id = format!("lean-workspace-{}", observed.workspace_id);
    let document = crate::workspace::IsolatedWorkspaceDoc {
        path_capability: crate::workspace::WorkspacePathCapability::exact_paths(vec![]).unwrap(),
        workspace_id: workspace_id.clone(),
        work_unit_id: format!("lean-work-unit-{}", case.name),
        repository_id: format!("lean-repository-{}", case.name),
        base_sha: "lean-base".into(),
        branch: format!("lean-branch-{}", case.name),
        creation_policy: "git_worktree_diff".into(),
        adapter: "git_worktree".into(),
        owner_agent_did: owner.into(),
        writer_principal: owner.into(),
        integrator_principal: owner.into(),
        instruction_manifest: "{}".into(),
        seal_hash: observed
            .workspace_seal_hash
            .map(|value| format!("lean-seal-{value}")),
        lifecycle_state: observed.state.clone(),
        caused_by_invocation_id: format!("lean-invocation-{}", case.name),
        caused_by_correlation: format!("lean-correlation-{}", case.name),
    };
    let placement = crate::workspace::WorkspacePlacementDoc {
        workspace_id,
        owner_agent_did: owner.into(),
        host_path: host_path.to_string_lossy().into_owned(),
        repository_placement_id: format!("lean-placement-{}", case.name),
        adapter: "git_worktree".into(),
        adapter_version: "1".into(),
        dirty_base: false,
        dirty_base_summary: String::new(),
        provisioning_state: "ready".into(),
        observed_tree_hash: String::new(),
    };
    for mutation in [
        crate::workspace::isolated_workspace_upsert_mutation(&document),
        crate::workspace::workspace_placement_upsert_mutation(
            &placement,
            &Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        ),
    ] {
        ConfigAccess::write_local(node, "test.delegated_child_workspace", &mutation)
            .await
            .unwrap();
    }
    guard
}

async fn projected_bridge(
    node: &EmbeddedNode,
    case: &LeanDelegatedChildResolutionCase,
    coordinator: &str,
    host: &str,
    behavior_id: &str,
) -> (String, String, String, String, String) {
    let copied = case.delegated_input.as_ref().unwrap();
    let args: Value = serde_json::from_str(&copied.arguments).unwrap();
    assert_eq!(args["name"].as_str(), Some(behavior_id));
    match &case.choice {
        // An absent workspace argument invokes the existing native default:
        // inherit when the copied parent stamp is present.
        LeanDelegatedChildChoice::Inherit { .. } => assert!(args.get("workspace").is_none()),
        LeanDelegatedChildChoice::Bind {
            workspace,
            requested_authority,
        } => {
            assert_eq!(
                args["workspace"]["id"],
                format!("lean-workspace-{}", workspace.workspace_id)
            );
            assert_eq!(
                args["workspace"]["authority"].as_str(),
                requested_authority.as_deref()
            );
        }
        _ => panic!("receiver fixture only binds inherit and bind"),
    }
    let input = DelegatedToolInput {
        source: PayloadRef {
            close_doc_id: format!("lean-close-{}", copied.source_close_doc_id),
            stream: u32::try_from(copied.source_stream).unwrap(),
        },
        arguments: copied.arguments.clone(),
        parent_subagent_depth: copied.parent_subagent_depth,
    };
    let stamp = case.parent_workspace.as_ref().unwrap();
    assert_eq!(stamp.workspace_owner_agent_did, case.child_agent);
    let workspace = DelegatedWorkspace {
        workspace_id: format!("lean-workspace-{}", stamp.workspace_id),
        workspace_owner_agent_did: host.into(),
        workspace_authority: stamp.workspace_authority.clone(),
        workspace_seal_hash: stamp
            .workspace_seal_hash
            .map(|value| format!("lean-seal-{value}")),
    };
    let input_literal = gents_protocol::graphql::graphql_input_literal(&json!(input)).unwrap();
    let workspace_literal =
        gents_protocol::graphql::graphql_input_literal(&json!(workspace)).unwrap();
    let request_id = format!("lean-parent-{}", case.name);
    let request_doc_id = format!("lean-absent-parent-doc-{}", case.name);
    let tool_call_id = format!("lean-call-{}", case.name);
    let child_id = format!("lean-child-{}", case.name);
    let session_id = format!("lean-session-{}", case.name);
    let key = format!("{session_id}:{tool_call_id}");
    let started = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let deadline =
        (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let mutation = format!(
        r#"mutation {{ create_AgentToolCall(input: {{
            tool_call_key: "{}", request_id: "{}", request_doc_id: "{}",
            session_id: "{}", agent_did: "{}", message_sequence: 1,
            tool_name: "spawn_subagent", tool_call_id: "{}",
            delegated_input: {input_literal}, delegated_workspace: {workspace_literal},
            lifecycle_state: "running", started_at: "{started}", deadline_at: "{deadline}",
            child_request_id: "{}", spawn_target_did: "{}",
            spawn_behavior_id: "{}", await_mode: "background", cancel_policy: "cascade"
        }}) {{ _docID }} }}"#,
        escape_graphql_string(&key),
        escape_graphql_string(&request_id),
        escape_graphql_string(&request_doc_id),
        escape_graphql_string(&session_id),
        escape_graphql_string(coordinator),
        escape_graphql_string(&tool_call_id),
        escape_graphql_string(&child_id),
        escape_graphql_string(host),
        escape_graphql_string(behavior_id),
    );
    let response = ConfigAccess::write_local(node, "test.projected_delegated_bridge", &mutation)
        .await
        .unwrap();
    let bridge_doc_id = crate::graphql::created_doc_id(&response, "AgentToolCall").unwrap();
    (
        bridge_doc_id,
        request_id,
        request_doc_id,
        tool_call_id,
        child_id,
    )
}

/// This binds the receiver to the model's accepted copied bridge fields. The
/// separate native publication test owns their provenance; this fixture does
/// not claim P2P delivery, document ACP, or an atomic cross-node transaction.
#[tokio::test]
async fn generated_delegated_child_cases_bind_host_receiver() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().delegated_child_resolution_cases;
    for name in [
        "remote_depth_two_inherit_at_bound",
        "remote_depth_three_rejects_child",
        "readonly_parent_bind_readwrite_attenuates",
    ] {
        let case = cases.iter().find(|case| case.name == name).unwrap();
        assert_eq!(case.parent_agent, 1);
        assert_eq!(case.child_agent, 2);
        let key_dir = tempfile::tempdir().unwrap();
        let host_identity: Arc<dyn AgentIdentity> =
            Arc::new(KeyIdentity::load_or_create(key_dir.path().join("host.key"), None).unwrap());
        let coordinator =
            KeyIdentity::load_or_create(key_dir.path().join("coordinator.key"), None).unwrap();
        let host = host_identity.did().to_owned();
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let behavior_id = "lean-behavior-8";
        install_cross_deployment_behavior(&node, &host, behavior_id).await;
        let workspace_guard = seed_observed_workspace(&node, case, &host).await;
        let (bridge_doc_id, parent_id, parent_doc_id, tool_id, child_id) =
            projected_bridge(&node, case, coordinator.did(), &host, behavior_id).await;
        let parent_query = format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            escape_graphql_string(&parent_doc_id),
        );
        let parent = graphql_with_transaction_retry(
            &node,
            &parent_query,
            "test.delegated_child_absent_parent",
        )
        .await
        .unwrap();
        assert_eq!(
            parent.data.as_ref().unwrap()["AgentRequest"]
                .as_array()
                .map(Vec::len),
            Some(0)
        );

        let snapshot = receiver_snapshot(host_identity, behavior_id);
        let (_tx, rx) = watch::channel(Arc::new(snapshot));
        let mut source = SubagentSource::with_subscription_source_for_test(
            node.clone(),
            rx,
            node.clone(),
            HashSet::from([coordinator.did().to_owned()]),
            CancellationToken::new(),
        );
        let result = source.build_intent_for_tool_call_doc(&bridge_doc_id).await;
        let child_query = format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            escape_graphql_string(&child_id),
            crate::request_admission::SIGNED_REQUEST_FIELDS,
        );
        let child_response =
            graphql_with_transaction_retry(&node, &child_query, "test.delegated_child_observe")
                .await
                .unwrap();
        let child_data = child_response.data.as_ref().unwrap();
        let rows = child_data["AgentRequest"].as_array().unwrap();
        match &case.expected {
            None => {
                let error = match result {
                    Err(error) => error,
                    Ok(_) => panic!("modeled depth rejection must reach child owner"),
                };
                assert!(matches!(
                    error.downcast_ref::<IllegalToolCallTransition>(),
                    Some(IllegalToolCallTransition::SubagentDepthExceeded)
                ));
                assert!(rows.is_empty(), "rejected case {name} wrote a child");
            }
            Some(expected) => {
                let intent = result.unwrap().expect("modeled receiver creates a child");
                assert_eq!(
                    intent.pre_materialized_request_id.as_deref(),
                    Some(child_id.as_str())
                );
                assert_eq!(rows.len(), 1, "case {name} must create one signed child");
                let child: AgentRequestRow = serde_json::from_value(rows[0].clone()).unwrap();
                assert_eq!(child.request_id, child_id);
                assert_eq!(child.agent_did.as_deref(), Some(host.as_str()));
                assert_eq!(child.requester_did.as_deref(), Some(coordinator.did()));
                assert_eq!(child.subagent_depth, Some(i64::from(expected.child_depth)));
                let copied: Value =
                    serde_json::from_str(&case.delegated_input.as_ref().unwrap().arguments)
                        .unwrap();
                assert_eq!(child.content.as_deref(), copied["prompt"].as_str());
                assert_eq!(
                    child.caused_by_parent_request_id.as_deref(),
                    Some(parent_id.as_str())
                );
                assert_eq!(
                    child.caused_by_parent_request_doc_id.as_deref(),
                    Some(parent_doc_id.as_str())
                );
                assert_eq!(
                    child.caused_by_parent_tool_call_id.as_deref(),
                    Some(tool_id.as_str())
                );
                assert_eq!(
                    child.caused_by_parent_tool_call_doc_id.as_deref(),
                    Some(bridge_doc_id.as_str())
                );
                assert_eq!(
                    rows[0]["admission_signer_did"].as_str(),
                    Some(host.as_str())
                );
                assert!(rows[0]["admission_signature"]
                    .as_str()
                    .is_some_and(|value| !value.is_empty()));
                crate::request_admission::verify_request_receipt_signature(&child)
                    .expect("signed child receipt must verify through the admission owner");
                let stamp = expected.child_workspace.as_ref().unwrap();
                assert_eq!(
                    child.workspace_id.as_deref(),
                    Some(format!("lean-workspace-{}", stamp.workspace_id).as_str())
                );
                assert_eq!(
                    child.workspace_owner_agent_did.as_deref(),
                    Some(host.as_str())
                );
                assert_eq!(
                    child.workspace_authority.as_deref(),
                    Some(stamp.workspace_authority.as_str())
                );
                assert_eq!(
                    child.workspace_seal_hash.as_deref(),
                    stamp
                        .workspace_seal_hash
                        .map(|value| format!("lean-seal-{value}"))
                        .as_deref()
                );
            }
        }
        drop(source);
        node.shutdown().await;
        drop(workspace_guard);
    }
}
