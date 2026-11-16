use std::sync::Arc;

use gents_inference::llm::ToolCallHookAction;

use gents_toolcalls::call::hook::fixtures::*;

use gents_loop::SessionHook;
use gents_toolcalls::test_support::script_composition::*;
#[tokio::test]
async fn goal_tool_uses_its_owner_when_composed() {
    let fixture = gents_toolcalls::test_support::background_hook::budget_hook_fixture(
        "script-goal",
        Default::default(),
        Default::default(),
        gents_automation::lifecycle_hooks::composition(),
    )
    .await;
    bind_admitted_deadline(&fixture).await;
    let hook = fixture
        .hook
        .with_goal_tool_authority(true, false)
        .with_goal_tools(Arc::new(
            gents_automation::goal::goal_tools::GoalToolAnswers,
        ));
    accept_hook_tool_call(&hook, "script", "plugin", "{}", None).await;
    assert!(matches!(
        hook.on_tool_call("plugin", None, "script", "{}").await,
        ToolCallHookAction::Continue
    ));
    let child = hook
        .admit_tool_effect(
            "script",
            1,
            gents_protocol::goal_names::GET_GOAL_TOOL_NAME,
            "{}",
        )
        .await
        .unwrap();
    assert!(matches!(
        hook.on_tool_call(
            gents_protocol::goal_names::GET_GOAL_TOOL_NAME,
            None,
            &child,
            "{}"
        )
        .await,
        ToolCallHookAction::Skip { .. }
    ));
    hook.finish_tool_effect(&child, None).await.unwrap();
    fixture.db.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_stopped_invocation_times_out_the_child_it_left_running() {
    let fixture = gents_toolcalls::test_support::background_hook::budget_hook_fixture(
        "script-timeout-child",
        Default::default(),
        Default::default(),
        gents_automation::lifecycle_hooks::composition(),
    )
    .await;
    bind_admitted_deadline(&fixture).await;
    let hook = &fixture.hook;
    accept_hook_tool_call(hook, "script", "plugin", "{}", None).await;
    assert!(matches!(
        hook.on_tool_call("plugin", None, "script", "{}").await,
        ToolCallHookAction::Continue
    ));
    let pending = hook
        .admit_tool_effect("script", 1, "stall", "{}")
        .await
        .unwrap();
    let running = hook
        .admit_tool_effect("script", 2, "stall", "{}")
        .await
        .unwrap();
    assert!(matches!(
        hook.on_tool_call("stall", None, &running, "{}").await,
        ToolCallHookAction::Continue
    ));
    hook.settle_tool_effects(
        "script",
        &gents_loop::tool_call_lifecycle::ToolOutcome::TimedOut { deadline_at: None },
    )
    .await
    .unwrap();
    for (child, state) in [(&pending, "cancelled"), (&running, "timedOut")] {
        let row = gents_toolcalls::test_support::background_hook::tool_row(
            &fixture.access,
            &fixture.session_id,
            child,
        )
        .await;
        assert_eq!(row["lifecycle_state"], state, "{child}");
    }
    assert!(hook.effect_lifecycles.lock().await.is_empty());
    assert!(hook.effect_tool_calls.lock().await.is_empty());
    fixture.db.node.shutdown().await.unwrap();
}
