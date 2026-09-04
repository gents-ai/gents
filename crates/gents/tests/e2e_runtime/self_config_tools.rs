use std::sync::Arc;

use defra_node::EmbeddedNode;
use gents::self_config::build_self_config_tools;
use gents::tool_surface::SelfConfigToolConfig;
use gents::{load_agent_behavior, load_tool_selection, ToolSelectionDocument};
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

    gents::document_config::upsert_tool_selection(
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
                max_concurrent: 1,
                max_queue_depth: 1,
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
    tools: &[Box<dyn gents::llm::tool::ToolDyn>],
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
        None,
        &tool_config(&["behavior"], false, false),
    );

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

    let error = call_tool(
        &tools,
        "configure_behavior",
        json!({ "patch": { "backend_id": "missing-backend", "system_prompt": "half applied?" } }),
    )
    .await
    .expect_err("dangling backend_id must be rejected");
    // `AgentBehavior::validate_references` (#1331, the single owner) phrases
    // this as "references missing backend_id", not "does not exist".
    assert!(error.contains("references missing backend_id"), "{error}");
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

    let guarded = build_self_config_tools(
        db.node.clone(),
        AGENT_DID.to_string(),
        None,
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

    let unguarded = build_self_config_tools(
        db.node.clone(),
        AGENT_DID.to_string(),
        None,
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
        None,
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

/// A patch value is a scalar in the model (`FieldValue := String` in
/// `proofs/Proofs/SelfConfig/Apply.lean`), but the tool schema accepts
/// arbitrary JSON. An object value used to reach the mutation renderer,
/// whose object keys land in identifier position — letting a patch on one
/// writable field write a protected field, or a document in another
/// collection entirely. Both must be refused before anything commits.
#[tokio::test]
async fn configure_rejects_non_scalar_patch_values() {
    let db = test_db("self-config-nonscalar").await;
    seed_config(&db.node).await;
    let tools = build_self_config_tools(
        db.node.clone(),
        AGENT_DID.to_string(),
        None,
        &tool_config(&["backend", "automation"], false, false),
    );

    let error = call_tool(
        &tools,
        "configure_backend",
        json!({ "patch": { "endpoint": {
            r#"x: 1 }, api_key: "leaked-by-injection", endpoint: "http://injected/v1""#: 1
        }}}),
    )
    .await
    .expect_err("an object-valued patch must be refused");
    assert!(
        error.contains("scalar") || error.contains("identifier"),
        "rejection should name the value-shape rule: {error}"
    );

    let stored = db
        .node
        .execute(
            r#"query { InferenceBackend(filter: { backend_id: { _eq: "self-config-backend" } }) { endpoint api_key } }"#,
        )
        .await;
    let row = stored
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceBackend"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .expect("backend row");
    assert_eq!(
        row["endpoint"], "http://127.0.0.1:11434/v1",
        "the injected endpoint must not have landed"
    );
    assert_eq!(
        row["api_key"], "sk-secret-should-never-leak",
        "a patch on a writable field must not reach the protected api_key"
    );

    call_tool(
        &tools,
        "configure_automation",
        json!({ "kind": "task", "id": "nonscalar-task", "patch": { "enabled": true } }),
    )
    .await
    .expect("task create commits");
    let error = call_tool(
        &tools,
        "configure_automation",
        json!({ "kind": "event_trigger", "id": "nonscalar-trigger", "patch": {
            "task_id": "nonscalar-task",
            "source_collection": "CustomerSignup",
            "event_kind": "created",
            "filter": {
                r#"x: 1 }) { _docID } create_AgentBehavior(input: { behavior_id: "evil-injected", agent_did: "did:key:zAttacker" }) { _docID } #"#: 1
            },
        }}),
    )
    .await
    .expect_err("an object-valued filter must be refused");
    assert!(
        error.contains("scalar") || error.contains("identifier"),
        "rejection should name the value-shape rule: {error}"
    );

    let forged = db
        .node
        .execute(r#"query { AgentBehavior(filter: { behavior_id: { _eq: "evil-injected" } }) { behavior_id } }"#)
        .await;
    let rows = forged
        .data
        .as_ref()
        .and_then(|data| data.get("AgentBehavior"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        rows.is_empty(),
        "no document may be forged in another collection: {rows:?}"
    );
}

#[tokio::test]
async fn configure_backend_rejects_env_var_when_a_secret_key_is_stored() {
    let db = test_db("self-config-backend-key-xor").await;
    seed_config(&db.node).await;
    let tools = build_self_config_tools(
        db.node.clone(),
        AGENT_DID.to_string(),
        None,
        &tool_config(&["backend"], false, false),
    );

    let error = call_tool(
        &tools,
        "configure_backend",
        json!({ "patch": { "api_key_env_var": "GENTS_TEST_API_KEY" } }),
    )
    .await
    .expect_err("a stored api_key and api_key_env_var must remain mutually exclusive");
    assert!(
        error.contains("must not set both api_key and api_key_env_var"),
        "{error}"
    );

    let response = db
        .node
        .execute(&format!(
            r#"{{ InferenceBackend(filter: {{ backend_id: {{ _eq: "{BACKEND_ID}" }} }}) {{ api_key_env_var }} }}"#
        ))
        .await;
    let api_key_env_var = response
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceBackend"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("api_key_env_var"));
    assert!(
        api_key_env_var.is_none_or(Value::is_null),
        "rejected patch must not commit: {response:?}"
    );
}

/// `filter` is spliced into the trigger engine's probe as a whole object
/// fragment. The confirmed #1038 payload closes the enclosing `_and: [ ... ]`
/// and appends its own selections; it must not be writable.
#[tokio::test]
async fn configure_automation_rejects_break_out_filter_fragments() {
    let db = test_db("self-config-filter-injection").await;
    seed_config(&db.node).await;
    let tools = build_self_config_tools(
        db.node.clone(),
        AGENT_DID.to_string(),
        None,
        &tool_config(&["automation"], false, false),
    );

    call_tool(
        &tools,
        "configure_automation",
        json!({ "kind": "task", "id": "filter-task", "patch": { "enabled": true } }),
    )
    .await
    .expect("task create commits");

    for hostile in [
        r#"{} ] }, limit: 1) { _docID } AgentBehavior(filter: { _and: [ {} ] }, limit: 1) { system_prompt } X(filter: { _and: [ {}"#,
        "{ a: 1 } # ",
        "{ a: 1 }) { x } (",
    ] {
        let Err(error) = call_tool(
            &tools,
            "configure_automation",
            json!({ "kind": "event_trigger", "id": "filter-trigger", "patch": {
                "task_id": "filter-task",
                "source_collection": "CustomerSignup",
                "event_kind": "created",
                "filter": hostile,
            }}),
        )
        .await
        else {
            panic!("break-out filter {hostile:?} must be rejected");
        };
        assert!(
            error.contains("filter"),
            "rejection should name the filter rule: {error}"
        );
    }

    let output = call_tool(
        &tools,
        "configure_automation",
        json!({ "kind": "event_trigger", "id": "filter-trigger", "patch": {
            "task_id": "filter-task",
            "source_collection": "CustomerSignup",
            "event_kind": "created",
            "filter": r#"{ kind: { _eq: "signup" } }"#,
        }}),
    )
    .await
    .expect("a well-formed filter still commits");
    assert!(output.contains("\"created\": true"), "{output}");
}

#[tokio::test]
async fn configure_automation_creates_owned_chain_and_rejects_foreign_tasks() {
    let db = test_db("self-config-automation").await;
    seed_config(&db.node).await;
    let tools = build_self_config_tools(
        db.node.clone(),
        AGENT_DID.to_string(),
        None,
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

/// `source_collection` is agent-writable and later interpolated into GraphQL
/// identifier positions by the trigger engine, where escaping cannot apply.
/// The self-config apply path is the trust boundary: it must reject any
/// value that is not a valid GraphQL collection identifier, so a principal
/// cannot shape the queries the runtime issues.
#[tokio::test]
async fn configure_automation_rejects_injection_shaped_source_collection() {
    let db = test_db("self-config-trigger-injection").await;
    seed_config(&db.node).await;
    let tools = build_self_config_tools(
        db.node.clone(),
        AGENT_DID.to_string(),
        None,
        &tool_config(&["automation"], false, false),
    );

    call_tool(
        &tools,
        "configure_automation",
        json!({ "kind": "task", "id": "watcher-task", "patch": {
            "name": "Watcher",
            "prompt_template": "React to new docs",
        }}),
    )
    .await
    .expect("task create commits");

    for hostile in [
        "Msg(limit: 1) { _docID } Foo",
        "AgentResponse { content } #",
        "__Type",
        "a b",
        "naïve",
    ] {
        let Err(error) = call_tool(
            &tools,
            "configure_automation",
            json!({ "kind": "event_trigger", "id": "watcher-trigger", "patch": {
                "task_id": "watcher-task",
                "source_collection": hostile,
                "event_kind": "created",
                "concurrency": "serial",
            }}),
        )
        .await
        else {
            panic!("injection-shaped source_collection {hostile:?} must be rejected");
        };
        assert!(
            error.contains("identifier") || error.contains("collection"),
            "rejection for {hostile:?} should name the identifier rule: {error}"
        );
    }

    let output = call_tool(
        &tools,
        "configure_automation",
        json!({ "kind": "event_trigger", "id": "watcher-trigger", "patch": {
            "task_id": "watcher-task",
            "source_collection": "CustomerSignup",
            "event_kind": "created",
            "concurrency": "serial",
        }}),
    )
    .await
    .expect("a grammar-valid source_collection commits");
    assert!(output.contains("\"created\": true"), "{output}");

    // Patching an existing trigger is the realistic attack shape — commit a
    // benign one, then flip the field. Create and patch share a branch today
    // because the check reads the merged doc; this keeps them from drifting.
    let error = call_tool(
        &tools,
        "configure_automation",
        json!({ "kind": "event_trigger", "id": "watcher-trigger", "patch": {
            "source_collection": "Msg(limit: 1) { _docID } Foo",
        }}),
    )
    .await
    .expect_err("patching source_collection to a hostile value must be rejected");
    assert!(
        error.contains("identifier") || error.contains("collection"),
        "rejection should name the identifier rule: {error}"
    );

    let stored = db
        .node
        .execute(
            r#"query { EventTrigger(filter: { trigger_id: { _eq: "watcher-trigger" } }) { source_collection } }"#,
        )
        .await;
    let row = stored
        .data
        .as_ref()
        .and_then(|data| data.get("EventTrigger"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .expect("trigger row");
    assert_eq!(
        row["source_collection"], "CustomerSignup",
        "the rejected patch must not have landed"
    );
}

#[tokio::test]
async fn writes_carry_the_agent_identity() {
    let db = test_db("self-config-identity").await;
    seed_config(&db.node).await;

    let tools = build_self_config_tools(
        db.node.clone(),
        "not-a-did".to_string(),
        None,
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

#[tokio::test]
async fn self_only_boundaries_hold_across_behaviors_and_agents() {
    let db = test_db("self-config-boundaries").await;
    seed_config(&db.node).await;

    for mutation in [
        r#"mutation { create_Task(input: {
            task_id: "victim-task", behavior_id: "victim-behavior", enabled: true
        }) { _docID } }"#,
        r#"mutation { create_Schedule(input: {
            schedule_id: "victim-schedule", task_id: "victim-task",
            interval_secs: 300, enabled: true
        }) { _docID } }"#,
        r#"mutation { create_ToolSelection(input: {
            selection_id: "victim-selection", agent_did: "did:key:zVictim",
            enable_bash: false
        }) { _docID } }"#,
    ] {
        let response = db.node.execute(mutation).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
    }

    let tools = build_self_config_tools(
        db.node.clone(),
        AGENT_DID.to_string(),
        None,
        &tool_config(&["behavior", "tools", "automation"], false, false),
    );

    call_tool(
        &tools,
        "configure_automation",
        json!({ "kind": "task", "id": "my-task", "patch": { "enabled": true } }),
    )
    .await
    .expect("own task create commits");
    let error = call_tool(
        &tools,
        "configure_automation",
        json!({ "kind": "schedule", "id": "victim-schedule", "patch": {
            "task_id": "my-task", "enabled": false,
        }}),
    )
    .await
    .expect_err("re-pointing a foreign schedule must be rejected");
    assert!(error.contains("not owned by this behavior"), "{error}");

    let error = call_tool(
        &tools,
        "configure_behavior",
        json!({ "patch": { "tool_selection_id": "victim-selection" } }),
    )
    .await
    .expect_err("binding a foreign selection must be rejected");
    assert!(error.contains("self only"), "{error}");

    let rebind = format!(
        r#"mutation {{
            update_AgentBehavior(filter: {{ behavior_id: {{ _eq: "{BEHAVIOR_ID}" }} }},
                input: {{ tool_selection_id: "victim-selection" }}) {{ _docID }}
        }}"#
    );
    let response = db.node.execute(&rebind).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let error = call_tool(
        &tools,
        "configure_tools",
        json!({ "patch": { "enable_bash": true } }),
    )
    .await
    .expect_err("patching a foreign selection must be rejected");
    assert!(error.contains("self only"), "{error}");
}
