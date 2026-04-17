use super::*;

#[test]
#[ignore = "hits live inference backend configured by DEFRA_AGENT_DESKTOP_LIVE_BACKEND_* or OPENROUTER_API_KEY"]
fn desktop_app_live_operator_config_round_trips() -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    let mut fixture = build_live_desktop_fixture("audit-live-operator", global_log_store())?;
    let docs = fixture.docs.clone();
    let backend = fixture.backend.clone();
    let agent_did = fixture
        .running_agent
        .as_ref()
        .ok_or_else(|| anyhow!("live fixture missing running agent"))?
        .did
        .clone();
    let shadow_backend_id = format!("{}:binding-backend", docs.behavior_id);
    let shadow_model_name = format!("{}:binding-model", backend.model_name);
    let shadow_tool_selection_id = format!("{}:binding-tools", docs.behavior_id);
    let shadow_inference_profile_id = format!("{}:binding-profile", docs.behavior_id);

    {
        let client = Arc::clone(
            fixture
                .driver
                .app
                .client
                .as_ref()
                .ok_or_else(|| anyhow!("desktop client missing"))?,
        );
        fixture.runtime.block_on(async {
            client
                .save_backend(&InferenceBackendRow {
                    backend_id: shadow_backend_id.clone(),
                    name: Some("Live Binding Backend".to_string()),
                    provider_kind: Some(backend.provider_kind.as_str().to_string()),
                    endpoint: Some(backend.endpoint.clone()),
                    api_key: backend.api_key.clone(),
                    api_key_env_var: backend.api_key_env_var.clone(),
                    max_concurrent: Some(1),
                    max_queue_depth: Some(100),
                    enabled: Some(true),
                    models: vec![shadow_model_name.clone()],
                    last_probe: None,
                    probe_status: Some("healthy".to_string()),
                })
                .await?;
            client
                .save_tool_selection(&ToolSelectionRow {
                    selection_id: shadow_tool_selection_id.clone(),
                    agent_did: Some(agent_did.clone()),
                    display_name: Some("Live Binding Tools".to_string()),
                    enable_file_tools: Some(false),
                    file_tools_mode: Some("readonly".to_string()),
                    enable_bash: Some(false),
                    bash_mode: Some("disabled".to_string()),
                    cli_tool_names: vec![],
                    enable_meta_tools: Some(false),
                    delegate_to: vec![],
                })
                .await?;
            client
                .save_inference_profile(&InferenceProfileRow {
                    profile_id: shadow_inference_profile_id.clone(),
                    display_name: Some("Live Binding Profile".to_string()),
                    context_window: Some(32768),
                    max_output_tokens: Some(512),
                    max_turns: Some(8),
                    temperature: Some(0.0),
                    stream_batch_ms: Some(40),
                    deadline_duration_secs: Some(120),
                })
                .await?;
            Ok::<(), anyhow::Error>(())
        })?;
    }

    {
        let driver = &mut fixture.driver;
        let _session_id = ensure_chat_session_selected(
            driver,
            "live operator fixture chat ready",
            Duration::from_secs(10),
        )?;
        let (request_id, response_text) = submit_chat_message_and_wait_for_observed_response(
            driver,
            "Reply with exactly CONFIG_READY for the operator audit",
        )?;
        assert!(!response_text.trim().is_empty());

        driver.open_activity(Activity::Operator);
        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::Runtime,
        ));
        let runtime_texts = driver.render();
        assert!(runtime_texts
            .iter()
            .any(|text| text.contains("Runtime Inspector")));
        assert!(runtime_texts.iter().any(|text| text.contains(&agent_did)));
        assert!(runtime_texts
            .iter()
            .any(|text| text.contains(&docs.behavior_id)));

        driver.click_target(&audit::targets::operator_section(
            crate::state::OperatorSection::RequestTimeline,
        ));
        driver.wait_for_target(
            "live operator request timeline row",
            Duration::from_secs(10),
            &audit::targets::operator_entity(&request_id),
        )?;
        let timeline_texts = driver.click_target(&audit::targets::operator_entity(&request_id));
        assert!(timeline_texts
            .iter()
            .any(|text| text.contains("CONFIG_READY")));
        assert!(timeline_texts
            .iter()
            .any(|text| text.contains(response_text.trim())));

        assert_operator_filter_round_trip(
            driver,
            OperatorSection::Behaviors,
            "Live Audit Default",
            &docs.behavior_id,
            "definitely-missing-live-behavior",
        )?;
        driver.click_target(&audit::targets::operator_entity(&docs.behavior_id));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Display Name"),
            "Discarded Live Behavior Draft",
        );
        driver.click_target(audit::targets::OPERATOR_DISCARD);
        match driver.app.state.operator.draft.as_ref() {
            Some(OperatorDraft::Behavior(draft)) => {
                assert_eq!(draft.display_name, "Live Audit Default");
            }
            other => panic!("expected behavior draft after discard, got {other:?}"),
        }
        assert!(driver
            .app
            .client
            .as_ref()
            .and_then(|client| {
                client
                    .store()
                    .snapshot()
                    .behaviors
                    .iter()
                    .find(|row| row.behavior_id == docs.behavior_id)
                    .and_then(|row| row.display_name.clone())
            })
            .is_some_and(|display_name| display_name == "Live Audit Default"));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Display Name"),
            "Live Audit Behavior Reviewed",
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("System Prompt"),
            "You are a live audited desktop operator. Return concise answers.",
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Backend ID"),
            &shadow_backend_id,
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Model Name"),
            &shadow_model_name,
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Tool Selection ID"),
            &shadow_tool_selection_id,
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Inference Profile ID"),
            &shadow_inference_profile_id,
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Compaction Strategy"),
            "StripThenSummarize",
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Compaction Threshold"),
            "0.88",
        );
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "live behavior edits persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                client
                    .store()
                    .snapshot()
                    .behaviors
                    .iter()
                    .find(|row| row.behavior_id == docs.behavior_id)
                    .filter(|row| {
                        row.display_name.as_deref()
                            == Some("Live Audit Behavior Reviewed")
                            && row.system_prompt.as_deref()
                                == Some(
                                    "You are a live audited desktop operator. Return concise answers.",
                                )
                            && row.backend_id.as_deref() == Some(shadow_backend_id.as_str())
                            && row.model_name.as_deref() == Some(shadow_model_name.as_str())
                            && row.tool_selection_id.as_deref()
                                == Some(shadow_tool_selection_id.as_str())
                            && row.inference_profile_id.as_deref()
                                == Some(shadow_inference_profile_id.as_str())
                            && row.compaction_strategy.as_deref() == Some("StripThenSummarize")
                            && row.compaction_threshold == Some(0.88)
                    })
                    .map(|row| row.behavior_id.clone())
            })
            },
        )?;

        assert_operator_filter_round_trip(
            driver,
            OperatorSection::Backends,
            &shadow_backend_id,
            &shadow_backend_id,
            "definitely-missing-live-backend",
        )?;
        driver.click_target(&audit::targets::operator_entity(&shadow_backend_id));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Name"),
            "Live Backend Reviewed",
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Provider Kind"),
            backend.provider_kind.as_str(),
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("Endpoint"),
            backend.endpoint.as_str(),
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("API Key"),
            "desktop-audit-placeholder-key",
        );
        driver.replace_text_in_target(
            &audit::targets::operator_field("API Key Env Var"),
            "DEFRA_AGENT_DESKTOP_AUDIT_API_KEY",
        );
        driver.replace_text_in_target(&audit::targets::operator_field("Max Concurrent"), "2");
        driver.replace_text_in_target(&audit::targets::operator_field("Max Queue Depth"), "200");
        driver.replace_text_in_target(&audit::targets::operator_field("Probe Status"), "reviewed");
        driver.click_target(&audit::targets::operator_toggle("Enabled"));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Models"),
            &format!("{shadow_model_name}, audit-shadow-model"),
        );
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "live backend edits persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .inference_backends
                        .iter()
                        .find(|row| row.backend_id == shadow_backend_id)
                        .filter(|row| {
                            row.name.as_deref() == Some("Live Backend Reviewed")
                                && row.provider_kind.as_deref()
                                    == Some(backend.provider_kind.as_str())
                                && row.endpoint.as_deref() == Some(backend.endpoint.as_str())
                                && row.api_key.as_deref() == Some("desktop-audit-placeholder-key")
                                && row.api_key_env_var.as_deref()
                                    == Some("DEFRA_AGENT_DESKTOP_AUDIT_API_KEY")
                                && row.max_concurrent == Some(2)
                                && row.max_queue_depth == Some(200)
                                && row.probe_status.as_deref() == Some("reviewed")
                                && row.enabled == Some(false)
                                && row.models.iter().any(|model| model == &shadow_model_name)
                                && row.models.iter().any(|model| model == "audit-shadow-model")
                        })
                        .map(|row| row.backend_id.clone())
                })
            },
        )?;

        assert_operator_filter_round_trip(
            driver,
            OperatorSection::ToolSelections,
            "Live Audit Tools",
            &docs.tool_selection_id,
            "definitely-missing-live-tools",
        )?;
        driver.click_target(&audit::targets::operator_entity(&docs.tool_selection_id));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Display Name"),
            "Live Tooling Reviewed",
        );
        driver.click_target(&audit::targets::operator_toggle("Enable File Tools"));
        driver.replace_text_in_target(
            &audit::targets::operator_field("File Tools Mode"),
            "readonly",
        );
        driver.click_target(&audit::targets::operator_toggle("Enable Bash"));
        driver.replace_text_in_target(&audit::targets::operator_field("Bash Mode"), "workspace");
        driver.replace_text_in_target(
            &audit::targets::operator_field("CLI Tool Names"),
            "rg\ncargo",
        );
        driver.click_target(&audit::targets::operator_toggle("Enable Meta Tools"));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Delegate To"),
            "planner\nreviewer",
        );
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "live tool selection edits persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .tool_selections
                        .iter()
                        .find(|row| row.selection_id == docs.tool_selection_id)
                        .filter(|row| {
                            row.display_name.as_deref() == Some("Live Tooling Reviewed")
                                && row.enable_file_tools == Some(true)
                                && row.file_tools_mode.as_deref() == Some("readonly")
                                && row.enable_bash == Some(true)
                                && row.bash_mode.as_deref() == Some("workspace")
                                && row.cli_tool_names == vec!["rg".to_string(), "cargo".to_string()]
                                && row.enable_meta_tools == Some(true)
                                && row.delegate_to
                                    == vec!["planner".to_string(), "reviewer".to_string()]
                        })
                        .map(|row| row.selection_id.clone())
                })
            },
        )?;

        assert_operator_filter_round_trip(
            driver,
            OperatorSection::InferenceProfiles,
            "Live Binding Profile",
            &shadow_inference_profile_id,
            "definitely-missing-live-profile",
        )?;
        driver.click_target(&audit::targets::operator_entity(
            &shadow_inference_profile_id,
        ));
        driver.replace_text_in_target(
            &audit::targets::operator_field("Display Name"),
            "Live Profile Reviewed",
        );
        driver.replace_text_in_target(&audit::targets::operator_field("Context Window"), "65536");
        driver.replace_text_in_target(&audit::targets::operator_field("Max Output Tokens"), "2048");
        driver.replace_text_in_target(&audit::targets::operator_field("Max Turns"), "14");
        driver.replace_text_in_target(&audit::targets::operator_field("Temperature"), "0.1");
        driver.replace_text_in_target(&audit::targets::operator_field("Stream Batch Ms"), "80");
        driver.replace_text_in_target(
            &audit::targets::operator_field("Deadline Duration Secs"),
            "180",
        );
        driver.click_target(audit::targets::OPERATOR_APPLY);
        wait_for_value(
            "live inference profile edits persisted",
            Duration::from_secs(5),
            || {
                driver.app.client.as_ref().and_then(|client| {
                    client
                        .store()
                        .snapshot()
                        .inference_profiles
                        .iter()
                        .find(|row| row.profile_id == shadow_inference_profile_id)
                        .filter(|row| {
                            row.display_name.as_deref() == Some("Live Profile Reviewed")
                                && row.context_window == Some(65536)
                                && row.max_output_tokens == Some(2048)
                                && row.max_turns == Some(14)
                                && row.temperature == Some(0.1)
                                && row.stream_batch_ms == Some(80)
                                && row.deadline_duration_secs == Some(180)
                        })
                        .map(|row| row.profile_id.clone())
                })
            },
        )?;
    }

    fixture.shutdown()
}
