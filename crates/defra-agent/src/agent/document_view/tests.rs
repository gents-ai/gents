use std::sync::Arc;

use super::*;
use crate::document_config::ToolSelectionDocument;
use crate::ensure_runtime_schemas;
use crate::graphql::escape_graphql_string;
use crate::identity::{AgentIdentity, SimpleIdentity};
use crate::tool_surface::ToolCeiling;

async fn test_node() -> Arc<defra_node::EmbeddedNode> {
    Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap())
}

fn test_identity(name: &str) -> SimpleIdentity {
    let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
    SimpleIdentity::new(name, path, None)
}

async fn bind_default_behavior_backend(
    node: &defra_node::EmbeddedNode,
    agent_did: &str,
    backend_id: &str,
    endpoint: &str,
) {
    let bootstrap = crate::ensure_agent_principal(node, agent_did)
        .await
        .unwrap();
    let escaped_backend_id = escape_graphql_string(backend_id);
    let escaped_endpoint = escape_graphql_string(endpoint);
    let mutation = format!(
        r#"mutation {{
            upsert_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }},
                add: {{
                    backend_id: "{escaped_backend_id}",
                    name: "{escaped_backend_id}",
                    endpoint: "{escaped_endpoint}",
                    max_concurrent: 1,
                    enabled: true,
                    models: ["default"],
                    probe_status: "healthy"
                }},
                update: {{
                    name: "{escaped_backend_id}",
                    endpoint: "{escaped_endpoint}",
                    max_concurrent: 1,
                    enabled: true,
                    probe_status: "healthy"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "upsert InferenceBackend failed: {:?}",
        response.errors
    );

    let mut default_behavior =
        crate::load_agent_behavior(node, &bootstrap.default_behavior.behavior_id)
            .await
            .unwrap()
            .expect("default behavior document");
    default_behavior.backend_id = Some(backend_id.to_string());
    crate::upsert_agent_behavior(node, &default_behavior)
        .await
        .unwrap();
}

#[tokio::test]
async fn load_document_runtime_view_includes_referenced_documents() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-load"));
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-document-view",
        "http://127.0.0.1:8121/v1",
    )
    .await;

    let selection_id = format!("{}:tools", identity.did());
    crate::upsert_tool_selection(
        node.as_ref(),
        &ToolSelectionDocument {
            selection_id: selection_id.clone(),
            agent_did: identity.did().to_string(),
            display_name: Some("Read tools".to_string()),
            enable_file_tools: Some(true),
            file_tools_mode: Some("ReadOnly".to_string()),
            enable_bash: Some(false),
            bash_mode: Some("Off".to_string()),
            cli_tool_names: Some(Vec::new()),
            enable_meta_tools: Some(false),
            delegate_to: Some(Vec::new()),
        },
    )
    .await
    .unwrap();

    let default_behavior_id = format!("{}:default", identity.did());
    let mut default_behavior = crate::load_agent_behavior(node.as_ref(), &default_behavior_id)
        .await
        .unwrap()
        .expect("default behavior");
    default_behavior.tool_selection_id = Some(selection_id.clone());
    crate::upsert_agent_behavior(node.as_ref(), &default_behavior)
        .await
        .unwrap();

    let view = load_document_runtime_view(node.as_ref(), identity.did())
        .await
        .expect("document view should load");

    assert_eq!(view.principal.value.agent_did, identity.did());
    assert!(view.behaviors.contains_key(&default_behavior_id));
    assert!(view.tool_selections.contains_key(&selection_id));
    assert!(view.backends.contains_key("backend-document-view"));
}

#[tokio::test]
async fn apply_control_update_reconciles_tool_selection_via_doc_id() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-update"));
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-document-view-update",
        "http://127.0.0.1:8122/v1",
    )
    .await;

    let resolve_context = DocumentResolveContext {
        identity: identity.clone(),
        tool_ceiling: ToolCeiling::readonly(),
    };
    let mut view = load_document_runtime_view(node.as_ref(), identity.did())
        .await
        .expect("initial document view");

    let selection_id = format!("{}:tools", identity.did());
    crate::upsert_tool_selection(
        node.as_ref(),
        &ToolSelectionDocument {
            selection_id: selection_id.clone(),
            agent_did: identity.did().to_string(),
            display_name: Some("Read tools".to_string()),
            enable_file_tools: Some(true),
            file_tools_mode: Some("ReadOnly".to_string()),
            enable_bash: Some(false),
            bash_mode: Some("Off".to_string()),
            cli_tool_names: Some(Vec::new()),
            enable_meta_tools: Some(false),
            delegate_to: Some(Vec::new()),
        },
    )
    .await
    .unwrap();

    let selection_doc_id =
        crate::document_config::load_tool_selection_record(node.as_ref(), &selection_id)
            .await
            .unwrap()
            .expect("tool selection record")
            .0;

    let default_behavior_id = format!("{}:default", identity.did());
    let mut default_behavior = crate::load_agent_behavior(node.as_ref(), &default_behavior_id)
        .await
        .unwrap()
        .expect("default behavior");
    default_behavior.tool_selection_id = Some(selection_id.clone());
    crate::upsert_agent_behavior(node.as_ref(), &default_behavior)
        .await
        .unwrap();

    let behavior_doc_id =
        crate::document_config::load_agent_behavior_record(node.as_ref(), &default_behavior_id)
            .await
            .unwrap()
            .expect("behavior record")
            .0;

    assert!(apply_control_update(
        node.as_ref(),
        identity.did(),
        "opaque-tool-selection-collection",
        &selection_doc_id,
        &mut view,
    )
    .await
    .is_ok_and(|outcome| outcome == ControlUpdateOutcome::Applied));
    assert!(apply_control_update(
        node.as_ref(),
        identity.did(),
        "opaque-agent-behavior-collection",
        &behavior_doc_id,
        &mut view,
    )
    .await
    .is_ok_and(|outcome| outcome == ControlUpdateOutcome::Applied));

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

#[tokio::test]
async fn resolve_document_runtime_snapshot_marks_backend_without_tool_support_unavailable() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("document-view-tool-capability"));
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-no-tools",
        "http://127.0.0.1:8129/v1",
    )
    .await;

    let selection_id = format!("{}:tools", identity.did());
    crate::upsert_tool_selection(
        node.as_ref(),
        &ToolSelectionDocument {
            selection_id: selection_id.clone(),
            agent_did: identity.did().to_string(),
            display_name: Some("Read tools".to_string()),
            enable_file_tools: Some(true),
            file_tools_mode: Some("ReadOnly".to_string()),
            enable_bash: Some(false),
            bash_mode: Some("Off".to_string()),
            cli_tool_names: Some(Vec::new()),
            enable_meta_tools: Some(false),
            delegate_to: Some(Vec::new()),
        },
    )
    .await
    .unwrap();

    let escaped_backend_id = escape_graphql_string("backend-no-tools");
    let mutation = format!(
        r#"mutation {{
            update_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }},
                input: {{
                    supports_tool_calls: false
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let default_behavior_id = format!("{}:default", identity.did());
    let mut default_behavior = crate::load_agent_behavior(node.as_ref(), &default_behavior_id)
        .await
        .unwrap()
        .expect("default behavior");
    default_behavior.tool_selection_id = Some(selection_id);
    crate::upsert_agent_behavior(node.as_ref(), &default_behavior)
        .await
        .unwrap();

    let resolve_context = DocumentResolveContext {
        identity: identity.clone(),
        tool_ceiling: ToolCeiling::readonly(),
    };
    let view = load_document_runtime_view(node.as_ref(), identity.did())
        .await
        .expect("document view should load");
    let snapshot =
        resolve_document_runtime_snapshot_from_view(node.as_ref(), &resolve_context, &view)
            .await
            .expect("snapshot resolution should succeed");

    assert!(snapshot.behaviors.is_empty());
    assert!(snapshot
        .unavailable_behaviors
        .get(&default_behavior_id)
        .is_some_and(|reason| reason.contains("does not support tool calling")));
}
