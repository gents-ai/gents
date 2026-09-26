//! Canonically accepted subagent bridges through the real source and cascade owners.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use crate::background_tools::r4c_args::{ListSubagentsArgs, ReadSubagentArgs};
use crate::background_tools::{
    handle_list_subagents, handle_read_subagent, load_steer_subagent_target, SteerSubagentTarget,
    AWAITING_CHILD_MATERIALIZATION,
};
use crate::config_client::{
    apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::identity::{AgentIdentity, RuntimePrincipal};
use crate::lifecycle::{ClaimOutcome, RequestLifecycle, RequestTerminalOutcome, TerminalizeResult};
use crate::runtime_snapshot::ActiveRuntimeSnapshot;
use crate::tool_call_lifecycle::admission_fixture::{
    claimed_request, publish_accepted_on_claimed_request, published_admission_with_owner,
    PublishedAdmission, PublishedAdmissionOptions,
};
use crate::tool_call_lifecycle::{
    AwaitMode, CancelCause, CancelPolicy, CascadeDispatch, ChildTerminal, FailureClass,
    ToolCallLifecycle,
};
use crate::{Collection, ConfigAccess};
use gents_protocol::output::TerminalOutput;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::json;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

struct Case {
    admission: PublishedAdmission,
    owner: RequestLifecycle,
    child_id: String,
}

async fn case(name: &str, policy: CancelPolicy) -> Case {
    let child_id = format!("child-{name}");
    let (mut admission, owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: name.into(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        cancel_policy: policy,
        spawn_plan: Some(crate::streaming::SpawnAdmissionPlan {
            tool_call_id: "bridge-native-tool".into(),
            child_request_id: child_id.clone(),
            spawn_target_did: "overridden-by-fixture".into(),
            spawn_behavior_id: "general".into(),
            delegated_workspace: None,
            await_mode: AwaitMode::Background,
        }),
        ..Default::default()
    })
    .await
    .expect("publish accepted subagent bridge");
    admission
        .tool
        .publish_background_receipt("child started")
        .await
        .expect("publish immutable background receipt");
    crate::test_support::install_test_behavior(&admission.node, &admission.agent_did, "general")
        .await;
    let documents = [
        (
            Collection::Tools,
            json!({
                "agent_did": admission.agent_did,
                "tools_id": "general:tools",
                "subagents": {"target_ids": ["cascade-target"], "spawn_enabled": true, "background_enabled": true, "allow_cross_principal": false}
            }),
        ),
        (
            Collection::SubagentTarget,
            json!({
                "agent_did": admission.agent_did,
                "target_id": "cascade-target",
                "name": "child",
                "target_agent_did": admission.agent_did,
                "behavior_id": "general"
            }),
        ),
    ];
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
    .expect("valid subagent authorization");
    ConfigAccess::transact_local(
        &admission.node,
        None,
        "test.accepted_cascade_source",
        |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        },
    )
    .await
    .expect("authorize accepted bridge");
    Case {
        admission,
        owner,
        child_id,
    }
}

async fn terminalize(case: &mut Case, outcome: RequestTerminalOutcome) {
    let header = case
        .admission
        .tool
        .accepted_header_doc_id()
        .expect("accepted assistant header");
    assert_eq!(
        case.owner
            .terminalize_owned(
                outcome,
                TerminalOutput::Message {
                    message_doc_id: header.into()
                },
                Some("staged parent terminal"),
            )
            .await
            .expect("terminalize claimed parent"),
        TerminalizeResult::Won
    );
}

fn source_snapshot(case: &Case) -> ActiveRuntimeSnapshot {
    let agent_did = &case.admission.agent_did;
    let identity: Arc<dyn AgentIdentity> = Arc::new(
        crate::KeyIdentity::load_or_create(case.admission.path.join("test-agent.key"), None)
            .expect("snapshot identity"),
    );
    assert_eq!(identity.did(), agent_did);
    let principal = Arc::new(RuntimePrincipal {
        agent_did: agent_did.into(),
        identity,
        default_behavior_id: "general".into(),
        display_name: None,
        enabled: true,
    });
    let behavior = crate::config::ResolvedBehavior {
        behavior_id: "general".into(),
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
        local_did: agent_did.into(),
        default_behavior_id: "general".into(),
        behaviors: HashMap::from([("general".into(), Arc::new(behavior))]),
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

async fn start_source(case: &Case) -> (CancellationToken, tokio::task::JoinHandle<()>) {
    let (snapshot_tx, rx) = watch::channel(Arc::new(source_snapshot(case)));
    let cancel = CancellationToken::new();
    let node = case.admission.node.clone();
    let source_cancel = cancel.clone();
    let handle = tokio::spawn(async move {
        let _snapshot_tx = snapshot_tx;
        crate::trigger_engine::run_subagent_source_for_test(
            node,
            rx,
            HashSet::new(),
            source_cancel,
        )
        .await;
    });
    (cancel, handle)
}

async fn wait_child(case: &Case) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let child_id = crate::graphql::escape_graphql_string(&case.child_id);
    loop {
        let response = case.admission.node.execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{child_id}" }} }}, limit: 2) {{ _docID caused_by_parent_tool_call_doc_id interrupt_requested_at }} }}"#,
        )).await;
        assert!(!response.has_errors(), "child query: {:?}", response.errors);
        let rows = response.data.as_ref().unwrap()["AgentRequest"]
            .as_array()
            .unwrap();
        assert!(rows.len() <= 1, "source must create one exact child");
        if let Some(child) = rows.first() {
            assert_eq!(
                child["caused_by_parent_tool_call_doc_id"],
                case.admission.tool.doc_id().unwrap()
            );
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "source did not materialize the child"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn assert_child_remains_uninterrupted(case: &Case) {
    tokio::time::sleep(Duration::from_millis(800)).await;
    let child_id = crate::graphql::escape_graphql_string(&case.child_id);
    let response = case.admission.node.execute(&format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{child_id}" }} }}, limit: 2) {{ interrupt_requested_at }} }}"#,
    )).await;
    assert!(!response.has_errors(), "child query: {:?}", response.errors);
    let rows = response.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0]["interrupt_requested_at"].is_null());
}

async fn finish(case: Case, cancel: CancellationToken, handle: tokio::task::JoinHandle<()>) {
    cancel.cancel();
    handle.await.expect("source joins");
    let PublishedAdmission {
        node, path, tool, ..
    } = case.admission;
    drop(tool);
    drop(case.owner);
    node.shutdown().await;
    std::fs::remove_dir_all(path).expect("remove exact cascade fixture database");
}

/// Lean's cascade trace requires an executing child to reach terminal
/// interrupted, beyond the source's durable interrupt latch alone.
#[tokio::test]
async fn canonical_source_cascade_interrupts_processing_child_trace() {
    let witness = crate::lean_vocab_test::lean_r6_background_theorem_witness(
        "Subagent.BridgedState.cascade_cancels_child",
    );
    assert_eq!(witness.numeric_bound, 2);
    assert_eq!(witness.kind_field("cancel_policy"), "cascade");
    assert_eq!(witness.kind_field("child_pre_state"), "processing");
    assert_eq!(witness.kind_field("child_pre_admission"), "executing");
    assert_eq!(witness.kind_field("child_post_state"), "interrupted");

    let mut case = case("processing-child-cascade-theorem", CancelPolicy::Cascade).await;
    let (cancel, handle) = start_source(&case).await;
    wait_child(&case).await;
    let child_id = crate::graphql::escape_graphql_string(&case.child_id);
    let response = case
        .admission
        .node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{child_id}" }} }}, limit: 2) {{ {} lifecycle_state }} }}"#,
            crate::watcher::AGENT_REQUEST_FIELDS,
        ))
        .await;
    let row: gents_protocol::row::AgentRequestRow =
        crate::graphql::first_row(&response, "AgentRequest")
            .expect("query source-materialized child")
            .expect("one physical child");
    assert_eq!(row.lifecycle_state, Some(RequestLifecycleState::Pending));
    let mut child = RequestLifecycle::new_with_agent_did(
        case.admission.node.clone(),
        "general",
        &case.admission.agent_did,
        row.try_into().expect("canonical child request"),
        60,
    );
    assert_eq!(
        child.claim().await.expect("claim child"),
        ClaimOutcome::Claimed
    );
    let writer = crate::streaming::DefraStreamWriter::new(
        case.admission.node.clone(),
        &case.admission.agent_did,
        Duration::ZERO,
    );
    child
        .begin_owned_execution(&writer)
        .await
        .expect("begin child execution");
    let child_doc_id = crate::graphql::escape_graphql_string(&child.request().doc_id);
    let processing = case
        .admission
        .node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{child_doc_id}" }} }}) {{ lifecycle_state }} }}"#,
        ))
        .await;
    assert!(
        !processing.has_errors(),
        "processing query: {:?}",
        processing.errors
    );
    assert_eq!(
        processing.data.expect("processing data")["AgentRequest"][0]["lifecycle_state"],
        witness.kind_field("child_pre_state")
    );

    let dispatch = case
        .admission
        .tool
        .cancel_during_run_with_cascade_dispatch(
            CancelCause::UserCancelled,
            &case.admission.agent_did,
        )
        .await
        .expect("cancel canonical accepted bridge")
        .expect("cascade dispatch");
    let CascadeDispatch::Local {
        intent,
        child: verified_child,
    } = dispatch
    else {
        panic!("same-principal child must dispatch locally");
    };
    assert_eq!(intent.child_request_id, case.child_id);
    assert_eq!(
        verified_child.doc_id.as_deref(),
        Some(child.request().doc_id.as_str())
    );
    crate::interrupt_request_by_doc_id(
        case.admission.node.as_ref(),
        verified_child
            .doc_id
            .as_deref()
            .expect("verified child document"),
        verified_child
            .agent_did
            .as_deref()
            .expect("verified child principal"),
        verified_child.requester_did.as_deref(),
    )
    .await
    .expect("latch exact child interruption");
    assert_eq!(
        child
            .terminalize_owned(
                RequestTerminalOutcome::Interrupted,
                TerminalOutput::NoMessage,
                Some("interrupted"),
            )
            .await
            .expect("terminalize owned child"),
        TerminalizeResult::Won
    );
    let tool_doc_id = crate::graphql::escape_graphql_string(
        case.admission
            .tool
            .doc_id()
            .expect("physical accepted tool"),
    );
    let result = case.admission.node.execute(&format!(
        r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{tool_doc_id}" }} }}) {{ cancel_cause cancel_cascade_intent_at }} AgentRequest(filter: {{ _docID: {{ _eq: "{child_doc_id}" }} }}) {{ lifecycle_state interrupt_requested_at }} }}"#,
    )).await;
    assert!(
        !result.has_errors(),
        "cascade trace query: {:?}",
        result.errors
    );
    let data = result.data.expect("cascade trace data");
    let tool = &data["AgentToolCall"][0];
    let child_row = &data["AgentRequest"][0];
    assert_eq!(tool["cancel_cause"], "userCancelled");
    assert!(tool["cancel_cascade_intent_at"].is_null());
    assert_eq!(
        child_row["lifecycle_state"],
        witness.kind_field("child_post_state")
    );
    assert!(child_row["interrupt_requested_at"].as_str().is_some());
    finish(case, cancel, handle).await;
}

/// A reserved accepted bridge stays observable before the source materializes
/// its child; no legacy in-memory bridge constructor can establish this state.
#[tokio::test]
async fn accepted_unmaterialized_child_remains_listed_readable_and_scoped() {
    let path = std::env::temp_dir().join(format!(
        "accepted-unmaterialized-child-{}",
        uuid::Uuid::new_v4()
    ));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&path)
            .build()
            .await
            .expect("build fixture node"),
    );
    crate::ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    let agent_did = "did:test:unmaterialized-owner";
    let remote_did = "did:key:z6MkUnclaimedRemoteTarget";
    let parent_request_id = "unmat-bg-parent";
    let session_id = "unmat-bg-session";
    let child_request_id = "unmat-bg-child";
    let call_id = "unmat-bg-tc";
    let mut parent = claimed_request(&node, parent_request_id, session_id, agent_did).await;
    let sibling = claimed_request(
        &node,
        "unmat-other-parent",
        "unmat-other-session",
        agent_did,
    )
    .await;
    let mut bridge = publish_accepted_on_claimed_request(
        node.clone(),
        &mut parent,
        agent_did,
        0,
        crate::toolset::SPAWN_SUBAGENT_TOOL_NAME,
        call_id,
        serde_json::json!({
            "name": "remote-coder",
            "agent_did": remote_did,
            "behavior_id": "remote-coder-behavior",
            "prompt": "cross-deployment child work",
            "await_mode": "background",
            "parent_subagent_depth": 0
        }),
        Some(crate::streaming::SpawnAdmissionPlan {
            tool_call_id: call_id.into(),
            child_request_id: child_request_id.into(),
            spawn_target_did: remote_did.into(),
            spawn_behavior_id: "remote-coder-behavior".into(),
            delegated_workspace: None,
            await_mode: AwaitMode::Background,
        }),
        AwaitMode::Background,
        CancelPolicy::Cascade,
        true,
    )
    .await
    .expect("publish accepted background bridge");
    assert!(bridge
        .publish_background_receipt("remote child accepted for background dispatch")
        .await
        .expect("publish one immutable receipt before background terminalization"));

    let escaped_child = crate::graphql::escape_graphql_string(child_request_id);
    let child = node.execute(&format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_child}" }} }}, limit: 1) {{ request_id }} }}"#,
    )).await;
    assert!(!child.has_errors(), "child query: {:?}", child.errors);
    assert!(child.data.unwrap()["AgentRequest"]
        .as_array()
        .unwrap()
        .is_empty());

    let all: ListSubagentsArgs =
        serde_json::from_value(serde_json::json!({"status":"all"})).unwrap();
    let entries = handle_list_subagents(&node, parent_request_id, all)
        .await
        .expect("list accepted bridge");
    let entry = entries
        .entries
        .iter()
        .find(|entry| entry.child_request_id == child_request_id)
        .expect("reserved unmaterialized child remains listed");
    assert_eq!(entry.status, AWAITING_CHILD_MATERIALIZATION);
    assert_eq!(entry.await_mode, "background");
    assert_eq!(entry.name.as_deref(), Some("remote-coder"));
    assert_eq!(entry.behavior_id.as_deref(), Some("remote-coder-behavior"));
    assert_eq!(entry.child_session_id, None);
    assert!(entry.diagnostic.as_deref().unwrap().contains(call_id));
    let running = handle_list_subagents(&node, parent_request_id, ListSubagentsArgs::default())
        .await
        .expect("default running list");
    assert!(running
        .entries
        .iter()
        .any(|entry| entry.child_request_id == child_request_id));

    let read_args: ReadSubagentArgs =
        serde_json::from_value(serde_json::json!({"child_request_id":child_request_id})).unwrap();
    let read = handle_read_subagent(node.as_ref(), parent_request_id, read_args)
        .await
        .expect("read accepted bridge")
        .expect("bridge-level read exists");
    assert!(!read.terminal);
    assert_eq!(read.lifecycle_state, AWAITING_CHILD_MATERIALIZATION);
    assert!(read.transcript.is_empty());
    assert_eq!(read.child_session_id, None);
    assert!(!read.has_more);
    assert!(read.diagnostic.as_deref().unwrap().contains(call_id));
    assert!(matches!(
        load_steer_subagent_target(node.as_ref(), parent_request_id, child_request_id)
            .await
            .expect("load pending steer target"),
        SteerSubagentTarget::AwaitingMaterialization {
            retryable: true,
            ..
        }
    ));
    assert!(bridge
        .bridge_failure(ChildTerminal::Failed {
            reason: "remote child never materialized".into(),
            failure_class: FailureClass::ServiceUnavailable,
        })
        .await
        .expect("terminalize accepted bridge"));
    assert!(matches!(
        load_steer_subagent_target(node.as_ref(), parent_request_id, child_request_id)
            .await
            .expect("load terminal steer target"),
        SteerSubagentTarget::Terminal(state) if state == "failed"
    ));

    let foreground_child = "unmat-fg-child";
    let foreground = publish_accepted_on_claimed_request(
        node.clone(),
        &mut parent,
        agent_did,
        1,
        crate::toolset::SPAWN_SUBAGENT_TOOL_NAME,
        "unmat-fg-tc",
        serde_json::json!({
            "name": "remote-foreground-coder",
            "agent_did": remote_did,
            "behavior_id": "remote-coder-behavior",
            "prompt": "cross-deployment foreground child work",
            "await_mode": "foreground",
            "parent_subagent_depth": 0
        }),
        Some(crate::streaming::SpawnAdmissionPlan {
            tool_call_id: "unmat-fg-tc".into(),
            child_request_id: foreground_child.into(),
            spawn_target_did: remote_did.into(),
            spawn_behavior_id: "remote-coder-behavior".into(),
            delegated_workspace: None,
            await_mode: AwaitMode::Foreground,
        }),
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        true,
    )
    .await
    .expect("publish accepted foreground bridge");
    assert!(foreground.doc_id().is_some());
    assert!(matches!(
        load_steer_subagent_target(node.as_ref(), parent_request_id, foreground_child)
            .await
            .expect("load foreground steer target"),
        SteerSubagentTarget::NotBackgrounded
    ));
    let stranger = handle_read_subagent(
        node.as_ref(),
        "unmat-other-parent",
        serde_json::from_value(serde_json::json!({"child_request_id":child_request_id})).unwrap(),
    )
    .await
    .expect("read sibling scope");
    assert!(stranger.is_none(), "sibling cannot read the reserved child");

    drop(foreground);
    drop(bridge);
    drop(parent);
    drop(sibling);
    node.shutdown().await;
    std::fs::remove_dir_all(path).expect("remove exact accepted fixture database");
}

#[tokio::test]
async fn interrupted_parent_recovery_leaves_spawned_child_running() {
    for policy in [CancelPolicy::Cascade, CancelPolicy::Detach] {
        let mut case = case(
            &format!("source-recovery-after-spawn-{}", policy.as_str()),
            policy,
        )
        .await;
        let (cancel, handle) = start_source(&case).await;
        wait_child(&case).await;
        terminalize(&mut case, RequestTerminalOutcome::Interrupted).await;
        ToolCallLifecycle::recover_all(&case.admission.node, &case.admission.agent_did)
            .await
            .unwrap();
        assert_child_remains_uninterrupted(&case).await;
        finish(case, cancel, handle).await;
    }
}

/// No parent terminal is a cancel signal for a child the source materializes
/// afterwards; only an explicit bridge cancellation is.
#[tokio::test]
async fn subagent_source_never_interrupts_child_for_parent_terminal() {
    for (outcome, policy) in [
        (RequestTerminalOutcome::Interrupted, CancelPolicy::Cascade),
        (RequestTerminalOutcome::Interrupted, CancelPolicy::Detach),
        (RequestTerminalOutcome::Dead, CancelPolicy::Cascade),
        (RequestTerminalOutcome::Completed, CancelPolicy::Cascade),
    ] {
        let mut case = case(
            &format!("source-parent-{outcome:?}-{}", policy.as_str()).to_lowercase(),
            policy,
        )
        .await;
        terminalize(&mut case, outcome).await;
        let (cancel, handle) = start_source(&case).await;
        wait_child(&case).await;
        assert_child_remains_uninterrupted(&case).await;
        finish(case, cancel, handle).await;
    }
}
