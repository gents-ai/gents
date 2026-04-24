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

use crate::graphql::escape_graphql_string;
use crate::lifecycle::DEFAULT_REQUEST_MAX_RETRIES;
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

    // New request_id / session_id — mirror how other lifecycle paths generate
    // them (see `materialize_claimed_with_execution_binding`).
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();

    let escaped_request_id = escape_graphql_string(&request_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_behavior_id = escape_graphql_string(behavior_id);
    let escaped_session_id = escape_graphql_string(&session_id);
    let escaped_content = escape_graphql_string(&content);
    let escaped_created_at = escape_graphql_string(&now);

    // `caused_by_trigger_id` is intentionally omitted so it stays null in the
    // persisted document — manual runs have no trigger id to reference.
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                agent_did: "{escaped_agent_did}",
                behavior_id: "{escaped_behavior_id}",
                session_id: "{escaped_session_id}",
                retry_parent_request: "",
                retry_root_request: "{escaped_request_id}",
                superseded_by_request: "",
                content: "{escaped_content}",
                status: "pending",
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "interactive",
                caused_by_trigger_kind: "manual",
                failure_reason: "",
                created_at: "{escaped_created_at}",
                retry_count: 0,
                max_retries: {max_retries}
            }}) {{ _docID }}
        }}"#,
        max_retries = DEFAULT_REQUEST_MAX_RETRIES,
    );

    let response = node.execute(&mutation).await;
    if response.has_errors() {
        anyhow::bail!(
            "create manual AgentRequest for task {task_id} failed: {:?}",
            response.errors
        );
    }

    // `create_AgentRequest` may return the `_docID` inline (as a single object
    // or first array element) or may omit it entirely; fall back to a follow-up
    // query filtered by request_id if the inline path yields nothing.
    let inline_doc_id = response
        .data
        .as_ref()
        .and_then(|d| d.get("create_AgentRequest"))
        .and_then(|value| {
            value
                .get("_docID")
                .and_then(|doc_id| doc_id.as_str())
                .or_else(|| {
                    value
                        .as_array()
                        .and_then(|rows| rows.first())
                        .and_then(|row| row.get("_docID"))
                        .and_then(|doc_id| doc_id.as_str())
                })
                .map(|s| s.to_string())
        });

    let doc_id = if let Some(doc_id) = inline_doc_id {
        doc_id
    } else {
        let query = format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}) {{ _docID }} }}"#
        );
        let query_resp = node.execute(&query).await;
        if query_resp.has_errors() {
            anyhow::bail!(
                "querying created manual AgentRequest doc id failed: {:?}",
                query_resp.errors
            );
        }
        query_resp
            .data
            .as_ref()
            .and_then(|d| d.get("AgentRequest"))
            .and_then(|v| v.as_array())
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("_docID"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("manual AgentRequest create returned no _docID"))?
            .to_string()
    };

    tracing::info!(
        task_id = %task_id,
        request_id = %request_id,
        doc_id = %doc_id,
        "manual task run enqueued as AgentRequest"
    );
    Ok(doc_id)
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
