use super::retry::{
    execute_mutation_with_retry, execute_query_timed, log_mutation_timing, retry_operation,
};
use super::rows::{ToolCallDocument, ToolCallResultRow};
use super::*;

pub(crate) async fn save_tool_call(
    node: &EmbeddedNode,
    session_id: &str,
    message_sequence: u32,
    tool_name: &str,
    tool_call_id: &str,
    args: &str,
    status: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let escaped_args = escape_graphql_string(args);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_tool_call_id = escape_graphql_string(tool_call_id);
    let escaped_tool_name = escape_graphql_string(tool_name);
    let escaped_status = escape_graphql_string(status);
    let tool_call_key = format!("{escaped_session_id}:{escaped_tool_call_id}");

    let mutation = format!(
        r#"mutation {{
            upsert_AgentToolCall(
                filter: {{ tool_call_key: {{ _eq: "{tool_call_key}" }} }},
                add: {{
                    tool_call_key: "{tool_call_key}",
                    session_id: "{escaped_session_id}",
                    message_sequence: {message_sequence},
                    tool_name: "{escaped_tool_name}",
                    tool_call_id: "{escaped_tool_call_id}",
                    args: "{escaped_args}",
                    result: "",
                    status: "{escaped_status}",
                    started_at: "{now}"
                }},
                update: {{
                    status: "{escaped_status}"
                }}
            ) {{ _docID }}
        }}"#
    );

    execute_mutation_with_retry(node, &mutation, "save_tool_call").await?;
    Ok(())
}

pub(crate) async fn complete_tool_call(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
    result: &str,
    status: &str,
) -> Result<()> {
    let escaped_result = escape_graphql_string(result);
    let escaped_status = escape_graphql_string(status);

    retry_operation("complete_tool_call", || async {
        let tool_call = load_tool_call_document(node, session_id, tool_call_id).await?;

        let now = chrono::Utc::now().to_rfc3339();
        let escaped_started_at = escape_graphql_string(&tool_call.started_at);
        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }}
                    }},
                    input: {{
                        started_at: "{escaped_started_at}",
                        result: "{escaped_result}",
                        status: "{escaped_status}",
                        completed_at: "{now}"
                    }}
                ) {{ _docID }}
            }}"#,
            doc_id = tool_call.doc_id,
        );

        let started_at = std::time::Instant::now();
        let resp = node.execute(&mutation).await;
        log_mutation_timing("complete_tool_call", started_at.elapsed());

        if !resp.has_errors() {
            return Ok(());
        }

        anyhow::bail!("complete_tool_call mutation failed: {:?}", resp.errors)
    })
    .await
}

pub async fn load_tool_call_result(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> Result<String> {
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_tool_call_id = escape_graphql_string(tool_call_id);
    let tool_call_key = format!("{escaped_session_id}:{escaped_tool_call_id}");
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    tool_call_key: {{ _eq: "{tool_call_key}" }}
                }},
                limit: 1
            ) {{
                result
            }}
        }}"#
    );

    let resp = execute_query_timed(node, &query, "load_tool_call_result").await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading tool call result for session_id={} tool_call_id={}: {:?}",
            session_id,
            tool_call_id,
            resp.errors
        );
    }

    let mut rows: Vec<ToolCallResultRow> = match resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
    {
        Some(value) => serde_json::from_value(value.clone())?,
        None => Vec::new(),
    };

    rows.pop().map(|row| row.result).ok_or_else(|| {
        anyhow::anyhow!(
            "loading tool call result: no AgentToolCall for session_id={session_id} tool_call_id={tool_call_id}"
        )
    })
}

async fn load_tool_call_document(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> Result<ToolCallDocument> {
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_tool_call_id = escape_graphql_string(tool_call_id);
    let tool_call_key = format!("{escaped_session_id}:{escaped_tool_call_id}");
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    tool_call_key: {{ _eq: "{tool_call_key}" }}
                }},
                limit: 1
            ) {{
                _docID
                started_at
            }}
        }}"#
    );

    let resp = execute_query_timed(node, &query, "load_tool_call_document").await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading tool call for completion session_id={} tool_call_id={}: {:?}",
            session_id,
            tool_call_id,
            resp.errors
        );
    }

    let mut rows: Vec<ToolCallDocument> = match resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
    {
        Some(value) => serde_json::from_value(value.clone())?,
        None => Vec::new(),
    };

    rows.pop().ok_or_else(|| {
        anyhow::anyhow!(
            "loading tool call for completion: no AgentToolCall for session_id={session_id} tool_call_id={tool_call_id}"
        )
    })
}
