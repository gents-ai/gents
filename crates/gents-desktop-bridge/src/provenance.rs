// Session-message provenance read from the immutable
// `AgentRequest.caused_by_parent_*` lineage that `lifecycle::materialize`
// writes for `create_session`/`send_message`. Read only; it confers no
// hierarchy, cascade or authority.

use std::collections::BTreeMap;
use std::sync::Arc;

use gents::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use gents_desktop_core::client::ClientCore;
use serde_json::Value;

use crate::types::{CausedRequestView, SessionProvenanceView};

/// Bounds each lineage read. A session past it reports `truncated`.
pub const PROVENANCE_REQUEST_LIMIT: usize = 512;

const REQUEST_FIELDS: &str = "_docID request_id session_id agent_did behavior_id lifecycle_state \
     interrupt_requested_at created_at subagent_depth caused_by_parent_request_id \
     caused_by_parent_request_doc_id caused_by_parent_tool_call_id";

pub async fn session_provenance(
    core: &Arc<ClientCore>,
    agent_did: &str,
    session_id: &str,
) -> Result<SessionProvenanceView, String> {
    let own = query_requests(
        core,
        &format!(
            r#"session_id: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }}"#,
            escape_graphql_string(session_id),
            escape_graphql_string(agent_did),
        ),
        "session provenance: own requests",
    )
    .await?;
    let mut truncated = own.len() >= PROVENANCE_REQUEST_LIMIT;

    let own_doc_ids: Vec<&str> = own.iter().map(|row| row.request_doc_id.as_str()).collect();
    let mut sent = if own_doc_ids.is_empty() {
        Vec::new()
    } else {
        query_requests(
            core,
            &format!(
                "caused_by_parent_request_doc_id: {{ _in: [{}] }}",
                quoted_list(&own_doc_ids)
            ),
            "session provenance: caused requests",
        )
        .await?
    };
    truncated |= sent.len() >= PROVENANCE_REQUEST_LIMIT;
    for row in &mut sent {
        row.caused_by_session_id = Some(session_id.to_owned());
    }

    let mut received: Vec<CausedRequestView> = own
        .into_iter()
        .filter(|row| row.caused_by_request_doc_id.is_some())
        .collect();
    let causing: Vec<&str> = received
        .iter()
        .filter_map(|row| row.caused_by_request_doc_id.as_deref())
        .collect();
    if !causing.is_empty() {
        let sessions = causing_sessions(core, &causing).await?;
        for row in &mut received {
            row.caused_by_session_id = row
                .caused_by_request_doc_id
                .as_ref()
                .and_then(|doc_id| sessions.get(doc_id).cloned());
        }
    }

    Ok(SessionProvenanceView {
        session_id: session_id.to_owned(),
        received,
        sent,
        truncated,
    })
}

async fn query_requests(
    core: &Arc<ClientCore>,
    filter: &str,
    operation: &str,
) -> Result<Vec<CausedRequestView>, String> {
    let query = format!(
        "{{ AgentRequest(filter: {{ {filter} }}, limit: {PROVENANCE_REQUEST_LIMIT}) {{ {REQUEST_FIELDS} }} }}"
    );
    let rows = rows(core, &query, operation).await?;
    Ok(rows.iter().filter_map(caused_request_from_row).collect())
}

/// The session of each causing request visible on this node, by document id.
async fn causing_sessions(
    core: &Arc<ClientCore>,
    doc_ids: &[&str],
) -> Result<BTreeMap<String, String>, String> {
    let query = format!(
        "{{ AgentRequest(filter: {{ _docID: {{ _in: [{}] }} }}) {{ _docID session_id }} }}",
        quoted_list(doc_ids)
    );
    let rows = rows(core, &query, "session provenance: causing requests").await?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            Some((
                string_field(row, "_docID")?,
                string_field(row, "session_id")?,
            ))
        })
        .collect())
}

async fn rows(core: &Arc<ClientCore>, query: &str, operation: &str) -> Result<Vec<Value>, String> {
    let response = graphql_with_transaction_retry(core.node(), query, operation)
        .await
        .map_err(|error| format!("{operation} failed: {error:#}"))?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

fn caused_request_from_row(row: &Value) -> Option<CausedRequestView> {
    Some(CausedRequestView {
        request_id: string_field(row, "request_id")?,
        request_doc_id: string_field(row, "_docID")?,
        session_id: string_field(row, "session_id"),
        agent_did: string_field(row, "agent_did"),
        behavior_id: string_field(row, "behavior_id"),
        lifecycle_state: string_field(row, "lifecycle_state"),
        interrupt_requested_at: string_field(row, "interrupt_requested_at"),
        created_at: string_field(row, "created_at"),
        hop: row.get("subagent_depth").and_then(Value::as_i64),
        caused_by_request_id: string_field(row, "caused_by_parent_request_id"),
        caused_by_request_doc_id: string_field(row, "caused_by_parent_request_doc_id"),
        caused_by_tool_call_id: string_field(row, "caused_by_parent_tool_call_id"),
        caused_by_session_id: None,
    })
}

fn quoted_list(values: &[&str]) -> String {
    values
        .iter()
        .map(|value| format!("\"{}\"", escape_graphql_string(value)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn string_field(row: &Value, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}
