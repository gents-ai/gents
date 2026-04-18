use std::time::Instant;

use super::*;

fn wait_for_live_backend_binding(
    runtime: &tokio::runtime::Runtime,
    desktop_client: &ClientCore,
    deployment: &LiveDeploymentCase<'_>,
    backend: &ManageBackendForm<'_>,
    model_name: &str,
    desktop_initial_generation: i64,
    remote_initial_generation: i64,
) -> Result<()> {
    let expected_api_key = (!backend.api_key.trim().is_empty()).then_some(backend.api_key);
    let expected_api_key_env_var =
        (!backend.api_key_env_var.trim().is_empty()).then_some(backend.api_key_env_var);

    wait_for_value(
        "created backend and behavior binding observed in desktop store",
        Duration::from_secs(120),
        || {
            runtime.block_on(desktop_client.refresh_store()).ok()?;
            let snapshot = desktop_client.store().snapshot();
            let backend_ready = snapshot
                .inference_backends
                .iter()
                .find(|row| row.backend_id == backend.backend_id)
                .is_some_and(|row| {
                    row.name.as_deref() == Some(backend.name)
                        && row.provider_kind.as_deref() == Some(backend.provider_kind)
                        && row.endpoint.as_deref() == Some(backend.endpoint)
                        && row.api_key.as_deref() == expected_api_key
                        && row.api_key_env_var.as_deref() == expected_api_key_env_var
                        && row.max_concurrent == Some(2)
                        && row.max_queue_depth == Some(16)
                        && row.enabled == Some(true)
                        && row.models.iter().any(|candidate| candidate == model_name)
                        && row.probe_status.as_deref() == Some(backend.probe_status)
                });
            let behavior_ready = snapshot
                .behaviors
                .iter()
                .find(|row| row.behavior_id == deployment.docs.behavior_id)
                .is_some_and(|row| {
                    row.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                        && row.backend_id.as_deref() == Some(backend.backend_id)
                        && row.model_name.as_deref() == Some(model_name)
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
            (backend_ready && behavior_ready && runtime_ready).then_some(())
        },
    )?;

    wait_for_value(
        "created backend and behavior binding replicated to remote runtime",
        Duration::from_secs(120),
        || {
            runtime
                .block_on(deployment.remote_core.refresh_store())
                .ok()?;
            let snapshot = deployment.remote_core.store().snapshot();
            let backend_ready = snapshot
                .inference_backends
                .iter()
                .find(|row| row.backend_id == backend.backend_id)
                .is_some_and(|row| {
                    row.name.as_deref() == Some(backend.name)
                        && row.provider_kind.as_deref() == Some(backend.provider_kind)
                        && row.endpoint.as_deref() == Some(backend.endpoint)
                        && row.enabled == Some(true)
                        && row.models.iter().any(|candidate| candidate == model_name)
                        && row.probe_status.as_deref() == Some(backend.probe_status)
                });
            let behavior_ready = snapshot
                .behaviors
                .iter()
                .find(|row| row.behavior_id == deployment.docs.behavior_id)
                .is_some_and(|row| {
                    row.agent_did.as_deref() == Some(deployment.agent_did.as_str())
                        && row.backend_id.as_deref() == Some(backend.backend_id)
                        && row.model_name.as_deref() == Some(model_name)
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
            (backend_ready && behavior_ready && runtime_ready).then_some(())
        },
    )?;

    wait_for_stable_runtime_ready(
        runtime,
        deployment.remote_core,
        "after live backend CRUD binding",
        &deployment.agent_did,
        Duration::from_secs(10),
        Duration::from_secs(90),
    )
}

fn wait_for_live_backend_section_ready(
    driver: &mut AuditDriver,
    deployment: &LiveDeploymentCase<'_>,
) -> Result<()> {
    if driver.app.state.activity != Activity::Manage {
        driver.open_activity(Activity::Manage);
    }
    if driver.app.state.manage.selected_peer_id.as_deref() != Some(deployment.peer_id.as_str()) {
        driver.click_target(&audit::targets::manage_deployment(&deployment.peer_id));
    }
    if driver.app.state.manage.selected_agent_did.as_deref()
        != Some(deployment.agent_did.as_str())
    {
        driver.click_target(&audit::targets::manage_agent(&deployment.agent_did));
    }
    if driver.app.state.manage.selected_section != ManageSection::Backends {
        driver.click_target(&audit::targets::manage_section(ManageSection::Backends));
    }

    wait_for_value(
        "live manage backend section ready for new document",
        Duration::from_secs(20),
        || {
            let client = Arc::clone(driver.app.client.as_ref()?);
            driver.app.block_on_runtime(client.refresh_store()).ok()?;
            driver.render();
            (driver.app.state.manage.selected_peer_id.as_deref()
                == Some(deployment.peer_id.as_str())
                && driver.app.state.manage.selected_agent_did.as_deref()
                    == Some(deployment.agent_did.as_str())
                && driver.app.state.manage.selected_section == ManageSection::Backends)
                .then_some(())
        },
    )?;

    driver.wait_for_target(
        "live manage new backend button",
        Duration::from_secs(20),
        audit::targets::MANAGE_NEW,
    )?;

    Ok(())
}

#[test]
#[ignore = "hits live inference backend configured by DEFRA_AGENT_DESKTOP_LIVE_BACKEND_* or OPENROUTER_API_KEY"]
fn desktop_app_live_manage_creates_backend_and_uses_it_for_inference() -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    let backend_config = AgentBackendConfig::live_from_env()?;
    let mut fixture = build_live_desktop_fixture("audit-live-backend-crud", global_log_store())?;
    let peer_id = fixture
        .driver
        .app
        .state
        .chat
        .shell
        .selected_peer_id
        .clone()
        .ok_or_else(|| anyhow!("missing selected peer id for live deployment"))?;
    let remote_core = fixture
        .remote_core
        .as_ref()
        .ok_or_else(|| anyhow!("missing remote core in live fixture"))?;
    let running_agent = fixture
        .running_agent
        .as_ref()
        .ok_or_else(|| anyhow!("missing running agent in live fixture"))?;
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

    let backend_id = format!(
        "{}:crud-backend:{}",
        deployment.docs.behavior_id,
        uuid::Uuid::new_v4().simple()
    );
    let backend_name = "Live CRUD Backend".to_string();
    let api_key = backend_config.api_key.clone().unwrap_or_default();
    let api_key_env_var = backend_config.api_key_env_var.clone().unwrap_or_default();
    let backend = ManageBackendForm {
        backend_id: backend_id.as_str(),
        name: backend_name.as_str(),
        provider_kind: backend_config.provider_kind.as_str(),
        endpoint: backend_config.endpoint.as_str(),
        api_key: api_key.as_str(),
        api_key_env_var: api_key_env_var.as_str(),
        max_concurrent: "2",
        max_queue_depth: "16",
        enabled: true,
        models: backend_config.model_name.as_str(),
        probe_status: "healthy",
    };

    {
        let driver = &mut fixture.driver;
        wait_for_live_backend_section_ready(driver, &deployment)?;
        driver.click_compact_target(audit::targets::MANAGE_NEW)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            driver.render();
            if matches!(
                driver.app.state.manage.draft_origin,
                Some(crate::state::ManageDraftOrigin::NewDocument)
            ) {
                break;
            }
            if Instant::now() >= deadline {
                anyhow::bail!(
                    "timed out waiting for live manage new backend draft active; section={:?} selected_entity_id={:?} draft_origin={:?} draft={:?} pending_shell_actions={:?}",
                    driver.app.state.manage.selected_section,
                    driver.app.state.manage.selected_entity_id,
                    driver.app.state.manage.draft_origin,
                    driver.app.state.manage.draft,
                    driver.app.state.pending_shell_actions,
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        fill_backend_draft(driver, &backend);
        assert_backend_draft_fields(driver, &backend);
        driver.click_target(&audit::targets::MANAGE_APPLY);
    }

    {
        let driver = &mut fixture.driver;
        wait_for_value(
            "created live backend selected after apply",
            Duration::from_secs(20),
            || {
                let client = Arc::clone(driver.app.client.as_ref()?);
                driver.app.block_on_runtime(client.refresh_store()).ok()?;
                let texts = driver.render();
                (driver.app.state.activity == Activity::Manage
                    && driver.app.state.manage.selected_section == ManageSection::Backends
                    && driver.app.state.manage.selected_entity_id.as_deref()
                        == Some(backend.backend_id)
                    && matches!(
                        driver.app.state.manage.draft.as_ref(),
                        Some(ManageDraft::Backend(_))
                    ))
                .then_some(texts)
            },
        )?;
        assert_backend_draft_fields(driver, &backend);
        if driver.app.state.manage.selected_section != ManageSection::Behaviors {
            driver.click_target(&audit::targets::manage_section(
                ManageSection::Behaviors,
            ));
        }
        driver.wait_for_target(
            "live behavior entity target for backend rebinding",
            Duration::from_secs(20),
            &audit::targets::manage_entity(deployment.docs.behavior_id.as_str()),
        )?;
        driver.click_target(&audit::targets::manage_entity(
            deployment.docs.behavior_id.as_str(),
        ));
        wait_for_value(
            "live behavior selected for backend rebinding",
            Duration::from_secs(20),
            || {
                let client = Arc::clone(driver.app.client.as_ref()?);
                driver.app.block_on_runtime(client.refresh_store()).ok()?;
                let texts = driver.render();
                (driver.app.state.activity == Activity::Manage
                    && driver.app.state.manage.selected_section == ManageSection::Behaviors
                    && driver.app.state.manage.selected_entity_id.as_deref()
                        == Some(deployment.docs.behavior_id.as_str())
                    && matches!(
                        driver.app.state.manage.draft.as_ref(),
                        Some(ManageDraft::Behavior(_))
                    ))
                .then_some(texts)
            },
        )?;
        driver.replace_text_in_target(
            &audit::targets::manage_field("Backend ID"),
            backend.backend_id,
        );
        driver.replace_text_in_target(
            &audit::targets::manage_field("Model Name"),
            backend_config.model_name.as_str(),
        );
        driver.click_target(&audit::targets::MANAGE_APPLY);
    }

    wait_for_live_backend_binding(
        fixture.runtime.as_ref(),
        desktop_client.as_ref(),
        &deployment,
        &backend,
        backend_config.model_name.as_str(),
        desktop_initial_generation,
        remote_initial_generation,
    )?;

    let token = format!("LIVE_BACKEND_CRUD_{}", uuid::Uuid::new_v4().simple());
    let prompt = format!(
        "Reply with exactly {token} and nothing else. Live backend CRUD inference audit {}",
        uuid::Uuid::new_v4()
    );
    let submission = {
        let driver = &mut fixture.driver;
        submit_custom_live_prompt_for_deployment(driver, &deployment, &prompt)?
    };

    assert_live_submission_rows(
        fixture.runtime.as_ref(),
        desktop_client.as_ref(),
        "desktop live backend CRUD",
        &deployment,
        &submission,
        Some(backend.backend_id),
    )?;
    assert_live_submission_rows(
        fixture.runtime.as_ref(),
        deployment.remote_core,
        "remote live backend CRUD",
        &deployment,
        &submission,
        Some(backend.backend_id),
    )?;
    assert!(
        submission.response.contains(token.as_str()),
        "expected live backend CRUD response to contain {token}: {}",
        submission.response
    );

    fixture.shutdown()
}
