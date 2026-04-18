use super::*;

#[test]
#[ignore = "hits live inference backend configured by DEFRA_AGENT_DESKTOP_LIVE_BACKEND_* or OPENROUTER_API_KEY"]
fn desktop_app_live_multi_agent_server_switching_and_config_inference() -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    let mut fixture =
        build_multi_agent_live_desktop_fixture("audit-live-multi-server", global_log_store())?;
    assert_eq!(fixture.deployments.len(), 2);

    let alpha_tool_token = fixture.deployments[0].running_agent.tool_token.clone();
    let alpha = live_deployment_case(&fixture.deployments[0]);
    let bravo = live_deployment_case(&fixture.deployments[1]);
    let backend = fixture.backend.clone();
    let desktop_client = Arc::clone(
        fixture
            .driver
            .app
            .client
            .as_ref()
            .ok_or_else(|| anyhow!("desktop client missing"))?,
    );
    let switch_config = prepare_live_switch_config(
        fixture.runtime.as_ref(),
        desktop_client.as_ref(),
        &alpha,
        &backend,
    )?;

    let alpha_initial_generation = refreshed_runtime_generation(
        fixture.runtime.as_ref(),
        desktop_client.as_ref(),
        &alpha.agent_did,
    )
    .unwrap_or_default();
    let alpha_remote_initial_generation = refreshed_runtime_generation(
        fixture.runtime.as_ref(),
        alpha.remote_core,
        &alpha.agent_did,
    )
    .unwrap_or_default();

    let (alpha_submission, bravo_submission);
    {
        let driver = &mut fixture.driver;
        alpha_submission = submit_live_prompt_for_deployment(driver, &alpha, "ALPHA_SERVER_READY")?;
        bravo_submission = submit_live_prompt_for_deployment(driver, &bravo, "BRAVO_SERVER_READY")?;
    }
    assert_live_submission_rows(
        fixture.runtime.as_ref(),
        desktop_client.as_ref(),
        "desktop alpha initial",
        &alpha,
        &alpha_submission,
        None,
    )?;
    assert_live_submission_rows(
        fixture.runtime.as_ref(),
        alpha.remote_core,
        "remote alpha initial",
        &alpha,
        &alpha_submission,
        None,
    )?;
    assert_live_submission_rows(
        fixture.runtime.as_ref(),
        desktop_client.as_ref(),
        "desktop bravo initial",
        &bravo,
        &bravo_submission,
        None,
    )?;
    assert_live_submission_rows(
        fixture.runtime.as_ref(),
        bravo.remote_core,
        "remote bravo initial",
        &bravo,
        &bravo_submission,
        None,
    )?;

    {
        let driver = &mut fixture.driver;
        assert_live_manage_switching_baseline(
            driver,
            &alpha,
            &alpha_submission,
            &bravo,
            &bravo_submission,
            &backend,
        )?;
        apply_live_switch_config_in_manage(driver, &alpha, &backend, &switch_config)?;
    }

    wait_for_live_switch_config_replication(
        fixture.runtime.as_ref(),
        desktop_client.as_ref(),
        &alpha,
        &backend,
        &switch_config,
        alpha_initial_generation,
        alpha_remote_initial_generation,
    )?;
    assert_live_deployment_default_config(
        fixture.runtime.as_ref(),
        desktop_client.as_ref(),
        "desktop bravo",
        &bravo,
        backend.model_name.as_str(),
    )?;
    assert_live_deployment_default_config(
        fixture.runtime.as_ref(),
        bravo.remote_core,
        "remote bravo",
        &bravo,
        backend.model_name.as_str(),
    )?;

    let post_config_submission;
    {
        let driver = &mut fixture.driver;
        let post_config_prompt = format!(
            "This is the alpha post-config tool audit {}. Call read_file for notes.txt. Reply with only the token from notes.txt.",
            uuid::Uuid::new_v4()
        );
        post_config_submission =
            submit_custom_live_prompt_for_deployment(driver, &alpha, &post_config_prompt)?;
        wait_for_value(
            "post-config request used switched backend",
            Duration::from_secs(30),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .requests
                        .iter()
                        .find(|row| row.request_id == post_config_submission.request_id)
                        .filter(|row| {
                            row.agent_did.as_deref() == Some(alpha.agent_did.as_str())
                                && row.behavior_id.as_deref()
                                    == Some(alpha.docs.behavior_id.as_str())
                                && row.backend_id.as_deref()
                                    == Some(switch_config.backend_id.as_str())
                        })
                        .map(|row| row.request_id.clone())
                })
            },
        )?;
    }
    assert_live_submission_rows(
        fixture.runtime.as_ref(),
        desktop_client.as_ref(),
        "desktop alpha post-config",
        &alpha,
        &post_config_submission,
        Some(switch_config.backend_id.as_str()),
    )?;
    assert_live_submission_rows(
        fixture.runtime.as_ref(),
        alpha.remote_core,
        "remote alpha post-config",
        &alpha,
        &post_config_submission,
        Some(switch_config.backend_id.as_str()),
    )?;
    assert!(
        post_config_submission
            .response
            .contains(alpha_tool_token.as_str()),
        "expected alpha post-config response to contain {}: {}",
        alpha_tool_token,
        post_config_submission.response
    );
    let alpha_post_config_tool_card_id = wait_for_session_tool_activity(
        fixture.runtime.as_ref(),
        desktop_client.as_ref(),
        "desktop alpha post-config tool activity",
        &post_config_submission.session_id,
        0,
        1,
        &[alpha_tool_token.clone()],
    )?;

    {
        let driver = &mut fixture.driver;
        open_chat_conversation_and_assert_isolation(
            driver,
            &bravo,
            &bravo_submission,
            post_config_submission.prompt.as_str(),
            "bravo transcript leaked alpha post-config prompt after switching deployments",
        )?;
        open_chat_conversation_and_assert_isolation(
            driver,
            &alpha,
            &post_config_submission,
            bravo_submission.prompt.as_str(),
            "alpha post-config transcript leaked bravo prompt after switching deployments",
        )?;
        driver.wait_for_target(
            "alpha post-config tool card visible",
            Duration::from_secs(10),
            &audit::targets::chat_tool_card(&alpha_post_config_tool_card_id),
        )?;
    }

    fixture.shutdown()
}
