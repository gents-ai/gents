use super::super::presentation::terminal_output_plan;
use super::{tool_delivery_key, Terminalization};
use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use gents_protocol::output::reconstruction::ObservedSegment;
use gents_protocol::output::rows::{OutputSegmentRow, CREATE_AGENT_MESSAGE_MUTATION};
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadPresentation, PresentedPayload, SegmentRun, SourceClose,
    StreamDeclaration, StreamPayload, TranscriptMessage,
};
use gents_session::request::lifecycle::queue::next_append_sequence_in_transaction;
use gents_store::config_client::session_children::transcript_message_bound_variables;
use gents_store::graphql::escape_graphql_string;

pub(super) fn plan_source_close(
    terminalization: Terminalization<'_, '_>,
    source_rows: &[OutputSegmentRow],
    source: OutputSource,
    writer: OutputWriter,
) -> Result<(OutputSegment, PayloadPresentation)> {
    let Terminalization {
        request_doc_id,
        node_did,
        requester_did,
        session_id,
        now,
        fields,
        text,
        pending_raw,
        presentation,
        output_budget,
        ..
    } = terminalization;
    let observed = source_rows
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    let extent = gents_protocol::output::extent::inspect_open_source(
        &observed,
        request_doc_id,
        &source,
        &writer,
    )
    .map_err(anyhow::Error::from)?;
    anyhow::ensure!(
        extent.streams.len() <= 1,
        "tool terminal source has unexpected multiple streams"
    );
    let prefix = extent
        .streams
        .first()
        .map(|stream| stream.text.as_str())
        .unwrap_or("");
    if let Some(last_created_at) = extent.last_created_at.as_deref() {
        let last_created_at = DateTime::parse_from_rfc3339(last_created_at)
            .context("tool raw output has an invalid created_at")?
            .with_timezone(&Utc);
        anyhow::ensure!(
            now > last_created_at,
            "clock moved backwards while closing canonical tool output"
        );
    }
    let plan = terminal_output_plan(
        prefix,
        text,
        pending_raw,
        presentation,
        fields.state,
        output_budget,
    )?;
    let suffix = plan.raw[prefix.len()..].to_owned();
    let final_flush = !suffix.is_empty() || extent.segments == 0;
    let stream_bytes = vec![u64::try_from(plan.raw.len())?];
    let delivered_presentation = plan.presentation;
    let segments = extent.segments + u32::from(final_flush);
    let runs = if final_flush {
        vec![SegmentRun {
            stream: 0,
            bytes: u32::try_from(suffix.len())
                .context("tool output exceeds canonical segment size")?,
            declaration: (extent.segments == 0).then_some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::ToolOutput,
            }),
        }]
    } else {
        Vec::new()
    };
    let segment = OutputSegment {
        node_did: node_did.to_owned(),
        requester_did: requester_did.map(str::to_owned),
        session_id: session_id.to_owned(),
        request_doc_id: request_doc_id.to_owned(),
        source,
        writer,
        ordinal: final_flush.then_some(extent.segments),
        runs,
        payload: suffix,
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments,
            stream_bytes,
        }),
        created_at: now.to_rfc3339_opts(SecondsFormat::Nanos, true),
    };
    Ok((segment, delivered_presentation))
}

pub(super) async fn transition_lifecycle(
    terminalization: Terminalization<'_, '_>,
    plugin_receipt_field: &str,
    completed_at: &str,
) -> Result<Option<String>> {
    let Terminalization {
        txn,
        message_sequence,
        spawned_by_tool_call_doc_id,
        plugin_effect,
        deadline_at,
        started_at,
        now,
        expected,
        fields,
        terminal_status,
        plugin_receipt,
        tool,
        request,
        agent,
        session,
        tool_id,
        name,
        requester_filter,
        ..
    } = terminalization;
    let (started_at, latency_ms) = match started_at {
        Some(started) => (
            format!(
                "\"{}\"",
                started.to_rfc3339_opts(SecondsFormat::Nanos, true)
            ),
            (now - started).num_milliseconds().to_string(),
        ),
        None => ("null".to_owned(), "null".to_owned()),
    };
    let deadline_at = deadline_at.to_rfc3339_opts(SecondsFormat::Nanos, true);
    let failure = fields
        .failure
        .map(|value| format!(r#", tool_failure_class: "{}""#, value.as_str()))
        .unwrap_or_default();
    let cancel = fields
        .cancel
        .map(|value| format!(r#", cancel_cause: "{}""#, value.as_str()))
        .unwrap_or_default();
    let state = fields.state.as_str();
    let spawned_filter = spawned_by_tool_call_doc_id
        .map(|parent| {
            format!(
                r#", spawned_by_tool_call_doc_id: {{ _eq: "{}" }}"#,
                escape_graphql_string(parent)
            )
        })
        .unwrap_or_else(|| ", spawned_by_tool_call_doc_id: { _eq: null }".to_owned());
    let effect_filter = match plugin_effect {
        Some(effect) => format!(
            r#", plugin_parent_tool_call_doc_id: {{ _eq: "{}" }}, plugin_effect_ordinal: {{ _eq: {} }}"#,
            escape_graphql_string(&effect.parent.tool_call_doc_id),
            effect.effect.ordinal
        ),
        None => {
            ", plugin_parent_tool_call_doc_id: { _eq: null }, plugin_effect_ordinal: { _eq: null }"
                .to_owned()
        }
    };
    let receipt_variable = if plugin_receipt.is_some() {
        "($plugin_receipt: JSON)"
    } else {
        ""
    };
    let variables = match plugin_receipt {
        Some(receipt) => serde_json::json!({"plugin_receipt": receipt}),
        None => serde_json::json!({}),
    };
    let lifecycle = txn.execute_with_variables(&format!(r#"mutation{receipt_variable} {{ update_AgentToolCall(docID: "{tool}", filter: {{
        _docID: {{ _eq: "{tool}" }}, request_doc_id: {{ _eq: "{request}" }},
        session_id: {{ _eq: "{session}" }}, node_did: {{ _eq: "{agent}" }},
        tool_call_id: {{ _eq: "{tool_id}" }}, tool_name: {{ _eq: "{name}" }},
        message_sequence: {{ _eq: {message_sequence} }},
        lifecycle_state: {{ _eq: "{expected}" }}{requester_filter}{spawned_filter}{effect_filter} }}, input: {{
        status: "{}", lifecycle_state: "{state}", started_at: {started_at},
        deadline_at: "{deadline_at}", completed_at: "{completed_at}", latency_ms: {latency_ms}{failure}{cancel}{plugin_receipt_field}
    }}) {{ _docID request_id }} }}"#, escape_graphql_string(terminal_status)), &variables).await?;
    if !lifecycle["data"]["update_AgentToolCall"]
        .as_array()
        .is_some_and(|rows| !rows.is_empty())
    {
        return Ok(None);
    }

    let request_id = lifecycle["data"]["update_AgentToolCall"][0]["request_id"]
        .as_str()
        .context("published tool terminal omitted logical request identity")?;
    Ok(Some(request_id.to_owned()))
}

pub(super) async fn bind_plugin_terminal_output(
    terminalization: Terminalization<'_, '_>,
    close_doc_id: &str,
    delivered_presentation: &PayloadPresentation,
) -> Result<()> {
    let Terminalization {
        txn,
        plugin_effect,
        fields,
        tool,
        request,
        agent,
        session,
        requester_filter,
        ..
    } = terminalization;
    let state = fields.state.as_str();
    if plugin_effect.is_some() {
        let terminal_output = PresentedPayload {
            output: gents_protocol::output::PayloadRef {
                close_doc_id: close_doc_id.to_owned(),
                stream: 0,
            },
            presentation: delivered_presentation.clone(),
        };
        let updated = txn.execute_with_variables(&format!(
            r#"mutation($terminal_output: JSON) {{ update_AgentToolCall(docID: "{tool}", filter: {{
                request_doc_id: {{ _eq: "{request}" }}, session_id: {{ _eq: "{session}" }},
                node_did: {{ _eq: "{agent}" }}, lifecycle_state: {{ _eq: "{state}" }}{requester_filter}
            }}, input: {{ terminal_output: $terminal_output }}) {{ _docID }} }}"#
        ), &serde_json::json!({"terminal_output": terminal_output})).await?;
        anyhow::ensure!(
            updated["data"]["update_AgentToolCall"]
                .as_array()
                .is_some_and(|rows| rows.len() == 1),
            "plugin terminal presentation lost its physical lifecycle binding"
        );
    }
    Ok(())
}

pub(super) async fn publish_delivery(
    terminalization: Terminalization<'_, '_>,
    close_doc_id: String,
    delivered_presentation: PayloadPresentation,
    completed_at: String,
) -> Result<()> {
    let Terminalization {
        txn,
        tool_doc_id,
        request_doc_id,
        node_did,
        requester_did,
        session_id,
        tool_call_id,
        call_id,
        ..
    } = terminalization;
    let sequence = next_append_sequence_in_transaction(txn, node_did, session_id).await?;
    let message = TranscriptMessage {
        native_request_id: None,
        message_key: tool_delivery_key(session_id, tool_doc_id),
        session_id: session_id.to_owned(),
        node_did: node_did.to_owned(),
        requester_did: requester_did.map(str::to_owned),
        request_doc_id: Some(request_doc_id.to_owned()),
        publication: MessagePublication::ToolDelivery {
            tool_call_doc_id: tool_doc_id.to_owned(),
        },
        outcome: OutputOutcome::Complete,
        sequence,
        role: MessageRole::User,
        native_id: None,
        blocks: vec![MessageBlock::ToolResult {
            tool_call_doc_id: tool_doc_id.to_owned(),
            id: tool_call_id.to_owned(),
            call_id: call_id.map(str::to_owned),
            parts: vec![gents_protocol::output::ToolResultPart::Text {
                text: PresentedPayload {
                    output: gents_protocol::output::PayloadRef {
                        close_doc_id,
                        stream: 0,
                    },
                    presentation: delivered_presentation,
                },
            }],
        }],
        created_at: completed_at,
    };
    txn.execute_with_variables(
        CREATE_AGENT_MESSAGE_MUTATION,
        &transcript_message_bound_variables(txn, &message).await?,
    )
    .await?;
    Ok(())
}
