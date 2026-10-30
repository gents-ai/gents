use super::background_receipt::{
    ensure_background_receipt_before_bridge_close, BackgroundReceiptKey,
};
use super::presentation::{
    diagnostic_replay_matches, render_presentation, terminal_metadata_matches, terminal_output_plan,
};
use super::TerminalFields;
use crate::config_client::ConfigApplyTxn;
use crate::graphql::{created_doc_id, escape_graphql_string};
use crate::request::lifecycle::queue::next_append_sequence_in_transaction;
use crate::session::canonical_rows::{
    decode_transcript_message_row, output_segment_create_variables,
    transcript_message_create_variables, AGENT_MESSAGE_FIELDS, CREATE_AGENT_MESSAGE_MUTATION,
    CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use crate::tool_call_lifecycle::ToolCallState;
use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use gents_protocol::output::reconstruction::{reconstruct_stream, ObservedSegment};
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadPresentation, PresentedPayload, SegmentRun, SourceClose,
    StreamDeclaration, StreamPayload, TranscriptMessage,
};

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
    plugin_effect: Option<&crate::tool_call_lifecycle::plugin_effect::PluginEffectTerminal>,
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
    let delivery_key = format!("{session_id}:tool-delivery:{tool_doc_id}");
    let delivery_key_escaped = escape_graphql_string(&delivery_key);
    let receipt_key = format!("{session_id}:background-receipt:{tool_doc_id}");
    let receipt_key_escaped = escape_graphql_string(&receipt_key);
    let source = OutputSource::ToolCall {
        tool_call_doc_id: tool_doc_id.to_owned(),
    };
    let writer = OutputWriter::ToolExecution {
        tool_call_doc_id: tool_doc_id.to_owned(),
    };

    let (accepted, _) = crate::session::load_canonical_message_in_txn(
        txn,
        accepted_header_doc_id,
        node_did,
        requester_did,
    )
    .await?;
    if let Some(effect) = plugin_effect {
        anyhow::ensure!(call_id.is_none(), "invalid plugin effect terminal identity");
        crate::tool_call_lifecycle::plugin_effect::validate_parent(
            txn,
            &effect.parent,
            accepted_header_doc_id,
            &effect.generation,
            message_sequence,
            deadline_at,
            false,
        )
        .await?;
        let (reference, _) = crate::tool_call_lifecycle::plugin_effect::arguments_in_txn(
            txn,
            &effect.parent,
            tool_doc_id,
            &effect.generation,
            tool_call_id,
            tool_name,
        )
        .await?;
        anyhow::ensure!(
            arguments == Some(&reference),
            "plugin effect terminal arguments changed"
        );
    } else if let Some(parent_doc_id) = spawned_by_tool_call_doc_id {
        let parent = escape_graphql_string(parent_doc_id);
        let parent_rows = txn
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{
            _docID: {{ _eq: "{parent}" }}, request_doc_id: {{ _eq: "{request}" }},
            session_id: {{ _eq: "{session}" }}, node_did: {{ _eq: "{agent}" }}{requester_filter}
        }}, limit: 2) {{ _docID tool_call_id tool_name message_sequence lifecycle_state
            spawned_by_tool_call_doc_id }} }}"#
            ))
            .await?;
        let parents = parent_rows["data"]["AgentToolCall"]
            .as_array()
            .context("spawned terminalization parent lookup omitted rows")?;
        anyhow::ensure!(
            parents.len() == 1,
            "spawned terminalization parent is missing or ambiguous"
        );
        let parent = &parents[0];
        let parent_id = parent["tool_call_id"]
            .as_str()
            .context("spawned parent lacks native ID")?;
        let parent_name = parent["tool_name"]
            .as_str()
            .context("spawned parent lacks tool name")?;
        anyhow::ensure!(
            parent["spawned_by_tool_call_doc_id"].is_null()
                && parent_name == crate::toolset::SPAWN_PROCESS_TOOL_NAME
                && parent["message_sequence"].as_u64() == Some(u64::from(message_sequence))
                && accepted.session_id == session_id
                && accepted.request_doc_id.as_deref() == Some(request_doc_id)
                && accepted.role == MessageRole::Assistant
                && accepted.outcome == OutputOutcome::Complete
                && accepted.sequence == message_sequence
                && matches!(accepted.publication, MessagePublication::RequestExecution { .. })
                && accepted.blocks.iter().any(|block| matches!(block,
                    MessageBlock::ToolCall { tool_call_doc_id, id, name, .. }
                    if tool_call_doc_id == parent_doc_id && id == parent_id && name == parent_name)),
            "spawned terminalization has no exact accepted spawn_process parent"
        );
    } else {
        let arguments =
            arguments.context("canonical direct terminalization requires accepted arguments")?;
        anyhow::ensure!(
            accepted.session_id == session_id
                && accepted.request_doc_id.as_deref() == Some(request_doc_id)
                && accepted.role == MessageRole::Assistant
                && accepted.outcome == OutputOutcome::Complete
                && accepted.sequence == message_sequence
                && matches!(
                    accepted.publication,
                    MessagePublication::RequestExecution { .. }
                )
                && accepted.blocks.iter().any(|block| matches!(block,
                    MessageBlock::ToolCall { tool_call_doc_id, id, call_id: accepted_call_id,
                        name, arguments: accepted_arguments, .. }
                    if tool_call_doc_id == tool_doc_id && id == tool_call_id
                        && accepted_call_id.as_deref() == call_id && name == tool_name
                        && accepted_arguments == arguments)),
            "accepted tool header does not bind this exact physical invocation"
        );
    }

    let plugin_receipt_field = if let Some(receipt) = plugin_receipt {
        let rows = txn
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{tool}" }},
                request_doc_id: {{ _eq: "{request}" }}, session_id: {{ _eq: "{session}" }},
                node_did: {{ _eq: "{agent}" }}, tool_call_id: {{ _eq: "{tool_id}" }},
                tool_name: {{ _eq: "{name}" }}, message_sequence: {{ _eq: {message_sequence} }}
                {requester_filter} }}, limit: 2) {{ lifecycle_state plugin_execution_receipt }} }}"#
            ))
            .await?;
        let rows = rows["data"]["AgentToolCall"]
            .as_array()
            .context("plugin receipt tool query omitted rows")?;
        anyhow::ensure!(
            rows.len() == 1,
            "plugin receipt lacks exact physical tool invocation"
        );
        let row = &rows[0];
        let state = row["lifecycle_state"]
            .as_str()
            .and_then(ToolCallState::from_persisted)
            .context("plugin receipt tool has invalid lifecycle state")?;
        let existing = if row["plugin_execution_receipt"].is_null() {
            None
        } else {
            Some(serde_json::from_value::<
                gents_protocol::plugin::PluginExecutionReceipt,
            >(row["plugin_execution_receipt"].clone())?)
        };
        match crate::tool_call_lifecycle::plugin_receipt::decide(
            state.is_terminal(),
            existing.as_ref(),
            receipt,
        ) {
            crate::tool_call_lifecycle::plugin_receipt::ReceiptDecision::Commit
            | crate::tool_call_lifecycle::plugin_receipt::ReceiptDecision::Replay => {}
            crate::tool_call_lifecycle::plugin_receipt::ReceiptDecision::Adopt => return Ok(false),
            crate::tool_call_lifecycle::plugin_receipt::ReceiptDecision::Conflict => {
                anyhow::bail!("plugin receipt conflicts with durable evidence")
            }
        }
        ", plugin_execution_receipt: $plugin_receipt".to_owned()
    } else {
        String::new()
    };

    let receipt_rows = txn.execute(&format!(
        r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{receipt_key_escaped}" }}, session_id: {{ _eq: "{session}" }}, node_did: {{ _eq: "{agent}" }}{requester_filter} }}, limit: 2) {{ _docID }} }}"#
    )).await?;
    let receipt_rows = receipt_rows["data"]["AgentMessage"]
        .as_array()
        .context("background receipt lookup omitted messages")?;
    anyhow::ensure!(receipt_rows.len() <= 1, "background receipt is ambiguous");
    let publish_native_result =
        publish_native_result && receipt_rows.is_empty() && plugin_effect.is_none();

    if !publish_native_result && spawned_by_tool_call_doc_id.is_none() && plugin_effect.is_none() {
        ensure_background_receipt_before_bridge_close(
            txn,
            BackgroundReceiptKey {
                tool_call_doc_id: tool_doc_id,
                request_doc_id,
                node_did,
                requester_did,
                session_id,
                tool_call_id,
                call_id,
            },
        )
        .await?;
    }

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

    let source_rows = crate::session::load_source_rows_in_txn(
        txn,
        request_doc_id,
        &source,
        node_did,
        session_id,
        requester_did,
    )
    .await?;
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
    if plugin_effect.is_some() {
        let stored = txn
            .execute(&format!(
                r#"{{ AgentToolCall(docID: "{tool}") {{ terminal_output }} }}"#
            ))
            .await?;
        let rows = stored["data"]["AgentToolCall"]
            .as_array()
            .context("plugin terminal presentation lookup omitted rows")?;
        anyhow::ensure!(
            rows.len() == 1 && rows[0]["terminal_output"].is_null(),
            "open plugin effect already carries terminal output"
        );
    }
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
    let completed_at = now.to_rfc3339_opts(SecondsFormat::Nanos, true);
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
        return Ok(false);
    }

    let request_id = lifecycle["data"]["update_AgentToolCall"][0]["request_id"]
        .as_str()
        .context("published tool terminal omitted logical request identity")?;
    crate::session::refresh_session_request_observation_in_txn(
        txn,
        node_did,
        requester_did,
        session_id,
        crate::session::SessionRequestRefresh {
            request_doc_id,
            request_id,
            reply_preview: None,
        },
        &completed_at,
    )
    .await?;

    let segment_response = txn
        .execute_with_variables(
            CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
            &output_segment_create_variables(&segment)?,
        )
        .await?;
    let close_doc_id = created_doc_id(&segment_response, "AgentOutputSegment")?;

    if plugin_effect.is_some() {
        let terminal_output = PresentedPayload {
            output: gents_protocol::output::PayloadRef {
                close_doc_id: close_doc_id.clone(),
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

    if spawned_by_tool_call_doc_id.is_some() || !publish_native_result {
        return Ok(true);
    }

    let sequence = next_append_sequence_in_transaction(txn, node_did, session_id).await?;
    let message = TranscriptMessage {
        message_key: delivery_key,
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
        &transcript_message_create_variables(&message)?,
    )
    .await?;
    Ok(true)
}
