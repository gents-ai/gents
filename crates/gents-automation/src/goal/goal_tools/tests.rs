use std::sync::Arc;

use chrono::Utc;
use gents_inference::llm::{HookAction, ToolCallHookAction};
use gents_store::identity::NodeIdentity;
use gents_toolcalls::call::hook::fixtures::*;
use gents_toolcalls::call::hook::{DefraSessionHook, FailurePolicy};
use serde_json::json;

mod goal_context;
mod goal_latency;

#[tokio::test]
async fn authorized_goal_hook_derives_ownership_and_runs_create_get_update_lifecycle() {
    let db = gents_inference::test_support::test_db(
        "goal-hook-create",
        crate::lifecycle_hooks::composition(),
    )
    .await;
    let node = db.node.clone();
    let identity = gents_store::test_support::held_runtime_identity(db.node_identity.did());
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
}

#[tokio::test]
async fn control_tool_completion_failure_never_reauthorizes_dispatch() {
    let replays =
        gents_lean_vocab::families::dispatch_observation_contracts_control_replay::cases();
    for replay in &replays {
        assert!(
            replay
                .input
                .inputs
                .iter()
                .all(|step| step.acknowledged && step.policy_allows),
            "{}: the control scenario elects acknowledged, allowed attempts",
            replay.input.name
        );
    }
    for case in gents_lean_vocab::families::contracts_goal_create_committed::cases::<
        crate::goal::GoalCreateDisposition,
    >() {
        let goal_case = &case.input;
        let args = json!({"objective": goal_case.objective}).to_string();
        for policy in [FailurePolicy::FailOpen, FailurePolicy::FailClosed] {
            let mut completion_mutation = None;
            for inject in [false, true] {
                let db = gents_inference::test_support::test_db(
                    "goal-dispatch-replay",
                    crate::lifecycle_hooks::composition(),
                )
                .await;
                let node = db.node.clone();
                let identity =
                    gents_store::test_support::held_runtime_identity(db.node_identity.did());
                let mut hook =
                    DefraSessionHook::with_identity(node.clone(), identity.did(), policy)
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
                    .unwrap();
                case.check(match goal {
                    Some(_) => crate::goal::GoalCreateDisposition::Fresh,
                    None => crate::goal::GoalCreateDisposition::Denied,
                });
                let goal = goal.expect("the control effect committed before completion");
                assert_eq!(goal.objective, goal_case.objective);
                if inject {
                    assert_eq!(Some(mutations), completion_mutation, "fault must fire");
                    assert!(
                        matches!(action, ToolCallHookAction::Terminate { .. }),
                        "{action:?}"
                    );
                    let row = fetch_tool_call_row(&hook, &session, "control-call").await;
                    let again = hook
                        .on_tool_call("create_goal", None, "control-call", &args)
                        .await;
                    for replay in &replays {
                        replay.check(Some(gents_lean_vocab::families::dispatch_observation_contracts_control_replay::ControlReplayOutput {
                            elected_running: row["lifecycle_state"].as_str() == Some("running"),
                            replay_may_invoke: matches!(again, ToolCallHookAction::Continue),
                        }));
                    }
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
                    let completion = trace
                        .iter()
                        .rposition(|operation| {
                            *operation != Some("session.publish_child_relationships")
                        })
                        .expect("the control call commits its completion")
                        + 1;
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
}
