use std::sync::Arc;

use super::*;
use crate::agent::DocumentResolveContext;
use crate::ensure_runtime_schemas;
use crate::graphql::escape_graphql_string;
use crate::identity::{AgentIdentity, KeyIdentity};
use crate::tool_surface::ToolCeiling;

async fn test_node() -> Arc<defra_node::EmbeddedNode> {
    Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap())
}

/// Extract a created document's `_docID` from a create/add mutation response,
/// regardless of the wrapper field name (`add_Skill`/`create_Skill`/…) or
/// whether the row is an object or a single-element array.
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

/// Install the canonical context/tools/profile/backend chain for a test
/// behavior through the shared desired-state owner and bind it as the
/// principal's explicit default. The chain derives its document IDs from
/// `behavior_id` (`<behavior_id>:context`, `:tools`, `:inference`,
/// `:backend`); there is no implicit principal default.
async fn bind_default_behavior_backend(
    node: &defra_node::EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
) {
    crate::test_support::install_test_behavior(node, agent_did, behavior_id).await;
    crate::upsert_agent_principal(node, agent_did, None, Some(behavior_id), true)
        .await
        .unwrap();
    crate::backend_registry::set_backend_probe_status(
        node,
        agent_did,
        &format!("{behavior_id}:backend"),
        "healthy",
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn load_document_runtime_view_includes_referenced_documents() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-load"));
    let default_behavior_id = crate::default_behavior_id_for_agent(identity.did());
    bind_default_behavior_backend(node.as_ref(), identity.did(), &default_behavior_id).await;

    let view = load_document_runtime_view(node.as_ref(), identity.did())
        .await
        .expect("document view should load");

    assert_eq!(view.principal.value.agent_did, identity.did());
    assert_eq!(
        view.principal.value.default_behavior_id.as_deref(),
        Some(default_behavior_id.as_str())
    );
    // Every referenced document of the default behavior loads into the view:
    // behavior -> context -> tools, behavior -> inference profile -> backend.
    assert!(view.behaviors.contains_key(&default_behavior_id));
    let behavior = &view.behaviors[&default_behavior_id].value;
    let context_id = behavior.context_id.as_deref().expect("context reference");
    assert!(view.contexts.contains_key(context_id));
    let context = &view.contexts[context_id].value;
    let tools_id = context.tools_id.as_deref().expect("tools reference");
    assert!(view.tools.contains_key(tools_id));
    let profile = &view.inference_profiles[&behavior.inference_profile_id].value;
    assert_eq!(profile.backend_id, format!("{default_behavior_id}:backend"));
    assert!(view
        .backends
        .contains_key(&format!("{default_behavior_id}:backend")));
}

#[tokio::test]
async fn apply_control_update_reconciles_tool_selection_via_doc_id() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-update"));
    let default_behavior_id = crate::default_behavior_id_for_agent(identity.did());
    bind_default_behavior_backend(node.as_ref(), identity.did(), &default_behavior_id).await;

    let resolve_context = DocumentResolveContext {
        identity: identity.clone(),
        tool_ceiling: ToolCeiling::readonly(),
        backend_health: crate::backend_health::BackendHealthMap::new(),
    };
    let mut view = load_document_runtime_view(node.as_ref(), identity.did())
        .await
        .expect("initial document view");

    // Load the initial view, then replace the behavior's Tools document with a
    // read-only file surface through the common Tools writer, then reload
    // through the shared desired-state owner.
    let tools_id = format!("{default_behavior_id}:tools");
    let tools_doc_id = view.tools[&tools_id].doc_id.clone();
    let mut tools_doc = view.tools[&tools_id].value.clone();
    tools_doc.host.get_or_insert_with(Default::default).files =
        Some(crate::document_config::FileTools {
            mode: crate::tool_surface::FileToolMode::ReadOnly,
            ..Default::default()
        });
    crate::config_client::write_tools_document(
        &crate::config_client::ConfigAccess::Local(node.clone()),
        &tools_doc,
    )
    .await
    .expect("write read-only Tools patch");

    let behavior_doc_id =
        crate::document_config::load_agent_behavior_record(node.as_ref(), &default_behavior_id)
            .await
            .unwrap()
            .expect("behavior record")
            .0;

    assert!(apply_control_update(
        node.as_ref(),
        identity.did(),
        "Tools",
        &tools_doc_id,
        &mut view,
    )
    .await
    .is_ok_and(|outcome| outcome == ControlUpdateOutcome::FullReload));
    assert!(apply_control_update(
        node.as_ref(),
        identity.did(),
        "AgentBehavior",
        &behavior_doc_id,
        &mut view,
    )
    .await
    .is_ok_and(|outcome| outcome == ControlUpdateOutcome::FullReload));

    // Hot reload must pull the patched Tools document, not retain the stale one.
    let reloaded = load_document_runtime_view(node.as_ref(), identity.did())
        .await
        .expect("reloaded document view");
    assert_eq!(
        reloaded.tools[&tools_id]
            .value
            .host
            .as_ref()
            .and_then(|host| host.files.as_ref())
            .map(|files| files.mode),
        Some(crate::tool_surface::FileToolMode::ReadOnly),
        "reloaded view must observe the read-only file-tools patch"
    );
    view = reloaded;

    let snapshot =
        resolve_document_runtime_snapshot_from_view(node.as_ref(), &resolve_context, &view)
            .await
            .expect("snapshot from updated document view");
    let tool_surface = snapshot
        .tool_surfaces
        .get(&default_behavior_id)
        .expect("tool surface for default behavior");
    let tool_names = tool_surface.tool_names();
    assert!(tool_names.contains(&"read_file".to_string()));
    assert!(tool_names.contains(&"list_files".to_string()));
}

/// Explicitly selected same-principal skills support progressive disclosure:
/// their name and description appear in
/// the prompt CATALOG (not its body), and `load_skill` returns the full body on
/// demand with a degrade note for tool_refs outside the behavior ceiling (D3).
#[tokio::test]
async fn resolve_composes_explicitly_selected_skill_into_prompt() {
    use crate::llm::tool::Tool;
    use crate::prompt::LayeredPromptBuilder;

    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-skill"));
    let default_behavior_id = crate::default_behavior_id_for_agent(identity.did());
    bind_default_behavior_backend(node.as_ref(), identity.did(), &default_behavior_id).await;

    // Principal-scoped skill referencing one in-ceiling tool (read_file) and one
    // ungranted tool (exercises the D3 degrade note).
    let create_skill = format!(
        r#"mutation {{ create_Skill(input: {{
            skill_id: "skill-research",
            agent_did: "{did}",
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

    let context_id = escape_graphql_string(&format!("{default_behavior_id}:context"));
    let did = escape_graphql_string(identity.did());
    let select = format!(
        r#"mutation {{ update_AgentContext(filter: {{context_id: {{_eq: "{context_id}"}}, agent_did: {{_eq: "{did}"}}}}, input: {{skill_ids: ["skill-research"]}}) {{_docID}} }}"#
    );
    let response = node.execute(&select).await;
    assert!(
        !response.has_errors(),
        "select skill: {:?}",
        response.errors
    );

    let resolve_context = DocumentResolveContext {
        identity: identity.clone(),
        tool_ceiling: ToolCeiling::readonly(),
        backend_health: crate::backend_health::BackendHealthMap::new(),
    };
    let view = load_document_runtime_view(node.as_ref(), identity.did())
        .await
        .expect("document view");
    let snapshot =
        resolve_document_runtime_snapshot_from_view(node.as_ref(), &resolve_context, &view)
            .await
            .expect("snapshot");

    let behavior = snapshot
        .behaviors
        .get(&default_behavior_id)
        .expect("resolved default behavior");
    assert_eq!(
        behavior.skills.len(),
        1,
        "explicitly selected skill must be available to the behavior"
    );
    assert_eq!(behavior.skills[0].skill_id, "skill-research");

    let tool_surface = snapshot
        .tool_surfaces
        .get(&default_behavior_id)
        .expect("tool surface");
    // The preamble holds the CATALOG (name + description + load_skill mandate),
    // NOT the skill body (progressive disclosure).
    let preamble = LayeredPromptBuilder::new(behavior.as_ref(), tool_surface.as_ref(), &[])
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

    // `load_skill` returns the full body on demand, with the D3 degrade note.
    // mcp_enabled=false: this read-only surface has no MCP, so an out-of-ceiling
    // ref is genuinely unavailable and must be flagged.
    let ceiling = crate::skills::skill_tool_ceiling(tool_surface.tool_names(), &[], false);
    let load_skill = crate::skills::LoadSkillTool::new(behavior.skills.clone(), ceiling);
    let loaded = load_skill
        .call(crate::skills::LoadSkillArgs {
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

/// Validates the raw GraphQL mutations the CLI `config skill` commands and the
/// Codex shim use against the live Skill schema: upsert (create/update),
/// update-by-filter (enable/disable), and delete-by-filter.
#[tokio::test]
async fn skill_crud_mutations_round_trip() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let did = "did:key:zSkillCrud";

    let create = format!(
        r#"mutation {{ upsert_Skill(
            filter: {{ skill_id: {{ _eq: "s1" }} }},
            add: {{ skill_id: "s1", agent_did: "{did}",  name: "S", tool_refs: ["read_file"], enabled: true }},
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

/// The control watcher must hot-reload Skill changes (#340): a Skill
/// create/delete drives `apply_control_update` to `FullReload` and the
/// reloaded view drops/raises the row, so a running agent picks up skills
/// without a restart. Foreign-owned rows stay irrelevant.
#[tokio::test]
async fn apply_control_update_hot_reloads_skill() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-skill-reload"));
    let default_behavior_id = crate::default_behavior_id_for_agent(identity.did());
    bind_default_behavior_backend(node.as_ref(), identity.did(), &default_behavior_id).await;

    let mut view = load_document_runtime_view(node.as_ref(), identity.did())
        .await
        .expect("document view");
    assert!(view.skills.is_empty(), "no skills before create");

    let create = format!(
        r#"mutation {{ create_Skill(input: {{
            skill_id: "s-reload", agent_did: "{did}",
            name: "Reload", instructions: "Reload me.", enabled: true
        }}) {{ _docID }} }}"#,
        did = escape_graphql_string(identity.did()),
    );
    let resp = node.execute(&create).await;
    assert!(!resp.has_errors(), "create_Skill: {:?}", resp.errors);
    let doc_id = created_skill_doc_id(resp.data.as_ref()).expect("created Skill _docID");

    let outcome = apply_control_update(node.as_ref(), identity.did(), "Skill", &doc_id, &mut view)
        .await
        .expect("apply skill create");
    assert_eq!(outcome, ControlUpdateOutcome::FullReload);
    let mut view = load_document_runtime_view(node.as_ref(), identity.did())
        .await
        .expect("reloaded view after skill create");
    assert!(view.skills.contains_key("s-reload"), "skill added to view");

    // Any config notification reloads the candidate; the scoped loader must
    // still exclude skills owned by another principal.
    let foreign = "mutation { create_Skill(input: { skill_id: \"s-foreign\", agent_did: \"did:key:zOther\",  name: \"F\", enabled: true }) { _docID } }";
    let resp = node.execute(foreign).await;
    let foreign_doc_id = created_skill_doc_id(resp.data.as_ref()).expect("foreign Skill _docID");
    let outcome = apply_control_update(
        node.as_ref(),
        identity.did(),
        "Skill",
        &foreign_doc_id,
        &mut view,
    )
    .await
    .expect("apply foreign skill");
    assert_eq!(outcome, ControlUpdateOutcome::FullReload);
    view = load_document_runtime_view(node.as_ref(), identity.did())
        .await
        .unwrap();
    assert!(!view.skills.contains_key("s-foreign"));

    // Deletion drops it from the view after the full reload.
    let delete =
        r#"mutation { delete_Skill(filter: { skill_id: { _eq: "s-reload" } }) { _docID } }"#;
    assert!(!node.execute(delete).await.has_errors());
    let outcome = apply_control_update(node.as_ref(), identity.did(), "Skill", &doc_id, &mut view)
        .await
        .expect("apply skill delete");
    assert_eq!(outcome, ControlUpdateOutcome::FullReload);
    let view = load_document_runtime_view(node.as_ref(), identity.did())
        .await
        .expect("reloaded view after skill delete");
    assert!(
        !view.skills.contains_key("s-reload"),
        "skill removed from view"
    );
}

/// Seed a `Tools` row directly with raw GraphQL — the replicated-row path:
/// documents can arrive without passing self-config validation (e.g.
/// replicated from a peer), so scoped resolution must fail closed on them.
async fn seed_raw_tools(
    node: &defra_node::EmbeddedNode,
    agent_did: &str,
    tools_id: &str,
    subagent_target_ids: &[&str],
) {
    let target_list = subagent_target_ids
        .iter()
        .map(|id| format!(r#""{}""#, escape_graphql_string(id)))
        .collect::<Vec<_>>()
        .join(", ");
    let mutation = format!(
        r#"mutation {{ update_Tools(filter: {{tools_id: {{_eq: "{tools_id}"}}, agent_did: {{_eq: "{agent_did}"}}}}, input: {{
            subagents: {{ target_ids: [{target_list}], spawn_enabled: true }}
        }}) {{ _docID }} }}"#,
        tools_id = escape_graphql_string(tools_id),
        agent_did = escape_graphql_string(agent_did),
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create_Tools (raw) failed: {:?}",
        response.errors
    );
}

#[tokio::test]
async fn resolve_quarantines_behavior_with_empty_subagent_target_id() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-invalid-tool-selection"));
    let agent_did = identity.did();
    let default_behavior_id = crate::default_behavior_id_for_agent(agent_did);
    bind_default_behavior_backend(node.as_ref(), agent_did, &default_behavior_id).await;

    // The Tools row carries an empty target id — valid at the DB level but
    // invalid per Tools::validate(); scope resolution must quarantine the
    // behavior, never activate it.
    let tools_id = format!("{default_behavior_id}:tools");
    seed_raw_tools(node.as_ref(), agent_did, &tools_id, &[""]).await;

    let resolve_context = DocumentResolveContext {
        identity: identity.clone(),
        tool_ceiling: ToolCeiling::readonly(),
        backend_health: crate::backend_health::BackendHealthMap::new(),
    };
    let view = load_document_runtime_view(node.as_ref(), agent_did)
        .await
        .expect("document view should load");
    let snapshot =
        resolve_document_runtime_snapshot_from_view(node.as_ref(), &resolve_context, &view)
            .await
            .expect("snapshot resolves; quarantine is per-behavior");

    assert!(
        !snapshot.behaviors.contains_key(&default_behavior_id),
        "behavior with a blank subagent target id must not be active"
    );
    let reason = snapshot
        .unavailable_behaviors
        .get(&default_behavior_id)
        .expect("behavior should be quarantined");
    assert!(
        reason.diagnostic.contains("target_ids"),
        "quarantine reason must name the invalid subagent target ids, got: {}",
        reason.diagnostic
    );
}

#[tokio::test]
async fn resolve_quarantines_behavior_with_missing_local_subagent_target() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-missing-subagent-target"));
    let agent_did = identity.did();
    let default_behavior_id = crate::default_behavior_id_for_agent(agent_did);
    bind_default_behavior_backend(node.as_ref(), agent_did, &default_behavior_id).await;

    // A LOCAL target (own agent_did) naming a behavior that does not exist
    // locally must be rejected; remote targets are exempt (delegation
    // admission owns cross-principal targets). Both rows are seeded raw to
    // model peer-replicated documents that skipped self-config validation.
    let tools_id = format!("{default_behavior_id}:tools");
    seed_raw_tools(
        node.as_ref(),
        agent_did,
        &tools_id,
        &["target-missing-behavior"],
    )
    .await;
    let target_mutation = format!(
        r#"mutation {{ create_SubagentTarget(input: {{
            target_id: "target-missing-behavior", agent_did: "{agent_did}",
            target_agent_did: "{agent_did}", behavior_id: "missing-behavior",
            name: "missing"
        }}) {{ _docID }} }}"#,
        agent_did = escape_graphql_string(agent_did),
    );
    let response = node.execute(&target_mutation).await;
    assert!(
        !response.has_errors(),
        "create_SubagentTarget (raw) failed: {:?}",
        response.errors
    );

    let resolve_context = DocumentResolveContext {
        identity: identity.clone(),
        tool_ceiling: ToolCeiling::readonly(),
        backend_health: crate::backend_health::BackendHealthMap::new(),
    };
    let view = load_document_runtime_view(node.as_ref(), agent_did)
        .await
        .expect("document view should load");
    let snapshot =
        resolve_document_runtime_snapshot_from_view(node.as_ref(), &resolve_context, &view)
            .await
            .expect("snapshot resolves; quarantine is per-behavior");

    assert!(
        !snapshot.behaviors.contains_key(&default_behavior_id),
        "behavior with a missing local subagent target must not be active"
    );
    let reason = snapshot
        .unavailable_behaviors
        .get(&default_behavior_id)
        .expect("behavior should be quarantined");
    assert!(
        reason.diagnostic.contains("missing-behavior") && reason.diagnostic.contains("behaviors"),
        "quarantine reason must name the missing target behavior, got: {}",
        reason.diagnostic
    );
}

/// Model replicated automation rows, including deliberately invalid references.
/// These loader tests bypass installer admission so they exercise quarantine.
async fn seed_automation(
    node: &defra_node::EmbeddedNode,
    documents: Vec<(crate::Collection, serde_json::Value)>,
) {
    crate::config_client::ConfigAccess::transact_local(node, None, "test.replicated_automation", |txn| {
        let documents = &documents;
        Box::pin(async move {
            for (collection, input) in documents {
                let name = collection.graphql_type();
                txn.execute_with_variables(
                    &format!("mutation($input: {name}MutationInputArg!) {{ create_{name}(input: $input) {{ _docID }} }}"),
                    &serde_json::json!({"input":input}),
                ).await?;
            }
            Ok(())
        })
    }).await.expect("seed replicated automation documents");
}

async fn create_task_bound(
    node: &defra_node::EmbeddedNode,
    agent_did: &str,
    task_id: &str,
    behavior_id: &str,
    prompt_template: &str,
    enabled: bool,
) {
    seed_automation(
        node,
        vec![(
            crate::Collection::Task,
            serde_json::json!({
                "agent_did": agent_did,
                "task_id": task_id,
                "behavior_id": behavior_id,
                "prompt_template": prompt_template,
                "enabled": enabled,
            }),
        )],
    )
    .await;
}

async fn create_schedule_with_concurrency(
    node: &defra_node::EmbeddedNode,
    agent_did: &str,
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
                    "agent_did": agent_did,
                    "schedule_id": schedule_id,
                    "cadence": {"kind": "interval", "interval_secs": interval_secs},
                }),
            ),
            (
                crate::Collection::Trigger,
                serde_json::json!({
                    "agent_did": agent_did,
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
    node: &defra_node::EmbeddedNode,
    agent_did: &str,
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
                    "agent_did": agent_did,
                    "event_source_id": event_source_id,
                    "source_collection": source_collection,
                    "event_kind": event_kind,
                }),
            ),
            (
                crate::Collection::Trigger,
                serde_json::json!({
                    "agent_did": agent_did,
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

/// Reserved-graph and unresolved collection notifications drive a full reload
/// through the shared owner; they never drop the runtime view silently.
#[tokio::test]
async fn apply_control_update_full_reloads_reserved_graph_triggers() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-graph-trigger"));
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        &crate::default_behavior_id_for_agent(identity.did()),
    )
    .await;
    let mut view = load_document_runtime_view(node.as_ref(), identity.did())
        .await
        .expect("initial document view");

    // A GraphRevision notification is a reserved graph collection: the watcher
    // must choose FullReload regardless of view-local visibility.
    let outcome = apply_control_update(
        node.as_ref(),
        identity.did(),
        "GraphRevision",
        "opaque-graph-revision-doc-id",
        &mut view,
    )
    .await
    .unwrap();
    assert_eq!(outcome, ControlUpdateOutcome::FullReload);

    // An unknown physical collection identifier also forces a full resync
    // rather than being ignored.
    let outcome = apply_control_update(
        node.as_ref(),
        identity.did(),
        "opaque-physical-collection-id",
        "opaque-doc-id",
        &mut view,
    )
    .await
    .unwrap();
    assert_eq!(outcome, ControlUpdateOutcome::FullReload);

    node.shutdown().await;
}

#[tokio::test]
async fn load_document_runtime_view_populates_tasks_and_schedules() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-tasks-schedules"));
    let agent_did = identity.did();
    bind_default_behavior_backend(
        node.as_ref(),
        agent_did,
        &crate::default_behavior_id_for_agent(agent_did),
    )
    .await;

    create_task_bound(
        node.as_ref(),
        agent_did,
        "task-alpha",
        "behavior-that-never-existed",
        "unused",
        false,
    )
    .await;
    create_task_bound(
        node.as_ref(),
        agent_did,
        "task-beta",
        "behavior-that-never-existed",
        "unused",
        false,
    )
    .await;
    create_schedule_with_concurrency(
        node.as_ref(),
        agent_did,
        "trigger-schedule-alpha",
        "task-alpha",
        "schedule-alpha",
        60,
        "serial",
    )
    .await;

    let view = load_document_runtime_view(node.as_ref(), agent_did)
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
        crate::document_config::ScheduleCadence::Interval { interval_secs: 60 },
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
        crate::document_config::TriggerSource::Schedule { schedule_id } => {
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
    let agent_did = identity.did();
    bind_default_behavior_backend(
        node.as_ref(),
        agent_did,
        &crate::default_behavior_id_for_agent(agent_did),
    )
    .await;

    create_task_bound(
        node.as_ref(),
        agent_did,
        "task-1",
        "behavior-that-never-existed",
        "unused",
        false,
    )
    .await;
    create_event_trigger(
        node.as_ref(),
        agent_did,
        "trig-1",
        "task-1",
        "CustomerSignup",
        "created",
        "serial",
    )
    .await;

    let view = load_document_runtime_view(node.as_ref(), agent_did)
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
        crate::document_config::TriggerSource::Event { event_source_id } => {
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
    let agent_did = identity.did();
    let default_behavior_id = crate::default_behavior_id_for_agent(agent_did);
    bind_default_behavior_backend(node.as_ref(), agent_did, &default_behavior_id).await;

    create_task_bound(
        node.as_ref(),
        agent_did,
        "task-resolve-active",
        &default_behavior_id,
        "do the thing",
        true,
    )
    .await;
    create_schedule_with_concurrency(
        node.as_ref(),
        agent_did,
        "trigger-resolve-active",
        "task-resolve-active",
        "schedule-resolve-active",
        60,
        "serial",
    )
    .await;

    let resolve_context = DocumentResolveContext {
        identity: identity.clone(),
        tool_ceiling: ToolCeiling::readonly(),
        backend_health: crate::backend_health::BackendHealthMap::new(),
    };
    let view = load_document_runtime_view(node.as_ref(), agent_did)
        .await
        .expect("document view should load");

    let snapshot =
        resolve_document_runtime_snapshot_from_view(node.as_ref(), &resolve_context, &view)
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
    // Active schedules are keyed by the trigger that fires them.
    let resolved = snapshot
        .active_schedules
        .get("trigger-resolve-active")
        .expect("trigger-resolve-active present in active_schedules");
    assert_eq!(resolved.schedule_id, "schedule-resolve-active");
    assert_eq!(resolved.task_id, "task-resolve-active");
    assert_eq!(resolved.task.behavior_id, default_behavior_id);
    assert_eq!(resolved.task.prompt_template, "do the thing");
    assert_eq!(
        resolved.cadence,
        crate::runtime_snapshot::ScheduleCadence::Interval { interval_secs: 60 }
    );
    assert_eq!(
        resolved.concurrency,
        crate::document_config::ConcurrencyMode::Serial
    );
}

#[tokio::test]
async fn resolve_produces_active_event_trigger_when_task_and_behavior_exist() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-resolve-trigger-active"));
    let agent_did = identity.did();
    let default_behavior_id = crate::default_behavior_id_for_agent(agent_did);
    bind_default_behavior_backend(node.as_ref(), agent_did, &default_behavior_id).await;

    create_task_bound(
        node.as_ref(),
        agent_did,
        "task-trigger-active",
        &default_behavior_id,
        "do the thing on event",
        true,
    )
    .await;
    create_event_trigger(
        node.as_ref(),
        agent_did,
        "trigger-active",
        "task-trigger-active",
        "CustomerSignup",
        "created",
        "serial",
    )
    .await;

    let resolve_context = DocumentResolveContext {
        identity: identity.clone(),
        tool_ceiling: ToolCeiling::readonly(),
        backend_health: crate::backend_health::BackendHealthMap::new(),
    };
    let view = load_document_runtime_view(node.as_ref(), agent_did)
        .await
        .expect("document view should load");

    let snapshot =
        resolve_document_runtime_snapshot_from_view(node.as_ref(), &resolve_context, &view)
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
    assert_eq!(resolved.task.behavior_id, default_behavior_id);
    assert_eq!(resolved.task.prompt_template, "do the thing on event");
    assert_eq!(resolved.source_collection, "CustomerSignup");
    assert_eq!(resolved.event_kind, "created");
    assert_eq!(
        resolved.concurrency,
        crate::document_config::ConcurrencyMode::Serial
    );
}

#[tokio::test]
async fn resolve_marks_event_trigger_unavailable_when_task_missing_or_disabled() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-resolve-trigger-unavailable"));
    let agent_did = identity.did();
    let default_behavior_id = crate::default_behavior_id_for_agent(agent_did);
    bind_default_behavior_backend(node.as_ref(), agent_did, &default_behavior_id).await;

    // Disabled task — trigger should be unavailable even though the task
    // document exists.
    create_task_bound(
        node.as_ref(),
        agent_did,
        "task-trigger-disabled",
        &default_behavior_id,
        "disabled task",
        false,
    )
    .await;
    create_event_trigger(
        node.as_ref(),
        agent_did,
        "trigger-task-disabled",
        "task-trigger-disabled",
        "CustomerSignup",
        "created",
        "serial",
    )
    .await;
    // Trigger whose task_id does not match any Task document.
    create_event_trigger(
        node.as_ref(),
        agent_did,
        "trigger-task-missing",
        "task-that-never-existed",
        "CustomerSignup",
        "created",
        "serial",
    )
    .await;

    let resolve_context = DocumentResolveContext {
        identity: identity.clone(),
        tool_ceiling: ToolCeiling::readonly(),
        backend_health: crate::backend_health::BackendHealthMap::new(),
    };
    let view = load_document_runtime_view(node.as_ref(), identity.did())
        .await
        .expect("document view should load");

    let snapshot =
        resolve_document_runtime_snapshot_from_view(node.as_ref(), &resolve_context, &view)
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
    let agent_did = identity.did();
    let default_behavior_id = crate::default_behavior_id_for_agent(agent_did);
    bind_default_behavior_backend(node.as_ref(), agent_did, &default_behavior_id).await;

    create_task_bound(
        node.as_ref(),
        agent_did,
        "task-trigger-injection",
        &default_behavior_id,
        "enabled task",
        true,
    )
    .await;
    create_event_trigger(
        node.as_ref(),
        agent_did,
        "trigger-injection",
        "task-trigger-injection",
        "Msg(limit: 1) { _docID } Foo",
        "created",
        "serial",
    )
    .await;
    create_event_trigger(
        node.as_ref(),
        agent_did,
        "trigger-introspection",
        "task-trigger-injection",
        "__Type",
        "created",
        "serial",
    )
    .await;

    let resolve_context = DocumentResolveContext {
        identity: identity.clone(),
        tool_ceiling: ToolCeiling::readonly(),
        backend_health: crate::backend_health::BackendHealthMap::new(),
    };
    let view = load_document_runtime_view(node.as_ref(), agent_did)
        .await
        .expect("document view should load");

    let snapshot =
        resolve_document_runtime_snapshot_from_view(node.as_ref(), &resolve_context, &view)
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
    let agent_did = identity.did();
    let default_behavior_id = crate::default_behavior_id_for_agent(agent_did);
    bind_default_behavior_backend(node.as_ref(), agent_did, &default_behavior_id).await;

    // Disabled task — schedule should be unavailable even though the task
    // document exists.
    create_task_bound(
        node.as_ref(),
        agent_did,
        "task-resolve-disabled",
        &default_behavior_id,
        "disabled task",
        false,
    )
    .await;
    create_schedule_with_concurrency(
        node.as_ref(),
        agent_did,
        "trigger-resolve-task-disabled",
        "task-resolve-disabled",
        "schedule-resolve-task-disabled",
        60,
        "serial",
    )
    .await;
    // Schedule whose task_id does not match any Task document.
    create_schedule_with_concurrency(
        node.as_ref(),
        agent_did,
        "trigger-resolve-task-missing",
        "task-that-never-existed",
        "schedule-resolve-task-missing",
        60,
        "serial",
    )
    .await;

    let resolve_context = DocumentResolveContext {
        identity: identity.clone(),
        tool_ceiling: ToolCeiling::readonly(),
        backend_health: crate::backend_health::BackendHealthMap::new(),
    };
    let view = load_document_runtime_view(node.as_ref(), agent_did)
        .await
        .expect("document view should load");

    let snapshot =
        resolve_document_runtime_snapshot_from_view(node.as_ref(), &resolve_context, &view)
            .await
            .expect("resolve should succeed");

    assert!(
        snapshot.active_schedules.is_empty(),
        "expected no active schedules, got {:?}",
        snapshot.active_schedules.keys().collect::<Vec<_>>()
    );
    // Unavailable schedules are keyed by their firing trigger.
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
    let agent_did = identity.did();
    let default_behavior_id = crate::default_behavior_id_for_agent(agent_did);
    bind_default_behavior_backend(node.as_ref(), agent_did, &default_behavior_id).await;

    // Enabled task bound to the ready default behavior — should land in
    // active_tasks.
    create_task_bound(
        node.as_ref(),
        agent_did,
        "task-active",
        &default_behavior_id,
        "hello",
        true,
    )
    .await;
    // Disabled task — should NOT land in active_tasks even though its
    // behavior is ready.
    create_task_bound(
        node.as_ref(),
        agent_did,
        "task-disabled",
        &default_behavior_id,
        "disabled",
        false,
    )
    .await;
    // Task bound to a behavior_id that does not resolve to any behavior
    // document — should NOT land in active_tasks.
    create_task_bound(
        node.as_ref(),
        agent_did,
        "task-missing-behavior",
        "behavior-that-never-existed",
        "orphan",
        true,
    )
    .await;

    let resolve_context = DocumentResolveContext {
        identity: identity.clone(),
        tool_ceiling: ToolCeiling::readonly(),
        backend_health: crate::backend_health::BackendHealthMap::new(),
    };
    let view = load_document_runtime_view(node.as_ref(), agent_did)
        .await
        .expect("document view should load");

    let snapshot =
        resolve_document_runtime_snapshot_from_view(node.as_ref(), &resolve_context, &view)
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
        !snapshot.active_tasks.contains_key("task-missing-behavior"),
        "task with unavailable behavior must NOT be in active_tasks"
    );

    let resolved = snapshot
        .active_tasks
        .get("task-active")
        .expect("task-active present");
    assert_eq!(resolved.task_id, "task-active");
    assert_eq!(resolved.behavior_id, default_behavior_id);
    assert_eq!(resolved.prompt_template, "hello");
    assert!(resolved.output_schema_ref.is_none());
}

/// Install the canonical chain for `behavior_id` and bind it as the principal's
/// explicit default, then swap the chain's InferenceBackend to the ChatGptCodex
/// provider so resolution requires a ChatGPT OAuthCredential.
async fn bind_subscription_backend(
    node: &defra_node::EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
    provider: &str,
    endpoint: &str,
    model: &str,
) {
    use crate::config_client::{
        apply_desired_state_plan, read_desired_state_record_in_txn, ConfigAccess,
        DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    crate::test_support::install_test_behavior(node, agent_did, behavior_id).await;
    crate::upsert_agent_principal(node, agent_did, None, Some(behavior_id), true)
        .await
        .unwrap();
    crate::backend_registry::set_backend_probe_status(
        node,
        agent_did,
        &format!("{behavior_id}:backend"),
        "healthy",
    )
    .await
    .unwrap();
    ConfigAccess::transact_local(node, None, "test.subscription_backend", |txn| {
        Box::pin(async move {
            let mut documents = Vec::new();
            for (collection, id, patch) in [
                (crate::Collection::InferenceBackend, format!("{behavior_id}:backend"), serde_json::json!({"provider_kind":provider, "endpoint":endpoint, "auth":{"kind":"principal_oauth"}})),
                (crate::Collection::InferenceProfile, format!("{behavior_id}:inference"), serde_json::json!({"model_name":model})),
            ] {
                let (_, mut value) = read_desired_state_record_in_txn(txn, collection, agent_did, &id).await?.expect("installed canonical component");
                value.as_object_mut().unwrap().extend(patch.as_object().unwrap().clone());
                documents.push(DesiredStateApplyDocument {collection, add:value.clone(), update:value});
            }
            apply_desired_state_plan(txn, &DesiredStateApplyPlan::new(documents)?).await
        })
    }).await.unwrap();
}

async fn bind_default_behavior_chatgpt_backend(
    node: &defra_node::EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
) {
    bind_subscription_backend(
        node,
        agent_did,
        behavior_id,
        "ChatGptCodex",
        "https://chatgpt.com/backend-api/codex",
        "gpt-5.2",
    )
    .await;
}

async fn insert_enabled_oauth_credential(node: &defra_node::EmbeddedNode, agent_did: &str) {
    let credential = crate::oauth_credential::OAuthCredential {
        doc_id: None,
        credential_id: crate::oauth_credential::oauth_credential_id(
            agent_did,
            crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
        ),
        agent_did: agent_did.to_string(),
        provider: crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER.to_string(),
        access_token: "access-token".to_string(),
        refresh_token: "refresh-token".to_string(),
        id_token: None,
        account_id: None,
        chatgpt_plan_type: None,
        is_fedramp: false,
        access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        last_refresh: None,
        enabled: true,
    };
    let mutation = crate::oauth_credential::oauth_credential_upsert_mutation(&credential);
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
    bind_default_behavior_chatgpt_backend(
        node.as_ref(),
        identity.did(),
        &crate::default_behavior_id_for_agent(identity.did()),
    )
    .await;
    let default_behavior_id = crate::default_behavior_id_for_agent(identity.did());

    let resolve_context = DocumentResolveContext {
        identity: identity.clone(),
        tool_ceiling: ToolCeiling::readonly(),
        backend_health: crate::backend_health::BackendHealthMap::new(),
    };
    let view = load_document_runtime_view(node.as_ref(), identity.did())
        .await
        .expect("document view");
    let snapshot =
        resolve_document_runtime_snapshot_from_view(node.as_ref(), &resolve_context, &view)
            .await
            .expect("snapshot");

    assert!(
        !snapshot.behaviors.contains_key(&default_behavior_id),
        "a ChatGptCodex behavior without an OAuthCredential must not be runnable (it would hang \
         startup readiness building the client)"
    );
    let reason = snapshot
        .unavailable_behaviors
        .get(&default_behavior_id)
        .expect("behavior should be reported unavailable");
    assert!(
        reason
            .diagnostic
            .contains("requires enabled OAuthCredential"),
        "unavailable reason should identify the missing canonical credential: {}",
        reason.diagnostic
    );
}

#[tokio::test]
async fn chatgpt_codex_behavior_with_enabled_credential_is_runnable() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-chatgpt-cred"));
    bind_default_behavior_chatgpt_backend(
        node.as_ref(),
        identity.did(),
        &crate::default_behavior_id_for_agent(identity.did()),
    )
    .await;
    insert_enabled_oauth_credential(node.as_ref(), identity.did()).await;
    let default_behavior_id = crate::default_behavior_id_for_agent(identity.did());

    let resolve_context = DocumentResolveContext {
        identity: identity.clone(),
        tool_ceiling: ToolCeiling::readonly(),
        backend_health: crate::backend_health::BackendHealthMap::new(),
    };
    let view = load_document_runtime_view(node.as_ref(), identity.did())
        .await
        .expect("document view");
    let snapshot =
        resolve_document_runtime_snapshot_from_view(node.as_ref(), &resolve_context, &view)
            .await
            .expect("snapshot");

    assert!(
        snapshot.behaviors.contains_key(&default_behavior_id),
        "a ChatGptCodex behavior with an enabled OAuthCredential must be runnable; unavailable: {:?}",
        snapshot.unavailable_behaviors
    );
}

/// Install the canonical chain for `behavior_id` and bind it as the principal's
/// explicit default, then swap the chain's InferenceBackend to the
/// ClaudeCliSubscription provider so resolution requires a Claude
/// OAuthCredential.
async fn bind_default_behavior_claude_backend(
    node: &defra_node::EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
) {
    bind_subscription_backend(
        node,
        agent_did,
        behavior_id,
        "ClaudeCliSubscription",
        crate::claude_subscription::default_backend_endpoint(),
        "default",
    )
    .await;
}

#[tokio::test]
async fn claude_subscription_behavior_requires_enabled_credential() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-claude-cred"));
    bind_default_behavior_claude_backend(
        node.as_ref(),
        identity.did(),
        &crate::default_behavior_id_for_agent(identity.did()),
    )
    .await;
    let default_behavior_id = crate::default_behavior_id_for_agent(identity.did());
    let resolve_context = DocumentResolveContext {
        identity: identity.clone(),
        tool_ceiling: ToolCeiling::readonly(),
        backend_health: crate::backend_health::BackendHealthMap::new(),
    };

    let view = load_document_runtime_view(node.as_ref(), identity.did())
        .await
        .expect("document view");
    let snapshot =
        resolve_document_runtime_snapshot_from_view(node.as_ref(), &resolve_context, &view)
            .await
            .expect("snapshot");
    assert!(
        !snapshot.behaviors.contains_key(&default_behavior_id),
        "a ClaudeCliSubscription behavior without an OAuthCredential must not be runnable"
    );
    let reason = snapshot
        .unavailable_behaviors
        .get(&default_behavior_id)
        .expect("behavior should be reported unavailable");
    assert_eq!(
        reason.public_reason,
        gents_protocol::row::BehaviorReadinessUnavailableReason::CredentialsRequired
    );
    assert!(
        reason
            .diagnostic
            .contains("requires enabled OAuthCredential"),
        "unavailable reason should identify the missing canonical credential: {}",
        reason.diagnostic
    );

    let credential = crate::claude_oauth::credential_from_login_tokens(
        identity.did(),
        crate::claude_oauth::CLAUDE_OAUTH_PROVIDER,
        &crate::claude_oauth::ClaudeLoginTokens {
            access_token: "access-token".to_string(),
            refresh_token: "refresh-token".to_string(),
            expires_in: Some(3600),
            scope: None,
        },
        chrono::Utc::now(),
    );
    crate::oauth_credential::upsert_oauth_credential(node.as_ref(), &credential)
        .await
        .expect("upsert credential");

    let view = load_document_runtime_view(node.as_ref(), identity.did())
        .await
        .expect("document view");
    let snapshot =
        resolve_document_runtime_snapshot_from_view(node.as_ref(), &resolve_context, &view)
            .await
            .expect("snapshot");
    assert!(
        snapshot.behaviors.contains_key(&default_behavior_id),
        "a ClaudeCliSubscription behavior with an enabled OAuthCredential must be runnable; unavailable: {:?}",
        snapshot.unavailable_behaviors
    );
}

#[tokio::test]
async fn apply_control_update_admits_chatgpt_behavior_when_credential_added() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-chatgpt-apply"));
    bind_default_behavior_chatgpt_backend(
        node.as_ref(),
        identity.did(),
        &crate::default_behavior_id_for_agent(identity.did()),
    )
    .await;
    let default_behavior_id = crate::default_behavior_id_for_agent(identity.did());
    let resolve_context = DocumentResolveContext {
        identity: identity.clone(),
        tool_ceiling: ToolCeiling::readonly(),
        backend_health: crate::backend_health::BackendHealthMap::new(),
    };

    let mut view = load_document_runtime_view(node.as_ref(), identity.did())
        .await
        .expect("document view");
    let before =
        resolve_document_runtime_snapshot_from_view(node.as_ref(), &resolve_context, &view)
            .await
            .expect("snapshot");
    assert!(
        !before.behaviors.contains_key(&default_behavior_id),
        "behavior must start unavailable without a credential"
    );

    // Runtime codex-login: create the credential, then drive the incremental control update.
    let credential = crate::oauth_credential::OAuthCredential {
        doc_id: None,
        credential_id: crate::oauth_credential::oauth_credential_id(
            identity.did(),
            crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
        ),
        agent_did: identity.did().to_string(),
        provider: crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER.to_string(),
        access_token: "access-token".to_string(),
        refresh_token: "refresh-token".to_string(),
        id_token: None,
        account_id: None,
        chatgpt_plan_type: None,
        is_fedramp: false,
        access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        last_refresh: None,
        enabled: true,
    };
    let doc_id = crate::oauth_credential::upsert_oauth_credential(node.as_ref(), &credential)
        .await
        .expect("upsert credential");

    let outcome = apply_control_update(
        node.as_ref(),
        identity.did(),
        "OAuthCredential",
        &doc_id,
        &mut view,
    )
    .await
    .expect("apply control update");
    assert_eq!(
        outcome,
        ControlUpdateOutcome::FullReload,
        "creating an OAuthCredential must drive a full reload reconcile"
    );

    let view = load_document_runtime_view(node.as_ref(), identity.did())
        .await
        .expect("reloaded document view");
    let after = resolve_document_runtime_snapshot_from_view(node.as_ref(), &resolve_context, &view)
        .await
        .expect("snapshot");
    assert!(
        after.behaviors.contains_key(&default_behavior_id),
        "behavior must become runnable once the credential exists; unavailable: {:?}",
        after.unavailable_behaviors
    );
}

// ---------------------------------------------------------------------------
// DatastoreToolSurface expand: surface ≡ inline, fail-closed
// ---------------------------------------------------------------------------

fn finding_decl() -> crate::document_config::WriteToolDecl {
    crate::document_config::WriteToolDecl {
        tool_name: "write_experiment_finding".to_string(),
        collection: "ExperimentFinding".to_string(),
        description: "Record a finding document for the next pipeline stage.".to_string(),
        fields: vec![
            crate::document_config::WriteToolField {
                name: "job_id".to_string(),
                required: true,
                fill: None,
            },
            crate::document_config::WriteToolField {
                name: "finding_id".to_string(),
                required: true,
                fill: None,
            },
            crate::document_config::WriteToolField {
                name: "content".to_string(),
                required: true,
                fill: None,
            },
            crate::document_config::WriteToolField {
                name: "stage".to_string(),
                required: true,
                fill: None,
            },
        ],
        output_obligation: None,
    }
}

fn empty_runtime_view(agent_did: &str) -> DocumentRuntimeView {
    DocumentRuntimeView {
        principal: DocumentRecord {
            doc_id: "principal".to_string(),
            value: crate::document_config::AgentPrincipal {
                agent_did: agent_did.to_string(),
                display_name: None,
                default_behavior_id: None,
                enabled: true,
                created_at: None,
                created_by: None,
                tags: Vec::new(),
            },
        },
        behaviors: Default::default(),
        contexts: Default::default(),
        compactions: Default::default(),
        skills: Default::default(),
        datastore_tool_surfaces: Default::default(),
        eth_tools: Default::default(),
        tools: Default::default(),
        inference_profiles: Default::default(),
        inference_sampling: Default::default(),
        inference_execution: Default::default(),
        inference_retry_policies: Default::default(),
        backends: Default::default(),
        oauth_credentials: Default::default(),
        tasks: Default::default(),
        schedules: Default::default(),
        triggers: Default::default(),
        event_sources: Default::default(),
        subagent_targets: Default::default(),
        callbacks: Default::default(),
        callback_bindings: Default::default(),
        chain_key_bindings: Default::default(),
        tool_services: Default::default(),
        projection_acp_bindings: Default::default(),
        callback_modules: Default::default(),
        repository_placements: Default::default(),
        backend_observations: Default::default(),
    }
}

/// Test tools selection: `datastore_tool_surface_ids` / `eth_tool_ids`
/// under the canonical nested groups.
fn tools_selection(
    agent_did: &str,
    datastore_tool_surface_ids: Option<Vec<String>>,
    eth_tool_ids: Option<Vec<String>>,
) -> crate::document_config::Tools {
    crate::document_config::Tools {
        tools_id: "sel".to_string(),
        agent_did: agent_did.to_string(),
        datastore: datastore_tool_surface_ids.map(|ids| crate::document_config::DatastoreTools {
            datastore_tool_surface_ids: Some(ids),
            ..Default::default()
        }),
        integrations: eth_tool_ids.map(|ids| crate::document_config::IntegrationTools {
            eth_tool_ids: Some(ids),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[test]
fn runtime_skill_projection_is_canonical_across_map_insertion_order() {
    fn skill(skill_id: &str) -> DocumentRecord<crate::document_config::SkillDocument> {
        DocumentRecord {
            doc_id: format!("doc-{skill_id}"),
            value: crate::document_config::SkillDocument {
                skill_id: skill_id.to_string(),
                agent_did: "did:key:owner".to_string(),
                tags: Vec::new(),
                name: Some(skill_id.to_string()),
                description: None,
                instructions: Some(format!("Instructions for {skill_id}")),
                tool_refs: Vec::new(),
                display_name: None,
                interface_json: None,
                enabled: true,
                created_at: None,
            },
        }
    }

    let mut forward = empty_runtime_view("did:key:owner");
    forward.skills.insert("alpha".to_string(), skill("alpha"));
    forward.skills.insert("zeta".to_string(), skill("zeta"));
    let mut reverse = empty_runtime_view("did:key:owner");
    reverse.skills.insert("zeta".to_string(), skill("zeta"));
    reverse.skills.insert("alpha".to_string(), skill("alpha"));

    let forward_ids = super::snapshot::sorted_skills(&forward)
        .into_iter()
        .map(|skill| skill.skill_id)
        .collect::<Vec<_>>();
    let reverse_ids = super::snapshot::sorted_skills(&reverse)
        .into_iter()
        .map(|skill| skill.skill_id)
        .collect::<Vec<_>>();

    assert_eq!(forward_ids, vec!["alpha", "zeta"]);
    assert_eq!(reverse_ids, forward_ids);
}

// Graph visibility is exercised through real publication, activation and run
// retirement in graph_pipeline::runtime::tests::revision_publication_and_run_start_are_transactional_and_pinned.

#[test]
fn merge_surface_expands_selected_surfaces() {
    let agent_did = "did:key:zSurfaceTest";
    let decl = finding_decl();
    let selection = tools_selection(agent_did, Some(vec!["experiment-writes".to_string()]), None);

    let mut view = empty_runtime_view(agent_did);
    view.datastore_tool_surfaces.insert(
        "experiment-writes".to_string(),
        DocumentRecord {
            doc_id: "surf-doc".to_string(),
            value: crate::document_config::DatastoreToolSurfaceDocument {
                tags: Vec::new(),
                surface_id: "experiment-writes".to_string(),
                agent_did: agent_did.to_string(),
                display_name: Some("experiment writes".to_string()),
                enabled: true,
                entries: Some(vec![crate::document_config::SurfaceToolDecl::Create(
                    decl.clone(),
                )]),
                created_at: None,
            },
        },
    );

    let from_surface = merge_surface_tools(&selection, &view).unwrap();
    assert_eq!(from_surface.write_tools.len(), 1);
    assert_eq!(
        from_surface.write_tools[0].tool_name,
        "write_experiment_finding"
    );
    assert_eq!(from_surface.write_tools[0].collection, "ExperimentFinding");
    assert_eq!(from_surface.write_tools[0].fields.len(), 4);
}

#[test]
fn merge_fails_closed_on_missing_surface() {
    let agent_did = "did:key:zSurfaceMissing";
    let selection = tools_selection(agent_did, Some(vec!["does-not-exist".to_string()]), None);
    let view = empty_runtime_view(agent_did);
    let err = merge_surface_tools(&selection, &view).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("missing") && msg.contains("does-not-exist"),
        "expected missing surface error, got: {msg}"
    );
}

#[test]
fn merge_fails_closed_on_disabled_surface() {
    let agent_did = "did:key:zSurfaceDisabled";
    let mut view = empty_runtime_view(agent_did);
    view.datastore_tool_surfaces.insert(
        "disabled-writes".to_string(),
        DocumentRecord {
            doc_id: "surf-doc".to_string(),
            value: crate::document_config::DatastoreToolSurfaceDocument {
                tags: Vec::new(),
                surface_id: "disabled-writes".to_string(),
                agent_did: agent_did.to_string(),
                display_name: None,
                enabled: false,
                entries: Some(vec![crate::document_config::SurfaceToolDecl::Create(
                    finding_decl(),
                )]),
                created_at: None,
            },
        },
    );
    let selection = tools_selection(agent_did, Some(vec!["disabled-writes".to_string()]), None);
    let err = merge_surface_tools(&selection, &view).unwrap_err();
    assert!(
        err.to_string().contains("disabled"),
        "expected disabled error, got: {err}"
    );
}

#[test]
fn merge_reports_invalid_output_obligation_fields() {
    let agent_did = "did:key:zSurfaceObligation";
    let mut decl = finding_decl();
    decl.output_obligation = Some(crate::document_config::WriteToolOutputObligation {
        scope: crate::document_config::WriteToolOutputObligationScope::Trigger,
        minimum_writes: 1,
        expected_count_field: Some("missing_count".to_string()),
    });

    let mut view = empty_runtime_view(agent_did);
    view.datastore_tool_surfaces.insert(
        "invalid-obligation-writes".to_string(),
        DocumentRecord {
            doc_id: "surf-doc".to_string(),
            value: crate::document_config::DatastoreToolSurfaceDocument {
                tags: Vec::new(),
                surface_id: "invalid-obligation-writes".to_string(),
                agent_did: agent_did.to_string(),
                display_name: None,
                enabled: true,
                entries: Some(vec![crate::document_config::SurfaceToolDecl::Create(decl)]),
                created_at: None,
            },
        },
    );
    let selection = tools_selection(
        agent_did,
        Some(vec!["invalid-obligation-writes".to_string()]),
        None,
    );

    let error = merge_surface_tools(&selection, &view).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("expected_count_field"), "got: {message}");
    assert!(!message.contains("zero minimum_writes"), "got: {message}");
}

#[test]
fn merge_fails_closed_on_foreign_agent_surface() {
    let agent_did = "did:key:zSurfaceOwner";
    let mut view = empty_runtime_view(agent_did);
    view.datastore_tool_surfaces.insert(
        "foreign-writes".to_string(),
        DocumentRecord {
            doc_id: "surf-doc".to_string(),
            value: crate::document_config::DatastoreToolSurfaceDocument {
                tags: Vec::new(),
                surface_id: "foreign-writes".to_string(),
                agent_did: "did:key:zOtherAgent".to_string(),
                display_name: None,
                enabled: true,
                entries: Some(vec![crate::document_config::SurfaceToolDecl::Create(
                    finding_decl(),
                )]),
                created_at: None,
            },
        },
    );
    let selection = tools_selection(agent_did, Some(vec!["foreign-writes".to_string()]), None);
    let err = merge_surface_tools(&selection, &view).unwrap_err();
    assert!(
        err.to_string()
            .contains("missing same-agent DatastoreToolSurface foreign-writes"),
        "foreign surface must not satisfy the scoped reference, got: {err}"
    );
}

#[test]
fn merge_fails_closed_on_duplicate_surface_entries() {
    let agent_did = "did:key:zSurfaceCollide";
    let decl = finding_decl();
    let mut view = empty_runtime_view(agent_did);
    view.datastore_tool_surfaces.insert(
        "experiment-writes".to_string(),
        DocumentRecord {
            doc_id: "surf-doc".to_string(),
            value: crate::document_config::DatastoreToolSurfaceDocument {
                tags: Vec::new(),
                surface_id: "experiment-writes".to_string(),
                agent_did: agent_did.to_string(),
                display_name: None,
                enabled: true,
                entries: Some(vec![
                    crate::document_config::SurfaceToolDecl::Create(decl.clone()),
                    crate::document_config::SurfaceToolDecl::Create(decl),
                ]),
                created_at: None,
            },
        },
    );
    let selection = tools_selection(agent_did, Some(vec!["experiment-writes".to_string()]), None);
    let err = merge_surface_tools(&selection, &view).unwrap_err();
    assert!(
        err.to_string().contains("duplicate"),
        "expected duplicate tool_name error, got: {err}"
    );
}

#[test]
fn merge_expands_query_entries_separately_from_creates() {
    let agent_did = "did:key:zSurfaceQuery";
    let write = finding_decl();
    let query = crate::document_config::QueryToolDecl {
        tool_name: "query_experiment_finding".to_string(),
        collection: "ExperimentFinding".to_string(),
        description: "Load findings for this run.".to_string(),
        fields: vec!["finding_id".into(), "content".into()],
        filter_fields: vec![crate::document_config::WriteToolField {
            name: "run_id".into(),
            required: false,
            fill: Some(crate::document_config::WriteToolFieldFill::Correlation),
        }],
    };
    let mut view = empty_runtime_view(agent_did);
    view.datastore_tool_surfaces.insert(
        "experiment-io".to_string(),
        DocumentRecord {
            doc_id: "surf-doc".to_string(),
            value: crate::document_config::DatastoreToolSurfaceDocument {
                tags: Vec::new(),
                surface_id: "experiment-io".to_string(),
                agent_did: agent_did.to_string(),
                display_name: None,
                enabled: true,
                entries: Some(vec![
                    crate::document_config::SurfaceToolDecl::Create(write.clone()),
                    crate::document_config::SurfaceToolDecl::Query(query.clone()),
                ]),
                created_at: None,
            },
        },
    );
    let selection = tools_selection(agent_did, Some(vec!["experiment-io".to_string()]), None);
    let merged = super::merge_surface_tools(&selection, &view).unwrap();
    assert_eq!(merged.write_tools, vec![write]);
    assert_eq!(merged.query_tools, vec![query]);
}

#[tokio::test]
async fn apply_control_update_evicts_surface_when_ownership_moves_away() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-surface-revoke"));
    let agent_did = identity.did();
    bind_default_behavior_backend(
        node.as_ref(),
        agent_did,
        &crate::default_behavior_id_for_agent(agent_did),
    )
    .await;

    let mut view = load_document_runtime_view(node.as_ref(), agent_did)
        .await
        .expect("document view");

    let entries = gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
        "entries": [finding_decl()],
    }))
    .unwrap();
    let create = format!(
        r#"mutation {{ create_DatastoreToolSurface(input: {{
            surface_id: "experiment-writes", agent_did: "{did}", enabled: true,
            entries: {entries}
        }}) {{ _docID }} }}"#,
        did = escape_graphql_string(agent_did),
    );
    let resp = node.execute(&create).await;
    assert!(
        !resp.has_errors(),
        "create_DatastoreToolSurface: {:?}",
        resp.errors
    );
    let doc_id = created_skill_doc_id(resp.data.as_ref()).expect("created surface _docID");

    let outcome = apply_control_update(
        node.as_ref(),
        agent_did,
        "DatastoreToolSurface",
        &doc_id,
        &mut view,
    )
    .await
    .expect("apply surface create");
    assert_eq!(outcome, ControlUpdateOutcome::FullReload);
    let mut view = load_document_runtime_view(node.as_ref(), agent_did)
        .await
        .expect("reloaded view with surface");
    assert!(view
        .datastore_tool_surfaces
        .contains_key("experiment-writes"));

    // Reassigning the surface to another principal must revoke the grant now,
    // not at the next process restart: the ownership move notification still
    // forces the reload and the scoped loader drops the foreign row.
    let reassign = format!(
        r#"mutation {{ update_DatastoreToolSurface(
            docID: "{doc_id}", input: {{ agent_did: "did:key:zOtherOwner" }}
        ) {{ _docID }} }}"#,
        doc_id = escape_graphql_string(&doc_id),
    );
    let resp = node.execute(&reassign).await;
    assert!(
        !resp.has_errors(),
        "update_DatastoreToolSurface: {:?}",
        resp.errors
    );

    let outcome = apply_control_update(
        node.as_ref(),
        agent_did,
        "DatastoreToolSurface",
        &doc_id,
        &mut view,
    )
    .await
    .expect("apply surface reassign");
    assert_eq!(outcome, ControlUpdateOutcome::FullReload);
    let view = load_document_runtime_view(node.as_ref(), agent_did)
        .await
        .expect("reloaded view after ownership move");
    assert!(
        view.datastore_tool_surfaces.is_empty(),
        "surface must be evicted once it is owned by another principal"
    );
}

fn sample_eth_tool(
    agent_did: &str,
    tool_id: &str,
    enabled: bool,
    methods: &[&str],
) -> crate::document_config::EthToolDocument {
    crate::document_config::EthToolDocument {
        tool_id: tool_id.to_string(),
        agent_did: agent_did.to_string(),
        display_name: Some(tool_id.to_string()),
        enabled,
        chain_id: Some(8453),
        rpc_url: Some("https://mainnet.base.org".to_string()),
        rpc_timeout_secs: None,
        query_methods: Some(methods.iter().map(|m| m.to_string()).collect()),
        calls: None,
        key_binding_id: None,
        created_at: None,
        tags: Vec::new(),
    }
}

#[test]
fn expand_eth_tools_skips_disabled_and_empty_methods() {
    let agent_did = "did:key:zEth";
    let selection = tools_selection(
        agent_did,
        None,
        Some(vec![
            "base-read".to_string(),
            "disabled".to_string(),
            "no-methods".to_string(),
        ]),
    );
    let mut view = empty_runtime_view(agent_did);
    view.eth_tools.insert(
        "base-read".to_string(),
        DocumentRecord {
            doc_id: "e1".to_string(),
            value: sample_eth_tool(agent_did, "base-read", true, &["eth_chainId"]),
        },
    );
    view.eth_tools.insert(
        "disabled".to_string(),
        DocumentRecord {
            doc_id: "e2".to_string(),
            value: sample_eth_tool(agent_did, "disabled", false, &["eth_chainId"]),
        },
    );
    view.eth_tools.insert(
        "no-methods".to_string(),
        DocumentRecord {
            doc_id: "e3".to_string(),
            value: sample_eth_tool(agent_did, "no-methods", true, &[]),
        },
    );
    let expanded = expand_eth_tools(&selection, &view).expect("expand");
    assert_eq!(expanded.queries.len(), 1);
    assert_eq!(expanded.queries[0].tool_name(), "base-read_query");
}

#[test]
fn expand_eth_tools_fails_closed_on_missing_and_foreign() {
    let agent_did = "did:key:zEth";
    let missing = tools_selection(agent_did, None, Some(vec!["nope".to_string()]));
    let err = expand_eth_tools(&missing, &empty_runtime_view(agent_did)).unwrap_err();
    assert!(err.to_string().contains("missing"));

    let foreign = tools_selection(agent_did, None, Some(vec!["other".to_string()]));
    let mut view = empty_runtime_view(agent_did);
    view.eth_tools.insert(
        "other".to_string(),
        DocumentRecord {
            doc_id: "e1".to_string(),
            value: sample_eth_tool("did:key:zOther", "other", true, &["eth_chainId"]),
        },
    );
    let err = expand_eth_tools(&foreign, &view).unwrap_err();
    assert!(err.to_string().contains("different agent"));
}
