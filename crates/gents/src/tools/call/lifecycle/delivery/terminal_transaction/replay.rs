use super::super::presentation::{
    diagnostic_replay_matches, terminal_metadata_matches, terminal_output_plan,
};
use crate::session::tool_presentation::render_presentation;
use super::{tool_delivery_key, tool_output_identity, Terminalization};
use crate::store::graphql::escape_graphql_string;
use crate::session::tool_call_state::ToolCallState;
use anyhow::{Context, Result};
use gents_protocol::output::reconstruction::{reconstruct_stream, ObservedSegment};
use gents_protocol::output::rows::{
    decode_transcript_message_row, OutputSegmentRow, AGENT_MESSAGE_FIELDS,
};
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, PresentedPayload, SourceClose,
};

pub(super) async fn competing_terminal_permits(
    terminalization: Terminalization<'_, '_>,
) -> Result<bool> {
    let Terminalization {
        txn,
        tool,
        request,
        session,
        agent,
        tool_id,
        name,
        message_sequence,
        requester_filter,
        expected,
        fields,
        terminal_status,
        adopt_competing_terminal,
        ..
    } = terminalization;
    if adopt_competing_terminal {
        let lifecycle = txn
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{
            _docID: {{ _eq: "{tool}" }}, request_doc_id: {{ _eq: "{request}" }},
            session_id: {{ _eq: "{session}" }}, node_did: {{ _eq: "{agent}" }},
            tool_call_id: {{ _eq: "{tool_id}" }}, tool_name: {{ _eq: "{name}" }},
            message_sequence: {{ _eq: {message_sequence} }}{requester_filter}
        }}, limit: 2) {{ lifecycle_state status tool_failure_class cancel_cause terminal_output }} }}"#
            ))
            .await?;
        let lifecycle_rows = lifecycle["data"]["AgentToolCall"]
            .as_array()
            .context("competing tool terminal lifecycle query omitted rows")?;
        anyhow::ensure!(
            lifecycle_rows.len() == 1,
            "competing tool terminal has no unique lifecycle"
        );
        let durable_state = lifecycle_rows[0]["lifecycle_state"]
            .as_str()
            .context("competing tool terminal lifecycle omitted state")?;
        if durable_state != expected && durable_state != fields.state.as_str() {
            let durable_state = ToolCallState::from_persisted(durable_state)
                .context("competing tool terminal has unknown lifecycle vocabulary")?;
            anyhow::ensure!(
                durable_state.is_terminal(),
                "tool terminal contender lost to a non-terminal lifecycle"
            );
            return Ok(false);
        }
        if durable_state == fields.state.as_str()
            && !terminal_metadata_matches(&lifecycle_rows[0], fields, terminal_status)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) async fn delivery_replay_permits(
    terminalization: Terminalization<'_, '_>,
    publish_native_result: bool,
) -> Result<bool> {
    let Terminalization {
        txn,
        tool_doc_id,
        request_doc_id,
        node_did,
        requester_did,
        session_id,
        tool_call_id,
        call_id,
        message_sequence,
        spawned_by_tool_call_doc_id,
        fields,
        terminal_status,
        text,
        pending_raw,
        presentation,
        output_budget,
        adopt_competing_terminal,
        tool,
        request,
        agent,
        session,
        tool_id,
        name,
        requester_filter,
        ..
    } = terminalization;
    let delivery_key_escaped = escape_graphql_string(&tool_delivery_key(session_id, tool_doc_id));
    let (source, writer) = tool_output_identity(tool_doc_id);
    let existing = txn.execute(&format!(
        r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{delivery_key_escaped}" }}, session_id: {{ _eq: "{session}" }}, node_did: {{ _eq: "{agent}" }} }}, limit: 2) {{ {AGENT_MESSAGE_FIELDS} }} }}"#
    )).await?;
    let existing_rows = existing["data"]["AgentMessage"]
        .as_array()
        .context("tool delivery lookup omitted rows")?;
    anyhow::ensure!(
        existing_rows.len() <= 1,
        "ambiguous canonical tool delivery replay"
    );
    if publish_native_result && spawned_by_tool_call_doc_id.is_none() {
        if let Some(row) = existing_rows.first() {
            let existing = decode_transcript_message_row(row)?;
            anyhow::ensure!(
                existing.message.request_doc_id.as_deref() == Some(request_doc_id)
                    && existing.message.session_id == session_id
                    && existing.message.node_did == node_did
                    && existing.message.requester_did.as_deref() == requester_did
                    && existing.message.role == MessageRole::User
                    && existing.message.outcome == OutputOutcome::Complete
                    && matches!(&existing.message.publication,
                    MessagePublication::ToolDelivery { tool_call_doc_id: id } if id == tool_doc_id)
                    && matches!(existing.message.blocks.as_slice(),
                    [MessageBlock::ToolResult { tool_call_doc_id: id, id: result_id,
                        call_id: result_call_id, .. }]
                    if id == tool_doc_id && result_id == tool_call_id
                        && result_call_id.as_deref() == call_id),
                "canonical tool delivery replay conflicts with accepted invocation"
            );
            let lifecycle = txn
                .execute(&format!(
                    r#"{{ AgentToolCall(filter: {{
            _docID: {{ _eq: "{tool}" }}, request_doc_id: {{ _eq: "{request}" }},
            session_id: {{ _eq: "{session}" }}, node_did: {{ _eq: "{agent}" }},
            tool_call_id: {{ _eq: "{tool_id}" }}, tool_name: {{ _eq: "{name}" }},
            message_sequence: {{ _eq: {message_sequence} }}{requester_filter}
        }}, limit: 2) {{ lifecycle_state status tool_failure_class cancel_cause terminal_output }} }}"#
                ))
                .await?;
            let lifecycle_rows = lifecycle["data"]["AgentToolCall"]
                .as_array()
                .context("canonical tool delivery replay lifecycle query omitted rows")?;
            anyhow::ensure!(
                lifecycle_rows.len() == 1,
                "canonical tool delivery replay has no unique lifecycle"
            );
            let durable_state = lifecycle_rows[0]["lifecycle_state"]
                .as_str()
                .context("canonical tool delivery replay lifecycle omitted state")?;
            if adopt_competing_terminal && durable_state != fields.state.as_str() {
                let durable_state = ToolCallState::from_persisted(durable_state)
                    .context("canonical tool delivery replay has unknown lifecycle vocabulary")?;
                anyhow::ensure!(
                    durable_state.is_terminal(),
                    "canonical tool delivery exists without a terminal lifecycle"
                );
                return Ok(false);
            }
            if durable_state == fields.state.as_str() {
                anyhow::ensure!(
                    terminal_metadata_matches(&lifecycle_rows[0], fields, terminal_status),
                    "canonical tool delivery replay conflicts with terminal cause"
                );
            }

            let result_payload = match existing.message.blocks.as_slice() {
                [MessageBlock::ToolResult { parts, .. }]
                    if matches!(parts.as_slice(),
                    [gents_protocol::output::ToolResultPart::Text { text }]
                    if text.output.stream == 0) =>
                {
                    let [gents_protocol::output::ToolResultPart::Text { text }] = parts.as_slice()
                    else {
                        unreachable!("guard established the exact native text part")
                    };
                    text
                }
                _ => anyhow::bail!(
                    "canonical tool delivery replay has a non-native payload reference"
                ),
            };
            let result_output = &result_payload.output;
            let replay_rows = crate::session::load_source_rows_in_txn(
                txn,
                request_doc_id,
                &source,
                node_did,
                session_id,
                requester_did,
            )
            .await?;
            anyhow::ensure!(
                replay_rows
                    .iter()
                    .any(|row| row.doc_id == result_output.close_doc_id
                        && row.segment.writer == writer
                        && matches!(
                            row.segment.close,
                            Some(SourceClose::Closed {
                                outcome: OutputOutcome::Complete,
                                ..
                            })
                        )),
                "canonical tool delivery replay payload does not name this tool closure"
            );
            let observed = replay_rows
                .iter()
                .map(|row| ObservedSegment {
                    doc_id: &row.doc_id,
                    segment: &row.segment,
                })
                .collect::<Vec<_>>();
            let reconstructed = reconstruct_stream(&observed, &[], &[], result_output)
                .map_err(anyhow::Error::from)?;
            let plan = terminal_output_plan(
                &reconstructed.text,
                text,
                pending_raw,
                presentation,
                fields.state,
                output_budget,
            )?;
            anyhow::ensure!(
                reconstructed.text == plan.raw
                    && if plan.diagnostic {
                        diagnostic_replay_matches(
                            &reconstructed.text,
                            text,
                            &result_payload.presentation,
                        )?
                    } else {
                        result_payload.presentation == plan.presentation
                            && render_presentation(
                                &reconstructed.text,
                                &result_payload.presentation,
                            )? == plan.rendered
                    },
                "canonical tool delivery replay payload differs from terminal plan"
            );
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) async fn closed_source_permits(
    terminalization: Terminalization<'_, '_>,
    source_rows: &[OutputSegmentRow],
) -> Result<bool> {
    let Terminalization {
        txn,
        tool_doc_id,
        message_sequence,
        plugin_effect,
        fields,
        terminal_status,
        text,
        pending_raw,
        presentation,
        output_budget,
        adopt_competing_terminal,
        tool,
        request,
        agent,
        session,
        tool_id,
        name,
        requester_filter,
        ..
    } = terminalization;
    let (_, writer) = tool_output_identity(tool_doc_id);
    if source_rows.iter().any(|row| row.segment.close.is_some()) {
        anyhow::ensure!(
            adopt_competing_terminal || plugin_effect.is_some(),
            "tool terminal source already has a closure"
        );
        let lifecycle = txn
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{
            _docID: {{ _eq: "{tool}" }}, request_doc_id: {{ _eq: "{request}" }},
            session_id: {{ _eq: "{session}" }}, node_did: {{ _eq: "{agent}" }},
            tool_call_id: {{ _eq: "{tool_id}" }}, tool_name: {{ _eq: "{name}" }},
            message_sequence: {{ _eq: {message_sequence} }}{requester_filter}
        }}, limit: 2) {{ lifecycle_state status tool_failure_class cancel_cause terminal_output }} }}"#
            ))
            .await?;
        let lifecycle_rows = lifecycle["data"]["AgentToolCall"]
            .as_array()
            .context("closed tool source lifecycle query omitted rows")?;
        anyhow::ensure!(
            lifecycle_rows.len() == 1,
            "closed tool source has no unique lifecycle"
        );
        let durable_state = lifecycle_rows[0]["lifecycle_state"]
            .as_str()
            .context("closed tool source lifecycle omitted state")?;
        anyhow::ensure!(
            durable_state == fields.state.as_str(),
            "closed tool source conflicts with terminal lifecycle"
        );
        anyhow::ensure!(
            terminal_metadata_matches(&lifecycle_rows[0], fields, terminal_status),
            "closed tool source conflicts with terminal cause"
        );
        anyhow::ensure!(
            source_rows.iter().all(|row| row.segment.writer == writer),
            "closed tool source has a foreign writer"
        );
        let closures = source_rows
            .iter()
            .filter(|row| row.segment.close.is_some())
            .collect::<Vec<_>>();
        anyhow::ensure!(
            closures.len() == 1,
            "closed tool source has ambiguous closures"
        );
        let close = closures[0];
        anyhow::ensure!(
            matches!(
                close.segment.close,
                Some(SourceClose::Closed {
                    outcome: OutputOutcome::Complete,
                    ..
                })
            ),
            "closed tool source has a non-complete closure"
        );
        let observed = source_rows
            .iter()
            .map(|row| ObservedSegment {
                doc_id: &row.doc_id,
                segment: &row.segment,
            })
            .collect::<Vec<_>>();
        let reconstructed = reconstruct_stream(
            &observed,
            &[],
            &[],
            &gents_protocol::output::PayloadRef {
                close_doc_id: close.doc_id.clone(),
                stream: 0,
            },
        )
        .map_err(anyhow::Error::from)?;
        let plan = terminal_output_plan(
            &reconstructed.text,
            text,
            pending_raw,
            presentation,
            fields.state,
            output_budget,
        )?;
        anyhow::ensure!(
            reconstructed.text == plan.raw,
            "closed tool source differs from terminal plan"
        );
        if plugin_effect.is_some() {
            let stored: PresentedPayload =
                serde_json::from_value(lifecycle_rows[0]["terminal_output"].clone())
                    .context("plugin terminal is missing its canonical presentation")?;
            anyhow::ensure!(
                stored.output.close_doc_id == close.doc_id && stored.output.stream == 0,
                "plugin terminal presentation names a foreign source closure"
            );
            anyhow::ensure!(
                if plan.diagnostic {
                    diagnostic_replay_matches(&reconstructed.text, text, &stored.presentation)?
                } else {
                    stored.presentation == plan.presentation
                        && render_presentation(&reconstructed.text, &stored.presentation)?
                            == plan.rendered
                },
                "plugin terminal replay changed its presentation"
            );
        }
        return Ok(false);
    }
    Ok(true)
}
