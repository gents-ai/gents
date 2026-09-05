use super::*;
use anyhow::Context as _;

pub(super) async fn load_child_linkage(
    node: &EmbeddedNode,
    child_request_id: &str,
) -> Result<Option<AgentRequestRow>> {
    let Some(child_request_doc_id) =
        crate::request_binding::resolve_request_doc_id(node, child_request_id).await?
    else {
        return Ok(None);
    };
    let escaped_child_request_doc_id = escape_graphql_string(&child_request_doc_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ _docID: {{ _eq: "{escaped_child_request_doc_id}" }} }},
                limit: 1
            ) {{
                request_id
                caused_by_parent_request_id
                caused_by_parent_request_doc_id
                caused_by_parent_tool_call_id
                caused_by_parent_tool_call_doc_id
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query child AgentRequest linkage {child_request_id} failed: {:?}",
            response.errors
        );
    }
    let row = request_rows(response.data.as_ref())?.into_iter().next();
    if row
        .as_ref()
        .is_some_and(|row| row.request_id != child_request_id)
    {
        anyhow::bail!(
            "AgentRequest {child_request_doc_id} changed logical request binding while loading child {child_request_id}"
        );
    }
    Ok(row)
}

pub(super) async fn load_request_id_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<String>> {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                limit: 1
            ) {{ request_id }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query AgentRequest doc {doc_id} failed: {:?}",
            response.errors
        );
    }
    Ok(request_rows(response.data.as_ref())?
        .into_iter()
        .next()
        .map(|row| row.request_id))
}

pub(super) async fn load_terminal_child_request_ids(node: &EmbeddedNode) -> Result<Vec<String>> {
    let terminal_states = RequestLifecycleState::terminal_graphql_list();
    let query = format!(
        r#"{{
        AgentRequest(
            filter: {{
                lifecycle_state: {{ _in: {terminal_states} }}
            }}
        ) {{
            request_id
            caused_by_parent_request_id
            caused_by_parent_tool_call_id
        }}
    }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query terminal child AgentRequests failed: {:?}",
            response.errors
        );
    }

    let rows = request_rows(response.data.as_ref())?;
    Ok(rows
        .into_iter()
        .filter(|row| {
            non_empty(row.caused_by_parent_request_id.as_deref()).is_some()
                && non_empty(row.caused_by_parent_tool_call_id.as_deref()).is_some()
        })
        .map(|row| row.request_id)
        .collect())
}

fn request_rows(data: Option<&serde_json::Value>) -> Result<Vec<AgentRequestRow>> {
    let value = data
        .and_then(|data| data.get("AgentRequest"))
        .context("AgentRequest field missing from query response")?;
    serde_json::from_value(value.clone()).context("decode AgentRequest rows")
}
