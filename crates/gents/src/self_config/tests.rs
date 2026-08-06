//! Unit tests for the self-config tool family. End-to-end lifecycle coverage
//! (identity-scoped writes, reconcile pickup) lives in
//! `tests/e2e_runtime/self_config_tools.rs`.

use super::*;

fn config(categories: &[&str]) -> SelfConfigToolConfig {
    SelfConfigToolConfig {
        enabled: true,
        behavior_id: "beh-test".to_string(),
        categories: categories.iter().map(|c| c.to_string()).collect(),
        no_lockout: false,
        dry_run: false,
    }
}

#[test]
fn tool_names_follow_enabled_categories() {
    let names = self_config_tool_names(&config(&["behavior", "tools", "profile"]));
    assert_eq!(
        names,
        vec![
            GET_MY_CONFIG_TOOL_NAME.to_string(),
            CONFIGURE_BEHAVIOR_TOOL_NAME.to_string(),
            CONFIGURE_PROFILE_TOOL_NAME.to_string(),
            CONFIGURE_TOOLS_TOOL_NAME.to_string(),
        ],
        "get_my_config always leads; configure tools follow the sorted category set"
    );

    let disabled = SelfConfigToolConfig::default();
    assert!(self_config_tool_names(&disabled).is_empty());
}

#[test]
fn every_tool_name_is_reserved_builtin() {
    for name in SELF_CONFIG_TOOL_NAMES {
        assert!(
            crate::document_config::is_reserved_builtin_tool_name(name),
            "{name} must be reserved so write_tools declarations cannot shadow it"
        );
    }
}

#[tokio::test]
async fn build_fails_closed_without_agent_did() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let node = defra_node::EmbeddedNode::builder()
        .data_path(tempdir.path().join("data"))
        .build()
        .await
        .expect("node");
    let tools = build_self_config_tools(
        std::sync::Arc::new(node),
        String::new(),
        &config(&["behavior"]),
    );
    assert!(
        tools.is_empty(),
        "an empty agent DID must register no self-config tools"
    );
}

#[tokio::test]
async fn build_registers_gated_family() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let node = defra_node::EmbeddedNode::builder()
        .data_path(tempdir.path().join("data"))
        .build()
        .await
        .expect("node");
    let tools = build_self_config_tools(
        std::sync::Arc::new(node),
        "did:key:zSelfConfigTest".to_string(),
        &config(&["behavior", "backend"]),
    );
    let names: Vec<String> = tools.iter().map(|tool| tool.name()).collect();
    assert_eq!(
        names,
        vec![
            GET_MY_CONFIG_TOOL_NAME.to_string(),
            CONFIGURE_BACKEND_TOOL_NAME.to_string(),
            CONFIGURE_BEHAVIOR_TOOL_NAME.to_string(),
        ]
    );
}

// -- configure_persona (#Task 5) --

async fn build_persona_node() -> std::sync::Arc<defra_node::EmbeddedNode> {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let node = defra_node::EmbeddedNode::builder()
        .data_path(tempdir.path().join("data"))
        .build()
        .await
        .expect("node");
    crate::ensure_runtime_schemas(&node)
        .await
        .expect("runtime schemas register");
    std::sync::Arc::new(node)
}

async fn call_persona_tool(
    tools: &[Box<dyn crate::llm::tool::ToolDyn>],
    args: serde_json::Value,
) -> Result<String, String> {
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIGURE_PERSONA_TOOL_NAME)
        .expect("configure_persona registered");
    tool.call(args.to_string())
        .await
        .map_err(|error| format!("{error:#}"))
}

#[tokio::test]
async fn persona_category_gates_the_tool() {
    let node = build_persona_node().await;

    let without_persona = build_self_config_tools(
        node.clone(),
        "did:key:zGate".to_string(),
        &config(&["behavior"]),
    );
    assert!(
        without_persona
            .iter()
            .all(|tool| tool.name() != CONFIGURE_PERSONA_TOOL_NAME),
        "configure_persona must not register without the persona category"
    );

    let with_persona =
        build_self_config_tools(node, "did:key:zGate".to_string(), &config(&["persona"]));
    assert!(
        with_persona
            .iter()
            .any(|tool| tool.name() == CONFIGURE_PERSONA_TOOL_NAME),
        "configure_persona must register when the persona category is enabled"
    );
}

#[tokio::test]
async fn persona_unknown_action_errors_cleanly() {
    let node = build_persona_node().await;
    let tools = build_self_config_tools(
        node,
        "did:key:zPersonaUnknown".to_string(),
        &config(&["persona"]),
    );

    let error = call_persona_tool(&tools, serde_json::json!({ "action": "delete" }))
        .await
        .expect_err("unknown action must error");
    assert!(
        error.contains("unknown action"),
        "error should name the bad action: {error}"
    );
}

#[derive(serde::Deserialize)]
struct PersonaRequestRowForTest {
    request_key: Option<String>,
    requester_did: Option<String>,
    agent_did: Option<String>,
    op: Option<String>,
    clone_from: Option<String>,
    preset: Option<String>,
}

async fn load_persona_rows_for_test(
    node: &defra_node::EmbeddedNode,
    agent_did: &str,
) -> Vec<PersonaRequestRowForTest> {
    let query = format!(
        r#"{{
            PersonaConfigRequest(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}) {{
                request_key
                requester_did
                agent_did
                op
                clone_from
                preset
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "query failed: {:?}",
        response.errors
    );
    serde_json::from_value(
        response
            .data
            .as_ref()
            .and_then(|data| data.get("PersonaConfigRequest"))
            .cloned()
            .unwrap_or(serde_json::Value::Array(Vec::new())),
    )
    .expect("decode rows")
}

/// Owns the tool as `Box<dyn ToolDyn>` so it can be moved into a spawned
/// task: `configure_persona` polls for up to 5s internally, and this test
/// must run a manual reconciler tick concurrently — NOT a background task —
/// while that poll is in flight, so the tool's own call observes the
/// converged status instead of timing out at "pending".
fn take_persona_tool(
    tools: Vec<Box<dyn crate::llm::tool::ToolDyn>>,
) -> Box<dyn crate::llm::tool::ToolDyn> {
    tools
        .into_iter()
        .find(|tool| tool.name() == CONFIGURE_PERSONA_TOOL_NAME)
        .expect("configure_persona registered")
}

#[tokio::test]
async fn persona_create_authors_row_and_applies_after_manual_tick() {
    let node = build_persona_node().await;
    let agent_did = "did:key:zPersonaCreateAgent";

    let seed = format!(
        r#"mutation {{
            create_AgentPrincipal(input: {{
                agent_did: "{agent_did}",
                display_name: "Persona Create Agent",
                enabled: true,
                created_at: "2026-07-23T00:00:00Z"
            }}) {{ _docID }}
            create_InferenceBackend(input: {{
                backend_id: "openai",
                name: "OpenAI",
                enabled: true,
                models: ["gpt-5"]
            }}) {{ _docID }}
            create_InferenceProfile(input: {{
                profile_id: "profile-1",
                display_name: "Fast"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&seed).await;
    assert!(!response.has_errors(), "seed failed: {:?}", response.errors);

    let tools = build_self_config_tools(node.clone(), agent_did.to_string(), &config(&["persona"]));
    let tool = take_persona_tool(tools);

    let args = serde_json::json!({
        "action": "create",
        "persona_name": "Research Assistant",
        "model": "openai|gpt-5",
        "preset": "write",
        "profile_id": "profile-1",
    })
    .to_string();

    // The tool call's internal poll runs to completion in the background
    // while THIS task drives one manual reconciler tick — the exact
    // production reconciler, called directly, never the spawned background
    // loop — so the call converges on "applied" instead of waiting out its
    // full 5s "still pending" ceiling.
    let call_handle = tokio::spawn(async move { tool.call(args).await });

    let mut request_key = None;
    for _ in 0..50 {
        let rows = load_persona_rows_for_test(&node, agent_did).await;
        if let Some(row) = rows.into_iter().next() {
            assert_eq!(
                row.requester_did.as_deref(),
                Some(agent_did),
                "self-authored requests set requester_did == agent_did"
            );
            assert_eq!(row.agent_did.as_deref(), Some(agent_did));
            request_key = row.request_key;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let request_key = request_key.expect("configure_persona authors a PersonaConfigRequest row");

    let store = crate::agent::p2p_reconcile::GraphqlPersonaRequestStore::new(node.clone());
    let outcome = crate::agent::p2p_reconcile::reconcile_persona_tick(&store, &node)
        .await
        .expect("manual reconcile tick");
    assert!(
        outcome.applied.contains(&request_key),
        "manual tick must apply the pending request: {outcome:?}"
    );

    let behaviors = crate::list_agent_behaviors(&node, agent_did)
        .await
        .expect("list behaviors");
    assert_eq!(behaviors.len(), 1, "exactly one behavior materialized");
    assert_eq!(
        behaviors[0].display_name,
        Some("Research Assistant".to_string())
    );

    let output = call_handle
        .await
        .expect("tool call task joins")
        .expect("configure_persona call succeeds");
    assert!(
        output.contains("\"status\": \"applied\""),
        "the tool's own poll must observe the manual tick's outcome: {output}"
    );
    assert!(output.contains(&request_key), "{output}");
}

#[tokio::test]
async fn persona_clone_accepts_sibling_behavior_id() {
    let node = build_persona_node().await;
    let agent_did = "did:key:zPersonaCloneAgent";
    let access = crate::config_client::ConfigAccess::Local(node.clone());

    let seed = format!(
        r#"mutation {{
            create_AgentPrincipal(input: {{
                agent_did: "{agent_did}",
                display_name: "Persona Clone Agent",
                enabled: true,
                created_at: "2026-07-23T00:00:00Z"
            }}) {{ _docID }}
            create_InferenceBackend(input: {{
                backend_id: "openai",
                name: "OpenAI",
                enabled: true,
                models: ["gpt-5"]
            }}) {{ _docID }}
            create_InferenceProfile(input: {{
                profile_id: "profile-1",
                display_name: "Fast"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&seed).await;
    assert!(!response.has_errors(), "seed failed: {:?}", response.errors);

    // A sibling behavior/selection this agent already owns — cloning FROM a
    // sibling of the same principal is the whole point of this tool.
    let sibling_selection = crate::document_config::ToolSelectionDocument {
        selection_id: "sel-sibling".to_string(),
        agent_did: agent_did.to_string(),
        enable_bash: Some(true),
        bash_mode: Some("ReadOnly".to_string()),
        enable_file_tools: Some(true),
        file_tools_mode: Some("ReadOnly".to_string()),
        ..Default::default()
    };
    crate::config_client::write_tool_selection_document(&access, &sibling_selection)
        .await
        .expect("seed sibling selection");
    let sibling_behavior = crate::AgentBehaviorDocument {
        behavior_id: "sibling-behavior".to_string(),
        agent_did: agent_did.to_string(),
        display_name: None,
        description: None,
        summary: None,
        system_prompt: None,
        request_context_template: None,
        backend_id: None,
        model_name: None,
        tool_selection_id: Some("sel-sibling".to_string()),
        inference_profile_id: None,
        compaction_strategy: None,
        compaction_threshold: None,
        enabled: true,
        skill_refs: Vec::new(),
        skill_excludes: Vec::new(),
        created_at: None,
    };
    crate::config_client::write_agent_behavior_document(&access, &sibling_behavior)
        .await
        .expect("seed sibling behavior");

    let tools = build_self_config_tools(node.clone(), agent_did.to_string(), &config(&["persona"]));
    let tool = take_persona_tool(tools);
    let args = serde_json::json!({
        "action": "clone",
        "persona_name": "Cloned Persona",
        "clone_from": "sibling-behavior",
        "model": "openai|gpt-5",
        "profile_id": "profile-1",
    })
    .to_string();
    let call_handle = tokio::spawn(async move { tool.call(args).await });

    let mut request_key = None;
    for _ in 0..50 {
        let rows = load_persona_rows_for_test(&node, agent_did).await;
        if let Some(row) = rows.into_iter().next() {
            assert_eq!(row.op.as_deref(), Some("create"));
            assert_eq!(
                row.clone_from.as_deref(),
                Some("sibling-behavior"),
                "clone_from must name the sibling behavior"
            );
            assert!(row.preset.is_none(), "clone must not also set a preset");
            request_key = row.request_key;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let request_key = request_key.expect("configure_persona authors a PersonaConfigRequest row");

    let store = crate::agent::p2p_reconcile::GraphqlPersonaRequestStore::new(node.clone());
    let outcome = crate::agent::p2p_reconcile::reconcile_persona_tick(&store, &node)
        .await
        .expect("manual reconcile tick");
    assert!(
        outcome.applied.contains(&request_key),
        "cloning from an enabled sibling must be admitted and applied: {outcome:?}"
    );

    let output = call_handle
        .await
        .expect("tool call task joins")
        .expect("configure_persona clone call succeeds");
    assert!(output.contains("\"status\": \"applied\""), "{output}");
}
