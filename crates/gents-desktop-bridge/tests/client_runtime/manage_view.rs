use anyhow::Result;
use gents_desktop_bridge::client_runtime::ClientRuntime;
use gents_documents::{
    Agent, InferenceBackend, InferenceProfile, Node, Schedule, SkillDocument, Task, Tools, Trigger,
};
use gents_store::client_journal::paths::DesktopPaths;
use serde_json::json;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn canonical_manage_document_saves_refresh_store() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let core = ClientRuntime::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        gents_desktop_bridge::client_test_support::file_journal_options(),
    )
    .await?;
    // The desktop's client key is the runtime's User: it creates the Node,
    // provisions the Agent and saves every document over its signed endpoint.
    let runtime = gents::client_test_support::ServedRuntimeHome::start_unpublished_for(Arc::new(
        core.node_identity().clone(),
    ))
    .await;
    runtime.publish_node(serde_json::json!({})).await;
    gents_desktop_bridge::client_test_support::connect_runtime(&runtime, &core).await;
    let node_did = runtime.node_did();

    let node: Node = serde_json::from_value(json!({
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

    let amy = core.provision_agent_identity(node_did).await?;
    let amy_default = amy.agent_did.as_str();
    let mut agent = runtime.agent_value_for_create(&amy, "profile-amy").await;
    agent["display_name"] = json!("Amy Default");
    agent["tags"] = json!(["managed"]);
    let agent: Agent = serde_json::from_value(agent)?;
    core.save_agent(&agent, None).await?;
    core.set_default_agent(node_did, amy_default).await?;

    let task: Task = serde_json::from_value(json!({
        "node_did": node_did,
        "task_id": "task-amy-daily",
        "display_name": "Daily Amy",
        "agent_did": amy_default,
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

    let mut publications = core.stores().subscribe();
    let trigger: Trigger = serde_json::from_value(json!({
        "node_did": node_did,
        "trigger_id": "trigger-amy-daily",
        "task_id": "task-amy-daily",
        "source": {"kind": "schedule", "schedule_id": "schedule-amy-daily"},
        "tags": ["managed"]
    }))?;
    core.save_trigger(&trigger).await?;

    loop {
        let snapshot = core.stores().snapshot(node_did);
        if snapshot
            .triggers
            .iter()
            .any(|row| row.node_did == node_did && row.trigger_id == "trigger-amy-daily")
        {
            break;
        }
        publications.changed().await?;
    }
    let snapshot = core.stores().snapshot(node_did);
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
            && row.agent_did == amy_default
            && row.inference_profile_id == "profile-amy"
    }));
    assert!(snapshot.tasks.iter().any(|row| {
        row.node_did == node_did && row.task_id == "task-amy-daily" && row.agent_did == amy_default
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
        .stores()
        .snapshot(node_did)
        .skills
        .iter()
        .any(|row| row.node_did == node_did && row.skill_id == "amy-skill"));
    Arc::new(core).shutdown().await?;
    runtime.shutdown().await;
    Ok(())
}

async fn enroll_desktop_client(
    core: &ClientRuntime,
    runtime: &gents::client_test_support::ServedRuntimeHome,
) -> Result<()> {
    let node_did = runtime.node_did();
    assert_ne!(runtime.user().did(), core.node_identity().did());
    let connection = gents_protocol::runtime_connection::RuntimeConnection {
        node_did: node_did.into(),
        label: "Served Runtime".into(),
        graphql: runtime.graphql().into(),
        local_node_home: Some(runtime.home_path().to_string_lossy().into_owned()),
        access_request: None,
        access_state: None,
    };
    let receipt = core.request_runtime_access(connection).await?;
    let approved = runtime
        .user_endpoint()
        .decide_runtime_access(&gents_protocol::client_access::DecideClientAccess {
            node_did: node_did.into(),
            request_id: receipt.request_id,
            decision: gents_protocol::client_access::ClientAccessDecision::Approve,
        })
        .await?;
    assert_eq!(
        approved.state,
        gents_protocol::client_access::ClientAccessState::Approved
    );
    core.refresh_node(node_did).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enrolled_desktop_saves_agent_with_on_prem_profile() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let core = ClientRuntime::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        gents_desktop_bridge::client_test_support::file_journal_options(),
    )
    .await?;
    let runtime = gents::client_test_support::ServedRuntimeHome::start().await;
    let node_did = runtime.node_did();
    enroll_desktop_client(&core, &runtime).await?;

    let owner_agent_binding = runtime.provision_agent().await;
    runtime
        .apply_as_user(
            "test.enrolled.on_prem_seed",
            vec![
                (
                    gents_store::collection::Collection::InferenceBackend,
                    json!({
                        "node_did": node_did,
                        "backend_id": "local-3",
                        "name": "Local Server",
                        "provider_kind": "OpenAiCompatible",
                        "endpoint": "http://127.0.0.1:11434/v1",
                        "auth": {"kind": "unauthenticated"}
                    }),
                ),
                (
                    gents_store::collection::Collection::InferenceProfile,
                    json!({
                        "node_did": node_did,
                        "profile_id": "profile-local-3",
                        "display_name": "Local 3",
                        "backend_id": "local-3",
                        "model_name": "qwen3"
                    }),
                ),
                (
                    gents_store::collection::Collection::InferenceBackend,
                    json!({
                        "node_did": node_did,
                        "backend_id": "local-keyed",
                        "name": "Keyed On-Prem",
                        "provider_kind": "OpenAiCompatible",
                        "endpoint": "http://127.0.0.1:11436/v1",
                        "auth": {"kind": "api_key", "key": "secret-on-prem-key"}
                    }),
                ),
                (
                    gents_store::collection::Collection::InferenceProfile,
                    json!({
                        "node_did": node_did,
                        "profile_id": "profile-local-keyed",
                        "display_name": "Local Keyed",
                        "backend_id": "local-keyed",
                        "model_name": "qwen3"
                    }),
                ),
                (
                    gents_store::collection::Collection::Tools,
                    json!({
                        "node_did": node_did,
                        "tools_id": "tools-owner",
                        "display_name": "Owner Tools",
                        "host": {"files": {"mode": "ReadOnly"}}
                    }),
                ),
                (
                    gents_store::collection::Collection::AgentContext,
                    json!({
                        "node_did": node_did,
                        "context_id": "ctx-owner",
                        "display_name": "Owner Context",
                        "tools_id": "tools-owner"
                    }),
                ),
                (
                    gents_store::collection::Collection::Agent,
                    json!({
                        "node_did": node_did,
                        "agent_did": owner_agent_binding.agent_did,
                        "creation_nonce": owner_agent_binding.creation_nonce,
                        "display_name": "Owner Agent",
                        "context_id": "ctx-owner",
                        "inference_profile_id": "profile-local-keyed"
                    }),
                ),
            ],
        )
        .await;

    let desktop_backend: InferenceBackend = serde_json::from_value(json!({
        "node_did": node_did,
        "backend_id": "local-desktop",
        "name": "Desktop Local",
        "provider_kind": "OpenAiCompatible",
        "endpoint": "http://127.0.0.1:11435/v1",
        "auth": {"kind": "unauthenticated"}
    }))?;
    core.save_backend(&desktop_backend).await?;
    let desktop_profile: InferenceProfile = serde_json::from_value(json!({
        "node_did": node_did,
        "profile_id": "profile-desktop",
        "display_name": "Desktop Local Profile",
        "backend_id": "local-desktop",
        "model_name": "qwen3"
    }))?;
    core.save_inference_profile(&desktop_profile).await?;

    let binding_save = core.provision_agent_identity(node_did).await?;
    let agent_save: Agent = serde_json::from_value(json!({
        "node_did": node_did,
        "agent_did": binding_save.agent_did,
        "creation_nonce": binding_save.creation_nonce,
        "display_name": "SourceEng",
        "context_id": "ctx-owner",
        "inference_profile_id": "profile-local-3"
    }))?;
    core.save_agent(&agent_save, None).await?;

    let binding_components = core.provision_agent_identity(node_did).await?;
    let pack: gents_documents::PackConfig = serde_json::from_value(json!({
        "node": {"node_did": node_did},
        "contexts": [{
            "node_did": node_did,
            "context_id": "ctx-new-on-prem",
            "display_name": "On-Prem Context",
            "tools_id": "tools-owner"
        }],
        "agents": [{
            "node_did": node_did,
            "agent_did": binding_components.agent_did,
            "creation_nonce": binding_components.creation_nonce,
            "display_name": "SourceEng Components",
            "context_id": "ctx-new-on-prem",
            "inference_profile_id": "profile-local-3"
        }]
    }))?;
    core.apply_config_components(&pack).await?;

    let binding_patch = core.provision_agent_identity(node_did).await?;
    let agent_patch: Agent = serde_json::from_value(json!({
        "node_did": node_did,
        "agent_did": binding_patch.agent_did,
        "creation_nonce": binding_patch.creation_nonce,
        "display_name": "SourceEng Keyed",
        "context_id": "ctx-owner",
        "inference_profile_id": "profile-local-keyed"
    }))?;
    let ctx_patch = gents_protocol::operations::agent::AgentContextPatch {
        context_id: "ctx-owner".into(),
        changes: [(
            "system_prompt".to_owned(),
            serde_json::Value::String("On-prem prompt".into()),
        )]
        .into_iter()
        .collect(),
    };
    core.save_agent(&agent_patch, Some(&ctx_patch)).await?;

    let edited_owner_agent: Agent = serde_json::from_value(json!({
        "node_did": node_did,
        "agent_did": owner_agent_binding.agent_did,
        "creation_nonce": owner_agent_binding.creation_nonce,
        "display_name": "Owner Agent",
        "context_id": "ctx-owner",
        "inference_profile_id": "profile-local-3"
    }))?;
    core.save_agent(&edited_owner_agent, None).await?;

    core.patch_config_components(
        node_did,
        &[(
            gents_documents::patch::SelfConfigTarget::Agent,
            owner_agent_binding.agent_did.clone(),
            vec![(
                "inference_profile_id".to_owned(),
                Some(serde_json::Value::String("profile-desktop".into())),
            )],
        )],
    )
    .await?;

    let snapshot = core.stores().snapshot(node_did);
    for (did, expected_profile) in [
        (&binding_save.agent_did, "profile-local-3"),
        (&binding_components.agent_did, "profile-local-3"),
        (&binding_patch.agent_did, "profile-local-keyed"),
        (&owner_agent_binding.agent_did, "profile-desktop"),
    ] {
        assert!(
            snapshot
                .agents
                .iter()
                .any(|row| row.agent_did == *did && row.inference_profile_id == expected_profile),
            "expected agent {did} with profile {expected_profile} in desktop snapshot"
        );
    }

    // Deleting a backend referenced by a profile refuses once and logs to desktop-errors.log.
    let error = core
        .delete_inference_backend("local-desktop", node_did)
        .await
        .expect_err("deleting a backend used by a profile must fail");
    let bridge = gents_store::client_error::BridgeError::classify_transport_error(error);
    assert!(
        bridge.message.contains("profile-desktop") && bridge.message.contains("local-desktop"),
        "expected dependent profile and backend in message: {}",
        bridge.message
    );
    assert_eq!(
        bridge.message.matches("profile-desktop").count(),
        1,
        "delete refusal must not repeat itself: {}",
        bridge.message
    );
    assert!(
        !bridge.message.contains("Review the fields and save again"),
        "delete refusal must not show a save hint: {}",
        bridge.message
    );
    let log = std::fs::read_to_string(gents_desktop_bridge::logging::desktop_error_log(
        tempdir.path(),
    ))?;
    assert!(
        log.contains("delete inference backend failed:") && log.contains("profile-desktop"),
        "expected desktop-errors.log entry: {log}"
    );

    Arc::new(core).shutdown().await?;
    runtime.shutdown().await;
    Ok(())
}
