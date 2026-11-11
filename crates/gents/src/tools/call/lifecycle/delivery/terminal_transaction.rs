use super::TerminalFields;
use crate::session::tool_call_state::ToolCallState;
use crate::store::config_client::session_children::output_segment_bound_variables;
use crate::store::config_client::ConfigApplyTxn;
use crate::store::graphql::{created_doc_id, escape_graphql_string};
use anyhow::Result;
use chrono::{DateTime, SecondsFormat, Utc};
use gents_protocol::output::rows::CREATE_AGENT_OUTPUT_SEGMENT_MUTATION;
use gents_protocol::output::{OutputSource, OutputWriter, PayloadPresentation};

mod admission;
mod publish;
mod replay;

#[derive(Clone, Copy)]
struct Terminalization<'a, 't> {
    txn: &'a ConfigApplyTxn<'t>,
    tool_doc_id: &'a str,
    request_doc_id: &'a str,
    accepted_header_doc_id: &'a str,
    node_did: &'a str,
    requester_did: Option<&'a str>,
    session_id: &'a str,
    tool_call_id: &'a str,
    call_id: Option<&'a str>,
    message_sequence: u32,
    arguments: Option<&'a gents_protocol::output::PayloadRef>,
    spawned_by_tool_call_doc_id: Option<&'a str>,
    plugin_effect: Option<&'a crate::tools::call::lifecycle::plugin_effect::PluginEffectTerminal>,
    tool_name: &'a str,
    deadline_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    expected: &'a str,
    fields: TerminalFields<'a>,
    terminal_status: &'a str,
    plugin_receipt: Option<&'a gents_protocol::plugin::PluginExecutionReceipt>,
    text: &'a str,
    pending_raw: Option<&'a str>,
    presentation: Option<&'a PayloadPresentation>,
    output_budget: Option<usize>,
    adopt_competing_terminal: bool,
    tool: &'a str,
    request: &'a str,
    agent: &'a str,
    session: &'a str,
    tool_id: &'a str,
    name: &'a str,
    requester_filter: &'a str,
}

fn tool_delivery_key(session_id: &str, tool_doc_id: &str) -> String {
    format!("{session_id}:tool-delivery:{tool_doc_id}")
}

fn tool_output_identity(tool_doc_id: &str) -> (OutputSource, OutputWriter) {
    let source = OutputSource::ToolCall {
        tool_call_doc_id: tool_doc_id.to_owned(),
    };
    let writer = OutputWriter::ToolExecution {
        tool_call_doc_id: tool_doc_id.to_owned(),
    };
    (source, writer)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn terminalize_transaction(
    txn: &ConfigApplyTxn<'_>,
    tool_doc_id: &str,
    request_doc_id: &str,
    accepted_header_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    tool_call_id: &str,
    call_id: Option<&str>,
    message_sequence: u32,
    arguments: Option<&gents_protocol::output::PayloadRef>,
    spawned_by_tool_call_doc_id: Option<&str>,
    plugin_effect: Option<&crate::tools::call::lifecycle::plugin_effect::PluginEffectTerminal>,
    tool_name: &str,
    deadline_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    expected: ToolCallState,
    fields: TerminalFields<'_>,
    terminal_status: &str,
    plugin_receipt: Option<&gents_protocol::plugin::PluginExecutionReceipt>,
    text: &str,
    pending_raw: Option<&str>,
    presentation: Option<&PayloadPresentation>,
    output_budget: Option<usize>,
    adopt_competing_terminal: bool,
    publish_native_result: bool,
) -> Result<bool> {
    let tool = escape_graphql_string(tool_doc_id);
    let request = escape_graphql_string(request_doc_id);
    let agent = escape_graphql_string(node_did);
    let session = escape_graphql_string(session_id);
    let expected = expected.as_str();
    let tool_id = escape_graphql_string(tool_call_id);
    let name = escape_graphql_string(tool_name);
    let requester_filter = requester_did
        .map(|value| {
            format!(
                r#", requester_did: {{ _eq: "{}" }}"#,
                escape_graphql_string(value)
            )
        })
        .unwrap_or_else(|| ", requester_did: { _eq: null }".to_owned());
    let terminalization = Terminalization {
        txn,
        tool_doc_id,
        request_doc_id,
        accepted_header_doc_id,
        node_did,
        requester_did,
        session_id,
        tool_call_id,
        call_id,
        message_sequence,
        arguments,
        spawned_by_tool_call_doc_id,
        plugin_effect,
        tool_name,
        deadline_at,
        started_at,
        now,
        expected,
        fields,
        terminal_status,
        plugin_receipt,
        text,
        pending_raw,
        presentation,
        output_budget,
        adopt_competing_terminal,
        tool: &tool,
        request: &request,
        agent: &agent,
        session: &session,
        tool_id: &tool_id,
        name: &name,
        requester_filter: &requester_filter,
    };
    if !replay::competing_terminal_permits(terminalization).await? {
        return Ok(false);
    }
    admission::validate_accepted_invocation(terminalization).await?;
    let Some(plugin_receipt_field) = admission::plugin_receipt_field(terminalization).await? else {
        return Ok(false);
    };
    let publish_native_result =
        admission::resolve_native_publication(terminalization, publish_native_result).await?;
    if !replay::delivery_replay_permits(terminalization, publish_native_result).await? {
        return Ok(false);
    }

    let (source, writer) = tool_output_identity(tool_doc_id);
    let source_rows = crate::session::load_source_rows_in_txn(
        txn,
        request_doc_id,
        &source,
        node_did,
        session_id,
        requester_did,
    )
    .await?;
    if !replay::closed_source_permits(terminalization, &source_rows).await? {
        return Ok(false);
    }
    admission::ensure_open_plugin_effect_has_no_terminal_output(terminalization).await?;
    let (segment, delivered_presentation) =
        publish::plan_source_close(terminalization, &source_rows, source, writer)?;
    let completed_at = now.to_rfc3339_opts(SecondsFormat::Nanos, true);
    let Some(request_id) =
        publish::transition_lifecycle(terminalization, &plugin_receipt_field, &completed_at)
            .await?
    else {
        return Ok(false);
    };

    crate::session::refresh_session_request_observation_in_txn(
        txn,
        node_did,
        requester_did,
        session_id,
        crate::session::SessionRequestRefresh {
            request_doc_id,
            request_id: &request_id,
            reply_preview: None,
        },
        &completed_at,
    )
    .await?;

    let segment_response = txn
        .execute_with_variables(
            CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
            &output_segment_bound_variables(txn, &segment).await?,
        )
        .await?;
    let close_doc_id = created_doc_id(&segment_response, "AgentOutputSegment")?;

    publish::bind_plugin_terminal_output(terminalization, &close_doc_id, &delivered_presentation)
        .await?;

    if spawned_by_tool_call_doc_id.is_some() || !publish_native_result {
        return Ok(true);
    }

    publish::publish_delivery(
        terminalization,
        close_doc_id,
        delivered_presentation,
        completed_at,
    )
    .await?;
    Ok(true)
}
