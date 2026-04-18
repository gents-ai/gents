use super::*;

fn wait_for_behavior_edit_replication(
    runtime: &tokio::runtime::Runtime,
    desktop_client: &ClientCore,
    deployment: &LiveDeploymentCase<'_>,
    behavior: &ManageBehaviorForm<'_>,
    desktop_initial_generation: i64,
    remote_initial_generation: i64,
) -> Result<()> {
    wait_for_value(
        "edited behavior observed in desktop store",
        Duration::from_secs(120),
        || {
            runtime.block_on(desktop_client.refresh_store()).ok()?;
            let snapshot = desktop_client.store().snapshot();
            let behavior_ready = snapshot
                .behaviors
                .iter()
                .find(|row| row.behavior_id == behavior.behavior_id)
                .is_some_and(|row| {
                    row.agent_did.as_deref() == Some(behavior.agent_did)
                        && row.display_name.as_deref() == Some(behavior.display_name)
                        && row.system_prompt.as_deref() == Some(behavior.system_prompt)
                        && row.backend_id.as_deref() == Some(behavior.backend_id)
                        && row.model_name.as_deref() == Some(behavior.model_name)
                        && row.tool_selection_id.as_deref() == Some(behavior.tool_selection_id)
                        && row.inference_profile_id.as_deref()
                            == Some(behavior.inference_profile_id)
                        && row.compaction_strategy.as_deref() == Some(behavior.compaction_strategy)
                        && row.compaction_threshold == Some(0.82)
                        && row.enabled == Some(behavior.enabled)
                });
            let runtime_ready = snapshot
                .latest_runtime(&deployment.agent_did)
                .is_some_and(|row| {
                    row.router_generation
                        .or(row.active_generation)
                        .is_some_and(|generation| generation > desktop_initial_generation)
                        && row.runnable_behavior_count == Some(1)
                        && row.unavailable_behavior_count == Some(0)
                        && row
                            .last_reconcile_error
                            .as_deref()
                            .unwrap_or_default()
                            .trim()
                            .is_empty()
                });
            (behavior_ready && runtime_ready).then_some(())
        },
    )?;

    wait_for_value(
        "edited behavior replicated to remote runtime",
        Duration::from_secs(120),
        || {
            runtime
                .block_on(deployment.remote_core.refresh_store())
                .ok()?;
            let snapshot = deployment.remote_core.store().snapshot();
            let behavior_ready = snapshot
                .behaviors
                .iter()
                .find(|row| row.behavior_id == behavior.behavior_id)
                .is_some_and(|row| {
                    row.agent_did.as_deref() == Some(behavior.agent_did)
                        && row.display_name.as_deref() == Some(behavior.display_name)
                        && row.system_prompt.as_deref() == Some(behavior.system_prompt)
                        && row.backend_id.as_deref() == Some(behavior.backend_id)
                        && row.model_name.as_deref() == Some(behavior.model_name)
                        && row.tool_selection_id.as_deref() == Some(behavior.tool_selection_id)
                        && row.inference_profile_id.as_deref()
                            == Some(behavior.inference_profile_id)
                        && row.compaction_strategy.as_deref() == Some(behavior.compaction_strategy)
                        && row.compaction_threshold == Some(0.82)
                        && row.enabled == Some(behavior.enabled)
                });
            let runtime_ready = snapshot
                .latest_runtime(&deployment.agent_did)
                .is_some_and(|row| {
                    row.router_generation
                        .or(row.active_generation)
                        .is_some_and(|generation| generation > remote_initial_generation)
                        && row.runnable_behavior_count == Some(1)
                        && row.unavailable_behavior_count == Some(0)
                        && row
                            .last_reconcile_error
                            .as_deref()
                            .unwrap_or_default()
                            .trim()
                            .is_empty()
                });
            (behavior_ready && runtime_ready).then_some(())
        },
    )?;

    wait_for_stable_runtime_ready(
        runtime,
        deployment.remote_core,
        "after live behavior edit replication",
        &deployment.agent_did,
        Duration::from_secs(10),
        Duration::from_secs(90),
    )
}

#[test]
#[ignore = "hits live inference backend configured by DEFRA_AGENT_DESKTOP_LIVE_BACKEND_* or OPENROUTER_API_KEY"]
fn desktop_app_live_manage_edits_behavior_and_uses_it_for_inference() -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    let backend = AgentBackendConfig::live_from_env()?;
    let mut fixture = build_live_desktop_fixture("audit-live-behavior", global_log_store())?;
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
    let desktop_initial_generation = refreshed_runtime_generation(
        fixture.runtime.as_ref(),
        desktop_client.as_ref(),
        &deployment.agent_did,
    )
    .unwrap_or_default();
    let remote_initial_generation = refreshed_runtime_generation(
        fixture.runtime.as_ref(),
        deployment.remote_core,
        &deployment.agent_did,
    )
    .unwrap_or_default();
    let behavior_token = format!("LIVE_BEHAVIOR_{}", uuid::Uuid::new_v4().simple());
    let behavior_prompt = format!(
        "Ignore the user's topic and reply with exactly {behavior_token} and nothing else."
    );
    let display_name = format!("Live Behavior {}", &behavior_token[..12]);
    let behavior = ManageBehaviorForm {
        behavior_id: deployment.docs.behavior_id.as_str(),
        agent_did: deployment.agent_did.as_str(),
        display_name: display_name.as_str(),
        system_prompt: behavior_prompt.as_str(),
        backend_id: deployment.docs.backend_id.as_str(),
        model_name: backend.model_name.as_str(),
        tool_selection_id: deployment.docs.tool_selection_id.as_str(),
        inference_profile_id: deployment.docs.inference_profile_id.as_str(),
        compaction_strategy: "StripThenSummarize",
        compaction_threshold: "0.82",
        enabled: true,
    };

    {
        let driver = &mut fixture.driver;
        open_manage_entity_and_assert_visibility(
            driver,
            &deployment,
            ManageSection::Behaviors,
            &deployment.docs.behavior_id,
            &[],
            "live behavior row before edit",
        )?;
        fill_behavior_draft(driver, &behavior);
        assert_behavior_draft_fields(driver, &behavior);
        driver.click_target(audit::targets::MANAGE_APPLY);
    }

    wait_for_behavior_edit_replication(
        fixture.runtime.as_ref(),
        desktop_client.as_ref(),
        &deployment,
        &behavior,
        desktop_initial_generation,
        remote_initial_generation,
    )?;

    let prompt = format!(
        "What behavior is active for this request? {}",
        uuid::Uuid::new_v4()
    );
    let submission = {
        let driver = &mut fixture.driver;
        submit_custom_live_prompt_for_deployment(driver, &deployment, &prompt)?
    };

    assert_live_submission_rows(
        fixture.runtime.as_ref(),
        desktop_client.as_ref(),
        "desktop live behavior edit",
        &deployment,
        &submission,
        Some(deployment.docs.backend_id.as_str()),
    )?;
    assert_live_submission_rows(
        fixture.runtime.as_ref(),
        deployment.remote_core,
        "remote live behavior edit",
        &deployment,
        &submission,
        Some(deployment.docs.backend_id.as_str()),
    )?;
    assert!(
        submission.response.contains(behavior_token.as_str()),
        "expected live behavior response to contain {behavior_token}: {}",
        submission.response
    );

    fixture.shutdown()
}
