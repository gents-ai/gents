use gents::store::config_client::GraphqlEndpoint;
use std::collections::BTreeSet;

use anyhow::{Context, Result};
use gents::{store::graphql::escape_graphql_string, ConfigAccess};
use gents_protocol::row::AgentRequestRow;
use serde_json::Value;

use crate::cli::args::RequestShowArgs;
use crate::cli::output_format::OutputFormat;
use crate::request_helpers::{observe_canonical_request_output, request_output_envelope};
use crate::{post_graphql, print_json, resolve_graphql_endpoint, resolve_request_id};

use super::show_text::render_request_show_text;
use super::show_tools::{
    active_native_executor_counts_by_tool, active_tool_call_keys, backgrounded_tool_view,
    child_request_view, native_executor_matches_request, native_executor_view, tool_call_view,
};
use super::show_views::{
    request_cancel_cause_view, request_header_view, string_field, terminal_cause,
    transition_history, value_array, RequestShowSnapshot,
};

pub(crate) async fn request_show(args: RequestShowArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let request_id =
        resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;
    let snapshot = load_request_show_snapshot(&graphql, &request_id).await?;
    match args
        .output
        .ensure_supported("request show", &[OutputFormat::Text, OutputFormat::Json])?
    {
        OutputFormat::Json => {
            let value = serde_json::to_value(&snapshot)?;
            print_json(&value)?;
        }
        OutputFormat::Text => {
            print!(
                "{}",
                render_request_show_text(&snapshot, chrono::Utc::now())
            );
        }
        _ => unreachable!("ensure_supported restricts request show output formats"),
    }
    Ok(())
}

#[derive(Debug, Clone, Default)]
struct RequestShowSchema {
    agent_request: BTreeSet<String>,
    agent_tool_call: BTreeSet<String>,
}

async fn load_request_show_snapshot(
    graphql: &GraphqlEndpoint,
    request_id: &str,
) -> Result<RequestShowSnapshot> {
    let schema = load_request_show_schema(graphql).await;
    let request_response = post_graphql(graphql, &request_show_request_query(request_id, &schema))
        .await
        .with_context(|| format!("loading AgentRequest {request_id}"))?;
    let request_rows = request_response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .context("request show query omitted rows")?;
    anyhow::ensure!(
        request_rows.len() == 1,
        "request {request_id} is missing or ambiguous"
    );
    let request_row = request_rows[0].clone();
    let canonical_request: AgentRequestRow = serde_json::from_value(request_row.clone())
        .with_context(|| format!("decoding AgentRequest {request_id}"))?;

    let output = observe_canonical_request_output(graphql, &canonical_request).await?;
    let envelope = request_output_envelope(&canonical_request, output)?;

    let tool_response = post_graphql(
        graphql,
        &request_show_tool_calls_query(&canonical_request, &schema)?,
    )
    .await
    .with_context(|| format!("loading AgentToolCall rows for {request_id}"))?;
    let tool_rows = value_array(&tool_response, "/data/AgentToolCall");

    let access = ConfigAccess::Graphql(graphql.clone());
    let child_requests = crate::caused_sessions::started_by_request(&access, &canonical_request)
        .await
        .with_context(|| format!("loading sessions started by {request_id}"))?
        .into_iter()
        .map(|(_, first)| serde_json::to_value(first).map(|row| child_request_view(&row)))
        .collect::<Result<Vec<_>, _>>()?;

    let request_terminal = canonical_request.is_terminal();
    let request_node_did = canonical_request.node_did.unwrap_or_default();
    let blocked = gents::goal::blocked_turn::blocked_turn(&access, &request_node_did, request_id)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(request_id, error = %format!("{error:#}"), "blocked turn unreadable");
            None
        });
    let liveness = crate::commands::status::load_liveness_value(graphql, &request_node_did).await;
    let active_tool_calls = active_tool_call_keys(&liveness);
    let native_executors_available = liveness
        .get("active_native_executors_available")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let native_executor_values = value_array(&liveness, "/active_native_executors");
    let tool_names = tool_rows
        .iter()
        .filter_map(|row| string_field(row, "tool_name"))
        .collect::<BTreeSet<_>>();
    let tool_call_ids = tool_rows
        .iter()
        .filter_map(|row| string_field(row, "tool_call_id"))
        .collect::<BTreeSet<_>>();
    let tool_call_keys = tool_rows
        .iter()
        .filter_map(|row| string_field(row, "tool_call_key"))
        .collect::<BTreeSet<_>>();
    let relevant_native_executor_values = native_executor_values
        .iter()
        .filter(|executor| {
            native_executor_matches_request(
                executor,
                request_id,
                &tool_names,
                &tool_call_ids,
                &tool_call_keys,
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let active_executor_counts =
        active_native_executor_counts_by_tool(&tool_rows, &relevant_native_executor_values);

    let tool_calls = tool_rows
        .iter()
        .map(|row| tool_call_view(row, &active_tool_calls, &active_executor_counts))
        .collect::<Vec<_>>();
    let backgrounded_tools = tool_calls
        .iter()
        .filter(|tool| {
            tool.await_mode == "background"
                || tool.active_tool_call
                || tool.active_native_executor_count > 0
        })
        .map(backgrounded_tool_view)
        .collect::<Vec<_>>();
    let native_executors = if request_terminal {
        Vec::new()
    } else {
        relevant_native_executor_values
            .iter()
            .map(native_executor_view)
            .collect()
    };
    let cancel_cause = request_cancel_cause_view(&request_row);
    let terminal_cause = terminal_cause(&request_row, cancel_cause.as_ref());
    let transition_history = transition_history(&request_row, terminal_cause.as_deref());
    let request = request_header_view(&request_row, terminal_cause, transition_history)?;

    Ok(RequestShowSnapshot {
        request,
        output: envelope.output,
        cancel_cause,
        tool_calls,
        backgrounded_tools,
        native_executors_available,
        native_executors,
        child_requests,
        blocked,
    })
}

async fn load_request_show_schema(graphql: &GraphqlEndpoint) -> RequestShowSchema {
    RequestShowSchema {
        agent_request: load_graphql_type_fields(graphql, "AgentRequest").await,
        agent_tool_call: load_graphql_type_fields(graphql, "AgentToolCall").await,
    }
}

async fn load_graphql_type_fields(graphql: &GraphqlEndpoint, type_name: &str) -> BTreeSet<String> {
    let query = format!(
        r#"{{
            __type(name: "{type_name}") {{
                fields {{ name }}
            }}
        }}"#
    );
    let Ok(response) = post_graphql(graphql, &query).await else {
        return BTreeSet::new();
    };
    response
        .pointer("/data/__type/fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|field| field.get("name").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect()
}

fn request_show_request_query(request_id: &str, schema: &RequestShowSchema) -> String {
    let mut fields = vec![
        "_docID",
        "requester_did",
        "request_id",
        "node_did",
        "agent_id",
        "session_id",
        "lifecycle_state",
        "backend_id",
        "execution_origin",
        "failure_reason",
        "retry_count",
        "max_retries",
        "max_total_tokens",
        "input",
        "created_at",
        "claimed_at",
        "deadline",
        "terminalized_at",
        "valid_until",
        "interrupt_requested_at",
        "retry_parent_request",
        "retry_root_request",
        "superseded_by_request",
        "caused_by_parent_request_id",
        "caused_by_parent_tool_call_id",
        "caused_by_trigger_id",
        "caused_by_trigger_kind",
        "caused_by_correlation",
        "caused_by_trigger_context",
        "caused_by_source_doc_id",
        "request_hop",
    ];
    append_optional_fields(
        &mut fields,
        &schema.agent_request,
        &[
            "cancel_cause",
            "terminal_output",
            "execution_generation",
            "execution_lease_secs",
            "execution_lease_expires_at",
            "workspace_id",
            "workspace_authority",
            "workspace_owner_node_did",
            "workspace_seal_hash",
        ],
    );
    let fields = fields.join("\n                ");
    format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }}
            ) {{
                {fields}
            }}
        }}"#,
        request_id = escape_graphql_string(request_id),
    )
}

fn request_show_tool_calls_query(
    request: &AgentRequestRow,
    schema: &RequestShowSchema,
) -> Result<String> {
    let request_doc_id = request
        .doc_id
        .as_deref()
        .context("request show missing physical identity")?;
    let scope = gents::session::session_scope_filter(
        request
            .node_did
            .as_deref()
            .context("request show missing node")?,
        request
            .session_id
            .as_deref()
            .context("request show missing session")?,
        request.requester_did.as_deref(),
    );
    let mut fields = vec![
        "tool_call_key",
        "request_id",
        "session_id",
        "message_sequence",
        "tool_name",
        "tool_call_id",
        "status",
        "lifecycle_state",
        "started_at",
        "deadline_at",
        "completed_at",
        "await_mode",
    ];
    append_optional_fields(&mut fields, &schema.agent_tool_call, &["cancel_cause"]);
    let fields = fields.join("\n                ");
    Ok(format!(
        r#"{{
            AgentToolCall(
                filter: {{ {scope}, request_doc_id: {{ _eq: "{request_doc_id}" }} }},
                order: {{ started_at: ASC }}
            ) {{
                {fields}
            }}
        }}"#,
        request_doc_id = escape_graphql_string(request_doc_id),
    ))
}

fn append_optional_fields(
    fields: &mut Vec<&'static str>,
    available_fields: &BTreeSet<String>,
    optional_fields: &[&'static str],
) {
    for field in optional_fields {
        if available_fields.contains(*field) {
            fields.push(field);
        }
    }
}
