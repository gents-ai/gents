use super::*;
use gents_store::config_client::ConfigApplyTxn;
use gents_toolcalls::call::lifecycle::ToolCallLifecycle;
use std::time::Duration;

#[tokio::test]
async fn dropped_wait_without_volatile_handle_settles_through_terminal_parent_owner() {
    let expected = gents_lean_vocab::lean_r6_backgrounding_case(
        "caller_interrupt_cancels_wait_call_preserves_background_process",
    );
    assert!(expected.legal);
    for during_completion in [false, true] {
        let fixture = super::background_budget::budget_hook_fixture(
            "dropped-wait",
            BackgroundToolRegistry::default(),
            BackgroundExecutionRegistry::default(),
        )
        .await;
        let super::background_budget::BudgetHookFixture {
            db,
            user,
            access,
            hook,
            request_id,
            session_id: session,
            ..
        } = fixture;
        let node = db.node.clone();
        let agent = db.node_identity.did();
        assert!(matches!(
            hook.on_completion_call(&user_text_message("Observe a process"), &[])
                .await,
            HookAction::Continue
        ));
        let request = request_id.as_str();
        let call = "dropped-wait-call";
        let args = r#"{"tool_call_id":"missing-process"}"#;
        let signer = hook_managed_identity(&hook, request).await;
        accept_hook_tool_call(&hook, call, "wait_process", args, None).await;
        let admitted = super::background_budget::tool_row(&access, &session, call).await;
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
                    let row = super::background_budget::tool_row(&access, &session, call).await;
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
        let row = super::background_budget::tool_row(&access, &session, call).await;
        assert_eq!(row["lifecycle_state"], "running");
        assert!(hook.in_flight_lifecycles.lock().await.is_empty());
        assert_eq!(
            ToolCallLifecycle::reconcile_terminal_parent_owned_tools(
                &node,
                agent,
                std::slice::from_ref(&signer)
            )
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
                    gents_session::request::lifecycle::RequestTerminalOutcome::Interrupted,
                    selection,
                    Some("wait observer dropped"),
                )
                .await
                .unwrap(),
            gents_session::request::lifecycle::TerminalizeResult::Won,
        );
        let report = ToolCallLifecycle::reconcile_terminal_parent_owned_tools(
            &node,
            agent,
            std::slice::from_ref(&signer),
        )
        .await
        .unwrap();
        assert_eq!(report.tool_calls_terminalized, 1);
        let settled = super::background_budget::tool_row(&access, &session, call).await;
        assert_eq!(settled["_docID"], admitted["_docID"]);
        assert_eq!(
            settled["lifecycle_state"].as_str(),
            Some(expected.terminal_state.as_str())
        );
        let result = gents_session::tool_call::query::result::load_tool_call_result(
            &access,
            document,
            agent,
            &session,
            Some(user.did()),
        )
        .await
        .unwrap();
        assert_eq!(
            gents_session::tool_call::query::result::render_tool_result(&result).unwrap(),
            ToolCallLifecycle::CANCEL_DURING_RUN_OUTPUT,
        );
        assert_eq!(
            ToolCallLifecycle::reconcile_terminal_parent_owned_tools(
                &node,
                agent,
                std::slice::from_ref(&signer)
            )
            .await
            .unwrap()
            .tool_calls_terminalized,
            0
        );
        node.shutdown().await.expect("shut down test node");
    }
}
