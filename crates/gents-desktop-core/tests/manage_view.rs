use anyhow::Result;
use gents::documents::{
    Agent, InferenceBackend, InferenceProfile, Node, Schedule, SkillDocument, Task, Tools, Trigger,
};
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use serde_json::json;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn canonical_manage_document_saves_refresh_store() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    )
    .await?;
    let node_did = "did:test:amy";

    let mut node: Node = serde_json::from_value(json!({
        "node_did": node_did,
        "display_name": "Amy",
        "tags": ["managed"]
    }))?;
    core.save_node(&node).await?;

    let backend: InferenceBackend = serde_json::from_value(json!({
        "node_did": node_did,
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
        "node_did": node_did,
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
        "node_did": node_did,
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
        "node_did": node_did,
        "skill_id": "amy-skill",
        "name": "Amy Skill",
        "instructions": "Inspect the queue.",
        "tool_refs": ["read_file"],
        "tags": ["managed"]
    }))?;
    core.save_skill(&skill).await?;

    let agent: Agent = serde_json::from_value(json!({
        "node_did": node_did,
        "agent_did": "amy-default",
        "display_name": "Amy Default",
        "inference_profile_id": "profile-amy",
        "tags": ["managed"]
    }))?;
    core.save_agent(&agent).await?;
    node.default_agent_did = Some("amy-default".into());
    core.save_node(&node).await?;

    let task: Task = serde_json::from_value(json!({
        "node_did": node_did,
        "task_id": "task-amy-daily",
        "display_name": "Daily Amy",
        "agent_did": "amy-default",
        "prompt_template": "Check the daily queue.",
        "tags": ["managed"]
    }))?;
    core.save_task(&task).await?;

    let schedule: Schedule = serde_json::from_value(json!({
        "node_did": node_did,
        "schedule_id": "schedule-amy-daily",
        "display_name": "Daily cadence",
        "cadence": {"kind": "interval", "interval_secs": 300},
        "tags": ["managed"]
    }))?;
    core.save_schedule(&schedule).await?;

    let trigger: Trigger = serde_json::from_value(json!({
        "node_did": node_did,
        "trigger_id": "trigger-amy-daily",
        "task_id": "task-amy-daily",
        "source": {"kind": "schedule", "schedule_id": "schedule-amy-daily"},
        "tags": ["managed"]
    }))?;
    core.save_trigger(&trigger).await?;

    let snapshot = core.store().snapshot();
    assert!(snapshot
        .nodes
        .iter()
        .any(|row| { row.node_did == node_did && row.tags == ["managed"] }));
    assert!(snapshot
        .inference_backends
        .iter()
        .any(|row| { row.node_did == node_did && row.backend_id == "backend-amy" }));
    assert!(snapshot.inference_profiles.iter().any(|row| {
        row.node_did == node_did
            && row.profile_id == "profile-amy"
            && row.backend_id == "backend-amy"
    }));
    assert!(snapshot
        .tools
        .iter()
        .any(|row| { row.node_did == node_did && row.tools_id == "tools-amy" }));
    assert!(snapshot.skills.iter().any(|row| {
        row.node_did == node_did && row.skill_id == "amy-skill" && row.tool_refs == ["read_file"]
    }));
    assert!(snapshot.agents.iter().any(|row| {
        row.node_did == node_did
            && row.agent_did == "amy-default"
            && row.inference_profile_id == "profile-amy"
    }));
    assert!(snapshot.tasks.iter().any(|row| {
        row.node_did == node_did
            && row.task_id == "task-amy-daily"
            && row.agent_did == "amy-default"
    }));
    assert!(snapshot
        .schedules
        .iter()
        .any(|row| { row.node_did == node_did && row.schedule_id == "schedule-amy-daily" }));
    assert!(snapshot.triggers.iter().any(|row| {
        row.node_did == node_did
            && row.trigger_id == "trigger-amy-daily"
            && row.task_id == "task-amy-daily"
    }));

    core.delete_skill("amy-skill", node_did).await?;
    assert!(!core
        .store()
        .snapshot()
        .skills
        .iter()
        .any(|row| row.node_did == node_did && row.skill_id == "amy-skill"));
    Arc::new(core).shutdown().await?;
    Ok(())
}
