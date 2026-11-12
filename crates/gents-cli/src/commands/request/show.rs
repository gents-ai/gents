use gents_store::config_client::GraphqlEndpoint;
use std::collections::BTreeSet;

use anyhow::{Context, Result};
use gents_protocol::row::AgentRequestRow;
use gents_store::config_client::ConfigAccess;
use serde_json::Value;

use crate::cli::args::RequestShowArgs;
use crate::cli::output_format::OutputFormat;
use crate::operation_failure::OperationFailure;
use crate::request_helpers::{request_output_envelope, request_ref};
use crate::{print_json, resolve_request_id};
use gents_service::home_state::{resolve_graphql_endpoint, resolve_node_did};

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
    let node_did = resolve_node_did(args.home.as_deref(), args.node_did.as_deref())?;
    let request_id =
        resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;
    let snapshot = load_request_show_snapshot(&graphql, &node_did, &request_id).await?;
    match args.output.ensure_supported(
        "utils debug request show",
        &[OutputFormat::Text, OutputFormat::Json],
    )? {
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

async fn load_request_show_snapshot(
    graphql: &GraphqlEndpoint,
    node_did: &str,
    request_id: &str,
) -> Result<RequestShowSnapshot> {
    let access = ConfigAccess::Graphql(graphql.clone());
    let canonical_request =
        gents_session::operations::request::get(&access, &request_ref(node_did, request_id))
            .await
            .map_err(OperationFailure::from)?;
    let request_row = serde_json::to_value(&canonical_request)?;

    let output =
        gents_session::session::observe_request_output(&access, &canonical_request).await?;
    let envelope = request_output_envelope(&canonical_request, output)?;

    let tool_response = access
        .read(&request_show_tool_calls_query(&canonical_request)?)
        .await
        .with_context(|| format!("loading AgentToolCall rows for {request_id}"))?;
    let tool_rows = gents_protocol::document_query::rows(&tool_response, "AgentToolCall").to_vec();

    let child_requests =
        gents_session::session::caused::started_by_request(&access, &canonical_request)
            .await
            .with_context(|| format!("loading sessions started by {request_id}"))?
            .into_iter()
            .map(|(_, first)| serde_json::to_value(first).map(|row| child_request_view(&row)))
            .collect::<Result<Vec<_>, _>>()?;

    let request_terminal = canonical_request.is_terminal();
    let request_node_did = canonical_request.node_did.unwrap_or_default();
    let blocked = gents_automation::goal::blocked_turn::blocked_turn(
        &access,
        &request_node_did,
        request_id,
    )
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

fn request_show_tool_calls_query(
    request: &AgentRequestRow,
) -> Result<gents_protocol::document_query::Rendered> {
    use gents_protocol::document_query::{
        fields::agent_tool_call as fields, session_scope, Direction, Filter, Query, Selection,
    };
    let request_doc_id = request
        .doc_id
        .as_deref()
        .context("debug request show missing physical identity")?;
    let scope = session_scope(
        request
            .node_did
            .as_deref()
            .context("debug request show missing node")?,
        request
            .session_id
            .as_deref()
            .context("debug request show missing session")?,
        request.requester_did.as_deref(),
    );
    Ok(Query::select(Selection::owned(
        "tool_call_key request_id session_id message_sequence tool_name tool_call_id status \
         lifecycle_state started_at deadline_at completed_at await_mode cancel_cause",
    ))
    .filter(scope.and(Filter::eq(fields::REQUEST_DOC_ID, request_doc_id)))
    .order_by(fields::STARTED_AT, Direction::Asc)
    .render())
}
