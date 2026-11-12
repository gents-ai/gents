use std::sync::Arc;

use chrono::Utc;
use defra_node::Node as EmbeddedNode;
use gents_inference::llm::{HookAction, ToolCallHookAction};
use gents_store::identity::{KeyIdentity, NodeIdentity};
use gents_store::schema::ensure_runtime_schemas;
use gents_toolcalls::call::hook::fixtures::*;
use gents_toolcalls::call::hook::{DefraSessionHook, FailurePolicy};
use serde_json::json;

use crate::test_support::goal_hook::{bind_signed_request, set_goal_as_session_owner};

mod goal_context;

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
    crate::lifecycle_hooks::attach(&node).unwrap();
    let mut hook =
        DefraSessionHook::with_identity(node.clone(), identity.did(), FailurePolicy::default())
            .with_goal_tool_authority(true, true)
            .with_goal_tools(Arc::new(crate::goal::goal_tools::GoalToolAnswers));
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
            Box::pin(crate::goal::load_canonical_goal_in_txn(
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
            Box::pin(crate::goal::load_canonical_goal_in_txn(
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
                Box::pin(crate::goal::load_canonical_goal_in_txn(
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
                Box::pin(crate::goal::load_canonical_goal_in_txn(
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
                Box::pin(crate::goal::load_canonical_goal_in_txn(
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
            crate::lifecycle_hooks::attach(&node).unwrap();
            let mut hook = DefraSessionHook::with_identity(node.clone(), identity.did(), policy)
                .with_goal_tool_authority(goal_case.goal_tools, goal_case.goal_create)
                .with_goal_tools(Arc::new(crate::goal::goal_tools::GoalToolAnswers));
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
                    Box::pin(crate::goal::load_canonical_goal_in_txn(
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
                        Box::pin(crate::goal::load_canonical_goal_in_txn(
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
    crate::lifecycle_hooks::attach(&node).unwrap();

    let mut hook =
        DefraSessionHook::with_identity(node.clone(), identity.did(), FailurePolicy::default())
            .with_goal_tool_authority(true, false)
            .with_goal_tools(Arc::new(crate::goal::goal_tools::GoalToolAnswers));
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
    crate::goal::set_goal_from_access(
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
    crate::goal::set_goal_from_access(
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
                let goal = crate::goal::load_canonical_goal_in_txn(txn, owner, session_id)
                    .await?
                    .expect("Goal exists");
                crate::goal::bind_goal_first_request_in_txn(txn, &goal, request_doc, None).await
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
            Box::pin(crate::goal::load_canonical_goal_in_txn(
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
    crate::lifecycle_hooks::attach(&node).unwrap();
    let mut hook =
        DefraSessionHook::with_identity(node.clone(), identity.did(), FailurePolicy::default())
            .with_goal_tool_authority(true, false)
            .with_goal_tools(Arc::new(crate::goal::goal_tools::GoalToolAnswers));
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
        bind_signed_request(
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
        let goal = crate::goal::load_canonical_goal(&node, identity.did(), &session_id)
            .await
            .expect("load goal")
            .expect("goal exists");
        assert_eq!(goal.objective, "revised while active");
        crate::goal::delete_goals_for_session_from_access(
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
        bind_signed_request(&node, &hook, &identity, &format!("goal-update-{}", uuid::Uuid::new_v4()), &session_id, None).await;
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
        let before = crate::goal::load_canonical_goal(&node, identity.did(), &session_id)
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
            let goal = crate::goal::load_canonical_goal(&node, identity.did(), &session_id)
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
        crate::goal::delete_goals_for_session_from_access(&hook.actor_access().unwrap(), identity.did(), &session_id)
            .await
            .expect("clear goal for next case");
    }

    node.shutdown().await.expect("shut down test node");
    let _ = std::fs::remove_dir_all(&data_path);
}
