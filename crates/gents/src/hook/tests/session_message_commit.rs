//! A session-message commit whose embedded receipt is lost after it
//! committed, driven through the real hook dispatch.

use super::*;
use crate::config_client::ConfigApplyTxn;
use crate::identity::NodeIdentity;

async fn write_config<T: serde::Serialize>(
    node: &EmbeddedNode,
    collection: crate::Collection,
    document: &T,
) {
    let value = serde_json::to_value(document).unwrap();
    let plan = crate::config_client::DesiredStateApplyPlan::new(vec![
        crate::config_client::DesiredStateApplyDocument {
            collection,
            add: value.clone(),
            update: value,
        },
    ])
    .unwrap();
    crate::config_client::ConfigAccess::transact_local(node, None, "test.write_config", |txn| {
        let plan = &plan;
        Box::pin(async move { crate::config_client::apply_desired_state_plan(txn, plan).await })
    })
    .await
    .unwrap();
}

/// `general` may start `general` on its own principal, and the `goal` Task
/// declares a Goal.
async fn enable_agents_tools(node: &EmbeddedNode, did: &str) {
    crate::test_support::install_test_agent(node, did, "general").await;
    write_config(
        node,
        crate::Collection::AgentTarget,
        &crate::document_config::AgentTargetDocument {
            target_id: "general:general".to_owned(),
            node_did: did.to_owned(),
            target_node_did: did.to_owned(),
            agent_id: "general".to_owned(),
            name: "general".to_owned(),
            description: None,
            tags: Vec::new(),
        },
    )
    .await;
    write_config(
        node,
        crate::Collection::Tools,
        &serde_json::json!({
            "node_did": did,
            "tools_id": "general:tools",
            "agents": { "enabled": true, "target_ids": ["general:general"] }
        }),
    )
    .await;
    write_config(
        node,
        crate::Collection::Task,
        &serde_json::json!({
            "node_did": did,
            "task_id": "goal",
            "agent_id": "general",
            "prompt_template": "pursue the goal",
            "goal_objective_template": "finish the work"
        }),
    )
    .await;
}

async fn rows(node: &EmbeddedNode, query: &str, collection: &str) -> Vec<serde_json::Value> {
    let response = node.execute(query).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    response.data.unwrap()[collection]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Dispatch one agents call with its commit's receipt lost after it
/// committed, and check the durable delivery: one running row, one caused
/// request carrying the call's edge, one receipt naming it, and no second
/// closure of the call's output.
async fn dispatch_with_lost_receipt(
    hook: &DefraSessionHook,
    call_id: &str,
    tool_name: &str,
    args: serde_json::Value,
    operation: &'static str,
) -> (String, serde_json::Value) {
    let args = args.to_string();
    accept_hook_tool_call(hook, call_id, tool_name, &args, None).await;
    let (action, fired) = ConfigApplyTxn::with_post_commit_receipt_loss_for_operation(
        Some(operation),
        hook.on_tool_call(tool_name, None, call_id, &args),
    )
    .await;
    assert!(fired, "{operation}: the receipt loss fired");
    let ToolCallHookAction::Skip { reason } = action else {
        panic!("{operation}: the call is answered by its receipt");
    };
    let receipt: serde_json::Value = serde_json::from_str(&reason).unwrap();
    assert_eq!(receipt["ok"], true, "{operation}: {reason}");
    assert!(receipt.get("retryable").is_none(), "{operation}: {reason}");

    let node = hook.node.as_ref();
    let tool = rows(
        node,
        &format!(
            r#"{{ AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{call_id}" }} }}) {{ _docID lifecycle_state }} }}"#
        ),
        "AgentToolCall",
    )
    .await;
    assert_eq!(tool.len(), 1, "{operation}");
    assert_eq!(tool[0]["lifecycle_state"], "running", "{operation}");
    let tool_doc_id = tool[0]["_docID"].as_str().unwrap().to_owned();
    let caused = rows(
        node,
        &format!(
            r#"{{ AgentRequest(filter: {{ caused_by_parent_tool_call_doc_id: {{ _eq: "{tool_doc_id}" }} }}) {{ _docID input }} }}"#
        ),
        "AgentRequest",
    )
    .await;
    assert_eq!(caused.len(), 1, "{operation}: one caused request");
    assert_eq!(
        receipt["request_doc_id"], caused[0]["_docID"],
        "{operation}"
    );
    let segments = rows(
        node,
        "{ AgentOutputSegment { source close } }",
        "AgentOutputSegment",
    )
    .await;
    let closures = segments
        .iter()
        .filter(|segment| {
            segment["source"].to_string().contains(&tool_doc_id) && !segment["close"].is_null()
        })
        .count();
    assert_eq!(
        closures, 1,
        "{operation}: the receipt's closure only, no failed-dispatch closure"
    );
    (tool_doc_id, caused[0].clone())
}

/// Every session-message commit decides a lost receipt from the durable row:
/// the committed delivery stands, is answered once, and settles once.
#[tokio::test]
async fn a_lost_commit_receipt_is_decided_by_the_durable_row() {
    let dir = tempfile::tempdir().unwrap();
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(dir.path())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let identity =
        crate::KeyIdentity::load_or_create(dir.path().join("test-agent.key"), None).unwrap();
    let did = identity.did().to_owned();
    enable_agents_tools(node.as_ref(), &did).await;
    let hook = DefraSessionHook::with_identity(node.clone(), &did, FailurePolicy::default());
    hook.on_completion_call(&user_text_message("start agents"), &[])
        .await;
    let session_id = hook.session_id().await.unwrap();
    bind_interruptible_request(
        node.as_ref(),
        &hook,
        "request-commit-receipt-loss",
        &session_id,
        chrono::Utc::now() + chrono::Duration::minutes(5),
    )
    .await;

    let (tool_doc_id, caused) = dispatch_with_lost_receipt(
        &hook,
        "new-call",
        crate::toolset::AGENT_NEW_TOOL_NAME,
        json!({ "agent": "general", "prompt": "work" }),
        "session_message.commit_request",
    )
    .await;
    let caused_row = crate::request_binding::load_agent_request_by_doc_id(
        &node,
        caused["_docID"].as_str().unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    crate::tool_call_lifecycle::admission_fixture::complete_child(
        &node,
        &caused_row.request_id,
        &did,
        "done",
    )
    .await;
    for expected in [1, 0] {
        assert_eq!(
            crate::background_completion::settle_running_session_message_rows(&node, &did)
                .await
                .unwrap(),
            expected
        );
    }
    let notifications = rows(
        node.as_ref(),
        &format!(
            r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            crate::background_completion::background_completion_notification_message_key(
                &tool_doc_id,
                "tool"
            )
        ),
        "AgentMessage",
    )
    .await;
    assert_eq!(notifications.len(), 1, "one completion notification");

    dispatch_with_lost_receipt(
        &hook,
        "goal-call",
        crate::toolset::AGENT_NEW_TOOL_NAME,
        json!({ "agent": "general", "task": { "task_id": "goal" } }),
        "session_message.commit_goal",
    )
    .await;

    // The started session is busy again, so a message steers it.
    let response = node
        .execute(&format!(
            r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "processing" }}) {{ _docID }} }}"#,
            caused_row.doc_id
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let (_, steering) = dispatch_with_lost_receipt(
        &hook,
        "steer-call",
        crate::toolset::AGENT_MESSAGE_TOOL_NAME,
        json!({ "session_id": caused_row.session_id, "message": "change course" }),
        "session_message.commit_request",
    )
    .await;
    assert_eq!(steering["input"]["queue"]["source"], "steering");
    assert_eq!(
        steering["input"]["queue"]["queued_after_request_id"],
        caused_row.request_id.as_str()
    );
    node.shutdown().await;
}
