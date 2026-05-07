use super::retry::{execute_query_timed, log_mutation_timing, retry_operation};
use super::rows::{ToolCallDocument, ToolCallResultRow};
use super::*;
use crate::trace_export::{analyze_tool_call, latency_ms, ToolCallTraceAnalysis, ToolFailureClass};

pub(crate) async fn save_tool_call(
    node: &EmbeddedNode,
    session_id: &str,
    message_sequence: u32,
    tool_name: &str,
    tool_call_id: &str,
    args: &str,
    status: &str,
) -> Result<()> {
    retry_operation("save_tool_call", || async {
        if let Some(tool_call) =
            load_optional_tool_call_document(node, session_id, tool_call_id).await?
        {
            if tool_call_is_completed(&tool_call) {
                return Ok(());
            }

            return update_started_tool_call(node, &tool_call, status).await;
        }

        create_tool_call(
            node,
            session_id,
            message_sequence,
            tool_name,
            tool_call_id,
            args,
            status,
        )
        .await
    })
    .await
}

#[allow(clippy::too_many_arguments)]
async fn create_tool_call(
    node: &EmbeddedNode,
    session_id: &str,
    message_sequence: u32,
    tool_name: &str,
    tool_call_id: &str,
    args: &str,
    status: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let trace_fields = trace_fields_for_started_call(tool_name, args);
    let escaped_args = escape_graphql_string(args);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_tool_call_id = escape_graphql_string(tool_call_id);
    let escaped_tool_name = escape_graphql_string(tool_name);
    let escaped_status = escape_graphql_string(status);
    let tool_call_key = format!("{escaped_session_id}:{escaped_tool_call_id}");
    let selected_service_id = nullable_string_literal(trace_fields.selected_service_id.as_deref());
    let selected_tool_name = nullable_string_literal(trace_fields.selected_tool_name.as_deref());
    let tool_failure_class = nullable_string_literal(trace_fields.tool_failure_class.as_deref());

    let mutation = format!(
        r#"mutation {{
            create_AgentToolCall(input: {{
                tool_call_key: "{tool_call_key}",
                session_id: "{escaped_session_id}",
                message_sequence: {message_sequence},
                tool_name: "{escaped_tool_name}",
                tool_call_id: "{escaped_tool_call_id}",
                args: "{escaped_args}",
                result: "",
                status: "{escaped_status}",
                started_at: "{now}",
                selected_service_id: {selected_service_id},
                selected_tool_name: {selected_tool_name},
                tool_failure_class: {tool_failure_class},
                latency_ms: null
            }}) {{ _docID }}
        }}"#
    );

    execute_tool_call_mutation_once(node, &mutation, "save_tool_call").await
}

async fn update_started_tool_call(
    node: &EmbeddedNode,
    tool_call: &ToolCallDocument,
    status: &str,
) -> Result<()> {
    let escaped_started_at = escape_graphql_string(&tool_call.started_at);
    let escaped_status = escape_graphql_string(status);
    let mutation = format!(
        r#"mutation {{
            update_AgentToolCall(
                filter: {{
                    _docID: {{ _eq: "{doc_id}" }}
                }},
                input: {{
                    started_at: "{escaped_started_at}",
                    status: "{escaped_status}"
                }}
            ) {{ _docID }}
        }}"#,
        doc_id = tool_call.doc_id,
    );

    execute_tool_call_mutation_once(node, &mutation, "save_tool_call").await
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
        let trace_fields = trace_fields_for_completed_call(&tool_call, result, status, &now);
        let escaped_started_at = escape_graphql_string(&tool_call.started_at);
        let selected_service_id =
            nullable_string_literal(trace_fields.selected_service_id.as_deref());
        let selected_tool_name =
            nullable_string_literal(trace_fields.selected_tool_name.as_deref());
        let tool_failure_class =
            nullable_string_literal(trace_fields.tool_failure_class.as_deref());
        let latency_ms = nullable_i64_literal(trace_fields.latency_ms);
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
                        completed_at: "{now}",
                        selected_service_id: {selected_service_id},
                        selected_tool_name: {selected_tool_name},
                        tool_failure_class: {tool_failure_class},
                        latency_ms: {latency_ms}
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
                tool_name
                args
                started_at
                completed_at
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

async fn load_optional_tool_call_document(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> Result<Option<ToolCallDocument>> {
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
                tool_name
                args
                started_at
                completed_at
            }}
        }}"#
    );

    let resp = execute_query_timed(node, &query, "load_tool_call_document").await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading tool call session_id={} tool_call_id={}: {:?}",
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

    Ok(rows.pop())
}

fn tool_call_is_completed(tool_call: &ToolCallDocument) -> bool {
    tool_call
        .completed_at
        .as_deref()
        .is_some_and(|value| !value.is_empty())
}

async fn execute_tool_call_mutation_once(
    node: &EmbeddedNode,
    mutation: &str,
    operation: &str,
) -> Result<()> {
    let started_at = std::time::Instant::now();
    let resp = node.execute(mutation).await;
    log_mutation_timing(operation, started_at.elapsed());

    if !resp.has_errors() {
        return Ok(());
    }

    anyhow::bail!("{operation} mutation failed: {:?}", resp.errors)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolCallTraceFields {
    selected_service_id: Option<String>,
    selected_tool_name: Option<String>,
    tool_failure_class: Option<String>,
    latency_ms: Option<i64>,
}

fn trace_fields_for_started_call(tool_name: &str, args: &str) -> ToolCallTraceFields {
    // At call start only arguments are known; completion recomputes with the real result/status.
    let analysis = analyze_tool_call(tool_name, args, "", "completed");
    ToolCallTraceFields::from_analysis(analysis, None)
}

fn trace_fields_for_completed_call(
    tool_call: &ToolCallDocument,
    result: &str,
    status: &str,
    completed_at: &str,
) -> ToolCallTraceFields {
    let analysis = analyze_tool_call(&tool_call.tool_name, &tool_call.args, result, status);
    ToolCallTraceFields::from_analysis(
        analysis,
        latency_ms(Some(&tool_call.started_at), Some(completed_at)),
    )
}

impl ToolCallTraceFields {
    fn from_analysis(analysis: ToolCallTraceAnalysis, latency_ms: Option<i64>) -> Self {
        Self {
            selected_service_id: analysis.selected_service_id,
            selected_tool_name: analysis.selected_tool_name,
            tool_failure_class: analysis
                .tool_failure_class
                .and_then(tool_failure_class_string),
            latency_ms,
        }
    }
}

fn tool_failure_class_string(failure_class: ToolFailureClass) -> Option<String> {
    match serde_json::to_value(failure_class).ok()? {
        serde_json::Value::String(value) => Some(value),
        _ => None,
    }
}

fn nullable_string_literal(value: Option<&str>) -> String {
    value
        .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
        .unwrap_or_else(|| "null".to_string())
}

fn nullable_i64_literal(value: Option<i64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string())
}
