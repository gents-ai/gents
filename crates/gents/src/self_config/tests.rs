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
        .find(|tool| tool.name() == CONFIGURE_PERSONA_TOOL_NAME)
        .expect("configure_persona registered");
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
            .all(|tool| tool.name() != CONFIGURE_PERSONA_TOOL_NAME),
        "configure_persona must not register without the persona category"
    );

    let with_persona =
        build_self_config_tools(node, agent_did, Some(identity), &config(&["persona"]));
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
    let tool = take_persona_tool(tools);

    let args = serde_json::json!({
        "action": "create",
        "persona_name": "Research Assistant",
        "preset": "write",
        // Short id — profile IDs are preserved exactly as authored; the
        // request row carries this value verbatim to admission.
        "profile_id": profile_id,
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
    let request_key = request_key.expect("configure_persona authors a PersonaConfigRequest row");

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
        "persona_name": "Cloned Persona",
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
    let request_key = request_key.expect("configure_persona authors a PersonaConfigRequest row");

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
        .expect("configure_persona clone call succeeds");
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
        .preview(tools_request(&core, patch.clone()))
        .await
        .unwrap();
    assert!(!preview.committed);
    let read = core
        .read_effective_config(&BTreeSet::new(), false, true)
        .await
        .unwrap();
    assert!(read["documents"]["Tools"]["self_config"].is_null());
    core.apply(tools_request(&core, patch)).await.unwrap();
    let guarded = core.clone().with_no_lockout(true);
    assert!(guarded
        .apply(tools_request(&guarded, vec![("self_config".into(), None)]))
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
            vec![("host".into(), Some(json!({"unexpected":true})))]
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
