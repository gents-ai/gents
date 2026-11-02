use gents::store::config_client::GraphqlEndpoint;
use std::time::Duration;

use anyhow::{Context, Result};
use gents::{
    store::graphql::escape_graphql_string, tools::call::lifecycle::CancelCause, ConfigAccess,
};
use gents_protocol::client_protocol::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde_json::{json, Value};
use tokio::time::Instant;

use crate::cli::args::RequestInterruptArgs;
use crate::cli::output_format::OutputFormat;
use crate::request_helpers::parse_duration_suffix;
use crate::{post_graphql, print_json, resolve_graphql_endpoint, resolve_request_id};

pub(super) async fn request_interrupt(args: RequestInterruptArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let request_id =
        resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;

    let before = fetch_interrupt_request_row(&graphql, &request_id).await?;
    let already_interrupted = nonempty_owned(before.interrupt_requested_at.as_deref()).is_some();
    let already_terminal = before.is_terminal();

    if !already_interrupted && !already_terminal {
        let physical = before
            .doc_id
            .as_deref()
            .context("request has no physical identity")?;
        let node = before
            .node_did
            .as_deref()
            .context("request has no node identity")?;
        gents::request::interrupt::interrupt_request_by_doc_id_with_access(
            &ConfigAccess::Graphql(graphql.clone()),
            physical,
            node,
            before.requester_did.as_deref(),
        )
        .await?;
    }

    let mut row = fetch_interrupt_request_row(&graphql, &request_id).await?;
    let interrupt_landed_at = nonempty_owned(row.interrupt_requested_at.as_deref());
    if !already_terminal && interrupt_landed_at.is_none() {
        anyhow::bail!("request {request_id} did not persist interrupt_requested_at");
    }

    if args.wait && !row.is_terminal() {
        let timeout = parse_duration_suffix(&args.timeout)?;
        row = wait_for_terminal_request_state(&graphql, &request_id, timeout, row).await?;
    }

    let summary = request_interrupt_summary(
        &row,
        CancelCause::UserCancelled.as_str(),
        interrupt_landed_at.as_deref(),
        already_interrupted,
        already_terminal,
    );
    match args.output.ensure_supported(
        "request interrupt",
        &[OutputFormat::Text, OutputFormat::Json],
    )? {
        OutputFormat::Json => print_json(&summary)?,
        OutputFormat::Text => print_interrupt_text(&summary)?,
        _ => unreachable!("ensure_supported restricts request interrupt output formats"),
    }
    Ok(())
}

async fn fetch_interrupt_request_row(
    graphql: &GraphqlEndpoint,
    request_id: &str,
) -> Result<AgentRequestRow> {
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                limit: 2
            ) {{
                _docID request_id
                node_did requester_did
                agent_id
                session_id
                lifecycle_state
                failure_reason
                retry_count
                max_retries
                created_at
                claimed_at
                deadline
                valid_until
                interrupt_requested_at
            }}
        }}"#,
        request_id = escape_graphql_string(request_id),
    );
    let response = post_graphql(graphql, &query).await?;
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .context("interrupt request query omitted rows")?;
    anyhow::ensure!(
        rows.len() == 1,
        "request {request_id} must resolve to exactly one accessible physical request"
    );
    serde_json::from_value(rows[0].clone())
        .with_context(|| format!("decoding AgentRequest {request_id}"))
}

async fn wait_for_terminal_request_state(
    graphql: &GraphqlEndpoint,
    request_id: &str,
    timeout: Duration,
    mut last_row: AgentRequestRow,
) -> Result<AgentRequestRow> {
    let deadline = Instant::now() + timeout;
    loop {
        if last_row.is_terminal() {
            return Ok(last_row);
        }
        if Instant::now() >= deadline {
            let state = last_row
                .lifecycle_state
                .map(RequestLifecycleState::as_str)
                .unwrap_or("<missing>");
            anyhow::bail!(
                "timed out waiting for request {request_id} to reach a terminal state after {}s (last lifecycle_state={state})",
                timeout.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        last_row = fetch_interrupt_request_row(graphql, request_id).await?;
    }
}

fn nonempty_owned(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn request_interrupt_summary(
    row: &AgentRequestRow,
    cause: &str,
    interrupt_landed_at: Option<&str>,
    already_interrupted: bool,
    already_terminal: bool,
) -> Value {
    let lifecycle_state = row
        .lifecycle_state
        .map(RequestLifecycleState::as_str)
        .unwrap_or_default();
    json!({
        "request_id": row.request_id,
        "node_did": nonempty_owned(row.node_did.as_deref()),
        "agent_id": nonempty_owned(row.agent_id.as_deref()),
        "session_id": nonempty_owned(row.session_id.as_deref()),
        "lifecycle_state": lifecycle_state,
        "failure_reason": nonempty_owned(row.failure_reason.as_deref()),
        "interrupt_requested_at": nonempty_owned(row.interrupt_requested_at.as_deref()),
        "interrupt_landed_at": interrupt_landed_at,
        "cause": cause,
        "already_interrupted": already_interrupted,
        "already_terminal": already_terminal,
        "terminal": row.is_terminal(),
        "created_at": nonempty_owned(row.created_at.as_deref()),
        "claimed_at": nonempty_owned(row.claimed_at.as_deref()),
        "deadline": nonempty_owned(row.deadline.as_deref()),
        "valid_until": nonempty_owned(row.valid_until.as_deref()),
    })
}

fn print_interrupt_text(summary: &Value) -> Result<()> {
    let text = |key: &str| {
        summary
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or("-")
            .to_string()
    };
    println!("request_id: {}", text("request_id"));
    println!("state: {}", text("lifecycle_state"));
    println!("interrupt_landed_at: {}", text("interrupt_landed_at"));
    println!("cause: {}", text("cause"));
    println!(
        "already_interrupted: {}",
        summary
            .get("already_interrupted")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    );
    println!(
        "already_terminal: {}",
        summary
            .get("already_terminal")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    );
    println!(
        "terminal: {}",
        summary
            .get("terminal")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    );
    if summary
        .get("already_terminal")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && summary
            .get("interrupt_landed_at")
            .is_none_or(Value::is_null)
    {
        println!("note: request was already terminal; interrupt was not latched");
    }
    if let Some(reason) = summary
        .get("failure_reason")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        println!("failure_reason: {reason}");
    }
    Ok(())
}
