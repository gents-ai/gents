use rapidhash::{HashMapExt, RapidHashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;

use gents_inference::llm::message::{
    AssistantContent, Message, Reasoning, Text, ToolCall, ToolFunction, ToolResultContent,
    UserContent,
};
use gents_inference::llm::{HookAction, ToolCallHookAction};
use gents_loop::provider_input::ProviderInputProfile;
use gents_store::identity::{KeyIdentity, NodeIdentity};
use serde_json::json;
use chrono::Utc;

use gents_store::schema::ensure_runtime_schemas;
use gents_toolcalls::call::hook::fixtures::*;
use gents_toolcalls::call::hook::*;
use defra_node::Node as EmbeddedNode;

mod background_admission;
mod background_budget;
mod background_panic;
mod delegated_finality_tests;
mod goal_context;
mod plugin_job_dispatch;
mod r4c_list_background_tools;
mod r4c_private_support;
mod r4c_read_tool_output;
mod script_composition;
mod wait_settlement;

#[tokio::test]
async fn authorized_goal_hook_derives_ownership_and_runs_create_get_update_lifecycle() {
    let identity_dir = tempfile::tempdir().unwrap();
    let identity =
        KeyIdentity::load_or_create(identity_dir.path().join("owner.key"), None).unwrap();
    let data_path =
        std::env::temp_dir().join(format!("agent-hook-goal-create-{}", uuid::Uuid::new_v4()));
    let node = Arc::new(
        gents_store::test_support::node_builder()
            .data_path(&data_path)
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap(),
    );
    gents_store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let mut hook =
        DefraSessionHook::with_identity(node.clone(), identity.did(), FailurePolicy::default())
            .with_goal_tool_authority(true, true);
    assert!(matches!(
        hook.on_completion_call(&user_text_message("start"), &[])
            .await,
        HookAction::Continue
    ));
    let session_id = hook.session_id().await.unwrap();
    bind_managed_hook_request(
        &mut hook,
        "goal-request",
        &identity,
        Utc::now() + chrono::Duration::minutes(5),
    )
    .await;
    let access = hook.actor_access().unwrap();

    accept_hook_tool_call(
        &hook,
        "goal-create-forged",
        gents_protocol::goal_names::CREATE_GOAL_TOOL_NAME,
        r#"{"objective":"ship","node_did":"did:test:other","session_id":"other"}"#,
        None,
    )
    .await;
    let forged = hook
        .on_tool_call(
            gents_protocol::goal_names::CREATE_GOAL_TOOL_NAME,
            None,
            "goal-create-forged",
            r#"{"objective":"ship","node_did":"did:test:other","session_id":"other"}"#,
        )
        .await;
    assert!(
        matches!(forged, ToolCallHookAction::Skip { .. }),
        "unexpected forged create action: {forged:?}"
    );
    assert!(access
        .transact_readonly("test.hook_goal", |txn| {
            Box::pin(gents_automation::goal::load_canonical_goal_in_txn(
                txn,
                identity.did(),
                &session_id,
            ))
        })
        .await
        .unwrap()
        .is_none());

    accept_hook_tool_call(
        &hook,
        "goal-create-valid",
        gents_protocol::goal_names::CREATE_GOAL_TOOL_NAME,
        r#"{"objective":"ship","token_budget":1000}"#,
        None,
    )
    .await;
    let created = hook
        .on_tool_call(
            gents_protocol::goal_names::CREATE_GOAL_TOOL_NAME,
            None,
            "goal-create-valid",
            r#"{"objective":"ship","token_budget":1000}"#,
        )
        .await;
    assert!(matches!(created, ToolCallHookAction::Skip { .. }));
    let goal = access
        .transact_readonly("test.hook_goal", |txn| {
            Box::pin(gents_automation::goal::load_canonical_goal_in_txn(
                txn,
                identity.did(),
                &session_id,
            ))
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(goal.node_did, identity.did());
    assert_eq!(goal.session_id, session_id);
    assert_eq!(goal.objective, "ship");
    assert_eq!(goal.token_budget, Some(1000));

    accept_hook_tool_call(
        &hook,
        "goal-update-forged",
        gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME,
        r#"{"status":"complete","session_id":"other"}"#,
        None,
    )
    .await;
    let forged_update = hook
        .on_tool_call(
            gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME,
            None,
            "goal-update-forged",
            r#"{"status":"complete","session_id":"other"}"#,
        )
        .await;
    assert!(matches!(forged_update, ToolCallHookAction::Skip { .. }));
    assert_eq!(
        access
            .transact_readonly("test.hook_goal", |txn| {
                Box::pin(gents_automation::goal::load_canonical_goal_in_txn(
                    txn,
                    identity.did(),
                    &session_id,
                ))
            })
            .await
            .unwrap()
            .unwrap()
            .parsed_status(),
        Some(gents_protocol::goal_status::GoalStatus::Active)
    );

    for (id, args) in [
        (
            "goal-edit-valid",
            r#"{"objective":"verify","token_budget":200}"#,
        ),
        (
            "goal-edit-retry",
            r#"{"status":"active","objective":"verify","token_budget":200}"#,
        ),
        (
            "goal-edit-forged",
            r#"{"objective":"other","session_id":"other"}"#,
        ),
        ("goal-edit-blank", r#"{"objective":"  "}"#),
        (
            "goal-edit-terminal-mix",
            r#"{"status":"complete","objective":"other"}"#,
        ),
    ] {
        accept_hook_tool_call(
            &hook,
            id,
            gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME,
            args,
            None,
        )
        .await;
        assert!(matches!(
            hook.on_tool_call(
                gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME,
                None,
                id,
                args
            )
            .await,
            ToolCallHookAction::Skip { .. }
        ));
        let updated = access
            .transact_readonly("test.hook_goal", |txn| {
                Box::pin(gents_automation::goal::load_canonical_goal_in_txn(
                    txn,
                    identity.did(),
                    &session_id,
                ))
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.objective, "verify", "{id}");
        assert_eq!(updated.token_budget, Some(200), "{id}");
        assert_eq!(
            updated.parsed_status(),
            Some(gents_protocol::goal_status::GoalStatus::Active),
            "{id}"
        );
        assert_eq!(updated.goal_id, goal.goal_id, "{id}");
    }

    accept_hook_tool_call(
        &hook,
        "goal-get-valid",
        gents_protocol::goal_names::GET_GOAL_TOOL_NAME,
        "{}",
        None,
    )
    .await;
    assert!(matches!(
        hook.on_tool_call(
            gents_protocol::goal_names::GET_GOAL_TOOL_NAME,
            None,
            "goal-get-valid",
            "{}",
        )
        .await,
        ToolCallHookAction::Skip { .. }
    ));
    accept_hook_tool_call(
        &hook,
        "goal-update-valid",
        gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME,
        r#"{"status":"complete","reason":"done"}"#,
        None,
    )
    .await;
    assert!(matches!(
        hook.on_tool_call(
            gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME,
            None,
            "goal-update-valid",
            r#"{"status":"complete","reason":"done"}"#,
        )
        .await,
        ToolCallHookAction::Skip { .. }
    ));
    assert_eq!(
        access
            .transact_readonly("test.hook_goal", |txn| {
                Box::pin(gents_automation::goal::load_canonical_goal_in_txn(
                    txn,
                    identity.did(),
                    &session_id,
                ))
            })
            .await
            .unwrap()
            .unwrap()
            .parsed_status(),
        Some(gents_protocol::goal_status::GoalStatus::Complete)
    );

    node.shutdown().await.expect("shut down test node");
    let _ = std::fs::remove_dir_all(data_path);
}

#[tokio::test]
async fn control_tool_completion_failure_never_reauthorizes_dispatch() {
    let identity_dir = tempfile::tempdir().unwrap();
    let identity =
        KeyIdentity::load_or_create(identity_dir.path().join("owner.key"), None).unwrap();
    let goal_case = gents_lean_vocab::lean_goal_create_cases()
        .iter()
        .find(|case| case.expected == "fresh" && case.token_budget.is_none())
        .unwrap();
    let replay = gents_lean_vocab::lean_canonical_dispatch_observation_cases()
        .iter()
        .find(|case| case.name == "won_dispatch_then_replay")
        .unwrap();
    let args = json!({"objective": goal_case.objective}).to_string();
    for policy in [FailurePolicy::FailOpen, FailurePolicy::FailClosed] {
        let mut completion_mutation = None;
        for inject in [false, true] {
            let node = Arc::new(
                gents_store::test_support::node_builder()
                    .with_node_identity_did(identity.did())
                    .build()
                    .await
                    .unwrap(),
            );
            ensure_runtime_schemas(&node).await.unwrap();
            let mut hook = DefraSessionHook::with_identity(node.clone(), identity.did(), policy)
                .with_goal_tool_authority(goal_case.goal_tools, goal_case.goal_create);
            assert!(matches!(
                hook.on_completion_call(&user_text_message("Create a goal"), &[])
                    .await,
                HookAction::Continue
            ));
            let session = hook.session_id().await.unwrap();
            bind_managed_hook_request(
                &mut hook,
                "control-receipt",
                &identity,
                Utc::now() + chrono::Duration::minutes(5),
            )
            .await;
            let access = hook.actor_access().unwrap();
            accept_hook_tool_call(&hook, "control-call", "create_goal", &args, None).await;
            let (action, trace) =
                gents_store::config_client::ConfigApplyTxn::with_successful_mutation_failure_and_trace(
                    completion_mutation,
                    hook.on_tool_call("create_goal", None, "control-call", &args),
                )
                .await;
            let mutations = trace.len();
            let goal = access
                .transact_readonly("test.hook_control_goal", |txn| {
                    Box::pin(gents_automation::goal::load_canonical_goal_in_txn(
                        txn,
                        identity.did(),
                        &session,
                    ))
                })
                .await
                .unwrap()
                .expect("the control effect committed before completion");
            assert_eq!(goal.objective, goal_case.objective);
            if inject {
                assert_eq!(Some(mutations), completion_mutation, "fault must fire");
                assert!(
                    matches!(action, ToolCallHookAction::Terminate { .. }),
                    "{action:?}"
                );
                let row = fetch_tool_call_row(&hook, &session, "control-call").await;
                assert_eq!(
                    row["lifecycle_state"].as_str() == Some("running"),
                    replay.expected[0].running,
                    "{row}"
                );
                let again = hook
                    .on_tool_call("create_goal", None, "control-call", &args)
                    .await;
                assert_eq!(
                    matches!(again, ToolCallHookAction::Continue),
                    replay.expected[1].may_invoke
                );
                let after = access
                    .transact_readonly("test.hook_control_goal", |txn| {
                        Box::pin(gents_automation::goal::load_canonical_goal_in_txn(
                            txn,
                            identity.did(),
                            &session,
                        ))
                    })
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    serde_json::to_value(after).unwrap(),
                    serde_json::to_value(goal).unwrap(),
                    "replay must not alter the committed goal"
                );
            } else {
                assert!(
                    matches!(action, ToolCallHookAction::Skip { .. }),
                    "{action:?}"
                );
                // The completion is the last mutation before the post-commit
                // publication of the created Goal's session relationships.
                let completion = trace
                    .iter()
                    .rposition(|operation| {
                        *operation != Some("session.publish_child_relationships")
                    })
                    .expect("the control call commits its completion")
                    + 1;
                assert!(
                    trace[..completion]
                        .iter()
                        .all(|operation| *operation != Some("session.publish_child_relationships")),
                    "{trace:?}"
                );
                completion_mutation = Some(completion);
            }
            hook_execution_fixtures()
                .lock()
                .await
                .remove(&hook_execution_fixture_key(&hook, "control-receipt"));
            node.shutdown().await.expect("shut down test node");
        }
    }
}

#[tokio::test]
async fn list_processes_skip_publishes_canonical_tool_result() {
    let dir = tempfile::tempdir().unwrap();
    let node = Arc::new(
        gents_store::test_support::node_builder()
            .data_path(dir.path())
            .build()
            .await
            .unwrap(),
    );
    gents_store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let hook = signed_hook(&node, FailurePolicy::default()).await;
    hook.on_completion_call(&user_text_message("list processes"), &[])
        .await;
    let session_id = hook.session_id().await.unwrap();
    bind_interruptible_request(
        &hook,
        "request-control-result",
        &session_id,
        chrono::Utc::now() + chrono::Duration::minutes(5),
    )
    .await;
    accept_hook_tool_call(&hook, "list-call", "list_processes", "{}", None).await;

    assert!(matches!(
        hook.on_tool_call("list_processes", None, "list-call", "{}")
            .await,
        ToolCallHookAction::Skip { .. }
    ));

    let timeline = gents_observe::observe::run_timeline_fetch::load_run_timeline_rows(
        &hook.actor_access().unwrap(),
        "request-control-result",
    )
    .await
    .unwrap();
    let results = timeline
        .messages
        .iter()
        .flat_map(|row| match &row.message {
            Message::User { content } => content.as_slice(),
            _ => &[],
        })
        .filter_map(|content| match content {
            UserContent::ToolResult(result) => Some(result),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 1, "timeline={timeline:#?}");
    assert_eq!(results[0].id, "list-call");
}

#[tokio::test]
async fn wait_and_cancel_process_skip_publish_canonical_tool_results() {
    let dir = tempfile::tempdir().unwrap();
    let node = Arc::new(
        gents_store::test_support::node_builder()
            .data_path(dir.path())
            .build()
            .await
            .unwrap(),
    );
    gents_store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let hook = signed_hook(&node, FailurePolicy::default()).await;
    hook.on_completion_call(&user_text_message("control processes"), &[])
        .await;
    let session_id = hook.session_id().await.unwrap();
    bind_interruptible_request(
        &hook,
        "request-control-results",
        &session_id,
        chrono::Utc::now() + chrono::Duration::minutes(5),
    )
    .await;

    let long_handle = "x".repeat(60 * 1024);
    let long_disallowed_tool = "y".repeat(60 * 1024);
    let cases = [
        (
            "wait-call",
            "wait_process",
            r#"{"tool_call_id":"missing-background-handle"}"#.to_string(),
        ),
        (
            "cancel-call",
            "cancel_process",
            r#"{"tool_call_id":"missing-background-handle"}"#.to_string(),
        ),
        (
            "wait-long-call",
            "wait_process",
            serde_json::json!({ "tool_call_id": long_handle }).to_string(),
        ),
        (
            "spawn-long-call",
            "spawn_process",
            serde_json::json!({ "tool_name": long_disallowed_tool, "args": {} }).to_string(),
        ),
    ];
    let mut skipped = Vec::new();
    for (id, name, args) in &cases {
        accept_hook_tool_call(&hook, id, name, args, None).await;
        let action = hook.on_tool_call(name, None, id, args).await;
        let ToolCallHookAction::Skip { reason } = action else {
            panic!("{name} should return its durable result to the provider; got {action:?}")
        };
        skipped.push((*id, reason));
    }

    let timeline = gents_observe::observe::run_timeline_fetch::load_run_timeline_rows(
        &hook.actor_access().unwrap(),
        "request-control-results",
    )
    .await
    .unwrap();
    let results = timeline
        .messages
        .iter()
        .flat_map(|row| match &row.message {
            Message::User { content } => content.as_slice(),
            _ => &[],
        })
        .filter_map(|content| match content {
            UserContent::ToolResult(result) => {
                result.content.iter().find_map(|content| match content {
                    ToolResultContent::Text(Text { text }) => {
                        Some((result.id.as_str(), text.as_str()))
                    }
                    _ => None,
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(results.len(), cases.len(), "timeline={timeline:#?}");
    for ((expected_id, provider_text), (actual_id, replayed_text)) in skipped.iter().zip(&results) {
        assert_eq!(actual_id, expected_id);
        assert_eq!(
            replayed_text, provider_text,
            "provider replay must be exact"
        );
    }
    let long_row = fetch_tool_call_row(&hook, &session_id, "wait-long-call").await;
    let full = long_row["result"].as_str().expect("full raw long result");
    let bounded = skipped
        .iter()
        .find(|(id, _)| *id == "wait-long-call")
        .unwrap()
        .1
        .as_str();
    assert!(
        full.len() > bounded.len(),
        "full raw output must be retained"
    );
    assert!(bounded.contains("[Showing lines"));
    let long_spawn_row = fetch_tool_call_row(&hook, &session_id, "spawn-long-call").await;
    let full_spawn = long_spawn_row["result"]
        .as_str()
        .expect("full raw spawn failure");
    let bounded_spawn = skipped
        .iter()
        .find(|(id, _)| *id == "spawn-long-call")
        .unwrap()
        .1
        .as_str();
    assert!(
        full_spawn.len() > bounded_spawn.len(),
        "full raw spawn failure must be retained"
    );
    assert!(bounded_spawn.contains("[Showing lines"));
}

#[tokio::test]
async fn update_goal_blocked_cannot_resurrect_budget_limited_goal() {
    let identity_dir = tempfile::tempdir().unwrap();
    let identity =
        KeyIdentity::load_or_create(identity_dir.path().join("owner.key"), None).unwrap();
    let data_path =
        std::env::temp_dir().join(format!("agent-hook-goal-guard-{}", uuid::Uuid::new_v4()));
    let node = Arc::new(
        gents_store::test_support::node_builder()
            .data_path(&data_path)
            .with_node_identity_did(identity.did())
            .build()
            .await
            .expect("embedded node"),
    );
    ensure_runtime_schemas(&node).await.unwrap();

    let mut hook =
        DefraSessionHook::with_identity(node.clone(), identity.did(), FailurePolicy::default())
            .with_goal_tool_authority(true, false);
    assert!(matches!(
        hook.on_completion_call(&user_text_message("start goal"), &[])
            .await,
        HookAction::Continue
    ));
    let session_id = hook.session_id().await.expect("session id");
    bind_managed_hook_request(
        &mut hook,
        "goal-wrapup-request",
        &identity,
        Utc::now() + chrono::Duration::minutes(5),
    )
    .await;
    let access = hook.actor_access().unwrap();
    gents_automation::goal::set_goal_from_access(
        &access,
        identity.did(),
        &session_id,
        Some("Do not resurrect after budget exhaustion"),
        Some(gents_protocol::goal_status::GoalStatus::Active),
        Some(Some(1)),
        None,
    )
    .await
    .expect("create active goal");
    gents_automation::goal::set_goal_from_access(
        &access,
        identity.did(),
        &session_id,
        None,
        Some(gents_protocol::goal_status::GoalStatus::BudgetLimited),
        None,
        None,
    )
    .await
    .expect("latch budget-limited goal");
    let request_doc = hook.active_request_doc_id().await.unwrap();
    access
        .transact_write("test.bind_goal_wrapup", |txn| {
            let request_doc = &request_doc;
            let session_id = &session_id;
            let owner = identity.did();
            Box::pin(async move {
                let goal = gents_automation::goal::load_canonical_goal_in_txn(txn, owner, session_id)
                    .await?
                    .expect("Goal exists");
                gents_automation::goal::bind_goal_first_request_in_txn(txn, &goal, request_doc, None).await
            })
        })
        .await
        .unwrap();

    accept_hook_tool_call(
        &hook,
        "blocked-during-wrapup",
        gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME,
        r#"{"status":"blocked","reason":"needs approval"}"#,
        None,
    )
    .await;
    let action = hook
        .on_tool_call(
            gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME,
            None,
            "blocked-during-wrapup",
            r#"{"status":"blocked","reason":"needs approval"}"#,
        )
        .await;
    assert!(matches!(action, ToolCallHookAction::Skip { .. }));
    let goal = access
        .transact_readonly("test.goal_wrapup", |txn| {
            Box::pin(gents_automation::goal::load_canonical_goal_in_txn(
                txn,
                identity.did(),
                &session_id,
            ))
        })
        .await
        .expect("load goal")
        .expect("goal exists");
    assert_eq!(
        goal.parsed_status(),
        Some(gents_protocol::goal_status::GoalStatus::BudgetLimited)
    );
    assert_eq!(goal.consecutive_blocked_audits, Some(0));
    assert_eq!(goal.wrapup_requested, Some(true));
    assert_eq!(goal.wrapup_completed, Some(false));

    node.shutdown().await.expect("shut down test node");
    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn update_goal_edit_from_non_active_reports_status_and_resume_path() {
    let identity_dir = tempfile::tempdir().unwrap();
    let identity =
        KeyIdentity::load_or_create(identity_dir.path().join("owner.key"), None).unwrap();
    let data_path =
        std::env::temp_dir().join(format!("agent-hook-goal-update-{}", uuid::Uuid::new_v4()));
    // The hook's session is signed by the runtime it runs as.
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
            .data_path(&data_path)
            .build()
            .await
            .expect("embedded node"),
    );
    gents_store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let mut hook =
        DefraSessionHook::with_identity(node.clone(), identity.did(), FailurePolicy::default())
            .with_goal_tool_authority(true, false);
    // The hook acts as the runtime that signs its session and requests.
    hook.actor = Some(identity::Did::new(identity.did()).unwrap());
    assert!(matches!(
        hook.on_completion_call(&user_text_message("start goal"), &[])
            .await,
        HookAction::Continue
    ));
    let session_id = hook.session_id().await.expect("session id");

    // Accepted active edit through the hook path: no status, objective updated,
    // stale-race message absent. Complements the edit_active_goal contract tests
    // (edited_fields_match_operator_updates_without_changing_goal_identity),
    // which cover active-edit success and CAS/identity rejection at the edit layer.
    {
        set_goal_as_session_owner(
            &node,
            &identity,
            &session_id,
            Some("blocked-edit semantics"),
            Some(gents_protocol::goal_status::GoalStatus::Active),
            None,
        )
        .await
        .expect("create active goal");
        goal_context::bind_signed_request(
            &node,
            &hook,
            &identity,
            &format!("goal-update-{}", uuid::Uuid::new_v4()),
            &session_id,
            None,
        )
        .await;
        let call_id = "active-edit-accepted";
        let args = r#"{"objective":"revised while active"}"#;
        accept_hook_tool_call(
            &hook,
            call_id,
            gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME,
            args,
            None,
        )
        .await;
        let ToolCallHookAction::Skip {
            reason: tool_result,
        } = hook
            .on_tool_call(
                gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME,
                None,
                call_id,
                args,
            )
            .await
        else {
            panic!("expected hook-managed skip for {call_id}");
        };
        assert!(
            serde_json::from_str::<serde_json::Value>(&tool_result).expect("JSON tool result")
                ["accepted"]
                == true,
            "active edit must be accepted: {tool_result}"
        );
        let goal = gents_automation::goal::load_canonical_goal(&node, identity.did(), &session_id)
            .await
            .expect("load goal")
            .expect("goal exists");
        assert_eq!(goal.objective, "revised while active");
        gents_automation::goal::delete_goals_for_session_from_access(
            &hook.actor_access().unwrap(),
            identity.did(),
            &session_id,
        )
        .await
        .expect("clear goal");
    }

    for (status, expected_hint) in [
        (
            gents_protocol::goal_status::GoalStatus::Blocked,
            "gents automation goal resume-request --session SESSION --from REQUEST_ID` (REQUEST_ID is the terminal latest predecessor request)",
        ),
        (
            gents_protocol::goal_status::GoalStatus::Paused,
            "gents automation goal resume-request --session SESSION --from REQUEST_ID` (REQUEST_ID is the terminal latest predecessor request)",
        ),
        (
            gents_protocol::goal_status::GoalStatus::UsageLimited,
            "gents automation goal resume-request --session SESSION --from REQUEST_ID` (REQUEST_ID is the terminal latest predecessor request)",
        ),
        (
            gents_protocol::goal_status::GoalStatus::Complete,
            "a complete goal is terminal and has no resume path",
        ),
        (
            gents_protocol::goal_status::GoalStatus::BudgetLimited,
            "a budget_limited goal cannot be reactivated and has no resume path",
        ),
    ] {
        set_goal_as_session_owner(
            &node,
            &identity,
            &session_id,
            Some("blocked-edit semantics"),
            Some(gents_protocol::goal_status::GoalStatus::Active),
            None,
        )
        .await
        .expect("create active goal");
        goal_context::bind_signed_request(&node, &hook, &identity, &format!("goal-update-{}", uuid::Uuid::new_v4()), &session_id, None).await;
        set_goal_as_session_owner(
            &node,
            &identity,
            &session_id,
            None,
            Some(status),
            None,
        )
        .await
        .expect("latch status");
        let before = gents_automation::goal::load_canonical_goal(&node, identity.did(), &session_id)
            .await
            .expect("load before")
            .expect("goal exists");
        for (index, args) in [
            r#"{"objective":"revised"}"#,
            r#"{"status":"active","objective":"revised"}"#,
            r#"{"status":"active"}"#,
            r#"{"status":" active ","objective":"revised"}"#,
            r#"{"token_budget":1000}"#,
        ]
        .into_iter()
        .enumerate()
        {
            let call_id = format!("blocked-edit-{}-{index}", status.as_str());
            accept_hook_tool_call(
                &hook,
                &call_id,
                gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME,
                args,
                None,
            )
            .await;
            let ToolCallHookAction::Skip {
                reason: tool_result,
            } = hook
                .on_tool_call(gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME, None, &call_id, args)
                .await
            else {
                panic!("expected hook-managed skip for {call_id}");
            };
            assert!(
                serde_json::from_str::<serde_json::Value>(&tool_result).expect("JSON tool result")
                    ["accepted"]
                    == false,
                "{tool_result}"
            );

            assert!(
                tool_result.contains(&format!("the current goal status is {}", status.as_str())),
                "{call_id}: {tool_result}"
            );
            assert!(
                tool_result.contains(expected_hint),
                "{call_id}: expected {expected_hint:?} in {tool_result}"
            );
            assert!(
                !tool_result.contains("no longer active or its continuation changed"),
                "{call_id}: stale-race message must not appear for a fresh non-active goal: {tool_result}"
            );
            let goal = gents_automation::goal::load_canonical_goal(&node, identity.did(), &session_id)
                .await
                .expect("load goal")
                .expect("goal exists");
            assert_eq!(
                goal.parsed_status(),
                Some(status),
                "{call_id}: refused edit must not change status"
            );
            assert_eq!(
                goal.objective, "blocked-edit semantics",
                "{call_id}: refused edit must not change objective"
            );
            assert_eq!(goal.token_budget, before.token_budget);
            assert_eq!(goal.continuation_sequence, before.continuation_sequence);
            assert_eq!(goal.goal_id, before.goal_id);
            assert_eq!(goal.updated_at, before.updated_at);
        }
        // Rebuild from a fresh active goal for the next case; complete and
        // budget_limited cannot be reset to active via set_goal.
        gents_automation::goal::delete_goals_for_session_from_access(&hook.actor_access().unwrap(), identity.did(), &session_id)
            .await
            .expect("clear goal for next case");
    }

    node.shutdown().await.expect("shut down test node");
    let _ = std::fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn goal_completion_shares_output_gate_and_preserves_operator_override() {
    let identity_dir = tempfile::tempdir().unwrap();
    let identity =
        KeyIdentity::load_or_create(identity_dir.path().join("owner.key"), None).unwrap();
    use gents_automation::goal::load_canonical_goal;
    use crate::runtime::agent::output_obligation::{ActiveOutputObligation, OutputObligationGate};
    use gents_documents::{WriteToolOutputObligation, WriteToolOutputObligationScope};
    use gents_protocol::goal_status::GoalStatus;

    // The hook's session is signed by the runtime it runs as.
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap(),
    );
    ensure_runtime_schemas(&node).await.unwrap();
    let mut hook =
        DefraSessionHook::with_identity(node.clone(), identity.did(), FailurePolicy::default())
            .with_goal_tool_authority(true, false);
    // The hook acts as the runtime that signs its session and requests.
    hook.actor = Some(identity::Did::new(identity.did()).unwrap());
    assert!(matches!(
        hook.on_completion_call(&user_text_message("publish required review output"), &[])
            .await,
        HookAction::Continue
    ));
    let session = hook.session_id().await.unwrap();
    set_goal_as_session_owner(
        &node,
        &identity,
        &session,
        Some("publish review output"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .unwrap();
    goal_context::bind_signed_request(
        &node,
        &hook,
        &identity,
        "goal-output-request",
        &session,
        None,
    )
    .await;
    let request_doc = hook.active_request_doc_id().await.unwrap();
    let gate = OutputObligationGate::new(
        node.clone(),
        identity::Did::new(identity.did()).unwrap(),
        request_doc.clone(),
        vec![ActiveOutputObligation {
            tool_name: "write_scan_result".into(),
            contract: WriteToolOutputObligation {
                scope: WriteToolOutputObligationScope::Trigger,
                minimum_writes: 1,
                expected_count_field: None,
            },
        }],
    );
    let hook = hook.with_output_obligation_gate(Some(Arc::new(gate.clone())));
    let persistence_hook = hook.clone();
    let before = load_canonical_goal(&node, identity.did(), &session)
        .await
        .unwrap()
        .unwrap();
    accept_hook_tool_call(
        &persistence_hook,
        "premature-goal-complete",
        gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME,
        r#"{"status":"complete","reason":"text alone is not output"}"#,
        None,
    )
    .await;
    assert!(matches!(
        persistence_hook
            .on_tool_call(
                gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME,
                None,
                "premature-goal-complete",
                r#"{"status":"complete","reason":"text alone is not output"}"#,
            )
            .await,
        ToolCallHookAction::Skip { .. }
    ));
    let denied = fetch_tool_call_row(&hook, &session, "premature-goal-complete").await;
    assert_eq!(denied["lifecycle_state"], "completed");
    let result: serde_json::Value =
        serde_json::from_str(denied["result"].as_str().unwrap()).unwrap();
    assert_eq!(result["accepted"], false);
    assert!(result["error"]
        .as_str()
        .unwrap()
        .contains("write_scan_result"));
    let after = load_canonical_goal(&node, identity.did(), &session)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(after.continuation_sequence, before.continuation_sequence);
    assert_eq!(after.completion_evidence, before.completion_evidence);

    accept_hook_tool_call(
        &persistence_hook,
        "required-output",
        "write_scan_result",
        "{}",
        None,
    )
    .await;
    assert!(matches!(
        persistence_hook
            .on_tool_call("write_scan_result", None, "required-output", "{}")
            .await,
        ToolCallHookAction::Continue
    ));
    let mut output = persistence_hook
        .in_flight_lifecycles
        .lock()
        .await
        .remove("required-output")
        .expect("accepted write lifecycle");
    assert_eq!(gate.unmet().await.unwrap().len(), 1);
    output
        .complete("persisted scan result acknowledgment")
        .await
        .unwrap();
    assert!(gate.unmet().await.unwrap().is_empty());
    accept_hook_tool_call(
        &persistence_hook,
        "acknowledged-goal-complete",
        gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME,
        r#"{"status":"complete","reason":"required scan output is durable"}"#,
        None,
    )
    .await;
    assert!(matches!(
        persistence_hook
            .on_tool_call(
                gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME,
                None,
                "acknowledged-goal-complete",
                r#"{"status":"complete","reason":"required scan output is durable"}"#,
            )
            .await,
        ToolCallHookAction::Skip { .. }
    ));
    let accepted = fetch_tool_call_row(&hook, &session, "acknowledged-goal-complete").await;
    assert_eq!(accepted["lifecycle_state"], "completed");
    let result: serde_json::Value =
        serde_json::from_str(accepted["result"].as_str().unwrap()).unwrap();
    assert_eq!(result["accepted"], true);
    let complete = load_canonical_goal(&node, identity.did(), &session)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(complete.parsed_status(), Some(GoalStatus::Complete));

    set_goal_as_session_owner(
        &node,
        &identity,
        "operator-output-session",
        Some("operator decision"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .unwrap();
    let empty_gate = OutputObligationGate::new(
        node.clone(),
        identity::Did::new(identity.did()).unwrap(),
        "operator-unwritten-request",
        vec![ActiveOutputObligation {
            tool_name: "write_scan_result".into(),
            contract: WriteToolOutputObligation {
                scope: WriteToolOutputObligationScope::Trigger,
                minimum_writes: 1,
                expected_count_field: None,
            },
        }],
    );
    assert_eq!(empty_gate.unmet().await.unwrap().len(), 1);
    set_goal_as_session_owner(
        &node,
        &identity,
        "operator-output-session",
        None,
        Some(GoalStatus::Complete),
        None,
    )
    .await
    .unwrap();
    let operator = load_canonical_goal(&node, identity.did(), "operator-output-session")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(operator.parsed_status(), Some(GoalStatus::Complete));
    assert_eq!(empty_gate.unmet().await.unwrap().len(), 1);
    drop(persistence_hook);
    drop(hook);
    node.shutdown().await.expect("shut down test node");
}

/// Writes the session's Goal as the runtime that signs the session.
async fn set_goal_as_session_owner(
    node: &Arc<EmbeddedNode>,
    owner: &KeyIdentity,
    session_id: &str,
    objective: Option<&str>,
    status: Option<gents_protocol::goal_status::GoalStatus>,
    token_budget: Option<Option<i64>>,
) -> anyhow::Result<gents_automation::goal::GoalDocument> {
    let access = ensure_runtime_session(node, owner, session_id).await?;
    gents_automation::goal::set_goal_from_access(
        &access,
        owner.did(),
        session_id,
        objective,
        status,
        token_budget,
        None,
    )
    .await
}

#[tokio::test]
async fn dispatch_receipt_scripts_bind_call_hook_under_both_persistence_policies() {
    let cases = &gents_lean_vocab::lean_contract_snapshot().canonical_dispatch_observation_cases;
    for case in cases
        .iter()
        .filter(|case| case.inputs.iter().all(|input| input.policy_allows))
    {
        for policy in [FailurePolicy::FailOpen, FailurePolicy::FailClosed] {
            let home = tempfile::tempdir().unwrap();
            let root = KeyIdentity::load_or_create(home.path().join("node.key"), None).unwrap();
            let node = Arc::new(
                EmbeddedNode::builder()
                    .with_node_identity_did(root.did())
                    .build()
                    .await
                    .unwrap(),
            );
            ensure_runtime_schemas(&node).await.unwrap();
            let mut hook = DefraSessionHook::with_identity(node.clone(), root.did(), policy);
            assert!(matches!(
                hook.on_completion_call(&user_text_message("Run a tool"), &[])
                    .await,
                HookAction::Continue
            ));
            let session_id = hook.session_id().await.unwrap();
            bind_managed_hook_request(
                &mut hook,
                "request-receipt",
                &root,
                chrono::Utc::now() + chrono::Duration::minutes(5),
            )
            .await;
            accept_hook_tool_call(&hook, "receipt-call", "read", "{}", None).await;
            let accepted = hook.accepted_tool_calls.lock().await["receipt-call"].clone();
            let admitted = fetch_tool_call_row(&hook, &session_id, "receipt-call").await;
            assert_eq!(case.inputs.len(), case.expected.len());
            for (input, expected) in case.inputs.iter().zip(&case.expected) {
                if expected.observation == "replay" {
                    hook.adopt_accepted_tool_calls(vec![("receipt-call".into(), accepted.clone())])
                        .await
                        .unwrap();
                }
                let call = hook.on_tool_call("read", None, "receipt-call", "{}");
                let action = if input.acknowledged {
                    call.await
                } else {
                    let (action, fired) =
                        gents_store::config_client::ConfigApplyTxn::with_post_commit_receipt_loss(
                            call,
                        )
                        .await;
                    assert!(fired, "{}: receipt fault must fire", case.name);
                    action
                };
                assert_eq!(
                    matches!(action, ToolCallHookAction::Continue),
                    expected.may_invoke,
                    "{}: {action:?}",
                    case.name
                );
                if expected.observation == "replay" {
                    assert!(
                        matches!(&action, ToolCallHookAction::Terminate { reason }
                        if reason.contains("no longer pending")),
                        "{action:?}"
                    );
                }
                let row = fetch_tool_call_row(&hook, &session_id, "receipt-call").await;
                assert_eq!(
                    row["lifecycle_state"].as_str() == Some("running"),
                    expected.running,
                    "{}: durable dispatch state",
                    case.name
                );
            }
            let mut fixture = hook_execution_fixtures()
                .lock()
                .await
                .remove(&hook_execution_fixture_key(&hook, "request-receipt"))
                .unwrap();
            let selection = fixture
                .writer
                .terminal_output(&fixture.lifecycle.request().doc_id)
                .await;
            let outcome = match case.parent_outcome.as_str() {
                "failed" => gents_session::request::lifecycle::RequestTerminalOutcome::Failed,
                other => panic!("unsupported modeled parent outcome {other}"),
            };
            let completion_outcome = match case.completion_probe_outcome.as_str() {
                "completed" => gents_session::request::lifecycle::RequestTerminalOutcome::Completed,
                other => panic!("unsupported modeled completion probe {other}"),
            };
            assert!(
                !case.completion_probe_accepted,
                "{}: this adapter branch requires a running-call completion rejection",
                case.name
            );
            let document =
                gents_store::graphql::escape_graphql_string(&fixture.lifecycle.request().doc_id);
            let request_query = format!(
                "{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{document}\" }} }}) {{ _docID lifecycle_state execution_generation terminal_output }} }}"
            );
            let access = hook.actor_access().unwrap();
            let before_probe = access.execute(&request_query).await.unwrap();
            let before_request = before_probe["data"]["AgentRequest"].as_array().unwrap();
            assert_eq!(before_request.len(), 1);
            let completion = fixture
                .lifecycle
                .terminalize_owned(completion_outcome, selection.clone(), None)
                .await;
            assert_eq!(completion.is_ok(), case.completion_probe_accepted);
            let native_in_flight = completion.as_ref().err().is_some_and(|rejection| {
                matches!(
                    rejection.downcast_ref::<gents_session::request::lifecycle::ToolAccountingRejection>(),
                    Some(gents_session::request::lifecycle::ToolAccountingRejection::ForegroundRunning)
                )
            });
            assert_eq!(
                native_in_flight,
                case.expected.last().unwrap().in_flight,
                "{}: {completion:?}",
                case.name
            );
            let after_probe = access.execute(&request_query).await.unwrap();
            assert_eq!(
                after_probe["data"]["AgentRequest"].as_array().unwrap(),
                before_request,
                "rejected completion must not terminalize or replace the request generation"
            );
            let terminalized = fixture
                .lifecycle
                .terminalize_owned(outcome, selection, Some("dispatch observation failed"))
                .await
                .unwrap();
            assert_eq!(
                terminalized,
                gents_session::request::lifecycle::TerminalizeResult::Won
            );
            let document =
                gents_store::graphql::escape_graphql_string(&fixture.lifecycle.request().doc_id);
            let observed = access.execute(&format!(
                "{{ AgentToolCall(filter: {{ request_doc_id: {{ _eq: \"{document}\" }} }}) {{ _docID lifecycle_state stuck_since }} AgentMessage(filter: {{ request_doc_id: {{ _eq: \"{document}\" }} }}) {{ _docID }} }}"
            )).await.unwrap();
            let data = &observed["data"];
            let rows = data["AgentToolCall"].as_array().unwrap();
            assert_eq!(
                rows.len(),
                1,
                "the exact committed call survives parent failure"
            );
            assert_eq!(rows[0]["_docID"], admitted["_docID"]);
            let expected = &case.expected_after_parent_failure;
            assert_eq!(
                rows[0]["lifecycle_state"].as_str() == Some("running"),
                expected.running
            );
            assert_eq!(rows[0]["stuck_since"].is_string(), expected.needs_recovery);
            assert_eq!(
                data["AgentMessage"].as_array().unwrap().len(),
                expected.message_count
            );
            let recovery = case
                .expected_after_parent_recovery
                .as_ref()
                .expect("a handed-off running call has a modeled recovery");
            let report =
                gents_toolcalls::call::lifecycle::ToolCallLifecycle::reconcile_terminal_parent_owned_tools(
                    &node,
                    root.did(),
                    &[Arc::new(fixture.lifecycle.managed_agent_signer().unwrap())],
                )
                .await
                .unwrap();
            assert_eq!(
                report.tool_calls_terminalized, recovery.terminalized,
                "{}",
                case.name
            );
            let recovered = fetch_tool_call_row(&hook, &session_id, "receipt-call").await;
            assert_eq!(recovered["_docID"], admitted["_docID"]);
            assert_eq!(
                recovered["lifecycle_state"].as_str(),
                Some(recovery.state.as_str()),
                "{}: terminal-parent recovery settles the handed-off call",
                case.name
            );
            hook.adopt_accepted_tool_calls(vec![("receipt-call".into(), accepted.clone())])
                .await
                .unwrap();
            let replay = hook.on_tool_call("read", None, "receipt-call", "{}").await;
            assert_eq!(
                matches!(replay, ToolCallHookAction::Continue),
                recovery.dispatchable,
                "{}: a settled call is never re-elected: {replay:?}",
                case.name
            );
            let after_replay = fetch_tool_call_row(&hook, &session_id, "receipt-call").await;
            assert_eq!(
                after_replay["lifecycle_state"],
                recovered["lifecycle_state"]
            );
            node.shutdown().await.expect("shut down test node");
        }
    }
}

#[tokio::test]
async fn call_tool_persists_concrete_dispatch_identity_without_rewriting_alias() {
    use gents_documents::{RemoteServiceTools, RemoteToolStyle, RemoteTools};

    let fixture = background_budget::budget_hook_fixture(
        "selected-tool-dispatch",
        BackgroundToolRegistry::default(),
        BackgroundExecutionRegistry::default(),
    )
    .await;
    let node = fixture.db.node.clone();
    let session_id = fixture.session_id.clone();
    let hook = fixture.hook.with_remote_tools(Some(RemoteTools {
        services: vec![RemoteServiceTools {
            mcp_service_id: "metrics-prod".into(),
            tool_names: vec!["query_metrics".into()],
            style: RemoteToolStyle::Discovery,
            ..Default::default()
        }],
    }));
    assert!(matches!(
        hook.on_completion_call(&user_text_message("Query metrics"), &[])
            .await,
        HookAction::Continue
    ));

    async fn selected_row(
        access: &gents_store::config_client::ConfigAccess,
        session_id: &str,
        tool_call_id: &str,
    ) -> serde_json::Value {
        let session = gents_store::graphql::escape_graphql_string(session_id);
        let call = gents_store::graphql::escape_graphql_string(tool_call_id);
        let response = access
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session}" }}, tool_call_id: {{ _eq: "{call}" }} }}) {{ selected_service_id selected_tool_name }} }}"#
            ))
            .await
            .expect("selected agent can read its tool call");
        let rows = response["data"]["AgentToolCall"]
            .as_array()
            .expect("tool call rows");
        let [row] = rows.as_slice() else {
            panic!("expected one tool call, found {}", rows.len());
        };
        row.clone()
    }
    let access = hook.actor_access().expect("hook has a selected agent");

    let args =
        r#"{"service_id":"metrics-prod","tool_name":"query_metrics","arguments":{"window":"5m"}}"#;
    accept_hook_tool_call(&hook, "call-selected", "call_tool", args, None).await;
    assert!(matches!(
        hook.on_tool_call("call_tool", None, "call-selected", args)
            .await,
        ToolCallHookAction::Continue
    ));
    let selected = selected_row(&access, &session_id, "call-selected").await;
    assert_eq!(
        selected
            .get("selected_service_id")
            .and_then(serde_json::Value::as_str),
        Some("metrics-prod")
    );
    assert_eq!(
        selected
            .get("selected_tool_name")
            .and_then(serde_json::Value::as_str),
        Some("query_metrics")
    );

    accept_hook_tool_call(&hook, "call-native", "read_file", "{}", None).await;
    assert!(matches!(
        hook.on_tool_call("read_file", None, "call-native", "{}")
            .await,
        ToolCallHookAction::Continue
    ));
    let native = selected_row(&access, &session_id, "call-native").await;
    assert!(native
        .get("selected_service_id")
        .is_none_or(serde_json::Value::is_null));
    assert!(native
        .get("selected_tool_name")
        .is_none_or(serde_json::Value::is_null));

    node.shutdown().await.expect("shut down test node");
}
