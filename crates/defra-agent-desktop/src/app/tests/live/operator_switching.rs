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
    let alpha_switch_backend_id = format!("{}:switch-backend", alpha.docs.behavior_id);
    let alpha_switch_profile_id = format!("{}:switch-profile", alpha.docs.behavior_id);
    let alpha_tool_prompt = "When the user asks about local files, you must call read_file instead of guessing. The token is not available in the conversation. For multi-file requests, call read_file separately for every requested path and respond with only the requested tokens.";
    let desktop_client = Arc::clone(
        fixture
            .driver
            .app
            .client
            .as_ref()
            .ok_or_else(|| anyhow!("desktop client missing"))?,
    );

    {
        fixture.runtime.block_on(async {
            desktop_client
                .save_backend(&InferenceBackendRow {
                    backend_id: alpha_switch_backend_id.clone(),
                    name: Some("Alpha Switch Backend".to_string()),
                    provider_kind: Some(backend.provider_kind.as_str().to_string()),
                    endpoint: Some(backend.endpoint.clone()),
                    api_key: backend.api_key.clone(),
                    api_key_env_var: backend.api_key_env_var.clone(),
                    max_concurrent: Some(2),
                    max_queue_depth: Some(100),
                    enabled: Some(true),
                    models: vec![backend.model_name.clone()],
                    last_probe: None,
                    probe_status: Some("healthy".to_string()),
                })
                .await?;
            desktop_client
                .save_inference_profile(&InferenceProfileRow {
                    profile_id: alpha_switch_profile_id.clone(),
                    display_name: Some("Alpha Switch Profile".to_string()),
                    context_window: Some(65536),
                    max_output_tokens: Some(2048),
                    max_turns: Some(16),
                    temperature: Some(0.0),
                    stream_batch_ms: Some(40),
                    deadline_duration_secs: Some(240),
                })
                .await?;
            Ok::<(), anyhow::Error>(())
        })?;
    }

    wait_for_value(
        "alpha switch backend saved in live desktop store",
        Duration::from_secs(20),
        || {
            fixture
                .runtime
                .block_on(desktop_client.refresh_store())
                .ok()?;
            let snapshot = desktop_client.store().snapshot();
            let has_backend = snapshot
                .inference_backends
                .iter()
                .any(|row| row.backend_id == alpha_switch_backend_id);
            let has_profile = snapshot
                .inference_profiles
                .iter()
                .any(|row| row.profile_id == alpha_switch_profile_id);
            (has_backend && has_profile).then_some(())
        },
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
        driver.open_activity(Activity::Chat);
        driver.click_target(&audit::targets::chat_deployment(&alpha.peer_id));
        driver.click_target(&audit::targets::chat_agent(&alpha.agent_did));
        assert_chat_context(driver, &alpha, None);
        let alpha_texts = driver.click_target(&audit::targets::chat_conversation(
            &alpha_submission.session_id,
        ));
        assert_chat_context(driver, &alpha, Some(alpha_submission.session_id.as_str()));
        assert!(alpha_texts
            .iter()
            .any(|text| text.contains(alpha_submission.prompt.as_str())));
        assert!(alpha_texts
            .iter()
            .any(|text| text.contains(alpha_submission.response.trim())));
        assert!(
            !alpha_texts
                .iter()
                .any(|text| text.contains(bravo_submission.prompt.as_str())),
            "alpha transcript leaked bravo prompt after switching deployments"
        );

        driver.click_target(&audit::targets::chat_deployment(&bravo.peer_id));
        driver.click_target(&audit::targets::chat_agent(&bravo.agent_did));
        assert_chat_context(driver, &bravo, None);
        let bravo_texts = driver.click_target(&audit::targets::chat_conversation(
            &bravo_submission.session_id,
        ));
        assert_chat_context(driver, &bravo, Some(bravo_submission.session_id.as_str()));
        assert!(bravo_texts
            .iter()
            .any(|text| text.contains(bravo_submission.prompt.as_str())));
        assert!(bravo_texts
            .iter()
            .any(|text| text.contains(bravo_submission.response.trim())));
        assert!(
            !bravo_texts
                .iter()
                .any(|text| text.contains(alpha_submission.prompt.as_str())),
            "bravo transcript leaked alpha prompt after switching deployments"
        );

        driver.open_activity(Activity::Operator);
        driver.click_target(&audit::targets::operator_deployment(&alpha.peer_id));
        driver.click_target(&audit::targets::operator_agent(&alpha.agent_did));
        driver.click_target(&audit::targets::operator_section(
            OperatorSection::Behaviors,
        ));
        assert_operator_context(driver, &alpha, OperatorSection::Behaviors, None);
        driver.wait_for_target(
            "alpha behavior row after operator server switch",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&alpha.docs.behavior_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(&bravo.docs.behavior_id)));
        driver.click_target(&audit::targets::operator_entity(&alpha.docs.behavior_id));
        assert_operator_context(
            driver,
            &alpha,
            OperatorSection::Behaviors,
            Some(alpha.docs.behavior_id.as_str()),
        );
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::Behavior(draft)) => {
                assert_eq!(draft.behavior_id, alpha.docs.behavior_id);
                assert_eq!(draft.agent_did, alpha.agent_did);
                assert_eq!(draft.backend_id, alpha.docs.backend_id);
                assert_eq!(draft.tool_selection_id, alpha.docs.tool_selection_id);
                assert_eq!(draft.inference_profile_id, alpha.docs.inference_profile_id);
            }
            other => panic!("expected alpha behavior draft, got {other:?}"),
        }

        driver.click_target(&audit::targets::operator_section(OperatorSection::Backends));
        assert_operator_context(driver, &alpha, OperatorSection::Backends, None);
        driver.wait_for_target(
            "alpha backend row after operator server switch",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&alpha.docs.backend_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(&bravo.docs.backend_id)));
        driver.click_target(&audit::targets::operator_entity(&alpha.docs.backend_id));
        assert_operator_context(
            driver,
            &alpha,
            OperatorSection::Backends,
            Some(alpha.docs.backend_id.as_str()),
        );
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::Backend(draft)) => {
                assert_eq!(draft.backend_id, alpha.docs.backend_id);
                assert_eq!(draft.provider_kind, backend.provider_kind.as_str());
                assert_eq!(draft.endpoint, backend.endpoint);
                assert!(draft.models.contains(backend.model_name.as_str()));
            }
            other => panic!("expected alpha backend draft, got {other:?}"),
        }

        driver.click_target(&audit::targets::operator_section(
            OperatorSection::InferenceProfiles,
        ));
        assert_operator_context(driver, &alpha, OperatorSection::InferenceProfiles, None);
        driver.wait_for_target(
            "alpha inference profile row after operator server switch",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&alpha.docs.inference_profile_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(
            &bravo.docs.inference_profile_id
        )));
        driver.click_target(&audit::targets::operator_entity(
            &alpha.docs.inference_profile_id,
        ));
        assert_operator_context(
            driver,
            &alpha,
            OperatorSection::InferenceProfiles,
            Some(alpha.docs.inference_profile_id.as_str()),
        );
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::InferenceProfile(draft)) => {
                assert_eq!(draft.profile_id, alpha.docs.inference_profile_id);
                assert_eq!(draft.max_output_tokens, "1024");
                assert_eq!(draft.max_turns, "12");
            }
            other => panic!("expected alpha inference profile draft, got {other:?}"),
        }

        driver.click_target(&audit::targets::operator_section(
            OperatorSection::RequestTimeline,
        ));
        assert_operator_context(driver, &alpha, OperatorSection::RequestTimeline, None);
        driver.wait_for_target(
            "alpha request row after operator server switch",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&alpha_submission.request_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(
            &bravo_submission.request_id
        )));
        let alpha_timeline_texts = driver.click_target(&audit::targets::operator_entity(
            &alpha_submission.request_id,
        ));
        assert_operator_context(
            driver,
            &alpha,
            OperatorSection::RequestTimeline,
            Some(alpha_submission.request_id.as_str()),
        );
        assert!(alpha_timeline_texts
            .iter()
            .any(|text| text.contains(alpha_submission.prompt.as_str())));
        assert!(alpha_timeline_texts
            .iter()
            .any(|text| text.contains(alpha_submission.response.trim())));

        driver.click_target(&audit::targets::operator_deployment(&bravo.peer_id));
        driver.click_target(&audit::targets::operator_agent(&bravo.agent_did));
        driver.click_target(&audit::targets::operator_section(
            OperatorSection::Behaviors,
        ));
        assert_operator_context(driver, &bravo, OperatorSection::Behaviors, None);
        driver.wait_for_target(
            "bravo behavior row after operator server switch",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&bravo.docs.behavior_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(&alpha.docs.behavior_id)));
        driver.click_target(&audit::targets::operator_entity(&bravo.docs.behavior_id));
        assert_operator_context(
            driver,
            &bravo,
            OperatorSection::Behaviors,
            Some(bravo.docs.behavior_id.as_str()),
        );
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::Behavior(draft)) => {
                assert_eq!(draft.behavior_id, bravo.docs.behavior_id);
                assert_eq!(draft.agent_did, bravo.agent_did);
                assert_eq!(draft.backend_id, bravo.docs.backend_id);
                assert_eq!(draft.tool_selection_id, bravo.docs.tool_selection_id);
                assert_eq!(draft.inference_profile_id, bravo.docs.inference_profile_id);
            }
            other => panic!("expected bravo behavior draft, got {other:?}"),
        }

        driver.click_target(&audit::targets::operator_section(OperatorSection::Backends));
        assert_operator_context(driver, &bravo, OperatorSection::Backends, None);
        driver.wait_for_target(
            "bravo backend row after operator server switch",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&bravo.docs.backend_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(&alpha.docs.backend_id)));
        driver.click_target(&audit::targets::operator_entity(&bravo.docs.backend_id));
        assert_operator_context(
            driver,
            &bravo,
            OperatorSection::Backends,
            Some(bravo.docs.backend_id.as_str()),
        );
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::Backend(draft)) => {
                assert_eq!(draft.backend_id, bravo.docs.backend_id);
                assert_eq!(draft.provider_kind, backend.provider_kind.as_str());
                assert_eq!(draft.endpoint, backend.endpoint);
                assert!(draft.models.contains(backend.model_name.as_str()));
            }
            other => panic!("expected bravo backend draft, got {other:?}"),
        }

        driver.click_target(&audit::targets::operator_section(
            OperatorSection::InferenceProfiles,
        ));
        assert_operator_context(driver, &bravo, OperatorSection::InferenceProfiles, None);
        driver.wait_for_target(
            "bravo inference profile row after operator server switch",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&bravo.docs.inference_profile_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(
            &alpha.docs.inference_profile_id
        )));
        driver.click_target(&audit::targets::operator_entity(
            &bravo.docs.inference_profile_id,
        ));
        assert_operator_context(
            driver,
            &bravo,
            OperatorSection::InferenceProfiles,
            Some(bravo.docs.inference_profile_id.as_str()),
        );
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::InferenceProfile(draft)) => {
                assert_eq!(draft.profile_id, bravo.docs.inference_profile_id);
                assert_eq!(draft.max_output_tokens, "1024");
                assert_eq!(draft.max_turns, "12");
            }
            other => panic!("expected bravo inference profile draft, got {other:?}"),
        }

        driver.click_target(&audit::targets::operator_section(
            OperatorSection::RequestTimeline,
        ));
        assert_operator_context(driver, &bravo, OperatorSection::RequestTimeline, None);
        driver.wait_for_target(
            "bravo request row after operator server switch",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&bravo_submission.request_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(
            &alpha_submission.request_id
        )));
        let bravo_timeline_texts = driver.click_target(&audit::targets::operator_entity(
            &bravo_submission.request_id,
        ));
        assert_operator_context(
            driver,
            &bravo,
            OperatorSection::RequestTimeline,
            Some(bravo_submission.request_id.as_str()),
        );
        assert!(bravo_timeline_texts
            .iter()
            .any(|text| text.contains(bravo_submission.prompt.as_str())));
        assert!(bravo_timeline_texts
            .iter()
            .any(|text| text.contains(bravo_submission.response.trim())));

        driver.click_target(&audit::targets::operator_deployment(&alpha.peer_id));
        driver.click_target(&audit::targets::operator_section(
            OperatorSection::Behaviors,
        ));
        assert_operator_context(driver, &alpha, OperatorSection::Behaviors, None);
        driver.wait_for_target(
            "alpha behavior row before config edit",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&alpha.docs.behavior_id),
        )?;
        driver.click_target(&audit::targets::operator_entity(&alpha.docs.behavior_id));
        assert_operator_context(
            driver,
            &alpha,
            OperatorSection::Behaviors,
            Some(alpha.docs.behavior_id.as_str()),
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("System Prompt"),
            alpha_tool_prompt,
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Backend ID"),
            &alpha_switch_backend_id,
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Model Name"),
            backend.model_name.as_str(),
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Inference Profile ID"),
            &alpha_switch_profile_id,
        );
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::Behavior(draft)) => {
                assert_eq!(draft.behavior_id, alpha.docs.behavior_id);
                assert_eq!(draft.backend_id, alpha_switch_backend_id);
                assert_eq!(draft.inference_profile_id, alpha_switch_profile_id);
                assert_eq!(draft.system_prompt, alpha_tool_prompt);
            }
            other => panic!("expected edited alpha behavior draft, got {other:?}"),
        }
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "alpha behavior config edit persisted on desktop",
            Duration::from_secs(10),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .behaviors
                        .iter()
                        .find(|row| row.behavior_id == alpha.docs.behavior_id)
                        .filter(|row| {
                            row.agent_did.as_deref() == Some(alpha.agent_did.as_str())
                                && row.backend_id.as_deref()
                                    == Some(alpha_switch_backend_id.as_str())
                                && row.inference_profile_id.as_deref()
                                    == Some(alpha_switch_profile_id.as_str())
                                && row.system_prompt.as_deref() == Some(alpha_tool_prompt)
                        })
                        .map(|row| row.behavior_id.clone())
                })
            },
        )?;

        driver.click_target(&audit::targets::operator_section(
            OperatorSection::ToolSelections,
        ));
        driver.wait_for_target(
            "alpha tool selection after config edit",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&alpha.docs.tool_selection_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(
            &bravo.docs.tool_selection_id
        )));
        driver.click_target(&audit::targets::operator_entity(
            &alpha.docs.tool_selection_id,
        ));
        assert_operator_context(
            driver,
            &alpha,
            OperatorSection::ToolSelections,
            Some(alpha.docs.tool_selection_id.as_str()),
        );
        driver.click_target(&audit::targets::operator_toggle("Enable File Tools"));
        driver.replace_text_in_target(
            &audit::targets::operator_field("File Tools Mode"),
            "ReadOnly",
        );
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::ToolSelection(draft)) => {
                assert_eq!(draft.selection_id, alpha.docs.tool_selection_id);
                assert_eq!(draft.agent_did, alpha.agent_did);
                assert!(draft.enable_file_tools);
                assert_eq!(draft.file_tools_mode, "ReadOnly");
                assert!(!draft.enable_bash);
            }
            other => panic!("expected edited alpha tool selection draft, got {other:?}"),
        }
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "alpha tool selection edit persisted on desktop",
            Duration::from_secs(10),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .tool_selections
                        .iter()
                        .find(|row| row.selection_id == alpha.docs.tool_selection_id)
                        .filter(|row| {
                            row.agent_did.as_deref() == Some(alpha.agent_did.as_str())
                                && row.enable_file_tools == Some(true)
                                && row.file_tools_mode.as_deref() == Some("ReadOnly")
                        })
                        .map(|row| row.selection_id.clone())
                })
            },
        )?;

        driver.click_target(&audit::targets::operator_section(OperatorSection::Backends));
        driver.wait_for_target(
            "alpha switched backend row after behavior binding edit",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&alpha_switch_backend_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(&alpha.docs.backend_id)));
        assert!(!driver.has_target(&audit::targets::operator_entity(&bravo.docs.backend_id)));
        driver.click_target(&audit::targets::operator_entity(&alpha_switch_backend_id));
        assert_operator_context(
            driver,
            &alpha,
            OperatorSection::Backends,
            Some(alpha_switch_backend_id.as_str()),
        );
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::Backend(draft)) => {
                assert_eq!(draft.backend_id, alpha_switch_backend_id);
                assert_eq!(draft.endpoint, backend.endpoint);
                assert!(draft.models.contains(backend.model_name.as_str()));
                assert_eq!(draft.probe_status, "healthy");
            }
            other => panic!("expected alpha switched backend draft, got {other:?}"),
        }

        driver.click_target(&audit::targets::operator_section(
            OperatorSection::InferenceProfiles,
        ));
        driver.wait_for_target(
            "alpha switched inference profile row after behavior binding edit",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&alpha_switch_profile_id),
        )?;
        assert!(!driver.has_target(&audit::targets::operator_entity(
            &alpha.docs.inference_profile_id
        )));
        assert!(!driver.has_target(&audit::targets::operator_entity(
            &bravo.docs.inference_profile_id
        )));
        driver.click_target(&audit::targets::operator_entity(&alpha_switch_profile_id));
        assert_operator_context(
            driver,
            &alpha,
            OperatorSection::InferenceProfiles,
            Some(alpha_switch_profile_id.as_str()),
        );
        driver.replace_text_in_target(&audit::targets::operator_field("Max Output Tokens"), "1536");
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::InferenceProfile(draft)) => {
                assert_eq!(draft.profile_id, alpha_switch_profile_id);
                assert_eq!(draft.max_output_tokens, "1536");
                assert_eq!(draft.max_turns, "16");
                assert_eq!(draft.temperature, "0");
            }
            other => panic!("expected edited alpha switched profile draft, got {other:?}"),
        }
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "alpha inference profile edit persisted on desktop",
            Duration::from_secs(10),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .inference_profiles
                        .iter()
                        .find(|row| row.profile_id == alpha_switch_profile_id)
                        .filter(|row| row.max_output_tokens == Some(1536))
                        .map(|row| row.profile_id.clone())
                })
            },
        )?;
    }

    wait_for_value(
        "alpha behavior/tool config and generation after UI edits",
        Duration::from_secs(120),
        || {
            fixture
                .runtime
                .block_on(desktop_client.refresh_store())
                .ok()?;
            let snapshot = desktop_client.store().snapshot();
            let behavior_ready = snapshot
                .behaviors
                .iter()
                .find(|row| row.behavior_id == alpha.docs.behavior_id)
                .is_some_and(|row| {
                    row.backend_id.as_deref() == Some(alpha_switch_backend_id.as_str())
                        && row.inference_profile_id.as_deref()
                            == Some(alpha_switch_profile_id.as_str())
                        && row.system_prompt.as_deref() == Some(alpha_tool_prompt)
                });
            let tools_ready = snapshot
                .tool_selections
                .iter()
                .find(|row| row.selection_id == alpha.docs.tool_selection_id)
                .is_some_and(|row| {
                    row.enable_file_tools == Some(true)
                        && row.file_tools_mode.as_deref() == Some("ReadOnly")
                });
            let profile_ready = snapshot
                .inference_profiles
                .iter()
                .find(|row| row.profile_id == alpha_switch_profile_id)
                .is_some_and(|row| row.max_output_tokens == Some(1536));
            let runtime_ready = snapshot
                .latest_runtime(&alpha.agent_did)
                .is_some_and(|row| {
                    row.router_generation
                        .or(row.active_generation)
                        .is_some_and(|generation| generation > alpha_initial_generation)
                        && row.runnable_behavior_count == Some(1)
                        && row.unavailable_behavior_count == Some(0)
                        && row
                            .last_reconcile_error
                            .as_deref()
                            .unwrap_or_default()
                            .trim()
                            .is_empty()
                });
            (behavior_ready && tools_ready && profile_ready && runtime_ready).then_some(())
        },
    )
    .with_context(|| {
        format!(
            "desktop state: {}\nremote state: {}",
            describe_live_config_state(
                fixture.runtime.as_ref(),
                desktop_client.as_ref(),
                "desktop",
                &alpha.agent_did,
                &alpha.docs,
                &alpha_switch_backend_id,
                &alpha_switch_profile_id,
            ),
            describe_live_config_state(
                fixture.runtime.as_ref(),
                alpha.remote_core,
                "alpha remote",
                &alpha.agent_did,
                &alpha.docs,
                &alpha_switch_backend_id,
                &alpha_switch_profile_id,
            )
        )
    })?;
    wait_for_value(
        "alpha behavior/tool config replicated to remote runtime",
        Duration::from_secs(120),
        || {
            fixture
                .runtime
                .block_on(alpha.remote_core.refresh_store())
                .ok()?;
            let snapshot = alpha.remote_core.store().snapshot();
            let behavior_ready = snapshot
                .behaviors
                .iter()
                .find(|row| row.behavior_id == alpha.docs.behavior_id)
                .is_some_and(|row| {
                    row.backend_id.as_deref() == Some(alpha_switch_backend_id.as_str())
                        && row.inference_profile_id.as_deref()
                            == Some(alpha_switch_profile_id.as_str())
                        && row.system_prompt.as_deref() == Some(alpha_tool_prompt)
                });
            let backend_ready = snapshot
                .inference_backends
                .iter()
                .find(|row| row.backend_id == alpha_switch_backend_id)
                .is_some_and(|row| {
                    row.endpoint.as_deref() == Some(backend.endpoint.as_str())
                        && row.models.iter().any(|model| model == &backend.model_name)
                });
            let tools_ready = snapshot
                .tool_selections
                .iter()
                .find(|row| row.selection_id == alpha.docs.tool_selection_id)
                .is_some_and(|row| {
                    row.enable_file_tools == Some(true)
                        && row.file_tools_mode.as_deref() == Some("ReadOnly")
                });
            let profile_ready = snapshot
                .inference_profiles
                .iter()
                .find(|row| row.profile_id == alpha_switch_profile_id)
                .is_some_and(|row| row.max_output_tokens == Some(1536));
            let runtime_ready = snapshot
                .latest_runtime(&alpha.agent_did)
                .is_some_and(|row| {
                    row.router_generation
                        .or(row.active_generation)
                        .is_some_and(|generation| generation > alpha_remote_initial_generation)
                        && row.runnable_behavior_count == Some(1)
                        && row.unavailable_behavior_count == Some(0)
                        && row
                            .last_reconcile_error
                            .as_deref()
                            .unwrap_or_default()
                            .trim()
                            .is_empty()
                });
            (behavior_ready && backend_ready && tools_ready && profile_ready && runtime_ready)
                .then_some(())
        },
    )
    .with_context(|| {
        format!(
            "desktop state: {}\nremote state: {}",
            describe_live_config_state(
                fixture.runtime.as_ref(),
                desktop_client.as_ref(),
                "desktop",
                &alpha.agent_did,
                &alpha.docs,
                &alpha_switch_backend_id,
                &alpha_switch_profile_id,
            ),
            describe_live_config_state(
                fixture.runtime.as_ref(),
                alpha.remote_core,
                "alpha remote",
                &alpha.agent_did,
                &alpha.docs,
                &alpha_switch_backend_id,
                &alpha_switch_profile_id,
            )
        )
    })?;
    wait_for_stable_runtime_ready(
        fixture.runtime.as_ref(),
        alpha.remote_core,
        "alpha after remote config replication",
        &alpha.agent_did,
        Duration::from_secs(10),
        Duration::from_secs(90),
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
                                    == Some(alpha_switch_backend_id.as_str())
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
        Some(alpha_switch_backend_id.as_str()),
    )?;
    assert_live_submission_rows(
        fixture.runtime.as_ref(),
        alpha.remote_core,
        "remote alpha post-config",
        &alpha,
        &post_config_submission,
        Some(alpha_switch_backend_id.as_str()),
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
        driver.open_activity(Activity::Chat);
        driver.click_target(&audit::targets::chat_deployment(&bravo.peer_id));
        driver.click_target(&audit::targets::chat_agent(&bravo.agent_did));
        assert_chat_context(driver, &bravo, None);
        let bravo_texts = driver.click_target(&audit::targets::chat_conversation(
            &bravo_submission.session_id,
        ));
        assert_chat_context(driver, &bravo, Some(bravo_submission.session_id.as_str()));
        assert!(bravo_texts
            .iter()
            .any(|text| text.contains(bravo_submission.prompt.as_str())));
        assert!(
            !bravo_texts
                .iter()
                .any(|text| text.contains(post_config_submission.prompt.as_str())),
            "bravo transcript leaked alpha post-config prompt after switching deployments"
        );

        driver.click_target(&audit::targets::chat_deployment(&alpha.peer_id));
        driver.click_target(&audit::targets::chat_agent(&alpha.agent_did));
        assert_chat_context(driver, &alpha, None);
        let alpha_post_config_texts = driver.click_target(&audit::targets::chat_conversation(
            &post_config_submission.session_id,
        ));
        assert_chat_context(
            driver,
            &alpha,
            Some(post_config_submission.session_id.as_str()),
        );
        assert!(alpha_post_config_texts
            .iter()
            .any(|text| text.contains(post_config_submission.prompt.as_str())));
        assert!(alpha_post_config_texts
            .iter()
            .any(|text| text.contains(post_config_submission.response.trim())));
        driver.wait_for_target(
            "alpha post-config tool card visible",
            Duration::from_secs(10),
            &audit::targets::chat_tool_card(&alpha_post_config_tool_card_id),
        )?;
    }

    fixture.shutdown()
}

