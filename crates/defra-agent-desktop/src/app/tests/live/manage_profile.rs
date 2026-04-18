use super::*;

fn wait_for_live_profile_binding(
    runtime: &tokio::runtime::Runtime,
    desktop_client: &ClientCore,
    deployment: &LiveDeploymentCase<'_>,
    profile: &ManageInferenceProfileForm<'_>,
    desktop_initial_generation: i64,
    remote_initial_generation: i64,
) -> Result<()> {
    wait_for_value(
        "created profile and behavior binding observed in desktop store",
        Duration::from_secs(120),
        || {
            runtime.block_on(desktop_client.refresh_store()).ok()?;
            let snapshot = desktop_client.store().snapshot();
            let profile_ready = snapshot
                .inference_profiles
                .iter()
                .find(|row| row.profile_id == profile.profile_id)
                .is_some_and(|row| {
                    row.display_name.as_deref() == Some(profile.display_name)
                        && row.context_window == Some(65536)
                        && row.max_output_tokens == Some(1536)
                        && row.max_turns == Some(16)
                        && row.temperature == Some(0.1)
                        && row.stream_batch_ms == Some(25)
                        && row.deadline_duration_secs == Some(240)
                });
            let behavior_ready = snapshot
                .behaviors
                .iter()
                .find(|row| row.behavior_id == deployment.docs.behavior_id)
                .is_some_and(|row| {
                    row.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                        && row.inference_profile_id.as_deref() == Some(profile.profile_id)
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
            (profile_ready && behavior_ready && runtime_ready).then_some(())
        },
    )?;

    wait_for_value(
        "created profile and behavior binding replicated to remote runtime",
        Duration::from_secs(120),
        || {
            runtime
                .block_on(deployment.remote_core.refresh_store())
                .ok()?;
            let snapshot = deployment.remote_core.store().snapshot();
            let profile_ready = snapshot
                .inference_profiles
                .iter()
                .find(|row| row.profile_id == profile.profile_id)
                .is_some_and(|row| {
                    row.display_name.as_deref() == Some(profile.display_name)
                        && row.context_window == Some(65536)
                        && row.max_output_tokens == Some(1536)
                        && row.max_turns == Some(16)
                        && row.temperature == Some(0.1)
                        && row.stream_batch_ms == Some(25)
                        && row.deadline_duration_secs == Some(240)
                });
            let behavior_ready = snapshot
                .behaviors
                .iter()
                .find(|row| row.behavior_id == deployment.docs.behavior_id)
                .is_some_and(|row| {
                    row.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                        && row.inference_profile_id.as_deref() == Some(profile.profile_id)
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
            (profile_ready && behavior_ready && runtime_ready).then_some(())
        },
    )?;

    wait_for_stable_runtime_ready(
        runtime,
        deployment.remote_core,
        "after live profile replication",
        &deployment.agent_did,
        Duration::from_secs(10),
        Duration::from_secs(90),
    )
}

#[test]
#[ignore = "hits live inference backend configured by DEFRA_AGENT_DESKTOP_LIVE_BACKEND_* or OPENROUTER_API_KEY"]
fn desktop_app_live_manage_creates_profile_and_rebinds_behavior() -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    let mut fixture = build_live_desktop_fixture("audit-live-profile", global_log_store())?;
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
    let profile_id = format!(
        "{}:profile:{}",
        deployment.docs.behavior_id,
        uuid::Uuid::new_v4().simple()
    );
    let profile = ManageInferenceProfileForm {
        profile_id: profile_id.as_str(),
        display_name: "Live Profile CRUD",
        context_window: "65536",
        max_output_tokens: "1536",
        max_turns: "16",
        temperature: "0.1",
        stream_batch_ms: "25",
        deadline_duration_secs: "240",
    };

    {
        let driver = &mut fixture.driver;
        if driver.app.state.activity != Activity::Manage {
            driver.open_activity(Activity::Manage);
        }
        if driver.app.state.manage.selected_peer_id.as_deref()
            != Some(deployment.peer_id.as_str())
        {
            driver.click_target(&audit::targets::manage_deployment(&deployment.peer_id));
        }
        if driver.app.state.manage.selected_agent_did.as_deref()
            != Some(deployment.agent_did.as_str())
        {
            driver.click_target(&audit::targets::manage_agent(&deployment.agent_did));
        }
        if driver.app.state.manage.selected_section != ManageSection::InferenceProfiles {
            driver.click_target(&audit::targets::manage_section(
                ManageSection::InferenceProfiles,
            ));
        }
        driver.wait_for_target(
            "live manage new profile button",
            Duration::from_secs(20),
            audit::targets::MANAGE_NEW,
        )?;
        driver.click_compact_target(audit::targets::MANAGE_NEW)?;
        wait_for_value(
            "manage new profile draft active",
            Duration::from_secs(5),
            || {
                matches!(
                    driver.app.state.manage.draft_origin,
                    Some(crate::state::ManageDraftOrigin::NewDocument)
                )
                .then_some(())
            },
        )?;
        fill_inference_profile_draft(driver, &profile);
        assert_inference_profile_draft_fields(driver, &profile);
        driver.click_target(audit::targets::MANAGE_APPLY);

        wait_for_value(
            "created live profile selected after apply",
            Duration::from_secs(20),
            || {
                let client = Arc::clone(driver.app.client.as_ref()?);
                driver.app.block_on_runtime(client.refresh_store()).ok()?;
                let texts = driver.render();
                (driver.app.state.manage.selected_section == ManageSection::InferenceProfiles
                    && driver.app.state.manage.selected_entity_id.as_deref()
                        == Some(profile.profile_id)
                    && matches!(
                        driver.app.state.manage.draft.as_ref(),
                        Some(ManageDraft::InferenceProfile(_))
                    ))
                .then_some(texts)
            },
        )?;
        assert_inference_profile_draft_fields(driver, &profile);

        open_manage_entity_and_assert_visibility(
            driver,
            &deployment,
            ManageSection::Behaviors,
            &deployment.docs.behavior_id,
            &[],
            "live behavior row before profile rebinding",
        )?;
        driver.replace_text_in_target(
            &audit::targets::manage_field("Inference Profile ID"),
            profile.profile_id,
        );
        match driver.app.state.manage.draft.as_ref() {
            Some(ManageDraft::Behavior(draft)) => {
                assert_eq!(draft.behavior_id, deployment.docs.behavior_id);
                assert_eq!(draft.inference_profile_id, profile.profile_id);
            }
            other => panic!("expected behavior draft while rebinding profile, got {other:?}"),
        }
        driver.click_target(audit::targets::MANAGE_APPLY);
    }

    wait_for_live_profile_binding(
        fixture.runtime.as_ref(),
        desktop_client.as_ref(),
        &deployment,
        &profile,
        desktop_initial_generation,
        remote_initial_generation,
    )?;

    let rebound_deployment = LiveDeploymentCase {
        docs: LiveAgentDocs {
            inference_profile_id: profile.profile_id.to_string(),
            ..deployment.docs.clone()
        },
        ..deployment.clone()
    };
    let token = format!("LIVE_PROFILE_{}", uuid::Uuid::new_v4().simple());
    let prompt = format!(
        "Reply with exactly {token} and nothing else. Live profile audit {}",
        uuid::Uuid::new_v4()
    );
    let submission = {
        let driver = &mut fixture.driver;
        submit_custom_live_prompt_for_deployment(driver, &rebound_deployment, &prompt)?
    };

    assert_live_submission_rows(
        fixture.runtime.as_ref(),
        desktop_client.as_ref(),
        "desktop live profile CRUD",
        &rebound_deployment,
        &submission,
        Some(deployment.docs.backend_id.as_str()),
    )?;
    assert_live_submission_rows(
        fixture.runtime.as_ref(),
        deployment.remote_core,
        "remote live profile CRUD",
        &rebound_deployment,
        &submission,
        Some(deployment.docs.backend_id.as_str()),
    )?;
    assert!(
        submission.response.contains(token.as_str()),
        "expected live profile response to contain {token}: {}",
        submission.response
    );

    fixture.shutdown()
}
