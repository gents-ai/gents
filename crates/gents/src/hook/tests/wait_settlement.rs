use super::*;
use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::tool_call_lifecycle::ToolCallLifecycle;
use std::time::Duration;

#[tokio::test]
async fn dropped_wait_without_volatile_handle_settles_through_terminal_parent_owner() {
    let expected = crate::lean_vocab_test::lean_r6_backgrounding_case(
        "caller_interrupt_cancels_wait_call_preserves_background_process",
    );
    assert!(expected.legal);
    for during_completion in [false, true] {
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
        ensure_runtime_schemas(&node).await.unwrap();
        let agent = "did:test:wait-settlement";
        let hook = DefraSessionHook::with_identity(node.clone(), agent, FailurePolicy::FailClosed);
        assert!(matches!(
            hook.on_completion_call(&user_text_message("Observe a process"), &[])
                .await,
            HookAction::Continue
        ));
        let session = hook.session_id().await.unwrap();
        let request = "dropped-wait-request";
        let call = "dropped-wait-call";
        let args = r#"{"tool_call_id":"missing-process"}"#;
        bind_interruptible_request(
            &node,
            &hook,
            request,
            &session,
            chrono::Utc::now() + chrono::Duration::minutes(5),
        )
        .await;
        accept_hook_tool_call(&hook, call, "wait_process", args, None).await;
        let admitted = fetch_tool_call_row(&node, &session, call).await;
        let document = admitted["_docID"].as_str().unwrap();
        if during_completion {
            let reached = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let mut pending = Box::pin(ConfigApplyTxn::with_successful_mutation_pause_at(
                2,
                reached.clone(),
                release,
                hook.on_tool_call("wait_process", None, call, args),
            ));
            tokio::time::timeout(Duration::from_secs(30), async {
                tokio::select! {
                    result = &mut pending => panic!("wait escaped completion pause: {result:?}"),
                    _ = reached.notified() => {}
                }
            })
            .await
            .expect("completion transaction reached");
            assert!(hook.in_flight_lifecycles.lock().await.is_empty());
            drop(pending);
        } else {
            let handles = hook.in_flight_lifecycles.lock().await;
            let mut pending = Box::pin(hook.on_tool_call("wait_process", None, call, args));
            let committed = async {
                loop {
                    let row = fetch_tool_call_row(&node, &session, call).await;
                    if row["lifecycle_state"] == "running" {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            };
            tokio::time::timeout(Duration::from_secs(30), async {
                tokio::select! {
                    result = &mut pending => panic!("wait escaped held handle map: {result:?}"),
                    _ = committed => {}
                }
            })
            .await
            .expect("dispatch committed before handle insertion");
            assert!(handles.is_empty());
            drop(pending);
            drop(handles);
        }
        let row = fetch_tool_call_row(&node, &session, call).await;
        assert_eq!(row["lifecycle_state"], "running");
        assert!(hook.in_flight_lifecycles.lock().await.is_empty());
        assert_eq!(
            ToolCallLifecycle::reconcile_terminal_parent_owned_tools(&node, agent)
                .await
                .unwrap()
                .tool_calls_terminalized,
            0,
            "a missing volatile handle is not authority to cancel a live parent's call"
        );
        let mut fixture = hook_execution_fixtures()
            .lock()
            .await
            .remove(&hook_execution_fixture_key(&hook, request))
            .unwrap();
        let selection = fixture
            .writer
            .terminal_output(&fixture.lifecycle.request().doc_id)
            .await;
        assert_eq!(
            fixture
                .lifecycle
                .terminalize_owned(
                    crate::lifecycle::RequestTerminalOutcome::Interrupted,
                    selection,
                    Some("wait observer dropped"),
                )
                .await
                .unwrap(),
            crate::lifecycle::TerminalizeResult::Won,
        );
        let report = ToolCallLifecycle::reconcile_terminal_parent_owned_tools(&node, agent)
            .await
            .unwrap();
        assert_eq!(report.tool_calls_terminalized, 1);
        let settled = fetch_tool_call_row(&node, &session, call).await;
        assert_eq!(settled["_docID"], admitted["_docID"]);
        assert_eq!(
            settled["lifecycle_state"].as_str(),
            Some(expected.terminal_state.as_str())
        );
        let result = crate::tool_call_lifecycle::query::load_tool_call_result(
            &ConfigAccess::local(node.clone()),
            document,
            agent,
            &session,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            crate::tool_call_lifecycle::query::render_tool_result(&result).unwrap(),
            ToolCallLifecycle::CANCEL_DURING_RUN_OUTPUT,
        );
        assert_eq!(
            ToolCallLifecycle::reconcile_terminal_parent_owned_tools(&node, agent)
                .await
                .unwrap()
                .tool_calls_terminalized,
            0
        );
        node.shutdown().await;
    }
}

#[tokio::test]
async fn wait_process_refuses_a_job_row_with_the_completion_message_text() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(&node).await.unwrap();
    let agent = "did:test:wait-job-row";
    let hook = DefraSessionHook::with_identity(node.clone(), agent, FailurePolicy::FailClosed);
    hook.on_completion_call(&user_text_message("Wait on a job"), &[])
        .await;
    let session = hook.session_id().await.unwrap();
    bind_interruptible_request(
        &node,
        &hook,
        "job-wait-request",
        &session,
        chrono::Utc::now() + chrono::Duration::minutes(5),
    )
    .await;
    accept_hook_tool_call(&hook, "job-call", "plugin_job_tool", "{}", None).await;
    let job = fetch_tool_call_row(&node, &session, "job-call").await;
    let job_doc = job["_docID"].as_str().unwrap();
    let flipped = node
        .execute(&format!(
            r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{job_doc}" }} }}, input: {{ await_mode: "background" }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(!flipped.has_errors(), "{:?}", flipped.errors);
    crate::tool_call_lifecycle::admission_fixture::write_fixture_job_pin(&node, job_doc)
        .await
        .unwrap();

    let args = r#"{"tool_call_id":"job-call"}"#;
    accept_hook_tool_call(&hook, "wait-call", "wait_process", args, None).await;
    let ToolCallHookAction::Skip { reason } = hook
        .on_tool_call("wait_process", None, "wait-call", args)
        .await
    else {
        panic!("wait_process must publish a structured refusal")
    };
    assert!(
        reason.contains("its result arrives only as a completion message"),
        "{reason}"
    );
    node.shutdown().await;
}
