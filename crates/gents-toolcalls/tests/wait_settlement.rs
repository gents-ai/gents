use gents_inference::llm::ToolCallHookAction;

use gents_toolcalls::call::hook::fixtures::*;
use gents_toolcalls::call::hook::*;

#[tokio::test]
async fn wait_process_refuses_a_job_row_with_the_completion_message_text() {
    let node_db = gents_store::test_support::TestStore::start(
        "wait-process-refuses-a-job-row-with-the-",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = node_db.node.clone();
    let hook = signed_hook(&node, FailurePolicy::FailClosed).await;
    hook.on_completion_call(&user_text_message("Wait on a job"), &[])
        .await;
    let session = hook.session_id().await.unwrap();
    bind_interruptible_request(&hook, "job-wait-request", &session).await;
    accept_hook_tool_call(&hook, "job-call", "plugin_job_tool", "{}", None).await;
    let job = fetch_tool_call_row(&hook, &session, "job-call").await;
    let job_doc = job["_docID"].as_str().unwrap();
    let access = hook.actor_access().unwrap();
    access
        .write(
            "test.wait_settlement.flip_background",
            &format!(
                r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{job_doc}" }} }}, input: {{ await_mode: "background" }}) {{ _docID }} }}"#
            ),
        )
        .await
        .unwrap();
    gents_toolcalls::call::lifecycle::admission_fixture::write_fixture_job_pin_as(&access, job_doc)
        .await
        .unwrap();

    let args = r#"{"tool_call_id":"job-call"}"#;
    accept_hook_tool_call(&hook, "wait-call", "wait_process", args, None).await;
    let ToolCallHookAction::Skip { reason } = hook
        .on_tool_call(
            "wait_process",
            None,
            "wait-call",
            args,
            &gents_loop::tool::ToolCallEnvelope::default(),
        )
        .await
    else {
        panic!("wait_process must publish a structured refusal")
    };
    assert!(
        reason.contains("wait_process only accepts spawn_process handles")
            && reason.contains("its result arrives only as a completion message"),
        "{reason}"
    );
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn wait_process_refuses_a_session_message_row_with_sessions_guidance() {
    let node_db = gents_store::test_support::TestStore::start(
        "wait-process-refuses-session-message-row",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = node_db.node.clone();
    let hook = signed_hook(&node, FailurePolicy::FailClosed).await;
    hook.on_completion_call(&user_text_message("Wait on agent_new"), &[])
        .await;
    let session = hook.session_id().await.unwrap();
    bind_interruptible_request(&hook, "agent-wait-request", &session).await;
    accept_hook_tool_call(&hook, "agent-call", "agent_new", "{}", None).await;
    let row = fetch_tool_call_row(&hook, &session, "agent-call").await;
    let row_doc = row["_docID"].as_str().unwrap();
    let access = hook.actor_access().unwrap();
    access
        .write(
            "test.wait_settlement.flip_agent_background",
            &format!(
                r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{row_doc}" }} }}, input: {{ await_mode: "background" }}) {{ _docID }} }}"#
            ),
        )
        .await
        .unwrap();

    let args = r#"{"tool_call_id":"agent-call"}"#;
    accept_hook_tool_call(&hook, "wait-call", "wait_process", args, None).await;
    let ToolCallHookAction::Skip { reason } = hook
        .on_tool_call(
            "wait_process",
            None,
            "wait-call",
            args,
            &gents_loop::tool::ToolCallEnvelope::default(),
        )
        .await
    else {
        panic!("wait_process must publish a structured refusal")
    };
    assert!(
        reason.contains("wait_process only accepts spawn_process handles")
            && reason.contains("sessions")
            && reason.contains("agent_session_read"),
        "{reason}"
    );
    assert!(
        !reason.contains("completion message"),
        "session message refusal must not tell caller to stop and wait for a completion message: {reason}"
    );
    node.shutdown().await.expect("shut down test node");
}
