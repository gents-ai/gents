use anyhow::Result;
use chrono::Utc;
use defra_agent_desktop::client::{
    ClientCore, ClientCoreOptions, ClientStore, ClientStoreRows, DesktopPaths,
};
use defra_agent_desktop::state::{OperatorDraft, OperatorSection, ShellState};
use defra_agent_desktop::views::operator::prepare_state;
use defra_agent_protocol::row::{
    AgentBehaviorRow, AgentPrincipalRow, InferenceBackendRow, InferenceProfileRow,
    ScheduledTaskRow, ToolSelectionRow,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn operator_document_saves_refresh_store() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    )
    .await?;

    let principal_resp = core
        .node()
        .execute(
            r#"mutation {
                add_AgentPrincipal(input: {
                    agent_did: "did:defra:amy"
                    display_name: "Amy"
                    default_behavior_id: "amy-default"
                    enabled: true
                }) { agent_did }
            }"#,
        )
        .await;
    assert!(!principal_resp.has_errors());

    core.refresh_store().await?;

    core.save_backend(&InferenceBackendRow {
        backend_id: "backend-amy".to_string(),
        name: Some("OpenRouter".to_string()),
        provider_kind: Some("openrouter".to_string()),
        endpoint: Some("https://openrouter.ai/api/v1".to_string()),
        api_key: None,
        api_key_env_var: Some("OPENROUTER_API_KEY".to_string()),
        max_concurrent: Some(2),
        enabled: Some(true),
        supports_tool_calls: Some(true),
        supports_streaming: Some(true),
        supports_structured_outputs: Some(true),
        supports_json_schema: Some(true),
        models: vec!["openai/gpt-5.4".to_string()],
        last_probe: None,
        probe_status: Some("healthy".to_string()),
    })
    .await?;

    core.save_inference_profile(&InferenceProfileRow {
        profile_id: "profile-amy".to_string(),
        display_name: Some("Amy Profile".to_string()),
        context_window: Some(128000),
        max_output_tokens: Some(4096),
        max_turns: Some(24),
        temperature: Some(0.2),
        stream_batch_ms: Some(50),
        deadline_duration_secs: Some(300),
    })
    .await?;

    core.save_tool_selection(&ToolSelectionRow {
        selection_id: "tools-amy".to_string(),
        agent_did: Some("did:defra:amy".to_string()),
        display_name: Some("Amy Tools".to_string()),
        enable_file_tools: Some(true),
        file_tools_mode: Some("workspace-write".to_string()),
        enable_bash: Some(true),
        bash_mode: Some("workspace".to_string()),
        cli_tool_names: vec!["rg".to_string(), "cargo".to_string()],
        enable_meta_tools: Some(true),
        delegate_to: vec!["planner".to_string()],
    })
    .await?;

    core.save_behavior(&AgentBehaviorRow {
        behavior_id: "amy-default".to_string(),
        agent_did: Some("did:defra:amy".to_string()),
        display_name: Some("Amy Default".to_string()),
        system_prompt: Some("You are Amy.".to_string()),
        backend_id: Some("backend-amy".to_string()),
        model_name: Some("openai/gpt-5.4".to_string()),
        tool_selection_id: Some("tools-amy".to_string()),
        inference_profile_id: Some("profile-amy".to_string()),
        compaction_strategy: Some("rolling-summary".to_string()),
        compaction_threshold: Some(0.7),
        enabled: Some(true),
        created_at: Some("2026-04-14T00:00:00Z".to_string()),
    })
    .await?;

    core.save_scheduled_task(&ScheduledTaskRow {
        task_id: "task-amy-daily".to_string(),
        agent_did: Some("did:defra:amy".to_string()),
        behavior_id: Some("amy-default".to_string()),
        name: Some("Daily Amy".to_string()),
        prompt: Some("Check the daily queue.".to_string()),
        interval_secs: Some(300),
        enabled: Some(true),
        next_run_at: Some("2026-04-15T00:00:00Z".to_string()),
        last_run_at: None,
        last_status: None,
        last_error: None,
        run_count: Some(0),
        created_at: None,
        updated_at: None,
    })
    .await?;

    core.run_scheduled_task_now(&ScheduledTaskRow {
        task_id: "task-amy-daily".to_string(),
        agent_did: Some("did:defra:amy".to_string()),
        behavior_id: Some("amy-default".to_string()),
        name: Some("Daily Amy".to_string()),
        prompt: Some("Check the daily queue.".to_string()),
        interval_secs: Some(300),
        enabled: Some(true),
        next_run_at: Some("2026-04-15T00:00:00Z".to_string()),
        last_run_at: None,
        last_status: None,
        last_error: None,
        run_count: Some(0),
        created_at: None,
        updated_at: None,
    })
    .await?;

    let snapshot = core.store().snapshot();

    assert!(snapshot
        .inference_backends
        .iter()
        .any(|row| row.backend_id == "backend-amy" && row.name.as_deref() == Some("OpenRouter")));
    assert!(snapshot
        .inference_profiles
        .iter()
        .any(|row| row.profile_id == "profile-amy"
            && row.display_name.as_deref() == Some("Amy Profile")));
    assert!(snapshot
        .tool_selections
        .iter()
        .any(|row| row.selection_id == "tools-amy" && row.cli_tool_names.len() == 2));
    assert!(snapshot
        .behaviors
        .iter()
        .any(|row| row.behavior_id == "amy-default"
            && row.backend_id.as_deref() == Some("backend-amy")
            && row.inference_profile_id.as_deref() == Some("profile-amy")
            && row.tool_selection_id.as_deref() == Some("tools-amy")));
    let scheduled = snapshot
        .scheduled_tasks
        .iter()
        .find(|row| row.task_id == "task-amy-daily")
        .expect("scheduled task should be present");
    assert_eq!(scheduled.behavior_id.as_deref(), Some("amy-default"));
    assert_eq!(scheduled.interval_secs, Some(300));
    assert_eq!(scheduled.enabled, Some(true));
    let next_run_at = chrono::DateTime::parse_from_rfc3339(
        scheduled
            .next_run_at
            .as_deref()
            .expect("run now should set next_run_at"),
    )?
    .with_timezone(&Utc);
    assert!(next_run_at <= Utc::now());

    core.shutdown().await?;
    Ok(())
}

#[test]
fn operator_prepare_state_reselects_entity_and_refreshes_draft() {
    let store = ClientStore::from_rows(ClientStoreRows {
        agent_principals: vec![AgentPrincipalRow {
            agent_did: "did:defra:amy".to_string(),
            display_name: Some("Amy".to_string()),
            default_behavior_id: Some("amy-default".to_string()),
            enabled: Some(true),
            created_at: None,
            created_by: None,
        }],
        behaviors: vec![AgentBehaviorRow {
            behavior_id: "amy-default".to_string(),
            agent_did: Some("did:defra:amy".to_string()),
            display_name: Some("Amy Default".to_string()),
            system_prompt: Some("You are Amy.".to_string()),
            backend_id: Some("backend-amy".to_string()),
            model_name: Some("openai/gpt-5.4".to_string()),
            tool_selection_id: Some("tools-amy".to_string()),
            inference_profile_id: Some("profile-amy".to_string()),
            compaction_strategy: Some("rolling-summary".to_string()),
            compaction_threshold: Some(0.7),
            enabled: Some(true),
            created_at: Some("2026-04-14T00:00:00Z".to_string()),
        }],
        ..ClientStoreRows::default()
    });
    let mut state = ShellState::default();
    state.operator.selected_section = OperatorSection::Behaviors;
    state.operator.selected_peer_id = Some("peer-missing".to_string());
    state.operator.selected_agent_did = Some("did:defra:amy".to_string());
    state.operator.selected_entity_id = Some("behavior-missing".to_string());
    state.operator.last_apply_error = Some("stale validation".to_string());
    state.operator.draft = Some(OperatorDraft::Behavior(
        defra_agent_desktop::state::BehaviorDraft {
            behavior_id: "behavior-missing".to_string(),
            agent_did: "did:defra:amy".to_string(),
            display_name: "Stale".to_string(),
            system_prompt: String::new(),
            backend_id: String::new(),
            model_name: String::new(),
            tool_selection_id: String::new(),
            inference_profile_id: String::new(),
            compaction_strategy: String::new(),
            compaction_threshold: String::new(),
            enabled: false,
            created_at: String::new(),
        },
    ));

    prepare_state(&mut state, None, Some(&store));

    assert_eq!(state.operator.selected_peer_id, None);
    assert_eq!(
        state.operator.selected_agent_did.as_deref(),
        Some("did:defra:amy")
    );
    assert_eq!(
        state.operator.selected_entity_id.as_deref(),
        Some("amy-default")
    );
    assert_eq!(state.operator.last_apply_error, None);
    match state.operator.draft.as_ref() {
        Some(OperatorDraft::Behavior(draft)) => {
            assert_eq!(draft.behavior_id, "amy-default");
            assert_eq!(draft.display_name, "Amy Default");
            assert_eq!(draft.backend_id, "backend-amy");
        }
        other => panic!("expected refreshed behavior draft, got {other:?}"),
    }
}
