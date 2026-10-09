use anyhow::Result;
use gents::document_config::{
    Agent, InferenceBackend, InferenceProfile, Node, Schedule, SkillDocument, Task, Tools, Trigger,
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
        "agent_id": "amy-default",
        "display_name": "Amy Default",
        "inference_profile_id": "profile-amy",
        "tags": ["managed"]
    }))?;
    core.save_agent(&agent).await?;
    node.default_agent_id = Some("amy-default".into());
    core.save_node(&node).await?;

    let task: Task = serde_json::from_value(json!({
        "node_did": node_did,
        "task_id": "task-amy-daily",
        "display_name": "Daily Amy",
        "agent_id": "amy-default",
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
            && row.agent_id == "amy-default"
            && row.inference_profile_id == "profile-amy"
    }));
    assert!(snapshot.tasks.iter().any(|row| {
        row.node_did == node_did && row.task_id == "task-amy-daily" && row.agent_id == "amy-default"
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
    core.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn desktop_task_run_defers_declared_goal_until_claim() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    )
    .await?;
    let node_did = core.node_identity().did().to_string();

    let mut node: Node = serde_json::from_value(json!({"node_did": node_did}))?;
    core.save_node(&node).await?;
    let backend: InferenceBackend = serde_json::from_value(json!({
        "node_did": node_did,
        "backend_id": "local",
        "name": "Local",
        "provider_kind": "OpenAiCompatible",
        "endpoint": "http://localhost:8000/v1",
        "auth": {"kind": "unauthenticated"}
    }))?;
    core.save_backend(&backend).await?;
    let profile: InferenceProfile = serde_json::from_value(json!({
        "node_did": node_did,
        "profile_id": "durable-profile",
        "backend_id": "local",
        "model_name": "model"
    }))?;
    core.save_inference_profile(&profile).await?;
    let agent: Agent = serde_json::from_value(json!({
        "node_did": node_did,
        "agent_id": "durable",
        "inference_profile_id": "durable-profile"
    }))?;
    core.save_agent(&agent).await?;
    node.default_agent_id = Some("durable".into());
    core.save_node(&node).await?;

    let task: Task = serde_json::from_value(json!({
        "node_did": node_did,
        "task_id": "durable-task",
        "display_name": "Durable task",
        "agent_id": "durable",
        "prompt_template": "Handle {{ args.item }}",
        "goal_objective_template": "Finish {{ args.item }}",
        "goal_token_budget": 10000
    }))?;
    core.save_task(&task).await?;
    let request_doc_id = core
        .fire_task_now(&task, json!({"item": "release"}))
        .await?;

    let request_doc_id = gents::graphql::escape_graphql_string(&request_doc_id);
    let owner = gents::graphql::escape_graphql_string(&node_did);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ _docID: {{ _eq: "{request_doc_id}" }} }} ) {{
                _docID request_id purpose node_did requester_did agent_id
                session_id retry_key content input lifecycle_state created_at execution_origin
            }}
            Goal(filter: {{ node_did: {{ _eq: "{owner}" }} }}) {{
                session_id objective status token_budget
            }}
            TriggerFire(filter: {{ owner_did: {{ _eq: "{owner}" }} }}) {{
                goal_assignment_applied goal_objective goal_token_budget
            }}
        }}"#,
    );
    let response = gents::graphql::graphql_with_transaction_retry(
        core.node(),
        &query,
        "test.desktop_task_before_claim",
    )
    .await?;
    let data = response.data.as_ref().expect("query data");
    let requests = data["AgentRequest"].as_array().expect("request rows");
    assert_eq!(requests.len(), 1);
    assert!(data["Goal"].as_array().unwrap().is_empty());
    assert_eq!(requests[0]["content"], "Handle release");
    assert_eq!(requests[0]["lifecycle_state"], "pending");
    assert!(requests[0]["retry_key"].as_str().is_some());
    let fires = data["TriggerFire"].as_array().unwrap();
    assert_eq!(fires.len(), 1);
    assert_eq!(fires[0]["goal_assignment_applied"], false);
    assert_eq!(fires[0]["goal_objective"], "Finish release");
    assert_eq!(fires[0]["goal_token_budget"], 10_000);
    let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(requests[0].clone())?;
    let request = gents::watcher::AgentRequest::try_from(row)?;
    let session_id = request.session_id.clone();
    let mut lifecycle = gents::lifecycle::RequestLifecycle::new_with_execution_binding(
        core.node_arc(),
        "durable",
        &node_did,
        request,
        60,
        gents::lifecycle::ExecutionOrigin::Interactive,
        "local",
    );
    assert_eq!(
        lifecycle.claim().await?,
        gents::lifecycle::ClaimOutcome::Claimed
    );
    let response = gents::graphql::graphql_with_transaction_retry(
        core.node(),
        &query,
        "test.desktop_task_after_claim",
    )
    .await?;
    let data = response.data.as_ref().expect("query data");
    assert_eq!(data["AgentRequest"][0]["lifecycle_state"], "claimed");
    assert_eq!(data["TriggerFire"][0]["goal_assignment_applied"], true);
    let goals = data["Goal"].as_array().expect("goal rows");
    assert_eq!(goals.len(), 1);
    assert_eq!(goals[0]["session_id"], session_id);
    assert_eq!(goals[0]["objective"], "Finish release");
    assert_eq!(goals[0]["status"], "active");
    assert_eq!(goals[0]["token_budget"], 10_000);
    drop(lifecycle);

    core.shutdown().await?;
    Ok(())
}
