use super::*;

fn build_operator_driver(runtime: Arc<Runtime>, core: ClientCore) -> AuditDriver {
    let mut driver = build_driver(runtime, core, Arc::new(DesktopLogStore::new(64)));
    driver.app.state.onboarding.first_launch_redirect_done = true;
    driver.app.state.activity = Activity::Operator;
    driver
}

#[test]
fn desktop_app_operator_creates_backend_and_rebinds_behavior() -> Result<()> {
    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("desktop")),
        ClientCoreOptions::local_only(),
    ))?;
    runtime.block_on(seed_operator_documents(&core))?;

    let mut driver = build_operator_driver(Arc::clone(&runtime), core);
    driver.wait_for_target(
        "operator local agent target",
        Duration::from_secs(10),
        &audit::targets::operator_agent("did:defra:amy"),
    )?;
    driver.click_target(&audit::targets::operator_agent("did:defra:amy"));
    wait_for_value("operator selected local agent", Duration::from_secs(10), || {
        let texts = driver.render();
        (driver.app.state.operator.selected_agent_did.as_deref() == Some("did:defra:amy")
            && texts.iter().any(|text| text.contains("Operator Console")))
        .then_some(())
    })?;

    driver.click_target(&audit::targets::operator_section(OperatorSection::Backends));
    driver.wait_for_target(
        "operator new backend button",
        Duration::from_secs(10),
        audit::targets::OPERATOR_NEW,
    )?;

    let backend = OperatorBackendForm {
        backend_id: "backend-amy-created",
        name: "Amy Created Backend",
        provider_kind: "openai-compatible",
        endpoint: "http://workstation-1:8000/v1",
        api_key: "local-backend-key",
        api_key_env_var: "LOCAL_BACKEND_KEY",
        max_concurrent: "4",
        max_queue_depth: "32",
        enabled: false,
        models: "MiniMax-M2.7-NVFP4\nopenai/gpt-5.4",
        probe_status: "healthy",
    };

    driver.click_interactable_target(audit::targets::OPERATOR_NEW)?;
    wait_for_value(
        "operator new backend draft active",
        Duration::from_secs(5),
        || {
            matches!(
                driver.app.state.operator.draft_origin,
                Some(crate::state::OperatorDraftOrigin::NewDocument)
            )
            .then_some(driver.app.state.operator.selected_entity_id.clone())
        },
    )?;
    assert_eq!(driver.app.state.operator.selected_entity_id, None);
    assert_eq!(
        driver.app.state.operator.draft_origin,
        Some(crate::state::OperatorDraftOrigin::NewDocument)
    );

    fill_backend_draft(&mut driver, &backend);
    assert_backend_draft_fields(&driver, &backend);
    driver.click_target(&audit::targets::OPERATOR_APPLY);

    let client = Arc::clone(
        driver
            .app
            .client
            .as_ref()
            .ok_or_else(|| anyhow!("desktop client missing"))?,
    );
    wait_for_value(
        "created backend persisted in local store",
        Duration::from_secs(20),
        || {
            driver.app.block_on_runtime(client.refresh_store()).ok()?;
            client
                .store()
                .snapshot()
                .inference_backends
                .iter()
                .find(|row| row.backend_id == backend.backend_id)
                .filter(|row| {
                    row.name.as_deref() == Some(backend.name)
                        && row.provider_kind.as_deref() == Some(backend.provider_kind)
                        && row.endpoint.as_deref() == Some(backend.endpoint)
                        && row.api_key.as_deref() == Some(backend.api_key)
                        && row.api_key_env_var.as_deref() == Some(backend.api_key_env_var)
                        && row.max_concurrent == Some(4)
                        && row.max_queue_depth == Some(32)
                        && row.enabled == Some(false)
                        && row.models
                            == vec![
                                "MiniMax-M2.7-NVFP4".to_string(),
                                "openai/gpt-5.4".to_string(),
                            ]
                        && row.probe_status.as_deref() == Some(backend.probe_status)
                })
                .map(|row| row.backend_id.clone())
        },
    )?;

    wait_for_value(
        "created backend selected after apply",
        Duration::from_secs(10),
        || {
            let texts = driver.render();
            (driver.app.state.operator.selected_section == OperatorSection::Backends
                && driver.app.state.operator.selected_entity_id.as_deref()
                    == Some(backend.backend_id)
                && matches!(
                    driver.app.state.operator.draft.as_ref(),
                    Some(OperatorDraft::Backend(_))
                ))
            .then_some(texts)
        },
    )?;
    assert_backend_draft_fields(&driver, &backend);

    driver.click_target(&audit::targets::operator_section(
        OperatorSection::Behaviors,
    ));
    driver.wait_for_target(
        "amy behavior entity target",
        Duration::from_secs(10),
        &audit::targets::operator_entity("amy-default"),
    )?;
    driver.click_target(&audit::targets::operator_entity("amy-default"));
    wait_for_value("amy behavior selected after explicit click", Duration::from_secs(10), || {
        let texts = driver.render();
        (driver.app.state.operator.selected_section == OperatorSection::Behaviors
            && driver.app.state.operator.selected_entity_id.as_deref() == Some("amy-default")
            && matches!(
                driver.app.state.operator.draft.as_ref(),
                Some(OperatorDraft::Behavior(_))
            ))
        .then_some(texts)
    })?;
    assert_behavior_draft_bindings(
        &driver,
        "amy-default",
        "did:defra:amy",
        "backend-amy",
        "tools-amy",
        "profile-amy",
    );

    driver.replace_text_in_target(
        &audit::targets::operator_field("Backend ID"),
        backend.backend_id,
    );
    driver.replace_text_in_target(
        &audit::targets::operator_field("Model Name"),
        "MiniMax-M2.7-NVFP4",
    );
    driver.click_target(&audit::targets::OPERATOR_APPLY);

    wait_for_value(
        "behavior rebound to created backend",
        Duration::from_secs(20),
        || {
            driver.app.block_on_runtime(client.refresh_store()).ok()?;
            client
                .store()
                .snapshot()
                .behaviors
                .iter()
                .find(|row| row.behavior_id == "amy-default")
                .filter(|row| {
                    row.agent_did.as_deref() == Some("did:defra:amy")
                        && row.backend_id.as_deref() == Some(backend.backend_id)
                        && row.model_name.as_deref() == Some("MiniMax-M2.7-NVFP4")
                        && row.tool_selection_id.as_deref() == Some("tools-amy")
                        && row.inference_profile_id.as_deref() == Some("profile-amy")
                })
                .map(|row| row.behavior_id.clone())
        },
    )?;

    wait_for_value(
        "amy behavior still selected after apply",
        Duration::from_secs(10),
        || {
            let texts = driver.render();
            (driver.app.state.operator.selected_section == OperatorSection::Behaviors
                && driver.app.state.operator.selected_entity_id.as_deref() == Some("amy-default")
                && matches!(
                    driver.app.state.operator.draft.as_ref(),
                    Some(OperatorDraft::Behavior(_))
                ))
            .then_some(texts)
        },
    )?;
    assert_behavior_draft_bindings(
        &driver,
        "amy-default",
        "did:defra:amy",
        backend.backend_id,
        "tools-amy",
        "profile-amy",
    );

    driver.app.shutdown_client();
    Ok(())
}
