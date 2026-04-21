use anyhow::{bail, Result};
use chrono::Utc;
use defra_node::EmbeddedNode;

use super::super::graphql::{escape_graphql_string, execute_mutation};

/// Idempotent interrupt latch: the first writer stamps
/// `interrupt_requested_at`; subsequent calls are no-ops so the runtime
/// observer always sees a single canonical timestamp (see S7 in
/// `proofs/Interrupt.lean`).
pub async fn interrupt_request(node: &EmbeddedNode, request_id: &str) -> Result<()> {
    if fetch_interrupt_requested_at(node, request_id)
        .await?
        .is_some()
    {
        return Ok(());
    }

    let now = Utc::now().to_rfc3339();
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_now = escape_graphql_string(&now);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                input: {{ interrupt_requested_at: "{escaped_now}" }}
            ) {{ _docID }}
        }}"#
    );
    execute_mutation(node, &mutation, "interrupt_request").await
}

/// Read the latch field. Returns `None` when the field is empty/unset or the
/// request does not exist.
pub async fn fetch_interrupt_requested_at(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<Option<String>> {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"query {{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{
                interrupt_requested_at
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        bail!(
            "fetch_interrupt_requested_at({request_id}) failed: {:?}",
            resp.errors
        );
    }
    Ok(resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|row| row.get("interrupt_requested_at"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from))
}
