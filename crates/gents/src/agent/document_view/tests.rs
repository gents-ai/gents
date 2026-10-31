use std::sync::Arc;

use super::*;
use crate::agent::DocumentResolveContext;
use crate::ensure_runtime_schemas;
use crate::store::graphql::escape_graphql_string;
use crate::store::identity::{KeyIdentity, NodeIdentity};
use crate::tools::surface::ToolCeiling;

async fn test_node() -> Arc<defra_node::EmbeddedNode> {
    Arc::new(crate::test_support::node_builder().build().await.unwrap())
}

fn created_skill_doc_id(data: Option<&serde_json::Value>) -> Option<String> {
    for value in data?.as_object()?.values() {
        let row = value
            .as_array()
            .and_then(|rows| rows.first())
            .unwrap_or(value);
        if let Some(id) = row.get("_docID").and_then(|v| v.as_str()) {
            return Some(id.to_string());
        }
    }
    None
}

fn test_identity(name: &str) -> KeyIdentity {
    let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
    KeyIdentity::load_or_create(path, None).unwrap()
}

async fn bind_default_agent_backend(
    node: &Arc<defra_node::EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
) {
    crate::test_support::install_test_agent(node, node_did, agent_id).await;
    crate::upsert_node(node, node_did, None, Some(agent_id), true, None)
        .await
        .unwrap();
    crate::inference::backends::registry::set_backend_probe_status(
        node,
        node_did,
        &format!("{agent_id}:backend"),
        "healthy",
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn existing_document_runtime_view_does_not_wait_for_mutation_gate() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let owner = "did:key:runtime-view-reader";
    let principal = crate::documents::ensure_node(&node, owner).await.unwrap();
    let response = crate::store::config_client::ConfigAccess::write_local_response(
        &node,
        "test.runtime_view_skill",
        &format!(
            r#"mutation {{ create_Skill(input: {{
            skill_id: "runtime-skill", node_did: "{}",
            name: "Runtime skill", instructions: "Loaded through the snapshot.", enabled: true
        }}) {{ _docID }} }}"#,
            escape_graphql_string(owner)
        ),
    )
    .await
    .unwrap();
    let skill_doc_id = created_skill_doc_id(response.data.as_ref()).unwrap();
    let held = crate::store::config_client::ConfigApplyTxn::begin_local(&node, None)
        .await
        .unwrap();
    let view = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        load_document_runtime_view(&node, owner),
    )
    .await
    .expect("runtime configuration reads must not wait for the mutation gate")
    .expect("existing node runtime view");
    assert_eq!(view.node.value, principal);
    assert_eq!(view.skills["runtime-skill"].doc_id, skill_doc_id);
    assert_eq!(
        view.skills["runtime-skill"].value.name.as_deref(),
        Some("Runtime skill")
    );
    held.discard().await.unwrap();
    node.shutdown().await;
}

#[tokio::test]
async fn load_document_runtime_view_includes_referenced_documents() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-load"));
    let default_agent_id = crate::default_agent_id_for_node(identity.did());
    bind_default_agent_backend(&node, identity.did(), &default_agent_id).await;

    let view = load_document_runtime_view(&node, identity.did())
        .await
        .expect("document view should load");

    assert_eq!(view.node.value.node_did, identity.did());
    assert_eq!(
        view.node.value.default_agent_id.as_deref(),
        Some(default_agent_id.as_str())
    );
    assert!(view.agents.contains_key(&default_agent_id));
    let agent_config = &view.agents[&default_agent_id].value;
    let context_id = agent_config
        .context_id
        .as_deref()
        .expect("context reference");
    assert!(view.contexts.contains_key(context_id));
    let context = &view.contexts[context_id].value;
    let tools_id = context.tools_id.as_deref().expect("tools reference");
    assert!(view.tools.contains_key(tools_id));
    let profile = &view.inference_profiles[&agent_config.inference_profile_id].value;
    assert_eq!(profile.backend_id, format!("{default_agent_id}:backend"));
    assert!(view
        .backends
        .contains_key(&format!("{default_agent_id}:backend")));
}

#[tokio::test]
async fn apply_control_update_reconciles_tool_selection_via_doc_id() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-update"));
    let default_agent_id = crate::default_agent_id_for_node(identity.did());
    bind_default_agent_backend(&node, identity.did(), &default_agent_id).await;

    let resolve_context = DocumentResolveContext::for_tests(
        identity.clone(),
        ToolCeiling::readonly(),
        crate::inference::backends::health::BackendHealthMap::new(),
    );
    let mut view = load_document_runtime_view(&node, identity.did())
        .await
        .expect("initial document view");

    let tools_id = format!("{default_agent_id}:tools");
    let tools_doc_id = view.tools[&tools_id].doc_id.clone();
    let mut tools_doc = view.tools[&tools_id].value.clone();
    tools_doc.host.get_or_insert_with(Default::default).files = Some(crate::documents::FileTools {
        mode: crate::tools::surface::FileToolMode::ReadOnly,
        ..Default::default()
    });
    crate::store::config_client::write_tools_document(
        &crate::store::config_client::ConfigAccess::local(node.clone()),
        &tools_doc,
    )
    .await
    .expect("write read-only Tools patch");

    let behavior_doc_id = crate::documents::load_agent_record(node.as_ref(), &default_agent_id)
        .await
        .unwrap()
        .expect("agent record")
        .0;

    assert!(
        apply_control_update(identity.did(), "Tools", &tools_doc_id, &mut view,)
            .await
            .is_ok_and(|outcome| outcome == ControlUpdateOutcome::FullReload)
    );
    assert!(
        apply_control_update(identity.did(), "Agent", &behavior_doc_id, &mut view,)
            .await
            .is_ok_and(|outcome| outcome == ControlUpdateOutcome::FullReload)
    );

    let reloaded = load_document_runtime_view(&node, identity.did())
        .await
        .expect("reloaded document view");
    assert_eq!(
        reloaded.tools[&tools_id]
            .value
            .host
            .as_ref()
            .and_then(|host| host.files.as_ref())
            .map(|files| files.mode),
        Some(crate::tools::surface::FileToolMode::ReadOnly),
        "reloaded view must observe the read-only file-tools patch"
    );
    view = reloaded;

    let snapshot = resolve_document_runtime_snapshot_from_view(&node, &resolve_context, &view)
        .await
        .expect("snapshot from updated document view");
    let tool_surface = snapshot
        .tool_surfaces
        .get(&default_agent_id)
        .expect("tool surface for default agent");
    let tool_names = tool_surface.tool_names();
    assert!(tool_names.contains(&"read_file".to_string()));
    assert!(tool_names.contains(&"list_files".to_string()));
}

#[tokio::test]
async fn resolve_composes_explicitly_selected_skill_into_prompt() {
    use crate::inference::llm::tool::Tool;
    use crate::prompt::LayeredPromptBuilder;

    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-skill"));
    let default_agent_id = crate::default_agent_id_for_node(identity.did());
    bind_default_agent_backend(&node, identity.did(), &default_agent_id).await;

    let create_skill = format!(
        r#"mutation {{ create_Skill(input: {{
            skill_id: "skill-research",
            node_did: "{did}",
            name: "Research",
            description: "Find and cite sources",
            instructions: "Always cite your sources.",
            tool_refs: ["read_file", "definitely_not_a_tool"],
            enabled: true
        }}) {{ _docID }} }}"#,
        did = escape_graphql_string(identity.did()),
    );
    let resp = node.execute(&create_skill).await;
    assert!(!resp.has_errors(), "create_Skill failed: {:?}", resp.errors);

    let context_id = escape_graphql_string(&format!("{default_agent_id}:context"));
    let did = escape_graphql_string(identity.did());
    let select = format!(
        r#"mutation {{ update_AgentContext(filter: {{context_id: {{_eq: "{context_id}"}}, node_did: {{_eq: "{did}"}}}}, input: {{skill_ids: ["skill-research"]}}) {{_docID}} }}"#
    );
    let response = node.execute(&select).await;
    assert!(
        !response.has_errors(),
        "select skill: {:?}",
        response.errors
    );

    let resolve_context = DocumentResolveContext::for_tests(
        identity.clone(),
        ToolCeiling::readonly(),
        crate::inference::backends::health::BackendHealthMap::new(),
    );
    let view = load_document_runtime_view(&node, identity.did())
        .await
        .expect("document view");
    let snapshot = resolve_document_runtime_snapshot_from_view(&node, &resolve_context, &view)
        .await
        .expect("snapshot");

    let agent_config = snapshot
        .agents
        .get(&default_agent_id)
        .expect("resolved default agent");
    assert_eq!(
        agent_config.skills.len(),
        1,
        "explicitly selected skill must be available to the agent"
    );
    assert_eq!(agent_config.skills[0].skill_id, "skill-research");

    let tool_surface = snapshot
        .tool_surfaces
        .get(&default_agent_id)
        .expect("tool surface");
    let preamble = LayeredPromptBuilder::new(agent_config.as_ref(), tool_surface.as_ref(), &[])
        .preamble()
        .to_string();
    assert!(
        preamble.contains("Research"),
        "catalog lists the skill name: {preamble}"
    );
    assert!(
        preamble.contains("Find and cite sources"),
        "catalog lists the skill description: {preamble}"
    );
    assert!(
        preamble.contains("load_skill"),
        "catalog directs the model to load_skill"
    );
    assert!(
        !preamble.contains("Always cite your sources."),
        "skill BODY must NOT be in the catalog (loaded on demand): {preamble}"
    );

    let ceiling =
        crate::documents::skills::skill_tool_ceiling(tool_surface.tool_names(), &[], false);
    let load_skill =
        crate::documents::skills::LoadSkillTool::new(agent_config.skills.clone(), ceiling);
    let loaded = load_skill
        .call(crate::documents::skills::LoadSkillArgs {
            name: "Research".to_string(),
        })
        .await
        .expect("load_skill");
    assert!(
        loaded.contains("Always cite your sources."),
        "load_skill returns the full body: {loaded}"
    );
    assert!(
        loaded.contains("definitely_not_a_tool"),
        "load_skill body carries the degrade note for the ungranted tool_ref: {loaded}"
    );
}

#[tokio::test]
async fn skill_crud_mutations_round_trip() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let did = "did:key:zSkillCrud";

    let create = format!(
        r#"mutation {{ upsert_Skill(
            filter: {{ skill_id: {{ _eq: "s1" }} }},
            add: {{ skill_id: "s1", node_did: "{did}",  name: "S", tool_refs: ["read_file"], enabled: true }},
            update: {{ enabled: true }}
        ) {{ _docID }} }}"#
    );
    let resp = node.execute(&create).await;
    assert!(!resp.has_errors(), "upsert_Skill: {:?}", resp.errors);

    let update = r#"mutation { update_Skill(
        filter: { skill_id: { _eq: "s1" } },
        input: { enabled: false }
    ) { _docID } }"#;
    let resp = node.execute(update).await;
    assert!(
        !resp.has_errors(),
        "update_Skill by filter: {:?}",
        resp.errors
    );

    let query = r#"{ Skill(filter: { skill_id: { _eq: "s1" } }) { skill_id enabled } }"#;
    let resp = node.execute(query).await;
    assert!(!resp.has_errors(), "query Skill: {:?}", resp.errors);
    let enabled = resp
        .data
        .as_ref()
        .and_then(|d| d.get("Skill"))
        .and_then(|a| a.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("enabled"))
        .and_then(|v| v.as_bool());
    assert_eq!(enabled, Some(false), "disable must persist");

    let delete = r#"mutation { delete_Skill(filter: { skill_id: { _eq: "s1" } }) { _docID } }"#;
    let resp = node.execute(delete).await;
    assert!(
        !resp.has_errors(),
        "delete_Skill by filter: {:?}",
        resp.errors
    );
}

#[tokio::test]
async fn apply_control_update_hot_reloads_skill() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-skill-reload"));
    let default_agent_id = crate::default_agent_id_for_node(identity.did());
    bind_default_agent_backend(&node, identity.did(), &default_agent_id).await;

    let mut view = load_document_runtime_view(&node, identity.did())
        .await
        .expect("document view");
    assert!(view.skills.is_empty(), "no skills before create");

    let create = format!(
        r#"mutation {{ create_Skill(input: {{
            skill_id: "s-reload", node_did: "{did}",
            name: "Reload", instructions: "Reload me.", enabled: true
        }}) {{ _docID }} }}"#,
        did = escape_graphql_string(identity.did()),
    );
    let resp = node.execute(&create).await;
    assert!(!resp.has_errors(), "create_Skill: {:?}", resp.errors);
    let doc_id = created_skill_doc_id(resp.data.as_ref()).expect("created Skill _docID");

    let outcome = apply_control_update(identity.did(), "Skill", &doc_id, &mut view)
        .await
        .expect("apply skill create");
    assert_eq!(outcome, ControlUpdateOutcome::FullReload);
    let mut view = load_document_runtime_view(&node, identity.did())
        .await
        .expect("reloaded view after skill create");
    assert!(view.skills.contains_key("s-reload"), "skill added to view");

    let foreign = "mutation { create_Skill(input: { skill_id: \"s-foreign\", node_did: \"did:key:zOther\",  name: \"F\", enabled: true }) { _docID } }";
    let resp = node.execute(foreign).await;
    let foreign_doc_id = created_skill_doc_id(resp.data.as_ref()).expect("foreign Skill _docID");
    let outcome = apply_control_update(identity.did(), "Skill", &foreign_doc_id, &mut view)
        .await
        .expect("apply foreign skill");
    assert_eq!(outcome, ControlUpdateOutcome::FullReload);
    view = load_document_runtime_view(&node, identity.did())
        .await
        .unwrap();
    assert!(!view.skills.contains_key("s-foreign"));

    let delete =
        r#"mutation { delete_Skill(filter: { skill_id: { _eq: "s-reload" } }) { _docID } }"#;
    assert!(!node.execute(delete).await.has_errors());
    let outcome = apply_control_update(identity.did(), "Skill", &doc_id, &mut view)
        .await
        .expect("apply skill delete");
    assert_eq!(outcome, ControlUpdateOutcome::FullReload);
    let view = load_document_runtime_view(&node, identity.did())
        .await
        .expect("reloaded view after skill delete");
    assert!(
        !view.skills.contains_key("s-reload"),
        "skill removed from view"
    );
}

async fn seed_raw_tools(
    node: &defra_node::EmbeddedNode,
    node_did: &str,
    tools_id: &str,
    agent_target_ids: &[&str],
) {
    let target_list = agent_target_ids
        .iter()
        .map(|id| format!(r#""{}""#, escape_graphql_string(id)))
        .collect::<Vec<_>>()
        .join(", ");
    let mutation = format!(
        r#"mutation {{ update_Tools(filter: {{tools_id: {{_eq: "{tools_id}"}}, node_did: {{_eq: "{node_did}"}}}}, input: {{
            agents: {{ target_ids: [{target_list}], enabled: true }}
        }}) {{ _docID }} }}"#,
        tools_id = escape_graphql_string(tools_id),
        node_did = escape_graphql_string(node_did),
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create_Tools (raw) failed: {:?}",
        response.errors
    );
}

#[tokio::test]
async fn resolve_quarantines_behavior_with_empty_agent_target_id() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-invalid-tool-selection"));
    let node_did = identity.did();
    let default_agent_id = crate::default_agent_id_for_node(node_did);
    bind_default_agent_backend(&node, node_did, &default_agent_id).await;

    let tools_id = format!("{default_agent_id}:tools");
    seed_raw_tools(node.as_ref(), node_did, &tools_id, &[""]).await;

    let resolve_context = DocumentResolveContext::for_tests(
        identity.clone(),
        ToolCeiling::readonly(),
        crate::inference::backends::health::BackendHealthMap::new(),
    );
    let view = load_document_runtime_view(&node, node_did)
        .await
        .expect("document view should load");
    let snapshot = resolve_document_runtime_snapshot_from_view(&node, &resolve_context, &view)
        .await
        .expect("snapshot resolves; quarantine is per-agent");

    assert!(
        !snapshot.agents.contains_key(&default_agent_id),
        "agent with a blank agent target id must not be active"
    );
    let reason = snapshot
        .unavailable_agents
        .get(&default_agent_id)
        .expect("agent should be quarantined");
    assert!(
        reason.diagnostic.contains("target_ids"),
        "quarantine reason must name the invalid agent target ids, got: {}",
        reason.diagnostic
    );
}

#[tokio::test]
async fn resolve_quarantines_behavior_with_missing_local_agent_target() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-missing-agent-target"));
    let node_did = identity.did();
    let default_agent_id = crate::default_agent_id_for_node(node_did);
    bind_default_agent_backend(&node, node_did, &default_agent_id).await;

    let tools_id = format!("{default_agent_id}:tools");
    seed_raw_tools(
        node.as_ref(),
        node_did,
        &tools_id,
        &["target-missing-agent"],
    )
    .await;
    let target_mutation = format!(
        r#"mutation {{ create_AgentTarget(input: {{
            target_id: "target-missing-agent", node_did: "{node_did}",
            target_node_did: "{node_did}", agent_id: "missing-agent",
            name: "missing"
        }}) {{ _docID }} }}"#,
        node_did = escape_graphql_string(node_did),
    );
    let response = node.execute(&target_mutation).await;
    assert!(
        !response.has_errors(),
        "create_AgentTarget (raw) failed: {:?}",
        response.errors
    );

    let resolve_context = DocumentResolveContext::for_tests(
        identity.clone(),
        ToolCeiling::readonly(),
        crate::inference::backends::health::BackendHealthMap::new(),
    );
    let view = load_document_runtime_view(&node, node_did)
        .await
        .expect("document view should load");
    let snapshot = resolve_document_runtime_snapshot_from_view(&node, &resolve_context, &view)
        .await
        .expect("snapshot resolves; quarantine is per-agent");

    assert!(
        !snapshot.agents.contains_key(&default_agent_id),
        "agent with a missing local agent target must not be active"
    );
    let reason = snapshot
        .unavailable_agents
        .get(&default_agent_id)
        .expect("agent should be quarantined");
    assert!(
        reason.diagnostic.contains("missing-agent") && reason.diagnostic.contains("agents"),
        "quarantine reason must name the missing target agent, got: {}",
        reason.diagnostic
    );
}

async fn seed_automation(
    node: &Arc<defra_node::EmbeddedNode>,
    documents: Vec<(crate::Collection, serde_json::Value)>,
) {
    crate::store::config_client::ConfigAccess::transact_local_write(node, None, "test.replicated_automation", |txn| {
        let documents = &documents;
        Box::pin(async move {
            for (collection, input) in documents {
                let name = collection.graphql_type();
                txn.execute_with_variables(
                    &format!("mutation($input: {name}MutationInputArg!) {{ create_{name}(input: $input) {{ _docID }} }}"),
                    &serde_json::json!({"input":input}),
                ).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
    }).await.expect("seed replicated automation documents");
}

async fn create_task_bound(
    node: &Arc<defra_node::EmbeddedNode>,
    node_did: &str,
    task_id: &str,
    agent_id: &str,
    prompt_template: &str,
    enabled: bool,
) {
    seed_automation(
        node,
        vec![(
            crate::Collection::Task,
            serde_json::json!({
                "node_did": node_did,
                "task_id": task_id,
                "agent_id": agent_id,
                "prompt_template": prompt_template,
                "enabled": enabled,
            }),
        )],
    )
    .await;
}

async fn create_schedule_with_concurrency(
    node: &Arc<defra_node::EmbeddedNode>,
    node_did: &str,
    trigger_id: &str,
    task_id: &str,
    schedule_id: &str,
    interval_secs: i64,
    concurrency: &str,
) {
    seed_automation(
        node,
        vec![
            (
                crate::Collection::Schedule,
                serde_json::json!({
                    "node_did": node_did,
                    "schedule_id": schedule_id,
                    "cadence": {"kind": "interval", "interval_secs": interval_secs},
                }),
            ),
            (
                crate::Collection::Trigger,
                serde_json::json!({
                    "node_did": node_did,
                    "trigger_id": trigger_id,
                    "task_id": task_id,
                    "source": {"kind": "schedule", "schedule_id": schedule_id},
                    "enabled": true,
                    "concurrency": concurrency,
                }),
            ),
        ],
    )
    .await;
}

async fn create_event_trigger(
    node: &Arc<defra_node::EmbeddedNode>,
    node_did: &str,
    trigger_id: &str,
    task_id: &str,
    source_collection: &str,
    event_kind: &str,
    concurrency: &str,
) -> String {
    let event_source_id = format!("{trigger_id}:source");
    seed_automation(
        node,
        vec![
            (
                crate::Collection::EventSource,
                serde_json::json!({
                    "node_did": node_did,
                    "event_source_id": event_source_id,
                    "source_collection": source_collection,
                    "event_kind": event_kind,
                }),
            ),
            (
                crate::Collection::Trigger,
                serde_json::json!({
                    "node_did": node_did,
                    "trigger_id": trigger_id,
                    "task_id": task_id,
                    "source": {"kind": "event", "event_source_id": event_source_id},
                    "enabled": true,
                    "concurrency": concurrency,
                }),
            ),
        ],
    )
    .await;
    event_source_id
}

#[test]
fn retired_reserved_id_predicate_matches_both_namespaces() {
    let graph_trigger = format!("graph-trigger-{}-{}", "a".repeat(64), "b".repeat(16));
    assert!(is_retired_reserved_id(&graph_trigger));
    assert!(is_retired_reserved_id("pkg-x"));
    assert!(!is_retired_reserved_id("ordinary"));
}

#[tokio::test]
async fn retired_reserved_ids_are_never_visible() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-retired-ids"));
    let node_did = identity.did();
    bind_default_agent_backend(&node, node_did, &crate::default_agent_id_for_node(node_did)).await;

    let graph_trigger = format!("graph-trigger-{}-{}", "a".repeat(64), "b".repeat(16));
    let mut documents = Vec::new();
    for id in [graph_trigger.as_str(), "pkg-x", "ordinary"] {
        documents.push((
            crate::Collection::Trigger,
            serde_json::json!({
                "node_did": node_did, "trigger_id": id, "task_id": "task-1",
                "source": {"kind": "event", "event_source_id": id}, "enabled": false,
                "concurrency": "serial",
            }),
        ));
        documents.push((
            crate::Collection::EventSource,
            serde_json::json!({
                "node_did": node_did, "event_source_id": id,
                "source_collection": "CustomerSignup", "event_kind": "created",
            }),
        ));
        documents.push((
            crate::Collection::Callback,
            serde_json::json!({
                "node_did": node_did, "callback_id": id,
                "handler": {"kind": "built_in", "emitter": "create_workspace"},
            }),
        ));
        documents.push((
            crate::Collection::CallbackBinding,
            serde_json::json!({
                "node_did": node_did, "binding_id": id, "event_source_id": id,
                "callback_id": id,
            }),
        ));
    }
    seed_automation(&node, documents).await;

    let view = load_document_runtime_view(&node, node_did)
        .await
        .expect("document view should load");

    fn ids<T>(map: &RapidHashMap<String, DocumentRecord<T>>) -> Vec<&str> {
        let mut ids: Vec<&str> = map.keys().map(String::as_str).collect();
        ids.sort_unstable();
        ids
    }
    assert_eq!(ids(&view.triggers), ["ordinary"]);
    assert_eq!(ids(&view.event_sources), ["ordinary"]);

    node.shutdown().await;
}

#[tokio::test]
async fn usage_collection_write_never_reloads_the_runtime_view() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-usage"));
    bind_default_agent_backend(
        &node,
        identity.did(),
        &crate::default_agent_id_for_node(identity.did()),
    )
    .await;
    let mut view = load_document_runtime_view(&node, identity.did())
        .await
        .expect("initial document view");

    let outcome = apply_control_update(
        identity.did(),
        "ProviderAccountUsage",
        "opaque-usage-doc-id",
        &mut view,
    )
    .await
    .unwrap();
    assert_eq!(outcome, ControlUpdateOutcome::Irrelevant);

    node.shutdown().await;
}

#[tokio::test]
async fn load_document_runtime_view_populates_tasks_and_schedules() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-tasks-schedules"));
    let node_did = identity.did();
    bind_default_agent_backend(&node, node_did, &crate::default_agent_id_for_node(node_did)).await;

    create_task_bound(
        &node,
        node_did,
        "task-alpha",
        "agent-that-never-existed",
        "unused",
        false,
    )
    .await;
    create_task_bound(
        &node,
        node_did,
        "task-beta",
        "agent-that-never-existed",
        "unused",
        false,
    )
    .await;
    create_schedule_with_concurrency(
        &node,
        node_did,
        "trigger-schedule-alpha",
        "task-alpha",
        "schedule-alpha",
        60,
        "serial",
    )
    .await;

    let view = load_document_runtime_view(&node, node_did)
        .await
        .expect("document view should load");

    assert_eq!(view.tasks.len(), 2, "expected two Task documents");
    assert!(view.tasks.contains_key("task-alpha"));
    assert!(view.tasks.contains_key("task-beta"));

    assert_eq!(view.schedules.len(), 1, "expected one Schedule document");
    assert!(view.schedules.contains_key("schedule-alpha"));
    let schedule_record = view
        .schedules
        .get("schedule-alpha")
        .expect("schedule-alpha present");
    assert_eq!(
        schedule_record.value.cadence,
        crate::documents::ScheduleCadence::Interval { interval_secs: 60 },
        "schedule carries its interval cadence"
    );
    assert_eq!(view.triggers.len(), 1, "expected one Trigger document");
    let trigger_record = view
        .triggers
        .get("trigger-schedule-alpha")
        .expect("trigger present");
    assert_eq!(
        trigger_record.value.task_id, "task-alpha",
        "trigger references task-alpha"
    );
    match &trigger_record.value.source {
        crate::documents::TriggerSource::Schedule { schedule_id } => {
            assert_eq!(
                schedule_id, "schedule-alpha",
                "trigger references schedule-alpha"
            );
        }
        other => panic!("trigger must carry a schedule source, got {other:?}"),
    }
}

#[tokio::test]
async fn load_document_runtime_view_populates_event_triggers() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-event-triggers"));
    let node_did = identity.did();
    bind_default_agent_backend(&node, node_did, &crate::default_agent_id_for_node(node_did)).await;

    create_task_bound(
        &node,
        node_did,
        "task-1",
        "agent-that-never-existed",
        "unused",
        false,
    )
    .await;
    create_event_trigger(
        &node,
        node_did,
        "trig-1",
        "task-1",
        "CustomerSignup",
        "created",
        "serial",
    )
    .await;

    let view = load_document_runtime_view(&node, node_did)
        .await
        .expect("document view should load");

    assert_eq!(view.event_sources.len(), 1);
    let source_record = view
        .event_sources
        .get("trig-1:source")
        .expect("trig-1:source present in event_sources");
    assert_eq!(source_record.value.source_collection, "CustomerSignup");
    assert_eq!(source_record.value.event_kind.as_deref(), Some("created"));

    assert_eq!(view.triggers.len(), 1);
    let trigger_record = view
        .triggers
        .get("trig-1")
        .expect("trig-1 present in triggers");
    assert_eq!(trigger_record.value.task_id, "task-1");
    match &trigger_record.value.source {
        crate::documents::TriggerSource::Event { event_source_id } => {
            assert_eq!(event_source_id, "trig-1:source");
        }
        other => panic!("trigger must carry an event source, got {other:?}"),
    }
}

#[tokio::test]
async fn resolve_produces_active_schedule_when_task_and_behavior_exist() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-resolve-schedule-active"));
    let node_did = identity.did();
    let default_agent_id = crate::default_agent_id_for_node(node_did);
    bind_default_agent_backend(&node, node_did, &default_agent_id).await;

    create_task_bound(
        &node,
        node_did,
        "task-resolve-active",
        &default_agent_id,
        "do the thing",
        true,
    )
    .await;
    create_schedule_with_concurrency(
        &node,
        node_did,
        "trigger-resolve-active",
        "task-resolve-active",
        "schedule-resolve-active",
        60,
        "serial",
    )
    .await;

    let resolve_context = DocumentResolveContext::for_tests(
        identity.clone(),
        ToolCeiling::readonly(),
        crate::inference::backends::health::BackendHealthMap::new(),
    );
    let view = load_document_runtime_view(&node, node_did)
        .await
        .expect("document view should load");

    let snapshot = resolve_document_runtime_snapshot_from_view(&node, &resolve_context, &view)
        .await
        .expect("resolve should succeed");

    assert_eq!(
        snapshot.active_schedules.len(),
        1,
        "expected exactly one active schedule"
    );
    assert!(
        snapshot.unavailable_schedules.is_empty(),
        "expected no unavailable schedules, got {:?}",
        snapshot.unavailable_schedules
    );
    let resolved = snapshot
        .active_schedules
        .get("trigger-resolve-active")
        .expect("trigger-resolve-active present in active_schedules");
    assert_eq!(resolved.schedule_id, "schedule-resolve-active");
    assert_eq!(resolved.task_id, "task-resolve-active");
    assert_eq!(resolved.task.agent_id, default_agent_id);
    assert_eq!(resolved.task.prompt_template, "do the thing");
    assert_eq!(
        resolved.cadence,
        crate::runtime_snapshot::ScheduleCadence::Interval { interval_secs: 60 }
    );
    assert_eq!(
        resolved.concurrency,
        crate::documents::ConcurrencyMode::Serial
    );
}

#[tokio::test]
async fn resolve_produces_active_event_trigger_when_task_and_behavior_exist() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-resolve-trigger-active"));
    let node_did = identity.did();
    let default_agent_id = crate::default_agent_id_for_node(node_did);
    bind_default_agent_backend(&node, node_did, &default_agent_id).await;

    create_task_bound(
        &node,
        node_did,
        "task-trigger-active",
        &default_agent_id,
        "do the thing on event",
        true,
    )
    .await;
    create_event_trigger(
        &node,
        node_did,
        "trigger-active",
        "task-trigger-active",
        "CustomerSignup",
        "created",
        "serial",
    )
    .await;

    let resolve_context = DocumentResolveContext::for_tests(
        identity.clone(),
        ToolCeiling::readonly(),
        crate::inference::backends::health::BackendHealthMap::new(),
    );
    let view = load_document_runtime_view(&node, node_did)
        .await
        .expect("document view should load");

    let snapshot = resolve_document_runtime_snapshot_from_view(&node, &resolve_context, &view)
        .await
        .expect("resolve should succeed");

    assert_eq!(
        snapshot.active_event_triggers.len(),
        1,
        "expected exactly one active event trigger"
    );
    assert!(
        snapshot.unavailable_event_triggers.is_empty(),
        "expected no unavailable event triggers, got {:?}",
        snapshot.unavailable_event_triggers
    );
    let resolved = snapshot
        .active_event_triggers
        .get("trigger-active")
        .expect("trigger-active present in active_event_triggers");
    assert_eq!(resolved.trigger_id, "trigger-active");
    assert_eq!(resolved.task_id, "task-trigger-active");
    assert_eq!(resolved.task.agent_id, default_agent_id);
    assert_eq!(resolved.task.prompt_template, "do the thing on event");
    assert_eq!(resolved.source_collection, "CustomerSignup");
    assert_eq!(resolved.event_kind, "created");
    assert_eq!(
        resolved.concurrency,
        crate::documents::ConcurrencyMode::Serial
    );
}

#[tokio::test]
async fn resolve_marks_event_trigger_unavailable_when_task_missing_or_disabled() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-resolve-trigger-unavailable"));
    let node_did = identity.did();
    let default_agent_id = crate::default_agent_id_for_node(node_did);
    bind_default_agent_backend(&node, node_did, &default_agent_id).await;

    create_task_bound(
        &node,
        node_did,
        "task-trigger-disabled",
        &default_agent_id,
        "disabled task",
        false,
    )
    .await;
    create_event_trigger(
        &node,
        node_did,
        "trigger-task-disabled",
        "task-trigger-disabled",
        "CustomerSignup",
        "created",
        "serial",
    )
    .await;
    create_event_trigger(
        &node,
        node_did,
        "trigger-task-missing",
        "task-that-never-existed",
        "CustomerSignup",
        "created",
        "serial",
    )
    .await;

    let resolve_context = DocumentResolveContext::for_tests(
        identity.clone(),
        ToolCeiling::readonly(),
        crate::inference::backends::health::BackendHealthMap::new(),
    );
    let view = load_document_runtime_view(&node, identity.did())
        .await
        .expect("document view should load");

    let snapshot = resolve_document_runtime_snapshot_from_view(&node, &resolve_context, &view)
        .await
        .expect("resolve should succeed");

    assert!(
        snapshot.active_event_triggers.is_empty(),
        "expected no active event triggers, got {:?}",
        snapshot.active_event_triggers.keys().collect::<Vec<_>>()
    );
    assert!(
        snapshot
            .unavailable_event_triggers
            .contains("trigger-task-missing"),
        "missing-task trigger should be in unavailable_event_triggers: {:?}",
        snapshot.unavailable_event_triggers
    );
    assert!(
        snapshot
            .unavailable_event_triggers
            .contains("trigger-task-disabled"),
        "disabled-task trigger should be in unavailable_event_triggers: {:?}",
        snapshot.unavailable_event_triggers
    );
}

/// `source_collection` is interpolated into GraphQL identifier positions by
/// the event source, where escaping cannot apply. A trigger document whose
/// `source_collection` is not a valid GraphQL Name (query injection) or is
/// `__`-prefixed (introspection-reserved) must be quarantined at resolve
/// time, never activated. This covers documents that bypass self-config
/// validation entirely (e.g. replicated from a peer).
#[tokio::test]
async fn resolve_quarantines_event_trigger_with_invalid_source_collection() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-resolve-trigger-injection"));
    let node_did = identity.did();
    let default_agent_id = crate::default_agent_id_for_node(node_did);
    bind_default_agent_backend(&node, node_did, &default_agent_id).await;

    create_task_bound(
        &node,
        node_did,
        "task-trigger-injection",
        &default_agent_id,
        "enabled task",
        true,
    )
    .await;
    create_event_trigger(
        &node,
        node_did,
        "trigger-injection",
        "task-trigger-injection",
        "Msg(limit: 1) { _docID } Foo",
        "created",
        "serial",
    )
    .await;
    create_event_trigger(
        &node,
        node_did,
        "trigger-introspection",
        "task-trigger-injection",
        "__Type",
        "created",
        "serial",
    )
    .await;

    let resolve_context = DocumentResolveContext::for_tests(
        identity.clone(),
        ToolCeiling::readonly(),
        crate::inference::backends::health::BackendHealthMap::new(),
    );
    let view = load_document_runtime_view(&node, node_did)
        .await
        .expect("document view should load");

    let snapshot = resolve_document_runtime_snapshot_from_view(&node, &resolve_context, &view)
        .await
        .expect("resolve should succeed");

    assert!(
        snapshot.active_event_triggers.is_empty(),
        "no injection-shaped trigger may activate, got {:?}",
        snapshot.active_event_triggers.keys().collect::<Vec<_>>()
    );
    for trigger_id in ["trigger-injection", "trigger-introspection"] {
        assert!(
            snapshot.unavailable_event_triggers.contains(trigger_id),
            "{trigger_id} should be quarantined in unavailable_event_triggers: {:?}",
            snapshot.unavailable_event_triggers
        );
    }
}

#[tokio::test]
async fn resolve_marks_schedule_unavailable_when_task_missing_or_disabled() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-resolve-schedule-unavailable"));
    let node_did = identity.did();
    let default_agent_id = crate::default_agent_id_for_node(node_did);
    bind_default_agent_backend(&node, node_did, &default_agent_id).await;

    create_task_bound(
        &node,
        node_did,
        "task-resolve-disabled",
        &default_agent_id,
        "disabled task",
        false,
    )
    .await;
    create_schedule_with_concurrency(
        &node,
        node_did,
        "trigger-resolve-task-disabled",
        "task-resolve-disabled",
        "schedule-resolve-task-disabled",
        60,
        "serial",
    )
    .await;
    create_schedule_with_concurrency(
        &node,
        node_did,
        "trigger-resolve-task-missing",
        "task-that-never-existed",
        "schedule-resolve-task-missing",
        60,
        "serial",
    )
    .await;

    let resolve_context = DocumentResolveContext::for_tests(
        identity.clone(),
        ToolCeiling::readonly(),
        crate::inference::backends::health::BackendHealthMap::new(),
    );
    let view = load_document_runtime_view(&node, node_did)
        .await
        .expect("document view should load");

    let snapshot = resolve_document_runtime_snapshot_from_view(&node, &resolve_context, &view)
        .await
        .expect("resolve should succeed");

    assert!(
        snapshot.active_schedules.is_empty(),
        "expected no active schedules, got {:?}",
        snapshot.active_schedules.keys().collect::<Vec<_>>()
    );
    assert!(
        snapshot
            .unavailable_schedules
            .contains("trigger-resolve-task-missing"),
        "missing-task schedule should be in unavailable_schedules: {:?}",
        snapshot.unavailable_schedules
    );
    assert!(
        snapshot
            .unavailable_schedules
            .contains("trigger-resolve-task-disabled"),
        "disabled-task schedule should be in unavailable_schedules: {:?}",
        snapshot.unavailable_schedules
    );
}

#[tokio::test]
async fn resolve_populates_active_tasks_for_enabled_tasks_with_ready_behaviors() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-resolve-active-tasks"));
    let node_did = identity.did();
    let default_agent_id = crate::default_agent_id_for_node(node_did);
    bind_default_agent_backend(&node, node_did, &default_agent_id).await;

    create_task_bound(
        &node,
        node_did,
        "task-active",
        &default_agent_id,
        "hello",
        true,
    )
    .await;
    create_task_bound(
        &node,
        node_did,
        "task-disabled",
        &default_agent_id,
        "disabled",
        false,
    )
    .await;
    create_task_bound(
        &node,
        node_did,
        "task-missing-agent",
        "agent-that-never-existed",
        "orphan",
        true,
    )
    .await;

    let resolve_context = DocumentResolveContext::for_tests(
        identity.clone(),
        ToolCeiling::readonly(),
        crate::inference::backends::health::BackendHealthMap::new(),
    );
    let view = load_document_runtime_view(&node, node_did)
        .await
        .expect("document view should load");

    let snapshot = resolve_document_runtime_snapshot_from_view(&node, &resolve_context, &view)
        .await
        .expect("resolve should succeed");

    assert_eq!(
        snapshot.active_tasks.len(),
        1,
        "expected exactly one active task, got {:?}",
        snapshot.active_tasks.keys().collect::<Vec<_>>()
    );
    assert!(
        snapshot.active_tasks.contains_key("task-active"),
        "task-active should be in active_tasks: {:?}",
        snapshot.active_tasks.keys().collect::<Vec<_>>()
    );
    assert!(
        !snapshot.active_tasks.contains_key("task-disabled"),
        "disabled task must NOT be in active_tasks"
    );
    assert!(
        !snapshot.active_tasks.contains_key("task-missing-agent"),
        "task with unavailable agent must NOT be in active_tasks"
    );

    let resolved = snapshot
        .active_tasks
        .get("task-active")
        .expect("task-active present");
    assert_eq!(resolved.task_id, "task-active");
    assert_eq!(resolved.agent_id, default_agent_id);
    assert_eq!(resolved.prompt_template, "hello");
    assert!(resolved.output_schema_ref.is_none());
}

async fn bind_subscription_backend(
    node: &Arc<defra_node::EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
    provider: &str,
    endpoint: &str,
    model: &str,
) {
    use crate::store::config_client::{
        apply_desired_state_plan, read_desired_state_record_in_txn, ConfigAccess,
        DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    crate::test_support::install_test_agent(node, node_did, agent_id).await;
    crate::upsert_node(node, node_did, None, Some(agent_id), true, None)
        .await
        .unwrap();
    crate::inference::backends::registry::set_backend_probe_status(
        node,
        node_did,
        &format!("{agent_id}:backend"),
        "healthy",
    )
    .await
    .unwrap();
    ConfigAccess::transact_local_write(node, None, "test.subscription_backend", |txn| {
        Box::pin(async move {
            let mut documents = Vec::new();
            for (collection, id, patch) in [
                (crate::Collection::InferenceBackend, format!("{agent_id}:backend"), serde_json::json!({"provider_kind":provider, "endpoint":endpoint, "auth":{"kind":"node_oauth"}})),
                (crate::Collection::InferenceProfile, format!("{agent_id}:inference"), serde_json::json!({"model_name":model})),
            ] {
                let (_, mut value) = read_desired_state_record_in_txn(txn, collection, node_did, &id).await?.expect("installed canonical component");
                value.as_object_mut().unwrap().extend(patch.as_object().unwrap().clone());
                documents.push(DesiredStateApplyDocument {collection, add:value.clone(), update:value});
            }
            apply_desired_state_plan(txn, &DesiredStateApplyPlan::new(documents)?).await
        })
    }).await.unwrap();
}

async fn bind_default_agent_chatgpt_backend(
    node: &Arc<defra_node::EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
) {
    bind_subscription_backend(
        node,
        node_did,
        agent_id,
        "ChatGptCodex",
        "https://chatgpt.com/backend-api/codex",
        "gpt-5.2",
    )
    .await;
}

async fn insert_enabled_oauth_credential(node: &defra_node::EmbeddedNode, node_did: &str) {
    let credential = crate::inference::auth::credential::OAuthCredential {
        doc_id: None,
        credential_id: crate::inference::auth::credential::oauth_credential_id(
            node_did,
            crate::inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
        ),
        node_did: node_did.to_string(),
        provider: crate::inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER.to_string(),
        access_token: "access-token".to_string(),
        refresh_token: "refresh-token".to_string(),
        id_token: None,
        account_id: None,
        chatgpt_plan_type: None,
        is_fedramp: false,
        access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        last_refresh: None,
        enabled: true,
        account_ref: None,
        connected_at: None,
        provider_account_key: None,
        label: None,
    };
    let mutation =
        crate::inference::auth::credential::oauth_credential_upsert_mutation(&credential);
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "upsert OAuthCredential failed: {:?}",
        response.errors
    );
}

#[tokio::test]
async fn chatgpt_codex_behavior_without_credential_is_unavailable() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-chatgpt-nocred"));
    bind_default_agent_chatgpt_backend(
        &node,
        identity.did(),
        &crate::default_agent_id_for_node(identity.did()),
    )
    .await;
    let default_agent_id = crate::default_agent_id_for_node(identity.did());

    let resolve_context = DocumentResolveContext::for_tests(
        identity.clone(),
        ToolCeiling::readonly(),
        crate::inference::backends::health::BackendHealthMap::new(),
    );
    let view = load_document_runtime_view(&node, identity.did())
        .await
        .expect("document view");
    let snapshot = resolve_document_runtime_snapshot_from_view(&node, &resolve_context, &view)
        .await
        .expect("snapshot");

    assert!(
        !snapshot.agents.contains_key(&default_agent_id),
        "a ChatGptCodex agent_config without an OAuthCredential must not be runnable (it would hang \
         startup readiness building the client)"
    );
    let reason = snapshot
        .unavailable_agents
        .get(&default_agent_id)
        .expect("agent_config should be reported unavailable");
    assert!(
        reason
            .diagnostic
            .contains("requires enabled OAuthCredential"),
        "unavailable reason should identify the missing canonical credential: {}",
        reason.diagnostic
    );
}

#[tokio::test]
async fn runtime_snapshot_skips_an_unknown_provider_kind_backend() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-unknown-kind"));
    let did = identity.did();
    bind_default_agent_backend(&node, did, "known").await;
    crate::test_support::install_test_agent(&node, did, "future").await;
    let response = node
        .execute(&format!(
            r#"mutation {{ update_InferenceBackend(filter: {{node_did: {{_eq: "{}"}}, backend_id: {{_eq: "future:backend"}}}}, input: {{provider_kind: "FutureProviderKind"}}) {{ _docID }} }}"#,
            escape_graphql_string(did)
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let view = load_document_runtime_view(&node, did)
        .await
        .expect("an unknown provider kind must not fail the whole view");
    assert!(
        !view.has_unresolved_agent_references(),
        "an agent_config on an unknown kind is unavailable, not pending: {:?}",
        view.pending_visibility_details()
    );
    let resolve_context = DocumentResolveContext::for_tests(
        identity.clone(),
        ToolCeiling::readonly(),
        crate::inference::backends::health::BackendHealthMap::new(),
    );
    let snapshot = resolve_document_runtime_snapshot_from_view(&node, &resolve_context, &view)
        .await
        .expect("snapshot");
    assert!(
        snapshot.agents.contains_key("known"),
        "unavailable: {:?}",
        snapshot.unavailable_agents
    );
    let future = snapshot
        .unavailable_agents
        .get("future")
        .expect("future agent_config is reported unavailable");
    assert_eq!(
        future.public_reason,
        gents_protocol::node_readiness::AgentReadinessUnavailableReason::InferenceProfileInvalid
    );
    assert!(
        future.diagnostic.contains("FutureProviderKind"),
        "{}",
        future.diagnostic
    );
}

#[tokio::test]
async fn chatgpt_codex_behavior_with_enabled_credential_is_runnable() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-chatgpt-cred"));
    bind_default_agent_chatgpt_backend(
        &node,
        identity.did(),
        &crate::default_agent_id_for_node(identity.did()),
    )
    .await;
    insert_enabled_oauth_credential(node.as_ref(), identity.did()).await;
    let default_agent_id = crate::default_agent_id_for_node(identity.did());

    let resolve_context = DocumentResolveContext::for_tests(
        identity.clone(),
        ToolCeiling::readonly(),
        crate::inference::backends::health::BackendHealthMap::new(),
    );
    let view = load_document_runtime_view(&node, identity.did())
        .await
        .expect("document view");
    let snapshot = resolve_document_runtime_snapshot_from_view(&node, &resolve_context, &view)
        .await
        .expect("snapshot");

    assert!(
        snapshot.agents.contains_key(&default_agent_id),
        "a ChatGptCodex agent_config with an enabled OAuthCredential must be runnable; unavailable: {:?}",
        snapshot.unavailable_agents
    );
}

#[tokio::test]
async fn readiness_follows_the_backend_account_reference() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-account-ref"));
    let default_agent_id = crate::default_agent_id_for_node(identity.did());
    bind_default_agent_chatgpt_backend(&node, identity.did(), &default_agent_id).await;
    insert_enabled_oauth_credential(node.as_ref(), identity.did()).await;
    let resolve_context = DocumentResolveContext::for_tests(
        identity.clone(),
        ToolCeiling::readonly(),
        crate::inference::backends::health::BackendHealthMap::new(),
    );
    let mut view = load_document_runtime_view(&node, identity.did())
        .await
        .expect("document view");
    for backend in view.backends.values_mut() {
        backend.value.auth = crate::documents::BackendAuth::NodeOAuth {
            account_ref: Some("acct-x".to_string()),
        };
    }
    let ready = |view: DocumentRuntimeView| {
        let node = node.clone();
        let resolve_context = &resolve_context;
        let default_agent_id = default_agent_id.clone();
        async move {
            let snapshot =
                resolve_document_runtime_snapshot_from_view(&node, resolve_context, &view)
                    .await
                    .expect("snapshot");
            match snapshot.unavailable_agents.get(&default_agent_id) {
                None => true,
                Some(reason) => {
                    assert_eq!(
                        reason.public_reason,
                        gents_protocol::node_readiness::AgentReadinessUnavailableReason::CredentialsRequired
                    );
                    false
                }
            }
        }
    };

    assert!(
        !ready(view.clone()).await,
        "only the original account is stored, so acct-x does not resolve"
    );

    let original = view
        .oauth_credentials
        .values()
        .next()
        .expect("original row")
        .clone();
    let mut account = original.clone();
    account.value.credential_id = format!("{}:acct-x", original.value.credential_id);
    account.value.account_ref = Some("acct-x".to_string());
    view.oauth_credentials
        .insert(account.value.credential_id.clone(), account.clone());
    assert!(ready(view.clone()).await, "an enabled acct-x row resolves");

    account.value.enabled = false;
    view.oauth_credentials
        .insert(account.value.credential_id.clone(), account);
    assert!(!ready(view).await, "a disabled acct-x row never resolves");
}

#[tokio::test]
async fn disabling_or_removing_an_account_stops_only_its_behaviors() {
    use crate::inference::auth::credential::{set_account_enabled, store_sign_in};
    use crate::store::config_client::ConfigAccess;
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-account-lifecycle"));
    let did = identity.did().to_string();
    let default_agent_id = crate::default_agent_id_for_node(&did);
    bind_default_agent_claude_backend(&node, &did, &default_agent_id).await;
    let access = ConfigAccess::local(node.clone());
    let sign_in = |who: &str| {
        crate::inference::auth::claude::credential_from_login_tokens(
            did.clone(),
            crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
            &crate::inference::auth::claude::ClaudeLoginTokens {
                access_token: format!("access-{who}"),
                refresh_token: format!("refresh-{who}"),
                expires_in: Some(3600),
                scope: None,
                account_id: Some(format!("label-{who}")),
                organization_uuid: Some("org-1".into()),
                account_uuid: Some(format!("account-{who}")),
            },
            chrono::Utc::now(),
        )
    };
    store_sign_in(&access, sign_in("a"), None).await.unwrap();
    let b = store_sign_in(&access, sign_in("b"), None).await.unwrap();
    let b_ref = b.credential.account_ref.clone().expect("b has a reference");
    let resolve_context = DocumentResolveContext::for_tests(
        identity.clone(),
        ToolCeiling::readonly(),
        crate::inference::backends::health::BackendHealthMap::new(),
    );
    let ready_on = |account_ref: Option<String>| {
        let node = node.clone();
        let did = did.clone();
        let resolve_context = &resolve_context;
        let default_agent_id = default_agent_id.clone();
        async move {
            let mut view = load_document_runtime_view(&node, &did)
                .await
                .expect("document view");
            for backend in view.backends.values_mut() {
                backend.value.auth = crate::documents::BackendAuth::NodeOAuth {
                    account_ref: account_ref.clone(),
                };
            }
            let snapshot =
                resolve_document_runtime_snapshot_from_view(&node, resolve_context, &view)
                    .await
                    .expect("snapshot");
            !snapshot.unavailable_agents.contains_key(&default_agent_id)
        }
    };
    assert!(ready_on(None).await && ready_on(Some(b_ref.clone())).await);

    set_account_enabled(&access, &did, &b.credential.credential_id, false)
        .await
        .unwrap();
    assert!(
        !ready_on(Some(b_ref.clone())).await,
        "a disabled account stops"
    );
    assert!(ready_on(None).await, "the other account keeps running");

    access
        .transact_write("test.remove_account", |txn| {
            let did = did.clone();
            let credential_id = b.credential.credential_id.clone();
            Box::pin(async move {
                crate::inference::auth::credential::remove_account_in_txn(txn, &did, &credential_id)
                    .await
            })
        })
        .await
        .unwrap();
    assert!(!ready_on(Some(b_ref)).await, "a removed account stops");
    assert!(ready_on(None).await, "the other account keeps running");
}

#[tokio::test]
async fn an_unavailable_account_is_a_behavior_unavailable_rejection() {
    use crate::inference::auth::credential::{set_account_enabled, store_sign_in};
    use crate::store::config_client::ConfigAccess;
    use gents_protocol::node_readiness::is_behavior_unavailable_rejection;
    use gents_protocol::node_readiness::AgentReadinessUnavailableReason as Reason;
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-account-rejection"));
    let did = identity.did().to_string();
    let x = crate::default_agent_id_for_node(&did);
    bind_default_agent_claude_backend(&node, &did, &x).await;
    let access = ConfigAccess::local(node.clone());
    let sign_in = |who: &str| {
        crate::inference::auth::claude::credential_from_login_tokens(
            did.clone(),
            crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
            &crate::inference::auth::claude::ClaudeLoginTokens {
                access_token: format!("access-{who}"),
                refresh_token: format!("refresh-{who}"),
                expires_in: Some(3600),
                scope: None,
                account_id: Some(format!("label-{who}")),
                organization_uuid: Some("org-1".into()),
                account_uuid: Some(format!("account-{who}")),
            },
            chrono::Utc::now(),
        )
    };
    store_sign_in(&access, sign_in("a"), None).await.unwrap();
    let b = store_sign_in(&access, sign_in("b"), None).await.unwrap();
    let b_backend = format!(
        "{}-{}",
        crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
        b.credential
            .account_ref
            .as_deref()
            .expect("b has a reference")
    );
    crate::inference::backends::registry::set_backend_probe_status(
        &node, &did, &b_backend, "healthy",
    )
    .await
    .unwrap();
    let resolve_context = DocumentResolveContext::for_tests(
        identity.clone(),
        ToolCeiling::readonly(),
        crate::inference::backends::health::BackendHealthMap::new(),
    );
    let on_a = format!("{x}:inference");
    let on_b = format!("{x}:inference-b");
    let y = "agent_config-y".to_string();
    let reasons = |compaction_on_b: bool| {
        let node = node.clone();
        let did = did.clone();
        let resolve_context = &resolve_context;
        let (x, y, on_a, on_b, b_backend) = (
            x.clone(),
            y.clone(),
            on_a.clone(),
            on_b.clone(),
            b_backend.clone(),
        );
        async move {
            let mut view = load_document_runtime_view(&node, &did)
                .await
                .expect("document view");
            let mut b_profile = view.inference_profiles[&on_a].clone();
            b_profile.value.profile_id = on_b.clone();
            b_profile.value.backend_id = b_backend;
            view.inference_profiles.insert(on_b.clone(), b_profile);
            let mut y_record = view.agents[&x].clone();
            y_record.value.agent_id = y.clone();
            view.agents.insert(y.clone(), y_record);
            let x_record = view.agents.get_mut(&x).unwrap();
            if compaction_on_b {
                let compaction: crate::documents::CompactionConfig =
                    serde_json::from_value(serde_json::json!({
                        "compaction_id": "compaction-b", "node_did": did,
                        "inference_profile_id": on_b,
                    }))
                    .unwrap();
                view.compactions.insert(
                    "compaction-b".into(),
                    DocumentRecord {
                        doc_id: "compaction-b".into(),
                        value: compaction,
                    },
                );
                let mut context =
                    view.contexts[x_record.value.context_id.as_deref().unwrap()].clone();
                context.value.context_id = "context-x".into();
                context.value.compaction_id = Some("compaction-b".into());
                view.contexts.insert("context-x".into(), context);
                x_record.value.context_id = Some("context-x".into());
            } else {
                x_record.value.inference_profile_id = on_b;
            }
            let snapshot =
                resolve_document_runtime_snapshot_from_view(&node, resolve_context, &view)
                    .await
                    .expect("snapshot");
            let reason = |id: &str| {
                snapshot
                    .unavailable_agents
                    .get(id)
                    .map(|unavailable| unavailable.public_reason)
            };
            (reason(&x), reason(&y))
        }
    };
    assert_eq!(reasons(false).await, (None, None));
    assert_eq!(reasons(true).await, (None, None));

    let rejected_only_x = |(x_reason, y_reason): (Option<Reason>, Option<Reason>),
                           expected: Reason| {
        assert_eq!(x_reason, Some(expected));
        assert!(is_behavior_unavailable_rejection(expected.public_message()));
        assert_eq!(y_reason, None, "Y on A keeps running");
    };
    set_account_enabled(&access, &did, &b.credential.credential_id, false)
        .await
        .unwrap();
    rejected_only_x(reasons(false).await, Reason::CredentialsRequired);
    rejected_only_x(reasons(true).await, Reason::ToolConfigurationInvalid);

    access
        .transact_write("test.remove_account", |txn| {
            let did = did.clone();
            let credential_id = b.credential.credential_id.clone();
            Box::pin(async move {
                crate::inference::auth::credential::remove_account_in_txn(txn, &did, &credential_id)
                    .await
            })
        })
        .await
        .unwrap();
    rejected_only_x(reasons(false).await, Reason::CredentialsRequired);
    rejected_only_x(reasons(true).await, Reason::ToolConfigurationInvalid);
}

async fn bind_default_agent_claude_backend(
    node: &Arc<defra_node::EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
) {
    bind_subscription_backend(
        node,
        node_did,
        agent_id,
        "ClaudeCliSubscription",
        crate::inference::clients::claude_subscription::default_backend_endpoint(),
        "default",
    )
    .await;
}

#[tokio::test]
async fn claude_subscription_behavior_requires_enabled_credential() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-claude-cred"));
    bind_default_agent_claude_backend(
        &node,
        identity.did(),
        &crate::default_agent_id_for_node(identity.did()),
    )
    .await;
    let default_agent_id = crate::default_agent_id_for_node(identity.did());
    let resolve_context = DocumentResolveContext::for_tests(
        identity.clone(),
        ToolCeiling::readonly(),
        crate::inference::backends::health::BackendHealthMap::new(),
    );

    let view = load_document_runtime_view(&node, identity.did())
        .await
        .expect("document view");
    let snapshot = resolve_document_runtime_snapshot_from_view(&node, &resolve_context, &view)
        .await
        .expect("snapshot");
    assert!(
        !snapshot.agents.contains_key(&default_agent_id),
        "a ClaudeCliSubscription agent_config without an OAuthCredential must not be runnable"
    );
    let reason = snapshot
        .unavailable_agents
        .get(&default_agent_id)
        .expect("agent_config should be reported unavailable");
    assert_eq!(
        reason.public_reason,
        gents_protocol::node_readiness::AgentReadinessUnavailableReason::CredentialsRequired
    );
    assert!(
        reason
            .diagnostic
            .contains("requires enabled OAuthCredential"),
        "unavailable reason should identify the missing canonical credential: {}",
        reason.diagnostic
    );

    let credential = crate::inference::auth::claude::credential_from_login_tokens(
        identity.did(),
        crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
        &crate::inference::auth::claude::ClaudeLoginTokens {
            access_token: "access-token".to_string(),
            refresh_token: "refresh-token".to_string(),
            expires_in: Some(3600),
            scope: None,
            account_id: None,
            organization_uuid: None,
            account_uuid: None,
        },
        chrono::Utc::now(),
    );
    crate::inference::auth::credential::upsert_oauth_credential(&node, &credential)
        .await
        .expect("upsert credential");

    let view = load_document_runtime_view(&node, identity.did())
        .await
        .expect("document view");
    let snapshot = resolve_document_runtime_snapshot_from_view(&node, &resolve_context, &view)
        .await
        .expect("snapshot");
    assert!(
        snapshot.agents.contains_key(&default_agent_id),
        "a ClaudeCliSubscription agent_config with an enabled OAuthCredential must be runnable; unavailable: {:?}",
        snapshot.unavailable_agents
    );
    for state in [
        crate::inference::backends::health::BackendHealthState::CredentialsRequired,
        crate::inference::backends::health::BackendHealthState::SubscriptionNotEntitled,
        crate::inference::backends::health::BackendHealthState::Unhealthy,
        crate::inference::backends::health::BackendHealthState::Healthy,
    ] {
        for backend in view.backends.values() {
            resolve_context
                .backend_health
                .set_for_test(&backend.value.backend_id, state, 1)
                .await;
        }
        let snapshot = resolve_document_runtime_snapshot_from_view(&node, &resolve_context, &view)
            .await
            .unwrap();
        let expected = state.readiness_reason().or_else(|| state.blocks_routing().then_some(
            gents_protocol::node_readiness::AgentReadinessUnavailableReason::BackendTemporarilyUnavailable));
        assert_eq!(
            snapshot
                .unavailable_agents
                .get(&default_agent_id)
                .map(|agent| agent.public_reason),
            expected
        );
        assert_eq!(
            snapshot.agents.contains_key(&default_agent_id),
            expected.is_none()
        );
    }
}

#[tokio::test]
async fn apply_control_update_admits_chatgpt_behavior_when_credential_added() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-chatgpt-apply"));
    bind_default_agent_chatgpt_backend(
        &node,
        identity.did(),
        &crate::default_agent_id_for_node(identity.did()),
    )
    .await;
    let default_agent_id = crate::default_agent_id_for_node(identity.did());
    let resolve_context = DocumentResolveContext::for_tests(
        identity.clone(),
        ToolCeiling::readonly(),
        crate::inference::backends::health::BackendHealthMap::new(),
    );

    let mut view = load_document_runtime_view(&node, identity.did())
        .await
        .expect("document view");
    let before = resolve_document_runtime_snapshot_from_view(&node, &resolve_context, &view)
        .await
        .expect("snapshot");
    assert!(
        !before.agents.contains_key(&default_agent_id),
        "agent_config must start unavailable without a credential"
    );

    let credential = crate::inference::auth::credential::OAuthCredential {
        doc_id: None,
        credential_id: crate::inference::auth::credential::oauth_credential_id(
            identity.did(),
            crate::inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
        ),
        node_did: identity.did().to_string(),
        provider: crate::inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER.to_string(),
        access_token: "access-token".to_string(),
        refresh_token: "refresh-token".to_string(),
        id_token: None,
        account_id: None,
        chatgpt_plan_type: None,
        is_fedramp: false,
        access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        last_refresh: None,
        enabled: true,
        account_ref: None,
        connected_at: None,
        provider_account_key: None,
        label: None,
    };
    let doc_id = crate::inference::auth::credential::upsert_oauth_credential(&node, &credential)
        .await
        .expect("upsert credential");

    let outcome = apply_control_update(identity.did(), "OAuthCredential", &doc_id, &mut view)
        .await
        .expect("apply control update");
    assert_eq!(
        outcome,
        ControlUpdateOutcome::FullReload,
        "creating an OAuthCredential must drive a full reload reconcile"
    );

    let view = load_document_runtime_view(&node, identity.did())
        .await
        .expect("reloaded document view");
    let after = resolve_document_runtime_snapshot_from_view(&node, &resolve_context, &view)
        .await
        .expect("snapshot");
    assert!(
        after.agents.contains_key(&default_agent_id),
        "agent_config must become runnable once the credential exists; unavailable: {:?}",
        after.unavailable_agents
    );
}

fn finding_decl() -> crate::documents::WriteToolDecl {
    crate::documents::WriteToolDecl {
        notification: None,
        tool_name: "write_experiment_finding".to_string(),
        collection: "ExperimentFinding".to_string(),
        description: "Record a finding document for the next pipeline stage.".to_string(),
        fields: vec![
            crate::documents::WriteToolField {
                name: "job_id".to_string(),
                required: true,
                fill: None,
            },
            crate::documents::WriteToolField {
                name: "finding_id".to_string(),
                required: true,
                fill: None,
            },
            crate::documents::WriteToolField {
                name: "content".to_string(),
                required: true,
                fill: None,
            },
            crate::documents::WriteToolField {
                name: "stage".to_string(),
                required: true,
                fill: None,
            },
        ],
        output_obligation: None,
    }
}
#[tokio::test]
async fn apply_control_update_evicts_surface_when_ownership_moves_away() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-surface-revoke"));
    let node_did = identity.did();
    bind_default_agent_backend(&node, node_did, &crate::default_agent_id_for_node(node_did)).await;

    let mut view = load_document_runtime_view(&node, node_did)
        .await
        .expect("document view");

    let entries = gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
        "entries": [finding_decl()],
    }))
    .unwrap();
    let create = format!(
        r#"mutation {{ create_DatastoreToolSurface(input: {{
            surface_id: "experiment-writes", node_did: "{did}", enabled: true,
            entries: {entries}
        }}) {{ _docID }} }}"#,
        did = escape_graphql_string(node_did),
    );
    let resp = node.execute(&create).await;
    assert!(
        !resp.has_errors(),
        "create_DatastoreToolSurface: {:?}",
        resp.errors
    );
    let doc_id = created_skill_doc_id(resp.data.as_ref()).expect("created surface _docID");

    let outcome = apply_control_update(node_did, "DatastoreToolSurface", &doc_id, &mut view)
        .await
        .expect("apply surface create");
    assert_eq!(outcome, ControlUpdateOutcome::FullReload);
    let mut view = load_document_runtime_view(&node, node_did)
        .await
        .expect("reloaded view with surface");
    assert!(view
        .datastore_tool_surfaces
        .contains_key("experiment-writes"));

    let reassign = format!(
        r#"mutation {{ update_DatastoreToolSurface(
            docID: "{doc_id}", input: {{ node_did: "did:key:zOtherOwner" }}
        ) {{ _docID }} }}"#,
        doc_id = escape_graphql_string(&doc_id),
    );
    let resp = node.execute(&reassign).await;
    assert!(
        !resp.has_errors(),
        "update_DatastoreToolSurface: {:?}",
        resp.errors
    );

    let outcome = apply_control_update(node_did, "DatastoreToolSurface", &doc_id, &mut view)
        .await
        .expect("apply surface reassign");
    assert_eq!(outcome, ControlUpdateOutcome::FullReload);
    let view = load_document_runtime_view(&node, node_did)
        .await
        .expect("reloaded view after ownership move");
    assert!(
        view.datastore_tool_surfaces.is_empty(),
        "surface must be evicted once it is owned by another node"
    );
}
