//! End-to-end lifecycle for the self-configuration tool family (#654):
//! seeded config documents, tools built exactly as the runtime surface builds
//! them, calls through the dyn tool boundary, and assertions on the stored
//! documents — identity-scoped transactional writes included.

use std::sync::Arc;

use defra_agent::self_config::build_self_config_tools;
use defra_agent::tool_surface::SelfConfigToolConfig;
use defra_agent::{load_agent_behavior, load_tool_selection, ToolSelectionDocument};
use defra_node::EmbeddedNode;
use serde_json::{json, Value};

use crate::support::test_db;

const AGENT_DID: &str = "did:key:zSelfConfigE2E";
const BEHAVIOR_ID: &str = "self-config-behavior";
const SELECTION_ID: &str = "self-config-selection";
const PROFILE_ID: &str = "self-config-profile";
const BACKEND_ID: &str = "self-config-backend";

async fn seed_config(node: &Arc<EmbeddedNode>) {
    let behavior_mutation = format!(
        r#"mutation {{
            create_AgentBehavior(input: {{
                behavior_id: "{BEHAVIOR_ID}",
                agent_did: "{AGENT_DID}",
                system_prompt: "original prompt",
                model_name: "model-small",
                backend_id: "{BACKEND_ID}",
                tool_selection_id: "{SELECTION_ID}",
                inference_profile_id: "{PROFILE_ID}",
                enabled: true,
                created_at: "2026-01-01T00:00:00Z"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&behavior_mutation).await;
    assert!(
        !response.has_errors(),
        "seed behavior: {:?}",
        response.errors
    );

    defra_agent::document_config::upsert_tool_selection(
        node,
        &ToolSelectionDocument {
            selection_id: SELECTION_ID.to_string(),
            agent_did: AGENT_DID.to_string(),
            enable_self_config: Some(true),
            enable_defra_query: Some(false),
            ..Default::default()
        },
    )
    .await
    .expect("seed selection");

    let profile_mutation = format!(
        r#"mutation {{
            create_InferenceProfile(input: {{
                profile_id: "{PROFILE_ID}",
                temperature: 0.7,
                max_turns: 40
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&profile_mutation).await;
    assert!(
        !response.has_errors(),
        "seed profile: {:?}",
        response.errors
    );

    let backend_mutation = format!(
        r#"mutation {{
            create_InferenceBackend(input: {{
                backend_id: "{BACKEND_ID}",
                name: "local",
                provider_kind: "OpenAiCompatible",
                endpoint: "http://127.0.0.1:11434/v1",
                api_key: "sk-secret-should-never-leak",
                enabled: true,
                models: ["model-small", "model-large"],
                probe_status: "healthy"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&backend_mutation).await;
    assert!(
        !response.has_errors(),
        "seed backend: {:?}",
        response.errors
    );
}

fn tool_config(categories: &[&str], no_lockout: bool, dry_run: bool) -> SelfConfigToolConfig {
    SelfConfigToolConfig {
        enabled: true,
        behavior_id: BEHAVIOR_ID.to_string(),
        categories: categories
            .iter()
            .map(|category| category.to_string())
            .collect(),
        no_lockout,
        dry_run,
    }
}

async fn call_tool(
    tools: &[Box<dyn defra_agent::llm::tool::ToolDyn>],
    name: &str,
    args: Value,
) -> Result<String, String> {
    let tool = tools
        .iter()
        .find(|tool| tool.name() == name)
        .unwrap_or_else(|| panic!("tool {name} not registered"));
    tool.call(args.to_string())
        .await
        .map_err(|error| format!("{error:#}"))
}

#[tokio::test]
async fn configure_behavior_patches_and_rejects_wholesale() {
    let db = test_db("self-config-behavior").await;
    seed_config(&db.node).await;
    let tools = build_self_config_tools(
        db.node.clone(),
        AGENT_DID.to_string(),
        &tool_config(&["behavior"], false, false),
    );

    // Happy path: prompt + model patch lands.
    let output = call_tool(
        &tools,
        "configure_behavior",
        json!({ "patch": { "system_prompt": "sharper prompt", "model_name": "model-large" } }),
    )
    .await
    .expect("behavior patch should commit");
    assert!(output.contains("\"committed\": true"), "{output}");

    let behavior = load_agent_behavior(&db.node, BEHAVIOR_ID)
        .await
        .expect("load behavior")
        .expect("behavior exists");
    assert_eq!(behavior.system_prompt.as_deref(), Some("sharper prompt"));
    assert_eq!(behavior.model_name.as_deref(), Some("model-large"));
    assert_eq!(behavior.agent_did, AGENT_DID, "identity untouched");

    // Identity fields are inadmissible; nothing may change.
    let error = call_tool(
        &tools,
        "configure_behavior",
        json!({ "patch": { "agent_did": "did:key:zAttacker", "system_prompt": "hijacked" } }),
    )
    .await
    .expect_err("identity patch must be rejected");
    assert!(error.contains("protected"), "{error}");
    let behavior = load_agent_behavior(&db.node, BEHAVIOR_ID)
        .await
        .expect("load behavior")
        .expect("behavior exists");
    assert_eq!(
        behavior.system_prompt.as_deref(),
        Some("sharper prompt"),
        "rejected patch must leave every field unchanged (transactional totality)"
    );

    // Dangling reference fails validation and aborts wholesale.
    let error = call_tool(
        &tools,
        "configure_behavior",
        json!({ "patch": { "backend_id": "missing-backend", "system_prompt": "half applied?" } }),
    )
    .await
    .expect_err("dangling backend_id must be rejected");
    assert!(error.contains("does not exist"), "{error}");
    let behavior = load_agent_behavior(&db.node, BEHAVIOR_ID)
        .await
        .expect("load behavior")
        .expect("behavior exists");
    assert_eq!(behavior.backend_id.as_deref(), Some(BACKEND_ID));
    assert_eq!(behavior.system_prompt.as_deref(), Some("sharper prompt"));
}

#[tokio::test]
async fn configure_tools_respects_gate_and_no_lockout() {
    let db = test_db("self-config-tools").await;
    seed_config(&db.node).await;

    // Guarded surface: disabling the gate is refused.
    let guarded = build_self_config_tools(
        db.node.clone(),
        AGENT_DID.to_string(),
        &tool_config(&["tools"], true, false),
    );
    let output = call_tool(
        &guarded,
        "configure_tools",
        json!({ "patch": { "enable_defra_query": true } }),
    )
    .await
    .expect("unrelated selection patch commits under the guard");
    assert!(output.contains("\"committed\": true"), "{output}");
    let error = call_tool(
        &guarded,
        "configure_tools",
        json!({ "patch": { "enable_self_config": false } }),
    )
    .await
    .expect_err("no-lockout guard must refuse gate removal");
    assert!(error.contains("no-lockout"), "{error}");

    // Operator/apply-managed fields stay protected.
    let error = call_tool(
        &guarded,
        "configure_tools",
        json!({ "patch": { "tool_policy_version": "v2" } }),
    )
    .await
    .expect_err("tool_policy_version is protected");
    assert!(error.contains("protected"), "{error}");

    let selection = load_tool_selection(&db.node, SELECTION_ID)
        .await
        .expect("load selection")
        .expect("selection exists");
    assert_eq!(selection.enable_defra_query, Some(true));
    assert_eq!(selection.enable_self_config, Some(true));

    // Unguarded surface: the agent may deliberately turn its own gate off.
    let unguarded = build_self_config_tools(
        db.node.clone(),
        AGENT_DID.to_string(),
        &tool_config(&["tools"], false, false),
    );
    call_tool(
        &unguarded,
        "configure_tools",
        json!({ "patch": { "enable_self_config": false } }),
    )
    .await
    .expect("without the guard, self-disable is a legal one-way door");
    let selection = load_tool_selection(&db.node, SELECTION_ID)
        .await
        .expect("load selection")
        .expect("selection exists");
    assert_eq!(selection.enable_self_config, Some(false));
}

#[tokio::test]
async fn get_my_config_reports_documents_and_never_the_api_key() {
    let db = test_db("self-config-read").await;
    seed_config(&db.node).await;
    let tools = build_self_config_tools(
        db.node.clone(),
        AGENT_DID.to_string(),
        &tool_config(&["behavior", "tools", "profile", "backend"], false, true),
    );

    let output = call_tool(&tools, "get_my_config", json!({}))
        .await
        .expect("read succeeds");
    let config: Value = serde_json::from_str(&output).expect("json output");
    assert_eq!(config["agent_did"], AGENT_DID);
    assert_eq!(config["behavior"]["behavior_id"], BEHAVIOR_ID);
    assert_eq!(config["tool_selection"]["selection_id"], SELECTION_ID);
    assert_eq!(config["inference_profile"]["profile_id"], PROFILE_ID);
    assert_eq!(config["inference_backend"]["backend_id"], BACKEND_ID);
    assert!(
        !output.contains("sk-secret-should-never-leak") && !output.contains("api_key"),
        "the backend secret must never round-trip through get_my_config"
    );

    // Dry-run preview returns the diff without committing.
    let preview = call_tool(
        &tools,
        "get_my_config",
        json!({ "preview": {
            "category": "behavior",
            "patch": { "system_prompt": "previewed prompt" },
        }}),
    )
    .await
    .expect("preview succeeds");
    assert!(preview.contains("previewed prompt"), "{preview}");
    assert!(preview.contains("dry-run"), "{preview}");
    let behavior = load_agent_behavior(&db.node, BEHAVIOR_ID)
        .await
        .expect("load behavior")
        .expect("behavior exists");
    assert_eq!(
        behavior.system_prompt.as_deref(),
        Some("original prompt"),
        "preview must not write"
    );
}

#[tokio::test]
async fn configure_automation_creates_owned_chain_and_rejects_foreign_tasks() {
    let db = test_db("self-config-automation").await;
    seed_config(&db.node).await;
    let tools = build_self_config_tools(
        db.node.clone(),
        AGENT_DID.to_string(),
        &tool_config(&["automation"], false, false),
    );

    let output = call_tool(
        &tools,
        "configure_automation",
        json!({ "kind": "task", "id": "nightly-review", "patch": {
            "name": "Nightly review",
            "prompt_template": "Review yesterday's sessions",
        }}),
    )
    .await
    .expect("task create commits");
    assert!(output.contains("\"created\": true"), "{output}");

    call_tool(
        &tools,
        "configure_automation",
        json!({ "kind": "schedule", "id": "nightly-review-cron", "patch": {
            "task_id": "nightly-review",
            "cron": "0 3 * * *",
            "timezone": "UTC",
        }}),
    )
    .await
    .expect("schedule create commits");

    // Runtime bookkeeping fields are protected.
    let error = call_tool(
        &tools,
        "configure_automation",
        json!({ "kind": "schedule", "id": "nightly-review-cron", "patch": {
            "fire_count": 0,
        }}),
    )
    .await
    .expect_err("runtime-owned schedule fields are protected");
    assert!(error.contains("protected"), "{error}");

    // A schedule may not point at another behavior's task.
    let foreign_task = r#"mutation {
        create_Task(input: {
            task_id: "foreign-task",
            behavior_id: "someone-elses-behavior",
            enabled: true
        }) { _docID }
    }"#;
    let response = db.node.execute(foreign_task).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let error = call_tool(
        &tools,
        "configure_automation",
        json!({ "kind": "schedule", "id": "foreign-schedule", "patch": {
            "task_id": "foreign-task",
            "interval_secs": 60,
        }}),
    )
    .await
    .expect_err("cross-behavior automation must be rejected");
    assert!(error.contains("owned"), "{error}");

    // Task ownership link is immutable via patch.
    let error = call_tool(
        &tools,
        "configure_automation",
        json!({ "kind": "task", "id": "nightly-review", "patch": {
            "behavior_id": "someone-elses-behavior",
        }}),
    )
    .await
    .expect_err("behavior_id is the pinned ownership link");
    assert!(error.contains("protected"), "{error}");
}

#[tokio::test]
async fn writes_carry_the_agent_identity() {
    // Every self-config statement must execute under the agent DID (the ACP
    // actor), not anonymously: a non-`did:key` identity therefore fails
    // closed instead of falling back to a root write.
    let db = test_db("self-config-identity").await;
    seed_config(&db.node).await;

    let tools = build_self_config_tools(
        db.node.clone(),
        "not-a-did".to_string(),
        &tool_config(&["behavior"], false, false),
    );
    let error = call_tool(
        &tools,
        "configure_behavior",
        json!({ "patch": { "system_prompt": "should not land" } }),
    )
    .await
    .expect_err("a non-did identity must not silently write as node root");
    assert!(error.contains("ACP-addressable"), "{error}");

    let behavior = load_agent_behavior(&db.node, BEHAVIOR_ID)
        .await
        .expect("load behavior")
        .expect("behavior exists");
    assert_eq!(behavior.system_prompt.as_deref(), Some("original prompt"));
}
