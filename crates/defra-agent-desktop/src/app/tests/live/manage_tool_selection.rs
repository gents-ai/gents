use super::*;

const TOOL_SELECTION_PROMPT: &str = "When the user asks for notes.txt, you must call read_file for notes.txt and reply with only the token from that file.";

fn wait_for_behavior_tooling_config(
    runtime: &tokio::runtime::Runtime,
    desktop_client: &ClientCore,
    deployment: &LiveDeploymentCase<'_>,
) -> Result<()> {
    wait_for_value(
        "desktop behavior/tool selection config persisted",
        Duration::from_secs(60),
        || {
            runtime.block_on(desktop_client.refresh_store()).ok()?;
            let snapshot = desktop_client.store().snapshot();
            let behavior_ready = snapshot
                .behaviors
                .iter()
                .find(|row| row.behavior_id == deployment.docs.behavior_id)
                .is_some_and(|row| row.system_prompt.as_deref() == Some(TOOL_SELECTION_PROMPT));
            let tools_ready = snapshot
                .tool_selections
                .iter()
                .find(|row| row.selection_id == deployment.docs.tool_selection_id)
                .is_some_and(|row| {
                    row.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                        && row.enable_file_tools == Some(true)
                        && row.file_tools_mode.as_deref() == Some("ReadOnly")
                });
            (behavior_ready && tools_ready).then_some(())
        },
    )?;

    wait_for_value(
        "remote behavior/tool selection config replicated",
        Duration::from_secs(90),
        || {
            runtime
                .block_on(deployment.remote_core.refresh_store())
                .ok()?;
            let snapshot = deployment.remote_core.store().snapshot();
            let behavior_ready = snapshot
                .behaviors
                .iter()
                .find(|row| row.behavior_id == deployment.docs.behavior_id)
                .is_some_and(|row| row.system_prompt.as_deref() == Some(TOOL_SELECTION_PROMPT));
            let tools_ready = snapshot
                .tool_selections
                .iter()
                .find(|row| row.selection_id == deployment.docs.tool_selection_id)
                .is_some_and(|row| {
                    row.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                        && row.enable_file_tools == Some(true)
                        && row.file_tools_mode.as_deref() == Some("ReadOnly")
                });
            (behavior_ready && tools_ready).then_some(())
        },
    )?;

    wait_for_stable_runtime_ready(
        runtime,
        deployment.remote_core,
        "after tool selection config replication",
        &deployment.agent_did,
        Duration::from_secs(10),
        Duration::from_secs(90),
    )
}

fn apply_behavior_tool_prompt(
    driver: &mut AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
) -> Result<()> {
    open_manage_entity_and_assert_visibility(
        driver,
        deployment,
        ManageSection::Behaviors,
        &deployment.docs.behavior_id,
        &[],
        "behavior row before tool policy edit",
    )?;
    driver.replace_text_in_target(
        &audit::targets::manage_field("System Prompt"),
        TOOL_SELECTION_PROMPT,
    );
    match driver.app.state.manage.draft.as_ref() {
        Some(ManageDraft::Behavior(draft)) => {
            assert_eq!(draft.behavior_id, deployment.docs.behavior_id);
            assert_eq!(draft.system_prompt, TOOL_SELECTION_PROMPT);
        }
        other => panic!("expected behavior draft while editing tool prompt, got {other:?}"),
    }
    driver.click_target(audit::targets::MANAGE_APPLY);
    Ok(())
}

fn apply_tool_selection_read_only(
    driver: &mut AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
) -> Result<()> {
    open_manage_entity_and_assert_visibility(
        driver,
        deployment,
        ManageSection::ToolSelections,
        &deployment.docs.tool_selection_id,
        &[],
        "tool selection row before tool policy edit",
    )?;

    let enable_file_tools = matches!(
        driver.app.state.manage.draft.as_ref(),
        Some(ManageDraft::ToolSelection(draft)) if draft.enable_file_tools
    );
    if !enable_file_tools {
        driver.click_target(&audit::targets::manage_toggle("Enable File Tools"));
    }
    driver.replace_text_in_target(
        &audit::targets::manage_field("File Tools Mode"),
        "ReadOnly",
    );

    match driver.app.state.manage.draft.as_ref() {
        Some(ManageDraft::ToolSelection(draft)) => {
            assert_eq!(draft.selection_id, deployment.docs.tool_selection_id);
            assert_eq!(draft.agent_did, deployment.agent_did);
            assert!(draft.enable_file_tools);
            assert_eq!(draft.file_tools_mode, "ReadOnly");
        }
        other => panic!("expected tool selection draft, got {other:?}"),
    }

    driver.click_target(audit::targets::MANAGE_APPLY);
    Ok(())
}

#[test]
#[ignore = "hits live inference backend configured by DEFRA_AGENT_DESKTOP_LIVE_BACKEND_* or OPENROUTER_API_KEY"]
fn desktop_app_live_manage_tool_selection_enables_read_file() -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    let mut fixture = build_live_desktop_fixture("audit-live-tool-selection", global_log_store())?;
    let remote_core = fixture
        .remote_core
        .as_ref()
        .ok_or_else(|| anyhow!("missing remote core in live fixture"))?;
    let running_agent = fixture
        .running_agent
        .as_ref()
        .ok_or_else(|| anyhow!("missing running agent in live fixture"))?;
    let peer_id = fixture
        .driver
        .app
        .state
        .chat
        .shell
        .selected_peer_id
        .clone()
        .ok_or_else(|| anyhow!("missing selected peer id for live deployment"))?;
    let deployment = LiveDeploymentCase {
        label: "single live deployment".to_string(),
        peer_id,
        agent_did: running_agent.did.clone(),
        docs: fixture.docs.clone(),
        remote_core: remote_core.as_ref(),
    };
    let desktop_client = Arc::clone(
        fixture
            .driver
            .app
            .client
            .as_ref()
            .ok_or_else(|| anyhow!("desktop client missing"))?,
    );

    {
        let driver = &mut fixture.driver;
        apply_behavior_tool_prompt(driver, &deployment)?;
        apply_tool_selection_read_only(driver, &deployment)?;
    }

    wait_for_behavior_tooling_config(
        fixture.runtime.as_ref(),
        desktop_client.as_ref(),
        &deployment,
    )?;

    let prompt = "Call read_file for notes.txt and reply with only the token from notes.txt.";
    let submission = {
        let driver = &mut fixture.driver;
        submit_custom_live_prompt_for_deployment(driver, &deployment, prompt)?
    };

    assert_live_submission_rows(
        fixture.runtime.as_ref(),
        desktop_client.as_ref(),
        "desktop tool-selection live submission",
        &deployment,
        &submission,
        None,
    )?;
    assert_live_submission_rows(
        fixture.runtime.as_ref(),
        deployment.remote_core,
        "remote tool-selection live submission",
        &deployment,
        &submission,
        None,
    )?;
    assert!(
        submission
            .response
            .contains(running_agent.tool_token.as_str()),
        "expected response to contain tool token {}: {}",
        running_agent.tool_token,
        submission.response
    );
    let _tool_card_id = wait_for_session_tool_activity(
        fixture.runtime.as_ref(),
        desktop_client.as_ref(),
        "tool selection tool activity observed",
        &submission.session_id,
        0,
        1,
        std::slice::from_ref(&running_agent.tool_token),
    )?;

    fixture.shutdown()
}
