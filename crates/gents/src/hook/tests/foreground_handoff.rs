use super::*;
use gents_loop::session_hook::ForegroundResult;
use gents_protocol::error_chain::ErrorChain;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[cfg(unix)]
#[tokio::test]
async fn foreground_background_handoff_keeps_the_same_process_after_parent_completion() {
    use crate::lean_vocab_test::{LeanCanonicalExecutionCase, LeanCanonicalExecutionOperation};
    let modeled = crate::lean_vocab_test::lean_contract_snapshot()
        .canonical_execution_gate_cases
        .iter()
        .find(|case| {
            matches!(case,
            LeanCanonicalExecutionCase::ModelExecution { name, .. }
            if name == "background_tool_closes_after_parent_terminal")
        })
        .expect("Lean foreground handoff scenario");
    let LeanCanonicalExecutionCase::ModelExecution {
        operations,
        expected_observations,
        ..
    } = modeled
    else {
        unreachable!()
    };
    let receipt_step = operations
        .iter()
        .position(|operation| {
            matches!(
                operation,
                LeanCanonicalExecutionOperation::PublishBackgroundReceipt { .. }
            )
        })
        .unwrap();
    let modeled_handoff = &expected_observations[receipt_step];
    let modeled_completion = expected_observations.last().unwrap();
    assert!(expected_observations
        .iter()
        .all(|observation| observation.accepted));
    let dir = tempfile::tempdir().unwrap();
    let gate = dir.path().join("gate");
    assert!(std::process::Command::new("mkfifo")
        .arg(&gate)
        .status()
        .unwrap()
        .success());
    let pid_file = dir.path().join("pid");
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(&node).await.unwrap();
    let hook =
        DefraSessionHook::with_identity(node.clone(), "did:test:general", FailurePolicy::default());
    let session = hook.session_id().await.unwrap();
    let deadline = Utc::now() + chrono::Duration::seconds(30);
    bind_interruptible_request(&node, &hook, "handoff-parent", &session, deadline).await;
    let mut lifecycle = accepted_hook_tool_lifecycle(
        &hook,
        "handoff-command",
        "bash_unrestricted",
        "{}",
        deadline,
        crate::tool_call_lifecycle::AwaitMode::Foreground,
    )
    .await;
    lifecycle.start_running().await.unwrap();
    let tool_doc = lifecycle.doc_id().unwrap().to_owned();
    let request_doc = lifecycle.request_doc_id().unwrap().to_owned();
    let stale_replay = lifecycle.clone();
    hook.in_flight_lifecycles
        .lock()
        .await
        .insert("handoff-command".into(), lifecycle);
    let parent_token = CancellationToken::new();
    let parent_for_task = parent_token.clone();
    let task_hook = hook.clone();
    let cwd = dir.path().to_owned();
    let pid_arg = pid_file.to_string_lossy().into_owned();
    let gate_arg = gate.to_string_lossy().into_owned();
    let running = tokio::spawn(async move {
        crate::tool_call_lifecycle::runtime::scope_request_tool_execution(
            Some(deadline), parent_for_task,
            task_hook.run_backgroundable_foreground("bash_unrestricted", "handoff-command", Box::new(move |token| Box::pin(async move {
                let result = crate::managed_exec::run_managed_exec(crate::managed_exec::ManagedExecRequest {
                    argv: vec!["/bin/sh".into(), "-c".into(), "printf '%s' $$ > \"$1\"; read value < \"$2\"; printf 'finished:%s' \"$value\"".into(), "sh".into(), pid_arg, gate_arg],
                    cwd, deadline_at: Some(deadline), cancellation_token: token,
                    max_output_bytes: 4096, stdin: Vec::new(), environment: None,
                    tool_name: Some("handoff-command".into()), live_output: None,
                }).await;
                match result {
                    crate::managed_exec::ManagedExecOutcome::Exited { stdout, .. } =>
                        crate::tool_call_lifecycle::ToolOutcome::Completed(String::from_utf8(stdout).unwrap()),
                    other => crate::tool_call_lifecycle::ToolOutcome::from_tool_call_error(&format!("{other:?}")),
                }
            }))),
        ).await
    });
    let pid: i32 = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(pid) = tokio::fs::read_to_string(&pid_file).await {
                if let Ok(pid) = pid.parse() {
                    break pid;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let access = crate::config_client::ConfigAccess::local(node.clone());
    assert!(crate::background_tools::request_foreground_backgrounding(
        &access,
        &hook.node_did,
        &session,
        "handoff-parent",
        "handoff-command",
    )
    .await
    .unwrap());
    let result = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap();
    let ForegroundResult::Backgrounded(receipt) = result else {
        panic!("expected background receipt");
    };
    stale_replay
        .clone()
        .background_with_receipt(&receipt)
        .await
        .unwrap();
    let capacity = crate::lean_vocab_test::lean_r6_backgrounding_case(
        "background_tool_budget_count_8_rejects_spawn",
    )
    .max_backgrounded;
    let mut fillers = Vec::new();
    let mut filler_ids = (1..=capacity)
        .map(|index| format!("handoff-capacity-{index}"))
        .collect::<Vec<_>>();
    let filler_calls = filler_ids
        .iter()
        .map(|id| (id.as_str(), None))
        .collect::<Vec<_>>();
    let filler_message = Message::Assistant {
        id: Some("handoff-capacity-turn".into()),
        content: filler_ids
            .iter()
            .map(|id| {
                AssistantContent::ToolCall(ToolCall {
                    id: id.clone(),
                    call_id: None,
                    function: ToolFunction {
                        name: "held_background_tool".into(),
                        arguments: serde_json::json!({}),
                    },
                    signature: None,
                    additional_params: None,
                })
            })
            .collect(),
    };
    publish_and_adopt_tool_turns(&hook, &filler_calls, filler_message).await;
    let overflow_id = filler_ids.pop().unwrap();
    for id in &filler_ids {
        let mut admitted = hook
            .adopt_accepted_tool_dispatch(
                id,
                None,
                "handoff-parent",
                &session,
                "held_background_tool",
                "{}",
                deadline,
                crate::tool_call_lifecycle::AwaitMode::Background,
            )
            .await
            .unwrap();
        admitted.start_running().await.unwrap();
        fillers.push(admitted);
    }
    let mut overflow = hook
        .adopt_accepted_tool_dispatch(
            &overflow_id,
            None,
            "handoff-parent",
            &session,
            "held_background_tool",
            "{}",
            deadline,
            crate::tool_call_lifecycle::AwaitMode::Background,
        )
        .await
        .unwrap();
    let refusal = overflow.start_running().await.unwrap_err();
    assert_eq!(
        refusal.background_capacity().map(|capacity| capacity.0),
        Some(capacity)
    );
    stale_replay
        .clone()
        .background_with_receipt(&receipt)
        .await
        .unwrap();
    for filler in &mut fillers {
        filler
            .cancel_during_run(crate::tool_call_lifecycle::CancelCause::UserCancelled)
            .await
            .unwrap();
    }
    let receipt_rows = access.execute(&format!(r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{}:background-receipt:{}" }} }}) {{ _docID }} }}"#, crate::graphql::escape_graphql_string(&session), crate::graphql::escape_graphql_string(&tool_doc))).await.unwrap();
    assert_eq!(
        receipt_rows["data"]["AgentMessage"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(crate::active_native_executors()
        .iter()
        .any(|entry| entry.pid == pid));
    assert_eq!(
        !hook.in_flight_lifecycles.lock().await.is_empty(),
        modeled_handoff.in_flight,
    );
    let handed_off = fetch_tool_call_row(&node, &session, "handoff-command").await;
    assert_eq!(
        handed_off["lifecycle_state"].as_str(),
        modeled_handoff.tool_state.as_deref()
    );
    {
        let mut fixtures = hook_execution_fixtures().lock().await;
        let fixture = fixtures
            .get_mut(&hook_execution_fixture_key(&hook, "handoff-parent"))
            .unwrap();
        let selection = fixture.writer.terminal_output(&request_doc).await;
        fixture
            .lifecycle
            .terminalize_owned(
                crate::lifecycle::RequestTerminalOutcome::Completed,
                selection,
                None,
            )
            .await
            .unwrap();
    }
    parent_token.cancel();
    assert!(crate::active_native_executors()
        .iter()
        .any(|entry| entry.pid == pid));
    tokio::fs::write(&gate, "released\n").await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        hook.background_executions
            .wait_for_completion("handoff-command"),
    )
    .await
    .unwrap();
    let row = fetch_tool_call_row(&node, &session, "handoff-command").await;
    assert_eq!(
        row["lifecycle_state"].as_str(),
        modeled_completion.tool_state.as_deref()
    );
    assert_eq!(row["await_mode"], "background");
    let output = crate::background_tools::canonical_tool_output(
        &node,
        &tool_doc,
        &request_doc,
        &session,
        &hook.node_did,
        None,
    )
    .await
    .unwrap();
    assert!(output.contains("finished:released"), "{output}");
    assert!(!crate::active_native_executors()
        .iter()
        .any(|entry| entry.pid == pid));
}

#[tokio::test]
async fn cancellation_winning_handoff_clears_the_request_and_cancels_the_same_execution() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(&node).await.unwrap();
    let hook =
        DefraSessionHook::with_identity(node.clone(), "did:test:general", FailurePolicy::default());
    let session = hook.session_id().await.unwrap();
    let deadline = Utc::now() + chrono::Duration::seconds(30);
    bind_interruptible_request(&node, &hook, "handoff-cancel-parent", &session, deadline).await;
    let mut lifecycle = accepted_hook_tool_lifecycle(
        &hook,
        "handoff-cancel-command",
        "bash_unrestricted",
        "{}",
        deadline,
        crate::tool_call_lifecycle::AwaitMode::Foreground,
    )
    .await;
    lifecycle.start_running().await.unwrap();
    let doc = lifecycle.doc_id().unwrap().to_owned();
    hook.in_flight_lifecycles
        .lock()
        .await
        .insert("handoff-cancel-command".into(), lifecycle);
    let token = CancellationToken::new();
    let parent = token.clone();
    let (started, observed) = tokio::sync::oneshot::channel();
    let runner = hook.clone();
    let running = tokio::spawn(async move {
        crate::tool_call_lifecycle::runtime::scope_request_tool_execution(
            Some(deadline),
            parent,
            runner.run_backgroundable_foreground(
                "bash_unrestricted",
                "handoff-cancel-command",
                Box::new(move |token| {
                    Box::pin(async move {
                        let _ = started.send(token.clone());
                        token.cancelled().await;
                        crate::tool_call_lifecycle::ToolOutcome::Cancelled
                    })
                }),
            ),
        )
        .await
    });
    let execution_token = observed.await.unwrap();
    let guard = hook.in_flight_lifecycles.lock().await;
    let access = crate::config_client::ConfigAccess::local(node.clone());
    assert!(crate::background_tools::request_foreground_backgrounding(
        &access,
        &hook.node_did,
        &session,
        "handoff-cancel-parent",
        "handoff-cancel-command"
    )
    .await
    .unwrap());
    token.cancel();
    drop(guard);
    let outcome = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        outcome,
        ForegroundResult::Settled(crate::tool_call_lifecycle::ToolOutcome::Cancelled)
    ));
    assert!(execution_token.is_cancelled());
    let doc = crate::graphql::escape_graphql_string(&doc);
    let state = access.execute(&format!(r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{doc}" }} }}) {{ await_mode background_requested_at }} }}"#)).await.unwrap();
    assert_eq!(
        state["data"]["AgentToolCall"][0]["await_mode"],
        "foreground"
    );
    assert!(state["data"]["AgentToolCall"][0]["background_requested_at"].is_null());
}

#[tokio::test]
async fn refused_foreground_handoff_can_be_retried() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(&node).await.unwrap();
    let hook =
        DefraSessionHook::with_identity(node.clone(), "did:test:general", FailurePolicy::default());
    let session = hook.session_id().await.unwrap();
    let deadline = Utc::now() + chrono::Duration::seconds(30);
    bind_interruptible_request(&node, &hook, "retry-parent", &session, deadline).await;
    let mut lifecycle = accepted_hook_tool_lifecycle(
        &hook,
        "retry-command",
        "bash_unrestricted",
        "{}",
        deadline,
        crate::tool_call_lifecycle::AwaitMode::Foreground,
    )
    .await;
    lifecycle.start_running().await.unwrap();
    let doc = crate::graphql::escape_graphql_string(lifecycle.doc_id().unwrap());
    hook.in_flight_lifecycles
        .lock()
        .await
        .insert("retry-command".into(), lifecycle.clone());
    let (started, observed) = tokio::sync::oneshot::channel();
    let (finish, finished) = tokio::sync::oneshot::channel();
    let runner = hook.clone();
    let running = tokio::spawn(async move {
        crate::tool_call_lifecycle::runtime::scope_request_tool_execution(
            Some(deadline),
            CancellationToken::new(),
            runner.run_backgroundable_foreground(
                "bash_unrestricted",
                "retry-command",
                Box::new(move |_| {
                    Box::pin(async move {
                        let _ = started.send(());
                        finished.await.unwrap();
                        crate::tool_call_lifecycle::ToolOutcome::Completed("finished".into())
                    })
                }),
            ),
        )
        .await
    });
    observed.await.unwrap();
    hook.in_flight_lifecycles
        .lock()
        .await
        .remove("retry-command");
    let access = crate::config_client::ConfigAccess::local(node);
    assert!(crate::background_tools::request_foreground_backgrounding(
        &access,
        &hook.node_did,
        &session,
        "retry-parent",
        "retry-command"
    )
    .await
    .unwrap());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = access.execute(&format!(r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{doc}" }} }}) {{ background_requested_at }} }}"#)).await.unwrap();
            if state["data"]["AgentToolCall"][0]["background_requested_at"].is_null() { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert!(!running.is_finished());
    hook.in_flight_lifecycles
        .lock()
        .await
        .insert("retry-command".into(), lifecycle);
    assert!(crate::background_tools::request_foreground_backgrounding(
        &access,
        &hook.node_did,
        &session,
        "retry-parent",
        "retry-command"
    )
    .await
    .unwrap());
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), running)
            .await
            .unwrap()
            .unwrap(),
        ForegroundResult::Backgrounded(_)
    ));
    finish.send(()).unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        hook.background_executions
            .wait_for_completion("retry-command"),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn concurrent_background_starts_on_one_request_fill_exactly_the_ceiling() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(&node).await.unwrap();
    let hook =
        DefraSessionHook::with_identity(node.clone(), "did:test:general", FailurePolicy::default());
    let session = hook.session_id().await.unwrap();
    let deadline = Utc::now() + chrono::Duration::seconds(30);
    bind_interruptible_request(&node, &hook, "concurrent-parent", &session, deadline).await;
    let capacity = crate::lean_vocab_test::lean_r6_backgrounding_case(
        "background_tool_budget_count_8_rejects_spawn",
    )
    .max_backgrounded;
    let ids = (0..=capacity)
        .map(|index| format!("concurrent-start-{index}"))
        .collect::<Vec<_>>();
    let calls = ids.iter().map(|id| (id.as_str(), None)).collect::<Vec<_>>();
    let message = Message::Assistant {
        id: Some("concurrent-start-turn".into()),
        content: ids
            .iter()
            .map(|id| {
                AssistantContent::ToolCall(ToolCall {
                    id: id.clone(),
                    call_id: None,
                    function: ToolFunction {
                        name: "held_background_tool".into(),
                        arguments: serde_json::json!({}),
                    },
                    signature: None,
                    additional_params: None,
                })
            })
            .collect(),
    };
    publish_and_adopt_tool_turns(&hook, &calls, message).await;
    let mut starts = Vec::new();
    for id in &ids {
        starts.push(
            hook.adopt_accepted_tool_dispatch(
                id,
                None,
                "concurrent-parent",
                &session,
                "held_background_tool",
                "{}",
                deadline,
                crate::tool_call_lifecycle::AwaitMode::Background,
            )
            .await
            .unwrap(),
        );
    }
    let results =
        futures::future::join_all(starts.iter_mut().map(|start| start.start_running())).await;
    let refusals = results
        .iter()
        .filter_map(|result| result.as_ref().err())
        .map(|error| {
            error
                .background_capacity()
                .map(|capacity| capacity.0)
                .unwrap_or_else(|| panic!("unexpected start failure: {}", ErrorChain(&error)))
        })
        .collect::<Vec<_>>();
    assert_eq!(refusals, vec![capacity]);
    for start in &mut starts {
        if start.state() == crate::tool_call_lifecycle::ToolCallState::Running {
            start
                .cancel_during_run(crate::tool_call_lifecycle::CancelCause::UserCancelled)
                .await
                .unwrap();
        }
    }
    node.shutdown().await;
}
