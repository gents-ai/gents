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
        enable_pack_install: false,
        process_ceiling: Default::default(),
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
fn pack_install_requires_its_separate_opt_in() {
    let without_install = config(&["behavior"]);
    assert!(!self_config_tool_names(&without_install)
        .iter()
        .any(|name| name == INSTALL_PACK_TOOL_NAME));

    let mut with_install = without_install;
    with_install.enable_pack_install = true;
    assert!(self_config_tool_names(&with_install)
        .iter()
        .any(|name| name == INSTALL_PACK_TOOL_NAME));
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
        None,
        &config(&["behavior"]),
    );
    assert!(
        tools.is_empty(),
        "an empty agent DID must register no self-config tools"
    );
}

#[tokio::test]
async fn build_registers_gated_family() {
    // Smoke assertion: the builder registers whatever the sorted name table
    // lists (owned by `tool_names_follow_enabled_categories`); this only
    // proves the real registration path is wired for a gated config.
    let tempdir = tempfile::tempdir().expect("tempdir");
    let node = defra_node::EmbeddedNode::builder()
        .data_path(tempdir.path().join("data"))
        .build()
        .await
        .expect("node");
    let tools = build_self_config_tools(
        std::sync::Arc::new(node),
        "did:key:zSelfConfigTest".to_string(),
        None,
        &config(&["behavior", "backend"]),
    );
    assert!(!tools.is_empty(), "a gated config must register tools");
}

#[tokio::test]
async fn pack_install_uses_current_principal_and_inference_chain() {
    let node = build_persona_node().await;
    let identity = persona_identity("pack-install");
    let agent_did = identity.did().to_string();
    crate::test_support::install_test_behavior(&node, &agent_did, "setup").await;

    let mut tool_config = config(&[]);
    tool_config.behavior_id = "setup".to_string();
    tool_config.enable_pack_install = true;
    let tools = build_self_config_tools(
        node.clone(),
        agent_did.clone(),
        Some(identity),
        &tool_config,
    );
    let tool = tools
        .iter()
        .find(|tool| tool.name() == INSTALL_PACK_TOOL_NAME)
        .expect("install_pack registered");
    for name in [
        LIST_GRAPHS_TOOL_NAME,
        RUN_GRAPH_TOOL_NAME,
        GET_GRAPH_RUN_TOOL_NAME,
        GET_GRAPH_RESULT_TOOL_NAME,
        CANCEL_GRAPH_RUN_TOOL_NAME,
    ] {
        assert!(
            tools.iter().any(|tool| tool.name() == name),
            "{name} must share the explicit graph authority gate"
        );
    }
    let output = tool
        .call(serde_json::json!({"package": "code_review"}).to_string())
        .await
        .expect("bundled pack installs");
    let output: serde_json::Value = serde_json::from_str(&output).expect("JSON receipt");
    assert_eq!(output["install"]["package_name"], "code_review");
    assert_eq!(output["activation"]["graph_id"], "code-review");
    assert_eq!(
        output["activation"]["active_digest"],
        output["install"]["revision_digest"]
    );

    let response = node
        .execute(&format!(
            "{{ GraphDefinition(filter: {{agent_did: {{_eq: \"{}\"}}}}) {{graph_id agent_did active_revision_digest}} }}",
            crate::graphql::escape_graphql_string(&agent_did)
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let definitions = response.data.unwrap()["GraphDefinition"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0]["agent_did"], agent_did);
    assert_eq!(definitions[0]["graph_id"], "code-review");
    assert_eq!(
        definitions[0]["active_revision_digest"],
        output["install"]["revision_digest"]
    );
    let list = tools
        .iter()
        .find(|tool| tool.name() == LIST_GRAPHS_TOOL_NAME)
        .unwrap()
        .call("{}".to_owned())
        .await
        .expect("installed graph is discoverable on the same node");
    let list: Value = serde_json::from_str(&list).unwrap();
    assert_eq!(list["node_bound"], true);
    assert_eq!(list["agent_did"], agent_did);
    assert_eq!(list["graphs"][0]["definition"]["graph_id"], "code-review");
    assert_eq!(
        list["graphs"][0]["active_plan"]["digest"],
        output["install"]["revision_digest"]
    );
    let run_error = tools
        .iter()
        .find(|tool| tool.name() == RUN_GRAPH_TOOL_NAME)
        .unwrap()
        .call(json!({"package": "code_review"}).to_string())
        .await
        .expect_err("code review cannot bypass this behavior's disabled file authority");
    assert!(
        run_error
            .to_string()
            .contains("requires effective read authority"),
        "{run_error:#}"
    );
    let runs = node
        .execute(&format!(
            r#"{{ GraphRun(filter: {{owner_did: {{_eq: "{}"}}}}) {{run_id}} }}"#,
            crate::graphql::escape_graphql_string(&agent_did)
        ))
        .await;
    assert!(!runs.has_errors(), "{:?}", runs.errors);
    assert!(runs.data.unwrap()["GraphRun"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn graph_tools_start_observe_and_cancel_on_the_current_node() {
    let node = build_persona_node().await;
    let identity = persona_identity("graph-tools");
    let agent_did = identity.did().to_string();
    crate::test_support::install_test_behavior(&node, &agent_did, "setup").await;
    let repository = tempfile::tempdir().expect("repository");
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repository.path())
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "--quiet"]);
    git(&["config", "user.email", "test@example.invalid"]);
    git(&["config", "user.name", "Graph Tool Test"]);
    std::fs::write(repository.path().join("review.txt"), "before\n").unwrap();
    git(&["add", "review.txt"]);
    git(&["commit", "--quiet", "-m", "base"]);
    std::fs::write(repository.path().join("review.txt"), "after\n").unwrap();
    git(&["add", "review.txt"]);
    git(&["commit", "--quiet", "-m", "head"]);

    let mut tool_config = config(&["tools"]);
    tool_config.behavior_id = "setup".to_owned();
    tool_config.enable_pack_install = true;
    tool_config.process_ceiling = crate::tool_surface::SelfConfigProcessCeiling {
        file_mode: crate::tool_surface::FileToolMode::ReadOnly,
        bash_mode: crate::tool_surface::BashMode::Off,
        root: Some(repository.path().to_owned()),
    };
    let tools = build_self_config_tools(node, agent_did.clone(), Some(identity), &tool_config);
    let call = |name: &str, args: Value| {
        let tool = tools
            .iter()
            .find(|tool| tool.name() == name)
            .unwrap_or_else(|| panic!("missing tool {name}"));
        tool.call(args.to_string())
    };

    call(
        CONFIGURE_TOOLS_TOOL_NAME,
        json!({"patch": {"host": {
            "root": repository.path().to_string_lossy(),
            "files": {"mode": "ReadOnly"}
        }}}),
    )
    .await
    .expect("current behavior receives explicit read authority");
    call(INSTALL_PACK_TOOL_NAME, json!({"package": "code_review"}))
        .await
        .expect("code-review pack installs");
    let started = call(
        RUN_GRAPH_TOOL_NAME,
        json!({
            "package": "code_review",
            "repository": repository.path().to_string_lossy(),
            "base": "HEAD^",
            "head": "HEAD",
            "focus": "Review the changed text.",
        }),
    )
    .await
    .expect("node-bound graph starts");
    let started: Value = serde_json::from_str(&started).unwrap();
    assert_eq!(started["node_bound"], true);
    assert_eq!(started["principal"], agent_did);
    assert_eq!(started["observed"]["status"], "running");
    let run_id = started["receipt"]["run_id"].as_str().unwrap();

    let observed = call(GET_GRAPH_RUN_TOOL_NAME, json!({"run_id": run_id}))
        .await
        .expect("same-node status is readable");
    let observed: Value = serde_json::from_str(&observed).unwrap();
    assert_eq!(observed["run_id"], run_id);
    assert_eq!(observed["owner_did"], agent_did);
    assert_eq!(observed["status"], "running");

    let not_yet_a_result = call(GET_GRAPH_RESULT_TOOL_NAME, json!({"run_id": run_id}))
        .await
        .expect("nonterminal result view remains inspectable");
    let not_yet_a_result: Value = serde_json::from_str(&not_yet_a_result).unwrap();
    assert_eq!(not_yet_a_result["status"], "running");
    assert_eq!(not_yet_a_result["result_contract_satisfied"], false);

    let cancelled = call(
        CANCEL_GRAPH_RUN_TOOL_NAME,
        json!({"run_id": run_id, "reason": "test cleanup"}),
    )
    .await
    .expect("same-node cancellation is persisted");
    let cancelled: Value = serde_json::from_str(&cancelled).unwrap();
    assert_eq!(cancelled["run_id"], run_id);
    assert_eq!(cancelled["cancellation_requested_by"], agent_did);
    assert_eq!(cancelled["cancellation_reason"], "test cleanup");
    assert_ne!(cancelled["status"], "succeeded");
}

#[tokio::test]
async fn configure_tools_cannot_self_grant_pack_install() {
    let node = build_persona_node().await;
    let identity = persona_identity("pack-self-grant");
    let agent_did = identity.did().to_string();
    crate::test_support::install_test_behavior(&node, &agent_did, "setup").await;

    let mut tool_config = config(&["tools"]);
    tool_config.behavior_id = "setup".to_string();
    let tools = build_self_config_tools(node, agent_did, Some(identity), &tool_config);
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIGURE_TOOLS_TOOL_NAME)
        .expect("configure_tools registered");
    let error = tool
        .call(
            serde_json::json!({
                "patch": {
                    "self_config": {
                        "enable_self_config": true,
                        "enable_pack_install": true
                    }
                }
            })
            .to_string(),
        )
        .await
        .expect_err("pack install cannot be self-granted");
    assert!(
        error.to_string().contains("cannot be self-granted"),
        "{error:#}"
    );
}

// -- configure_behaviors (#Task 5) --

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

fn persona_identity(label: &str) -> std::sync::Arc<dyn crate::AgentIdentity> {
    let tempdir = tempfile::tempdir().expect("identity tempdir");
    std::sync::Arc::new(
        crate::KeyIdentity::load_or_create(&tempdir.path().join(format!("{label}.key")), None)
            .expect("test identity"),
    )
}

async fn call_persona_tool(
    tools: &[Box<dyn crate::llm::tool::ToolDyn>],
    args: serde_json::Value,
) -> Result<String, String> {
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIGURE_BEHAVIORS_TOOL_NAME)
        .expect("configure_behaviors registered");
    tool.call(args.to_string())
        .await
        .map_err(|error| format!("{error:#}"))
}

#[tokio::test]
async fn persona_category_gates_the_tool() {
    let node = build_persona_node().await;
    let identity = persona_identity("persona-gate");
    let agent_did = identity.did().to_string();

    let without_persona = build_self_config_tools(
        node.clone(),
        agent_did.clone(),
        None,
        &config(&["behavior"]),
    );
    assert!(
        without_persona
            .iter()
            .all(|tool| tool.name() != CONFIGURE_BEHAVIORS_TOOL_NAME),
        "configure_behaviors must not register without the persona category"
    );

    let with_persona =
        build_self_config_tools(node, agent_did, Some(identity), &config(&["persona"]));
    assert!(
        with_persona
            .iter()
            .any(|tool| tool.name() == CONFIGURE_BEHAVIORS_TOOL_NAME),
        "configure_behaviors must register when the persona category is enabled"
    );
}

#[tokio::test]
async fn persona_unknown_action_errors_cleanly() {
    let node = build_persona_node().await;
    let identity = persona_identity("persona-unknown");
    let tools = build_self_config_tools(
        node,
        identity.did().to_string(),
        Some(identity),
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
/// task: `configure_behaviors` polls for up to 5s internally, and this test
/// must run a manual reconciler tick concurrently — NOT a background task —
/// while that poll is in flight, so the tool's own call observes the
/// converged status instead of timing out at "pending".
fn take_persona_tool(
    tools: Vec<Box<dyn crate::llm::tool::ToolDyn>>,
) -> Box<dyn crate::llm::tool::ToolDyn> {
    tools
        .into_iter()
        .find(|tool| tool.name() == CONFIGURE_BEHAVIORS_TOOL_NAME)
        .expect("configure_behaviors registered")
}

#[tokio::test]
async fn persona_create_authors_row_and_applies_after_manual_tick() {
    let node = build_persona_node().await;
    let identity = persona_identity("persona-create");
    let agent_did = identity.did().to_string();

    crate::test_support::install_test_behavior(&node, &agent_did, "seed").await;
    let profile_id = "seed:inference";

    let tools = build_self_config_tools(
        node.clone(),
        agent_did.clone(),
        Some(identity.clone()),
        &config(&["persona"]),
    );
    let rejected_preview = call_persona_tool(
        &tools,
        json!({
            "action": "preview",
            "operation": "create",
            "display_name": "Research Assistant",
            "preset": "write",
            "profile_id": profile_id,
        }),
    )
    .await
    .expect("invalid preview is an inspectable no-write result");
    let rejected_preview: Value = serde_json::from_str(&rejected_preview).unwrap();
    assert_eq!(rejected_preview["committed"], false);
    assert_eq!(rejected_preview["admitted"], false);
    assert!(rejected_preview["rejection"]
        .as_str()
        .unwrap()
        .contains("system_prompt is required"));
    let admitted_preview = call_persona_tool(
        &tools,
        json!({
            "action": "preview",
            "operation": "create",
            "display_name": "Research Assistant",
            "description": "Researches a focused question",
            "system_prompt": "Research the question and cite evidence.",
            "preset": "write",
            "profile_id": profile_id,
            "make_default": true,
        }),
    )
    .await
    .expect("complete preview succeeds");
    let admitted_preview: Value = serde_json::from_str(&admitted_preview).unwrap();
    assert_eq!(admitted_preview["committed"], false);
    assert_eq!(admitted_preview["admitted"], true);
    assert!(admitted_preview.get("preset_requested").is_some());
    assert!(admitted_preview.get("preset_effective").is_none());
    assert!(admitted_preview["note"]
        .as_str()
        .unwrap()
        .contains("does not verify materialization"));
    assert!(load_persona_rows_for_test(&node, &agent_did)
        .await
        .is_empty());
    let tool = take_persona_tool(tools);

    let args = serde_json::json!({
        "action": "create",
        "display_name": "Research Assistant",
        "description": "Researches a focused question",
        "system_prompt": "Research the question and cite evidence.",
        "preset": "write",
        // Short id — profile IDs are preserved exactly as authored; the
        // request row carries this value verbatim to admission.
        "profile_id": profile_id,
        "make_default": true,
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
        let rows = load_persona_rows_for_test(&node, &agent_did).await;
        if let Some(row) = rows.into_iter().next() {
            assert_eq!(
                row.requester_did.as_deref(),
                Some(agent_did.as_str()),
                "self-authored requests set requester_did == agent_did"
            );
            assert_eq!(row.agent_did.as_deref(), Some(agent_did.as_str()));
            request_key = row.request_key;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let request_key = request_key.expect("configure_behaviors authors a PersonaConfigRequest row");

    let store = crate::agent::p2p_reconcile::GraphqlPersonaRequestStore::with_local_identity(
        node.clone(),
        None,
        identity.clone(),
    );
    let outcome = crate::agent::p2p_reconcile::reconcile_persona_tick(&store, &node)
        .await
        .expect("manual reconcile tick");
    assert!(
        outcome.applied.contains(&request_key),
        "manual tick must apply the pending request: {outcome:?}"
    );

    let behaviors = crate::list_agent_behaviors(&node, &agent_did)
        .await
        .expect("list behaviors");
    let created: Vec<_> = behaviors
        .iter()
        .filter(|behavior| behavior.behavior_id != "seed")
        .collect();
    assert_eq!(created.len(), 1, "exactly one new behavior materialized");
    assert_eq!(
        created[0].display_name,
        Some("Research Assistant".to_string())
    );
    let context_id = crate::graphql::escape_graphql_string(
        created[0]
            .context_id
            .as_deref()
            .expect("created behavior has context"),
    );
    let response = node
        .execute(&format!(
            r#"{{ AgentContext(filter: {{ context_id: {{ _eq: "{context_id}" }} }}) {{ description system_prompt }} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "query failed: {:?}",
        response.errors
    );
    let prompt = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentContext"))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("system_prompt"))
        .and_then(serde_json::Value::as_str);
    assert_eq!(prompt, Some("Research the question and cite evidence."));
    let description = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentContext"))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("description"))
        .and_then(serde_json::Value::as_str);
    assert_eq!(description, Some("Researches a focused question"));

    let output = call_handle
        .await
        .expect("tool call task joins")
        .expect("configure_behaviors call succeeds");
    assert!(
        output.contains("\"status\": \"applied\""),
        "the tool's own poll must observe the manual tick's outcome: {output}"
    );
    assert!(output.contains(&request_key), "{output}");
    let output: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        output["materialized_ids"]["behavior_id"],
        created[0].behavior_id
    );
    assert_eq!(
        output["effective"]["effective_config"]["context"]["system_prompt"],
        "Research the question and cite evidence."
    );
    assert_eq!(output["activation"]["durable"], "confirmed");

    let snapshot = crate::agent::resolve_document_runtime_snapshot(
        node.as_ref(),
        &crate::agent::DocumentResolveContext {
            identity,
            tool_ceiling: crate::tool_surface::ToolCeiling::readonly(),
            backend_health: Default::default(),
        },
    )
    .await
    .expect("restart-style runtime resolution accepts the materialized behavior");
    assert_eq!(snapshot.default_behavior_id, created[0].behavior_id);
    let runtime_behavior = snapshot
        .behaviors
        .get(&created[0].behavior_id)
        .unwrap_or_else(|| {
            panic!(
                "new default is runnable after re-resolution: {:?}",
                snapshot.unavailable_behaviors
            )
        });
    assert_eq!(
        runtime_behavior.system_prompt,
        "Research the question and cite evidence."
    );
}

#[tokio::test]
async fn persona_clone_accepts_sibling_behavior_id() {
    let node = build_persona_node().await;
    let identity = persona_identity("persona-clone");
    let agent_did = identity.did().to_string();
    let qualified_sibling_id = "sibling-behavior".to_owned();
    crate::test_support::install_test_behavior(&node, &agent_did, &qualified_sibling_id).await;
    let profile_id = "sibling-behavior:inference";

    let tools = build_self_config_tools(
        node.clone(),
        agent_did.clone(),
        Some(identity.clone()),
        &config(&["persona"]),
    );
    let tool = take_persona_tool(tools);
    let args = serde_json::json!({
        "action": "clone",
        "display_name": "Cloned Persona",
        // Short ids are preserved exactly as authored; the request row
        // carries `clone_from` verbatim to admission.
        "clone_from": "sibling-behavior",
        "profile_id": profile_id,
    })
    .to_string();
    let call_handle = tokio::spawn(async move { tool.call(args).await });

    let mut request_key = None;
    for _ in 0..50 {
        let rows = load_persona_rows_for_test(&node, &agent_did).await;
        if let Some(row) = rows.into_iter().next() {
            assert_eq!(row.op.as_deref(), Some("create"));
            assert_eq!(
                row.clone_from.as_deref(),
                Some(qualified_sibling_id.as_str()),
                "clone_from must resolve to the sibling's fully-qualified behavior_id"
            );
            assert!(row.preset.is_none(), "clone must not also set a preset");
            request_key = row.request_key;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let request_key = request_key.expect("configure_behaviors authors a PersonaConfigRequest row");

    let store = crate::agent::p2p_reconcile::GraphqlPersonaRequestStore::with_local_identity(
        node.clone(),
        None,
        identity,
    );
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
        .expect("configure_behaviors clone call succeeds");
    assert!(output.contains("\"status\": \"applied\""), "{output}");
}

#[tokio::test]
async fn canonical_self_config_preview_and_apply_preserve_scope_and_reject_lockout() {
    let node = build_persona_node().await;
    let identity = persona_identity("canonical-self-config");
    let other = persona_identity("other-self-config");
    let owner = identity.did().to_string();
    let foreign = other.did().to_string();
    crate::test_support::install_test_behavior(&node, &owner, "same").await;
    crate::test_support::install_test_behavior(&node, &foreign, "same").await;
    crate::test_support::install_test_behavior(&node, &owner, "unconfigured").await;
    let core = SelfConfigCore::new(node.clone(), owner.clone(), "same".into()).unwrap();
    let patch = vec![(
        "self_config".into(),
        Some(json!({"enable_self_config":true})),
    )];
    let preview = core
        .preview(tools_request(&core, patch.clone(), false))
        .await
        .unwrap();
    assert!(!preview.committed);
    let read = core
        .read_effective_config(&BTreeSet::new(), false, true)
        .await
        .unwrap();
    assert!(read["documents"]["Tools"]["self_config"].is_null());
    core.apply(tools_request(&core, patch, false))
        .await
        .unwrap();
    let guarded = core.clone().with_no_lockout(true);
    assert!(guarded
        .apply(tools_request(
            &guarded,
            vec![("self_config".into(), None)],
            false,
        ))
        .await
        .is_err());
    assert!(guarded
        .preview(behavior_request(
            &guarded,
            vec![("context_id".into(), Some(json!("unconfigured:context")))]
        ))
        .await
        .is_err());
    assert!(core
        .apply(anchored_request(
            SelfConfigTarget::AgentContext,
            "context_id",
            vec![("tools_id".into(), Some(json!("missing")))]
        ))
        .await
        .is_err());
    assert!(core
        .preview(tools_request(
            &core,
            vec![("host".into(), Some(json!({"unexpected":true})))],
            false,
        ))
        .await
        .is_err());
    let own = core
        .read_effective_config(&BTreeSet::new(), true, true)
        .await
        .unwrap();
    assert_eq!(own["context"]["tools_id"], "same:tools");
    assert_eq!(
        own["documents"]["Tools"]["self_config"]["enable_self_config"],
        true
    );
    let foreign_core = SelfConfigCore::new(node.clone(), foreign, "same".into()).unwrap();
    let foreign_read = foreign_core
        .read_effective_config(&BTreeSet::new(), false, false)
        .await
        .unwrap();
    assert!(foreign_read["documents"]["Tools"]["self_config"].is_null());
}

#[tokio::test]
async fn backend_self_config_protects_raw_keys_in_writes_reads_and_diffs() {
    let node = build_persona_node().await;
    let identity = persona_identity("self-config-secret");
    let owner = identity.did().to_string();
    crate::test_support::install_test_behavior(&node, &owner, "secret").await;
    let core = SelfConfigCore::new(node.clone(), owner.clone(), "secret".into()).unwrap();
    let raw = vec![(
        "auth".into(),
        Some(json!({"kind":"api_key","key":"do-not-expose"})),
    )];
    assert!(core.preview(backend_request(raw.clone())).await.is_err());
    assert!(core.apply(backend_request(raw)).await.is_err());
    let owner = escape_graphql_string(&owner);
    let response=node.execute(&format!(r#"mutation {{update_InferenceBackend(filter:{{agent_did:{{_eq:"{owner}"}},backend_id:{{_eq:"secret:backend"}}}},input:{{auth:{{kind:"api_key",key:"operator-secret"}}}}) {{_docID}}}}"#)).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    // Lean authPatchAllowed permits preserving an existing raw key exactly.
    core.preview(backend_request(vec![(
        "auth".into(),
        Some(json!({"kind":"api_key","key":"operator-secret"})),
    )]))
    .await
    .unwrap();
    assert!(core
        .apply(backend_request(vec![(
            "auth".into(),
            Some(json!({"kind":"api_key","key":"replacement-secret"}))
        )]))
        .await
        .is_err());
    let read = core
        .read_effective_config(&BTreeSet::new(), false, true)
        .await
        .unwrap();
    assert!(!read.to_string().contains("operator-secret"));
    let patch = vec![(
        "auth".into(),
        Some(json!({"kind":"environment","variable":"GENTS_TEST_KEY_REFERENCE"})),
    )];
    let preview = core.preview(backend_request(patch)).await.unwrap();
    assert!(!serde_json::to_string(&preview)
        .unwrap()
        .contains("operator-secret"));
}

#[tokio::test]
async fn explicit_tools_grant_preserves_lsp_settings_guard_for_preview_and_apply() {
    let node = build_persona_node().await;
    let identity = persona_identity("self-config-lsp-guard");
    let owner = identity.did().to_string();
    crate::test_support::install_test_behavior(&node, &owner, "beh-test").await;
    let mut tool_config = config(&["tools"]);
    tool_config.dry_run = true;
    let tools = build_self_config_tools(node, owner, None, &tool_config);
    let configure = tools
        .iter()
        .find(|tool| tool.name() == CONFIGURE_TOOLS_TOOL_NAME)
        .unwrap();
    let inspect = tools
        .iter()
        .find(|tool| tool.name() == GET_MY_CONFIG_TOOL_NAME)
        .unwrap();
    let safe = json!({"integrations":{"lsp":{"config":json!({"servers":{"rust-analyzer":{"disabled":true,"priority":2}}}).to_string()}}});
    inspect
        .call(json!({"preview":{"category":"tools","patch":safe}}).to_string())
        .await
        .unwrap();
    configure
        .call(json!({"patch":safe}).to_string())
        .await
        .unwrap();
    let baseline: Value = serde_json::from_str(&inspect.call("{}".into()).await.unwrap()).unwrap();
    assert_eq!(
        baseline["documents"]["Tools"]["integrations"],
        safe["integrations"]
    );
    for field in ["settings", "init_options", "initOptions"] {
        let server = json!({field:{"check":{"overrideCommand":["custom-check"]}}});
        let raw = json!({"servers":{"rust-analyzer":server}}).to_string();
        // Operator configuration keeps its existing broader admission.
        crate::toolset::lsp::LspConfigDocument::parse_operator(Some(&raw)).unwrap();
        let patch = json!({"integrations":{"lsp":{"config":raw}}});
        let preview = inspect
            .call(json!({"preview":{"category":"tools","patch":patch}}).to_string())
            .await
            .unwrap_err();
        assert!(preview.to_string().contains(field), "{preview}");
        let apply = configure
            .call(json!({"patch":patch}).to_string())
            .await
            .unwrap_err();
        assert!(apply.to_string().contains(field), "{apply}");
        let after: Value = serde_json::from_str(&inspect.call("{}".into()).await.unwrap()).unwrap();
        assert_eq!(after["documents"]["Tools"], baseline["documents"]["Tools"]);
    }
}
