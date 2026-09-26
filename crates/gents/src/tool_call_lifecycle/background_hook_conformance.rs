//! Background bridge conformance at the production projector and child-source
//! seams. Native admissions come from canonical assistant publication.

use crate::background_completion::{
    project_background_subagent_completion, BackgroundCompletionOutcome,
};
use crate::graphql::escape_graphql_string;
use crate::identity::AgentIdentity;
use crate::lean_vocab_test::{lean_bridge_step_cases, LeanBridgeStepCase};
use crate::streaming::SpawnAdmissionPlan;
use crate::tool_call_lifecycle::admission_fixture::{
    complete_child, published_admission, PublishedAdmissionOptions,
};
use crate::tool_call_lifecycle::{AwaitMode, CancelCause, CancelPolicy, ToolCallLifecycle};
use std::sync::Arc;

struct PendingTool;

impl crate::llm::tool::ToolDyn for PendingTool {
    fn name(&self) -> String {
        "slow_tool".into()
    }
    fn definition<'a>(
        &'a self,
        _prompt: String,
    ) -> crate::llm::tool::BoxFuture<'a, crate::llm::tool::ToolDefinition> {
        Box::pin(async {
            crate::llm::tool::ToolDefinition {
                name: "slow_tool".into(),
                description: "test tool".into(),
                parameters: serde_json::json!({"type":"object"}),
            }
        })
    }
    fn call<'a>(
        &'a self,
        _args: String,
    ) -> crate::llm::tool::BoxFuture<'a, Result<String, crate::llm::tool::ToolError>> {
        Box::pin(std::future::pending())
    }
}

async fn set_request_state(node: &crate::defra_node::EmbeddedNode, request_id: &str, state: &str) {
    let request_id = escape_graphql_string(request_id);
    let state = escape_graphql_string(state);
    let response = node.execute(&format!(r#"mutation {{ update_AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, input: {{ lifecycle_state: "{state}" }}) {{ _docID }} }}"#)).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
}

async fn tool_state(node: &crate::defra_node::EmbeddedNode, tool_id: &str) -> Option<String> {
    let tool_id = escape_graphql_string(tool_id);
    let response = node.execute(&format!(r#"{{ AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{tool_id}" }} }}, limit: 1) {{ lifecycle_state }} }}"#)).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row: serde_json::Value = crate::graphql::first_row(&response, "AgentToolCall")
        .unwrap()
        .unwrap();
    row["lifecycle_state"].as_str().map(str::to_owned)
}

async fn bridge_fixture(
    case: &LeanBridgeStepCase,
) -> (
    crate::tool_call_lifecycle::admission_fixture::PublishedAdmission,
    String,
) {
    let child_id = format!("bridge-step-child-{}", case.name);
    let cancel_policy = match case.cancel_policy.as_str() {
        "cascade" => CancelPolicy::Cascade,
        "detach" => CancelPolicy::Detach,
        other => panic!("unknown modeled cancel policy {other}"),
    };
    let mut admission = published_admission(PublishedAdmissionOptions {
        name: format!("bridge-step-{}", case.name),
        real_identity: true,
        await_mode: AwaitMode::Background,
        cancel_policy,
        spawn_plan: Some(SpawnAdmissionPlan {
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
    .expect("publish accepted bridge");
    crate::test_support::install_test_behavior(&admission.node, &admission.agent_did, "general")
        .await;
    admission
        .tool
        .publish_background_receipt("child started")
        .await
        .unwrap();
    let parent_doc_id = admission
        .tool
        .request_doc_id()
        .expect("accepted parent doc id")
        .to_owned();
    let tool_doc_id = admission
        .tool
        .doc_id()
        .expect("accepted tool doc id")
        .to_owned();
    crate::tool_call_lifecycle::create_subagent_request_with_request_id(
        admission.node.as_ref(),
        child_id.clone(),
        format!("request-bridge-step-{}", case.name),
        parent_doc_id,
        admission.tool.tool_call_id().to_owned(),
        tool_doc_id,
        0,
        admission.agent_did.clone(),
        "general".into(),
        format!("prompt for {}", case.name),
        Some(chrono::Utc::now() + chrono::Duration::minutes(4)),
    )
    .await
    .expect("materialize canonical bridge child");
    if case.parent_state == "interrupted" {
        set_request_state(
            &admission.node,
            &format!("request-bridge-step-{}", case.name),
            "interrupted",
        )
        .await;
    }
    (admission, child_id)
}

async fn publish_hook_action(
    hook: &crate::hook::DefraSessionHook,
    writer: &crate::streaming::DefraStreamWriter,
    owner: &crate::lifecycle::RequestLifecycle,
    turn: usize,
    internal_id: &str,
    tool_name: &str,
    args: serde_json::Value,
) -> (crate::llm::ToolCallHookAction, String) {
    writer
        .start_provider_attempt(
            &owner.request().doc_id,
            turn,
            0,
            format!("inference.{}", turn + 1).parse().unwrap(),
        )
        .await;
    let message = gents_protocol::message::Message::Assistant {
        id: Some(format!("hook-assistant-{internal_id}")),
        content: vec![gents_protocol::message::AssistantContent::ToolCall(
            gents_protocol::message::ToolCall {
                id: internal_id.into(),
                call_id: None,
                function: gents_protocol::message::ToolFunction::new(
                    tool_name.into(),
                    args.clone(),
                ),
                signature: None,
                additional_params: None,
            },
        )],
    };
    let mut published = writer
        .publish_native_turn(owner, turn, 0, &message)
        .await
        .unwrap();
    assert_eq!(published.accepted_tools.len(), 1);
    let accepted = published.accepted_tools.pop().unwrap();
    hook.register_stream_tool_call_identity(internal_id, &accepted.id, None)
        .await;
    hook.adopt_accepted_tool_calls(vec![(internal_id.into(), accepted)])
        .await
        .unwrap();
    let action = hook
        .on_tool_call(tool_name, None, internal_id, &args.to_string())
        .await;
    (action, published.message_doc_id)
}

async fn publish_hook_call(
    hook: &crate::hook::DefraSessionHook,
    writer: &crate::streaming::DefraStreamWriter,
    owner: &crate::lifecycle::RequestLifecycle,
    turn: usize,
    internal_id: &str,
    tool_name: &str,
    args: serde_json::Value,
) -> (serde_json::Value, String) {
    let (action, header) =
        publish_hook_action(hook, writer, owner, turn, internal_id, tool_name, args).await;
    let crate::llm::ToolCallHookAction::Skip { reason } = action else {
        panic!("accepted hook call {internal_id} did not return a result: {action:?}");
    };
    (serde_json::from_str(&reason).unwrap(), header)
}

#[tokio::test]
async fn generated_absent_requester_process_control_uses_accepted_hook_calls() {
    let path = std::env::temp_dir().join(format!("background-hook-scope-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    let identity = crate::KeyIdentity::load_or_create(path.join("agent.key"), None).unwrap();
    let did = identity.did().to_owned();
    let node = Arc::new(
        crate::defra_node::EmbeddedNode::builder()
            .data_path(&path)
            .with_node_identity_did(&did)
            .build()
            .await
            .unwrap(),
    );
    crate::schema::ensure_runtime_schemas(&node).await.unwrap();
    crate::test_support::install_test_behavior(&node, &did, "general").await;
    let session_id = "background-hook-scope-session";
    let request_id = "background-hook-scope-origin";
    let mut origin = crate::tool_call_lifecycle::admission_fixture::claimed_request(
        &node, request_id, session_id, &did,
    )
    .await;
    let writer =
        crate::streaming::DefraStreamWriter::new(node.clone(), &did, std::time::Duration::ZERO);
    origin.begin_owned_execution(&writer).await.unwrap();
    let registry = crate::hook::BackgroundToolRegistry::from_tools(
        vec![Box::new(PendingTool)],
        &["slow_tool".into()],
    );
    let hook = crate::hook::DefraSessionHook::resume_with_identity_policy(
        node.clone(),
        session_id,
        "general",
        &did,
        None,
        crate::hook::FailurePolicy::default(),
    )
    .await
    .unwrap()
    .with_background_tool_registry(registry);
    hook.set_active_request_binding(
        Some(request_id.into()),
        Some(origin.request().doc_id.clone()),
        None,
    )
    .await;
    hook.set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::minutes(5)))
        .await;
    let (spawned, header_doc_id) = publish_hook_call(
        &hook,
        &writer,
        &origin,
        0,
        "scope-spawn",
        "spawn_process",
        serde_json::json!({"tool_name":"slow_tool","args":{}}),
    )
    .await;
    assert_eq!(spawned["ok"], true);
    let handle = spawned["tool_call_id"]
        .as_str()
        .expect("background handle")
        .to_owned();
    let row = node.execute(&format!(r#"{{ AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{}" }} }}, limit: 1) {{ requester_did }} }}"#, escape_graphql_string(&handle))).await;
    assert!(!row.has_errors(), "{:?}", row.errors);
    let row: serde_json::Value = crate::graphql::first_row(&row, "AgentToolCall")
        .unwrap()
        .unwrap();
    assert!(row
        .get("requester_did")
        .is_some_and(serde_json::Value::is_null));

    // Exercise the originating-request denial while its owner is live. The
    // owned completion boundary must close it before a second request can
    // claim the same principal's execution slot.
    let originating_name = "originating_request_without_matching_requester_is_denied";
    let originating_case = crate::lean_vocab_test::lean_r6_backgrounding_case(originating_name);
    assert!(!originating_case.legal);
    let background = ToolCallLifecycle::load(node.clone(), session_id, &handle)
        .await
        .unwrap()
        .expect("persisted background owner");
    let originating_scope = crate::background_tools::ProcessControlScope {
        request_id: request_id.into(),
        session_id: session_id.into(),
        agent_did: did.clone(),
        requester_did: Some("did:requester".into()),
    };
    assert!(
        !originating_scope.authorizes(
            background.session_id(),
            background.agent_did(),
            background.requester_did(),
        ),
        "{originating_name}: persisted owner must reject mismatched requester"
    );
    hook.set_active_request_binding(
        Some(request_id.into()),
        Some(origin.request().doc_id.clone()),
        Some("did:requester".into()),
    )
    .await;
    let (denied, _) = publish_hook_action(
        &hook,
        &writer,
        &origin,
        1,
        "scope-origin-denied",
        "read_process",
        serde_json::json!({"tool_call_id":handle}),
    )
    .await;
    let crate::llm::ToolCallHookAction::Terminate { reason } = denied else {
        panic!("{originating_name}: mismatched physical header must fail closed: {denied:?}");
    };
    assert!(
        reason.contains("canonical header is unresolved or unauthorized"),
        "{originating_name}: {reason}"
    );

    assert_eq!(
        origin
            .terminalize_owned(
                crate::lifecycle::RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::Message {
                    message_doc_id: header_doc_id
                },
                None,
            )
            .await
            .unwrap(),
        crate::lifecycle::TerminalizeResult::Won
    );

    let next_id = "background-hook-scope-next";
    let mut next = crate::tool_call_lifecycle::admission_fixture::claimed_request(
        &node, next_id, session_id, &did,
    )
    .await;
    let next_writer =
        crate::streaming::DefraStreamWriter::new(node.clone(), &did, std::time::Duration::ZERO);
    next.begin_owned_execution(&next_writer).await.unwrap();
    let absent_name = "absent_requester_next_turn_authorized";
    assert!(crate::lean_vocab_test::lean_r6_backgrounding_case(absent_name).legal);
    hook.set_active_request_binding(
        Some(next_id.into()),
        Some(next.request().doc_id.clone()),
        None,
    )
    .await;
    let (read, _) = publish_hook_call(
        &hook,
        &next_writer,
        &next,
        0,
        "scope-read-absent",
        "read_process",
        serde_json::json!({"tool_call_id":handle}),
    )
    .await;
    assert_eq!(read["status"], "running", "{absent_name}: {read}");
    assert_eq!(read["tool_call_id"], handle, "{absent_name}");

    let empty_name = "empty_requester_does_not_alias_absent";
    assert!(!crate::lean_vocab_test::lean_r6_backgrounding_case(empty_name).legal);
    let empty_scope = crate::background_tools::ProcessControlScope {
        request_id: next_id.into(),
        session_id: session_id.into(),
        agent_did: did.clone(),
        requester_did: Some(String::new()),
    };
    assert!(
        !empty_scope.authorizes(
            background.session_id(),
            background.agent_did(),
            background.requester_did(),
        ),
        "{empty_name}: empty requester must not alias absence"
    );
    hook.set_active_request_binding(
        Some(next_id.into()),
        Some(next.request().doc_id.clone()),
        Some(String::new()),
    )
    .await;
    let (denied, _) = publish_hook_call(
        &hook,
        &next_writer,
        &next,
        1,
        "scope-read-empty",
        "read_process",
        serde_json::json!({"tool_call_id":handle}),
    )
    .await;
    assert_eq!(denied["ok"], false, "{empty_name}: {denied}");
    assert_eq!(
        denied["failure_class"], "tool_not_allowed",
        "{empty_name}: {denied}"
    );
    assert_eq!(tool_state(&node, &handle).await.as_deref(), Some("running"));
    let escaped_handle = escape_graphql_string(&handle);
    let response = node.execute(&format!(r#"{{ AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{escaped_handle}" }} }}, limit: 1) {{ cancel_cause }} }}"#)).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row: serde_json::Value = crate::graphql::first_row(&response, "AgentToolCall")
        .unwrap()
        .unwrap();
    assert!(
        row["cancel_cause"].is_null(),
        "denied reads must not cancel the background process"
    );
    hook.set_active_request_binding(
        Some(next_id.into()),
        Some(next.request().doc_id.clone()),
        None,
    )
    .await;
    let (cancelled, _) = publish_hook_call(
        &hook,
        &next_writer,
        &next,
        2,
        "scope-cleanup",
        "cancel_process",
        serde_json::json!({"tool_call_id":handle}),
    )
    .await;
    assert_eq!(cancelled["status"], "cancelled");
    node.shutdown().await;
    std::fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn generated_bridge_steps_drive_real_background_projector_and_cascade() {
    let cases = lean_bridge_step_cases();
    assert_eq!(cases.len(), 11);
    let mut driven = 0;
    for case in cases {
        if !case.bridge_committed {
            assert!(!case.legal);
            assert!(case.post_tool_state.is_none());
            continue;
        }
        let (mut admission, child_id) = bridge_fixture(case).await;
        match case.event.as_str() {
            "bridge_complete" | "bridge_failure" => {
                if case.child_state == "completed" {
                    complete_child(
                        &admission.node,
                        &child_id,
                        &admission.agent_did,
                        "bridge child final",
                    )
                    .await;
                } else {
                    set_request_state(&admission.node, &child_id, &case.child_state).await;
                }
                let outcome = project_background_subagent_completion(
                    admission.node.clone(),
                    &child_id,
                    &admission.agent_did,
                )
                .await
                .expect("project durable child terminal");
                let state = tool_state(&admission.node, admission.tool.tool_call_id()).await;
                if case.legal {
                    assert!(
                        matches!(outcome, BackgroundCompletionOutcome::Projected { .. }),
                        "{}: {outcome:?}",
                        case.name
                    );
                    assert_eq!(
                        state.as_deref(),
                        case.post_tool_state.as_deref(),
                        "{}",
                        case.name
                    );
                } else if case.child_state == "processing" {
                    assert!(
                        matches!(outcome, BackgroundCompletionOutcome::NotTerminal),
                        "{}: {outcome:?}",
                        case.name
                    );
                    assert_eq!(state.as_deref(), Some("running"), "{}", case.name);
                } else {
                    assert_eq!(case.child_state, "completed", "{}", case.name);
                    assert!(
                        matches!(outcome, BackgroundCompletionOutcome::Projected { .. }),
                        "{}: {outcome:?}",
                        case.name
                    );
                    assert_eq!(state.as_deref(), Some("completed"), "{}", case.name);
                }
            }
            "bridge_cancel_cascade" => {
                set_request_state(&admission.node, &child_id, "processing").await;
                if case.bridge_state == "running" {
                    assert!(!case.legal, "{}", case.name);
                    assert!(
                        admission.tool.bridge_cancel_cascade().await.is_err(),
                        "{}",
                        case.name
                    );
                } else {
                    assert_eq!(case.bridge_state, "cancelled", "{}", case.name);
                    admission
                        .tool
                        .cancel_during_run(CancelCause::UserCancelled)
                        .await
                        .unwrap();
                    let intent = admission.tool.bridge_cancel_cascade().await.unwrap();
                    if case.post_child_interrupt_set {
                        assert!(case.legal, "{}", case.name);
                        assert_eq!(intent.unwrap().child_request_id, child_id, "{}", case.name);
                    } else {
                        assert!(!case.legal, "{}", case.name);
                        assert!(intent.is_none(), "{}", case.name);
                    }
                }
            }
            other => panic!("unhandled modeled bridge event {other}"),
        }
        driven += 1;
        admission.node.shutdown().await;
        std::fs::remove_dir_all(&admission.path).unwrap();
    }
    assert_eq!(driven, 10);
}
