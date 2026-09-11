use anyhow::Result;
use gents::document_config::{
    AgentBehavior, AgentPrincipal, InferenceBackend, InferenceProfile, Schedule, SkillDocument,
    Task, Tools, Trigger,
};
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn canonical_manage_document_saves_refresh_store() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    )
    .await?;
    let agent_did = "did:test:amy";

    let mut principal: AgentPrincipal = serde_json::from_value(json!({
        "agent_did": agent_did,
        "display_name": "Amy",
        "tags": ["managed"]
    }))?;
    core.save_agent_principal(&principal).await?;

    let backend: InferenceBackend = serde_json::from_value(json!({
        "agent_did": agent_did,
        "backend_id": "backend-amy",
        "name": "OpenRouter",
        "provider_kind": "OpenRouter",
        "endpoint": "https://openrouter.ai/api/v1",
        "auth": {"kind": "environment", "variable": "OPENROUTER_API_KEY"},
        "max_concurrent": 2,
        "max_queue_depth": 100,
        "tags": ["managed"]
    }))?;
    core.save_backend(&backend).await?;

    let profile: InferenceProfile = serde_json::from_value(json!({
        "agent_did": agent_did,
        "profile_id": "profile-amy",
        "display_name": "Amy Profile",
        "backend_id": "backend-amy",
        "model_name": "openai/gpt-5.4",
        "context_window": 128000,
        "max_output_tokens": 4096,
        "tags": ["managed"]
    }))?;
    core.save_inference_profile(&profile).await?;

    let tools: Tools = serde_json::from_value(json!({
        "agent_did": agent_did,
        "tools_id": "tools-amy",
        "display_name": "Amy Tools",
        "host": {
            "files": {"mode": "ReadWrite"},
            "bash": {"mode": "Unrestricted"},
            "cli": [{"name": "rg"}, {"name": "cargo"}]
        },
        "tags": ["managed"]
    }))?;
    core.save_tools(&tools).await?;

    let skill: SkillDocument = serde_json::from_value(json!({
        "agent_did": agent_did,
        "skill_id": "amy-skill",
        "name": "Amy Skill",
        "instructions": "Inspect the queue.",
        "tool_refs": ["read_file"],
        "tags": ["managed"]
    }))?;
    core.save_skill(&skill).await?;

    let behavior: AgentBehavior = serde_json::from_value(json!({
        "agent_did": agent_did,
        "behavior_id": "amy-default",
        "display_name": "Amy Default",
        "inference_profile_id": "profile-amy",
        "tags": ["managed"]
    }))?;
    core.save_behavior(&behavior).await?;
    principal.default_behavior_id = Some("amy-default".into());
    core.save_agent_principal(&principal).await?;

    let task: Task = serde_json::from_value(json!({
        "agent_did": agent_did,
        "task_id": "task-amy-daily",
        "display_name": "Daily Amy",
        "behavior_id": "amy-default",
        "prompt_template": "Check the daily queue.",
        "tags": ["managed"]
    }))?;
    core.save_task(&task).await?;

    let schedule: Schedule = serde_json::from_value(json!({
        "agent_did": agent_did,
        "schedule_id": "schedule-amy-daily",
        "display_name": "Daily cadence",
        "cadence": {"kind": "interval", "interval_secs": 300},
        "tags": ["managed"]
    }))?;
    core.save_schedule(&schedule).await?;

    let trigger: Trigger = serde_json::from_value(json!({
        "agent_did": agent_did,
        "trigger_id": "trigger-amy-daily",
        "task_id": "task-amy-daily",
        "source": {"kind": "schedule", "schedule_id": "schedule-amy-daily"},
        "tags": ["managed"]
    }))?;
    core.save_trigger(&trigger).await?;

    let snapshot = core.store().snapshot();
    assert!(snapshot
        .agent_principals
        .iter()
        .any(|row| { row.agent_did == agent_did && row.tags == ["managed"] }));
    assert!(snapshot
        .inference_backends
        .iter()
        .any(|row| { row.agent_did == agent_did && row.backend_id == "backend-amy" }));
    assert!(snapshot.inference_profiles.iter().any(|row| {
        row.agent_did == agent_did
            && row.profile_id == "profile-amy"
            && row.backend_id == "backend-amy"
    }));
    assert!(snapshot
        .tools
        .iter()
        .any(|row| { row.agent_did == agent_did && row.tools_id == "tools-amy" }));
    assert!(snapshot.skills.iter().any(|row| {
        row.agent_did == agent_did && row.skill_id == "amy-skill" && row.tool_refs == ["read_file"]
    }));
    assert!(snapshot.behaviors.iter().any(|row| {
        row.agent_did == agent_did
            && row.behavior_id == "amy-default"
            && row.inference_profile_id == "profile-amy"
    }));
    assert!(snapshot.tasks.iter().any(|row| {
        row.agent_did == agent_did
            && row.task_id == "task-amy-daily"
            && row.behavior_id == "amy-default"
    }));
    assert!(snapshot
        .schedules
        .iter()
        .any(|row| { row.agent_did == agent_did && row.schedule_id == "schedule-amy-daily" }));
    assert!(snapshot.triggers.iter().any(|row| {
        row.agent_did == agent_did
            && row.trigger_id == "trigger-amy-daily"
            && row.task_id == "task-amy-daily"
    }));

    core.delete_skill("amy-skill", agent_did).await?;
    assert!(!core
        .store()
        .snapshot()
        .skills
        .iter()
        .any(|row| row.agent_did == agent_did && row.skill_id == "amy-skill"));
    core.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn desktop_task_run_atomically_provisions_declared_goal() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    )
    .await?;
    let agent_did = core.principal().did().to_string();

    let mut principal: AgentPrincipal = serde_json::from_value(json!({"agent_did": agent_did}))?;
    core.save_agent_principal(&principal).await?;
    let backend: InferenceBackend = serde_json::from_value(json!({
        "agent_did": agent_did,
        "backend_id": "local",
        "name": "Local",
        "provider_kind": "OpenAiCompatible",
        "endpoint": "http://localhost:8000/v1",
        "auth": {"kind": "unauthenticated"}
    }))?;
    core.save_backend(&backend).await?;
    let profile: InferenceProfile = serde_json::from_value(json!({
        "agent_did": agent_did,
        "profile_id": "durable-profile",
        "backend_id": "local",
        "model_name": "model"
    }))?;
    core.save_inference_profile(&profile).await?;
    let behavior: AgentBehavior = serde_json::from_value(json!({
        "agent_did": agent_did,
        "behavior_id": "durable",
        "inference_profile_id": "durable-profile"
    }))?;
    core.save_behavior(&behavior).await?;
    principal.default_behavior_id = Some("durable".into());
    core.save_agent_principal(&principal).await?;

    let task: Task = serde_json::from_value(json!({
        "agent_did": agent_did,
        "task_id": "durable-task",
        "display_name": "Durable task",
        "behavior_id": "durable",
        "prompt_template": "Handle {{ args.item }}",
        "goal_objective_template": "Finish {{ args.item }}",
        "goal_token_budget": 10000
    }))?;
    core.save_task(&task).await?;
    let request_doc_id = core
        .fire_task_now(&task, json!({"item": "release"}))
        .await?;

    let response = core
        .node()
        .execute(&format!(
            r#"{{
                AgentRequest(filter: {{ _docID: {{ _eq: "{request_doc_id}" }} }}) {{
                    session_id retry_key content lifecycle_state
                }}
                Goal(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}) {{
                    session_id objective status token_budget
                }}
            }}"#,
        ))
        .await;
    assert!(
        !response.has_errors(),
        "query task run: {:?}",
        response.errors
    );
    let data = response.data.as_ref().expect("query data");
    let requests = data["AgentRequest"].as_array().expect("request rows");
    let goals = data["Goal"].as_array().expect("goal rows");
    assert_eq!(requests.len(), 1);
    assert_eq!(goals.len(), 1);
    assert_eq!(requests[0]["content"], "Handle release");
    assert_eq!(requests[0]["lifecycle_state"], "pending");
    assert!(requests[0]["retry_key"].as_str().is_some());
    assert_eq!(goals[0]["session_id"], requests[0]["session_id"]);
    assert_eq!(goals[0]["objective"], "Finish release");
    assert_eq!(goals[0]["status"], "active");
    assert_eq!(goals[0]["token_budget"], 10_000);

    core.shutdown().await?;
    Ok(())
}
