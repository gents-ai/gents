//! Helper for out-of-engine manual runs — used by CLI `config task run`
//! and desktop "Run Now" buttons.
//!
//! Instead of pushing into `ManualSource`, this helper writes an
//! `AgentRequest` document directly with the manual lineage tuple. The
//! running agent's lifecycle watcher picks up the Pending row via normal
//! intake. Two reasons for this path:
//!
//! 1. CLI runs out-of-process and has no `ManualTriggerHandle`.
//! 2. Desktop can use it too — same code, same lineage, same
//!    observability. Avoids maintaining two paths.
//!
//! Both paths produce the same `(caused_by_trigger_id = null,
//! caused_by_trigger_kind = "manual")` lineage tuple.

use anyhow::{anyhow, Result};
use defra_node::EmbeddedNode;
use serde_json::Value;

use crate::lifecycle::{write_pending_agent_request_with_lineage, ExecutionOrigin, TriggerLineage};
use crate::template::{render_template, TemplateScope};

/// Write an `AgentRequest` row representing a manual task run, after rendering
/// the task's `prompt_template` against the given `args`.
///
/// Returns the new `AgentRequest`'s `_docID`. The row lands at
/// `lifecycle_state = "pending"` so the agent's normal intake path picks it
/// up. Lineage is `caused_by_trigger_kind = "manual"` and
/// `caused_by_trigger_id = null` (the field is omitted from the create input
/// so it remains null in the document).
pub async fn write_manual_agent_request(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
    task_id: &str,
    prompt_template: &str,
    args: Value,
) -> Result<String> {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let scope = TemplateScope {
        event: serde_json::json!({
            "fired_at": now,
            "trigger_id": serde_json::Value::Null,
            "trigger_kind": "manual",
        }),
        doc: None,
        args: Some(args),
    };
    let content = render_template(prompt_template, &scope)
        .map_err(|e| anyhow!("render manual template for task {task_id}: {e}"))?;

    let enqueued = write_pending_agent_request_with_lineage(
        node,
        agent_did,
        behavior_id,
        &content,
        ExecutionOrigin::Interactive,
        TriggerLineage {
            trigger_id: None,
            trigger_kind: Some("manual".to_string()),
        },
    )
    .await
    .map_err(|e| anyhow!("create manual AgentRequest for task {task_id}: {e}"))?;

    tracing::info!(
        task_id = %task_id,
        request_id = %enqueued.request_id,
        doc_id = %enqueued.doc_id,
        "manual task run enqueued as AgentRequest"
    );
    Ok(enqueued.doc_id)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use defra_node::EmbeddedNode;
    use serde_json::Value;

    use super::write_manual_agent_request;
    use crate::schema::ensure_schemas;

    async fn test_node() -> Arc<EmbeddedNode> {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        ensure_schemas(node.as_ref()).await.unwrap();
        node
    }

    #[tokio::test]
    async fn writes_manual_request_with_rendered_template_and_manual_lineage() {
        let node = test_node().await;

        let doc_id = write_manual_agent_request(
            node.as_ref(),
            "did:agent:test",
            "behavior-1",
            "task-1",
            "hello {{ args.name }}",
            serde_json::json!({"name": "Amy"}),
        )
        .await
        .unwrap();
        assert!(!doc_id.is_empty());

        let query = format!(
            r#"{{
                AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}) {{
                    _docID
                    content
                    caused_by_trigger_kind
                    caused_by_trigger_id
                    execution_origin
                    lifecycle_state
                    status
                }}
            }}"#
        );
        let response = node.execute(&query).await;
        assert!(
            !response.has_errors(),
            "query failed: {:?}",
            response.errors
        );

        let rows = response
            .data
            .as_ref()
            .and_then(|d| d.get("AgentRequest"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(rows.len(), 1, "expected exactly one row, got {rows:?}");
        let row = &rows[0];

        assert_eq!(row["content"].as_str(), Some("hello Amy"));
        assert_eq!(row["caused_by_trigger_kind"].as_str(), Some("manual"));
        assert!(
            row["caused_by_trigger_id"].is_null(),
            "caused_by_trigger_id should be null for manual runs, got {:?}",
            row["caused_by_trigger_id"]
        );
        assert_eq!(row["execution_origin"].as_str(), Some("interactive"));
        assert_eq!(row["lifecycle_state"].as_str(), Some("pending"));
        assert_eq!(row["status"].as_str(), Some("pending"));
    }

    #[tokio::test]
    async fn surfaces_template_render_errors() {
        let node = test_node().await;
        let err = write_manual_agent_request(
            node.as_ref(),
            "did:agent:test",
            "behavior-1",
            "task-err",
            "hello {{ args.missing }}",
            serde_json::json!({}),
        )
        .await
        .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("render manual template for task task-err"),
            "expected render error context, got: {msg}"
        );
    }
}
