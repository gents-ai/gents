use super::SessionGoals;
use gents_store::config_client::ConfigApplyTxn;
use gents_store::identity::NodeIdentity;
use gents_toolcalls::call::hook::fixtures::{
    accept_hook_tool_call, bind_claimed_hook_request, user_text_message,
};
use gents_toolcalls::call::hook::{DefraSessionHook, FailurePolicy, ToolCallHookAction};
use serde_json::json;
use std::sync::Arc;

async fn rows(
    access: &gents_store::config_client::ConfigAccess,
    query: &str,
    collection: &str,
) -> Vec<serde_json::Value> {
    access.execute(query).await.unwrap()["data"][collection]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
async fn a_goal_task_with_a_lost_receipt_is_decided_by_the_durable_row_and_inherits_the_caller_directory(
) {
    let db = gents_inference::test_support::test_db(
        "goal-commit-receipt",
        crate::lifecycle_hooks::composition(),
    )
    .await;
    let node = db.node.clone();
    let identity = gents_store::test_support::held_runtime_identity(db.node_identity.did());
    let did = identity.did().to_owned();
    let mut hook = DefraSessionHook::with_identity(node.clone(), &did, FailurePolicy::default())
        .with_session_goals(Arc::new(SessionGoals));
    let session = hook.session_id().await.unwrap();
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        "goal-commit-parent",
        &did,
        &did,
        "general",
        &session,
        "start agents",
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(&did),
    );
    create.input.cwd = Some(db.data_path().to_string_lossy().into_owned());
    let lifecycle =
        gents_toolcalls::call::lifecycle::admission_fixture::claimed_signed_request_create(
            &node, &identity, create,
        )
        .await;
    let agent_signer = lifecycle.managed_agent_signer().unwrap();
    hook.actor = Some(identity::Did::new(agent_signer.did()).unwrap());
    hook = hook.with_managed_identity(&agent_signer).unwrap();
    gents_toolcalls::test_support::enable_agents_tools_with_access(
        &gents_store::config_client::ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(&did).unwrap(),
        ),
        &did,
        agent_signer.did(),
    )
    .await;
    bind_claimed_hook_request(
        &hook,
        lifecycle,
        chrono::Utc::now() + chrono::Duration::minutes(5),
    )
    .await;
    hook.on_completion_call(&user_text_message("start agents"), &[])
        .await;

    let args = json!({"agent": "general", "task": {"task_id": "goal"}}).to_string();
    accept_hook_tool_call(
        &hook,
        "goal-call",
        gents_protocol::tool_names::AGENT_NEW_TOOL_NAME,
        &args,
        None,
    )
    .await;
    let (action, fired) = ConfigApplyTxn::with_post_commit_receipt_loss_for_operation(
        Some("session_message.commit_goal"),
        hook.on_tool_call(
            gents_protocol::tool_names::AGENT_NEW_TOOL_NAME,
            None,
            "goal-call",
            &args,
        ),
    )
    .await;
    assert!(fired, "the receipt loss fired; action={action:?}");
    let ToolCallHookAction::Skip { reason } = action else {
        panic!("the call is answered by its receipt");
    };
    let receipt: serde_json::Value = serde_json::from_str(&reason).unwrap();
    assert_eq!(receipt["ok"], true, "{reason}");
    assert!(receipt.get("retryable").is_none(), "{reason}");

    let access = hook.actor_access().unwrap();
    let tool = rows(
        &access,
        r#"{ AgentToolCall(filter: { tool_call_id: { _eq: "goal-call" } }) { _docID lifecycle_state } }"#,
        "AgentToolCall",
    )
    .await;
    assert_eq!(tool.len(), 1);
    assert_eq!(tool[0]["lifecycle_state"], "running");
    let tool_doc_id = tool[0]["_docID"].as_str().unwrap();
    let caused = rows(
        &access,
        &format!(
            r#"{{ AgentRequest(filter: {{ caused_by_parent_tool_call_doc_id: {{ _eq: "{tool_doc_id}" }} }}) {{ _docID input }} }}"#
        ),
        "AgentRequest",
    )
    .await;
    assert_eq!(caused.len(), 1, "one caused request");
    assert_eq!(receipt["request_doc_id"], caused[0]["_docID"]);
    assert_eq!(
        caused[0]["input"]["cwd"],
        db.data_path().to_string_lossy().as_ref()
    );
    node.shutdown().await.expect("shut down test node");
}
