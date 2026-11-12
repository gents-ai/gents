use anyhow::Result;
use gents_protocol::client_protocol::RequestLifecycleState;
use gents_protocol::operations::request::{RequestInterruptInput, RequestWaitInput};
use gents_protocol::row::AgentRequestRow;
use gents_session::session::tool_call_state::CancelCause;
use gents_store::config_client::ConfigAccess;
use serde_json::{json, Value};

use crate::cli::args::RequestInterruptArgs;
use crate::cli::output_format::OutputFormat;
use crate::operation_failure::OperationFailure;
use crate::request_helpers::{parse_duration_suffix, request_ref};
use crate::{print_json, resolve_request_id};
use gents_service::home_state::{resolve_graphql_endpoint, resolve_node_did};

pub(super) async fn request_interrupt(args: RequestInterruptArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let node_did = resolve_node_did(args.home.as_deref(), args.node_did.as_deref())?;
    let request_id =
        resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;
    let access = ConfigAccess::Graphql(graphql);
    let request = request_ref(&node_did, &request_id);

    let receipt = gents::operations::request::interrupt(
        &access,
        RequestInterruptInput {
            request: request.clone(),
        },
    )
    .await
    .map_err(OperationFailure::from)?;

    let row = if args.wait {
        let timeout = parse_duration_suffix(&args.timeout)?;
        let input = RequestWaitInput {
            request,
            timeout_secs: timeout.as_secs(),
            poll_secs: 1,
        };
        gents_session::operations::request::wait(&access, input, || async { Ok(false) })
            .await
            .map_err(OperationFailure::from)?
            .0
    } else {
        gents_session::operations::request::get(&access, &request)
            .await
            .map_err(OperationFailure::from)?
    };

    let cause = (!receipt.already_interrupted).then(|| CancelCause::UserCancelled.as_str());
    let interrupt_landed_at = (!receipt.already_interrupted)
        .then_some(receipt.interrupt_requested_at.as_deref())
        .flatten();
    let summary = request_interrupt_summary(
        &row,
        cause,
        interrupt_landed_at,
        receipt.already_interrupted,
    );
    match args.output.ensure_supported(
        "utils debug request interrupt",
        &[OutputFormat::Text, OutputFormat::Json],
    )? {
        OutputFormat::Json => print_json(&summary)?,
        OutputFormat::Text => print_interrupt_text(&summary)?,
        _ => unreachable!("ensure_supported restricts request interrupt output formats"),
    }
    Ok(())
}

fn nonempty_owned(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn request_interrupt_summary(
    row: &AgentRequestRow,
    cause: Option<&str>,
    interrupt_landed_at: Option<&str>,
    already_interrupted: bool,
) -> Value {
    let lifecycle_state = row
        .lifecycle_state
        .map(RequestLifecycleState::as_str)
        .unwrap_or_default();
    json!({
        "request_id": row.request_id,
        "node_did": nonempty_owned(row.node_did.as_deref()),
        "agent_did": nonempty_owned(row.agent_did.as_deref()),
        "session_id": nonempty_owned(row.session_id.as_deref()),
        "lifecycle_state": lifecycle_state,
        "failure_reason": nonempty_owned(row.failure_reason.as_deref()),
        "interrupt_requested_at": nonempty_owned(row.interrupt_requested_at.as_deref()),
        "interrupt_landed_at": interrupt_landed_at,
        "cause": cause,
        "already_interrupted": already_interrupted,
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
        "terminal: {}",
        summary
            .get("terminal")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    );
    if let Some(reason) = summary
        .get("failure_reason")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        println!("failure_reason: {reason}");
    }
    Ok(())
}
