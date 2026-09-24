use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents_protocol::output::reconstruction::{reconstruct_message, ObservedSegment};
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, PayloadPresentation, PayloadRef,
    PresentedPayload, SourceClose, TranscriptMessage,
};
use gents_protocol::row::AgentRequestRow;

use crate::lean_vocab_test::{
    CanonicalExecutionAdapter, ExecutionFuture, LeanCanonicalExecutionObservation,
    LeanCanonicalExecutionOperation, LeanCanonicalExecutionSeed, LeanCanonicalMessage,
    LeanCanonicalSegment, LeanCanonicalSource, LeanCanonicalToolAdmission, LeanCanonicalWriter,
    LeanMessageBlock, LeanMessagePublication, LeanMessageRole, LeanOutcome, LeanPayloadKind,
    LeanPayloadSpec, LeanPresentation, LeanPresentationPart, LeanResultPart, LeanTerminalSelection,
};

const FIXTURE_EPOCH_SECONDS: i64 = 1_700_000_000;

fn fixture_time(value: u64) -> Result<DateTime<Utc>> {
    let seconds = i64::try_from(value).context("modeled time exceeds native range")?;
    DateTime::from_timestamp(FIXTURE_EPOCH_SECONDS + seconds, 0)
        .context("modeled time overflows native timestamp")
}

fn symbolic_generation(value: u64) -> String {
    format!("lean-generation-{value}")
}

fn parse_generation(value: Option<&str>) -> Option<u64> {
    value?.strip_prefix("lean-generation-")?.parse().ok()
}

fn modeled_time(value: &str) -> Result<u64> {
    u64::try_from(DateTime::parse_from_rfc3339(value)?.timestamp() - FIXTURE_EPOCH_SECONDS)
        .context("native timestamp precedes fixture epoch")
}

// Native session sequences are one-based; the Lean allocator starts at zero.
// Normalize only the representation, preserving order and reservation gaps.
fn modeled_sequence(value: u64) -> Result<u64> {
    value
        .checked_sub(1)
        .context("native sequence must be positive")
}

fn native_sequence(value: u64) -> Result<u32> {
    u32::try_from(value.checked_add(1).context("modeled sequence overflow")?)
        .context("native sequence exceeds u32")
}

pub(crate) struct NativeCanonicalExecutionAdapter;

pub(crate) struct NativeCanonicalExecution {
    node: Arc<EmbeddedNode>,
    request_doc_id: String,
    request_id: u64,
    principal_id: u64,
    query_document: u64,
    remote_routes: Vec<crate::lean_vocab_test::LeanCanonicalRemoteRoute>,
    transcript_session_id: u64,
    next_sequence: u64,
    session_id: String,
    principal: String,
    segments: Vec<LeanCanonicalSegment>,
    messages: Vec<LeanCanonicalMessage<LeanPayloadSpec>>,
    tool_state: Option<String>,
    physical_tool_request: Option<u64>,
    segment_ids: HashMap<String, u64>,
    message_ids: HashMap<String, u64>,
    modeled_message_keys: HashMap<String, String>,
    canonical_message_keys: HashMap<String, String>,
    tool_ids: HashMap<String, u64>,
    accepted_spawns: HashMap<
        u64,
        (
            crate::streaming::AcceptedToolCall,
            u64,
            crate::tool_call_lifecycle::AwaitMode,
            crate::tool_call_lifecycle::CancelPolicy,
        ),
    >,
}

impl NativeCanonicalExecution {
    async fn revoke_corrupt(
        &mut self,
        now: u64,
        expected_generation: u64,
        fresh_generation: u64,
        outcome: &str,
        selection: &LeanTerminalSelection,
    ) -> Result<LeanCanonicalExecutionObservation> {
        anyhow::ensure!(
            outcome == "dead",
            "modeled corrupt revocation outcome changed"
        );
        let request = crate::graphql::escape_graphql_string(&self.request_doc_id);
        let response = self.node.execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{request}" }} }}, limit: 1) {{ {} }} }}"#,
            crate::watcher::AGENT_REQUEST_FIELDS,
        )).await;
        anyhow::ensure!(
            !response.has_errors(),
            "read revocation request: {:?}",
            response.errors
        );
        let observed = crate::graphql::first_row::<AgentRequestRow>(&response, "AgentRequest")?
            .context("native revocation request disappeared")?;
        let result = crate::lifecycle::revoke_execution_preserving_output_at(
            &self.node,
            &observed,
            crate::lifecycle::RequestTerminalOutcome::Dead,
            "canonical output integrity failure",
            &symbolic_generation(expected_generation),
            &symbolic_generation(fresh_generation),
            fixture_time(now)?,
        )
        .await?;
        if matches!(result, crate::lifecycle::TerminalizeResult::Lost) {
            return self.observe(false).await;
        }
        let response = self.node.execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{request}" }} }}, limit: 1) {{ {} terminal_output }} }}"#,
            crate::watcher::AGENT_REQUEST_FIELDS,
        )).await;
        anyhow::ensure!(
            !response.has_errors(),
            "read revoked request: {:?}",
            response.errors
        );
        let persisted = crate::graphql::first_row::<AgentRequestRow>(&response, "AgentRequest")?
            .context("native revoked request disappeared")?;
        let expected_selection = match selection {
            LeanTerminalSelection::NoMessage => gents_protocol::output::TerminalOutput::NoMessage,
            LeanTerminalSelection::Message { id } => {
                let physical = self
                    .message_ids
                    .iter()
                    .find_map(|(physical, symbolic)| (*symbolic == *id).then_some(physical.clone()))
                    .context("modeled corrupt revocation header has no physical identity")?;
                gents_protocol::output::TerminalOutput::Message {
                    message_doc_id: physical,
                }
            }
        };
        anyhow::ensure!(
            persisted.terminal_output.as_ref() == Some(&expected_selection),
            "native revocation selected a different immutable header: expected {expected_selection:?}, got {:?}",
            persisted.terminal_output
        );
        self.observe(true).await
    }

    async fn complete_foreground(
        &mut self,
        now: u64,
        document: u64,
        authority_outcome: &str,
        record: &LeanCanonicalSegment,
        message: &LeanCanonicalMessage<LeanPayloadSpec>,
    ) -> Result<LeanCanonicalExecutionObservation> {
        anyhow::ensure!(
            authority_outcome == "complete",
            "native completion requires complete authority"
        );
        anyhow::ensure!(
            matches!(record.coordinate.source, LeanCanonicalSource::Tool { call } if call == document)
                && matches!(record.writer, LeanCanonicalWriter::Tool { call } if call == document),
            "modeled tool closure is not bound to selected physical tool"
        );
        let raw = String::from_utf8(
            record
                .flush
                .as_ref()
                .context("modeled tool closure omitted raw flush")?
                .payload
                .clone(),
        )?;
        let [LeanMessageBlock::ToolResult { doc_id, parts, .. }] = message.blocks.as_slice() else {
            anyhow::bail!("modeled native completion has no sole ToolResult block")
        };
        anyhow::ensure!(*doc_id == document, "modeled result refers to another tool");
        let [LeanResultPart::Text { payload }] = parts.as_slice() else {
            anyhow::bail!("modeled native completion has no sole text result")
        };
        anyhow::ensure!(
            payload.reference.close_id == record.id && payload.reference.stream == 0,
            "modeled result does not reference the closing tool stream"
        );
        let presentation = match &payload.presentation {
            LeanPresentation::Full => PayloadPresentation::Full,
            LeanPresentation::Composed { parts } => PayloadPresentation::Composed {
                parts: parts
                    .iter()
                    .map(|part| -> Result<_> {
                        Ok(match part {
                            LeanPresentationPart::Range { start, end } => {
                                gents_protocol::output::PresentationPart::OutputRange {
                                    start_byte: *start,
                                    end_byte: *end,
                                }
                            }
                            LeanPresentationPart::Literal { bytes } => {
                                gents_protocol::output::PresentationPart::Literal {
                                    text: String::from_utf8(bytes.clone())?,
                                }
                            }
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
            },
        };
        let rendered =
            crate::tool_call_lifecycle::delivery::render_presentation(&raw, &presentation)?;
        let physical = self.physical_tool(document)?.to_owned();
        let mut tool = crate::tool_call_lifecycle::ToolCallLifecycle::load_by_doc_id(
            self.node.clone(),
            &physical,
            &self.principal,
            &self.session_id,
            None,
        )
        .await?
        .context("modeled tool disappeared before atomic completion")?;
        let accepted = tool
            .complete_raw_with_presentation_at(&raw, &rendered, presentation, fixture_time(now)?)
            .await?;
        if !accepted {
            return self.observe(false).await;
        }
        let request = crate::graphql::escape_graphql_string(&self.request_doc_id);
        let segments = self.node.execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ {} }} }}"#,
            crate::session::canonical_rows::AGENT_OUTPUT_SEGMENT_FIELDS,
        )).await;
        anyhow::ensure!(
            !segments.has_errors(),
            "read native completed tool source: {:?}",
            segments.errors
        );
        let closing = segments
            .data
            .as_ref()
            .and_then(|data| data.get("AgentOutputSegment"))
            .and_then(serde_json::Value::as_array)
            .context("native completed tool source query omitted rows")?
            .iter()
            .map(crate::session::canonical_rows::decode_output_segment_row)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|row| {
                matches!(&row.segment.source,
                gents_protocol::output::OutputSource::ToolCall { tool_call_doc_id }
                    if tool_call_doc_id == &physical)
                    && row.segment.close.is_some()
            })
            .collect::<Vec<_>>();
        let [closing] = closing.as_slice() else {
            anyhow::bail!("atomic tool completion did not persist one exact source closure")
        };
        self.segment_ids.insert(closing.doc_id.clone(), record.id);
        let messages = self.node.execute(&format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ {} }} }}"#,
            crate::session::canonical_rows::AGENT_MESSAGE_FIELDS,
        )).await;
        anyhow::ensure!(
            !messages.has_errors(),
            "read native completed tool result: {:?}",
            messages.errors
        );
        let delivered = messages
            .data
            .as_ref()
            .and_then(|data| data.get("AgentMessage"))
            .and_then(serde_json::Value::as_array)
            .context("native completed tool result query omitted rows")?
            .iter()
            .map(crate::session::canonical_rows::decode_transcript_message_row)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|row| {
                matches!(&row.message.publication,
                MessagePublication::ToolDelivery { tool_call_doc_id }
                    if tool_call_doc_id == &physical)
            })
            .collect::<Vec<_>>();
        let [delivered] = delivered.as_slice() else {
            anyhow::bail!("atomic tool completion did not publish one exact native result")
        };
        self.message_ids
            .insert(delivered.doc_id.clone(), message.header.id);
        self.modeled_message_keys
            .insert(delivered.doc_id.clone(), message.key.clone());
        self.observe(true).await
    }

    async fn recover_terminal(
        &mut self,
        now: u64,
        expected_generation: u64,
        fresh_generation: u64,
        outcome: &str,
        selection: &LeanTerminalSelection,
        items: &[crate::lean_vocab_test::LeanCanonicalRecoveryItem],
    ) -> Result<LeanCanonicalExecutionObservation> {
        if outcome == "interrupted" {
            crate::interrupt::interrupt_request_by_doc_id(
                &self.node,
                &self.request_doc_id,
                &self.principal,
                None,
            )
            .await?;
        }
        let request = crate::graphql::escape_graphql_string(&self.request_doc_id);
        let response = self.node.execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{request}" }} }}, limit: 1) {{ {} }} }}"#,
            crate::watcher::AGENT_REQUEST_FIELDS,
        )).await;
        anyhow::ensure!(
            !response.has_errors(),
            "read recovery request: {:?}",
            response.errors
        );
        let observed = crate::graphql::first_row::<AgentRequestRow>(&response, "AgentRequest")?
            .context("native recovery request disappeared")?;
        let expiry = observed
            .execution_lease_expires_at
            .as_deref()
            .context("native recovery request omitted deadline")?;
        let choice = match selection {
            LeanTerminalSelection::NoMessage => {
                crate::lifecycle::RecoverySelectionChoice::NoMessage
            }
            LeanTerminalSelection::Message { id } => {
                let accepted_key = self.message_ids.iter().find_map(|(physical, symbolic)| {
                    (*symbolic == *id)
                        .then(|| self.canonical_message_keys.get(physical))
                        .flatten()
                        .cloned()
                });
                let key = accepted_key
                    .or(items
                        .iter()
                        .find_map(|item| {
                            (item.message.as_ref()?.header.id == *id).then_some(&item.closing)
                        })
                        .map(|closing| -> Result<String> {
                            let source =
                                self.provider_segment(closing, expected_generation)?.source;
                            crate::streaming::canonical::partial_message_key(
                                &self.request_doc_id,
                                &source,
                            )
                        })
                        .transpose()?)
                    .unwrap_or_else(|| format!("lean-unmapped-recovery-message-{id}"));
                crate::lifecycle::RecoverySelectionChoice::MessageKey(key)
            }
        };
        let result = crate::lifecycle::recover_expired_generation_with_facts(
            &self.node,
            &observed,
            &symbolic_generation(expected_generation),
            expiry,
            symbolic_generation(fresh_generation),
            fixture_time(now)?,
            Some(choice),
            Some(if outcome == "interrupted" {
                gents_protocol::request_lifecycle::RequestLifecycleState::Interrupted
            } else {
                gents_protocol::request_lifecycle::RequestLifecycleState::Failed
            }),
        )
        .await;
        match result {
            Ok(crate::lifecycle::RecoveryResult::Lost) => return self.observe(false).await,
            Err(error)
                if error
                    .downcast_ref::<crate::lifecycle::RecoverySelectionRejected>()
                    .is_some()
                    || error
                        .downcast_ref::<gents_protocol::output::ReconstructionError>()
                        .is_some() =>
            {
                return self.observe(false).await;
            }
            Err(error) => return Err(error.context("native terminal recovery owner failed")),
            Ok(crate::lifecycle::RecoveryResult::Won { .. }) => {}
        }
        let segments = self.node.execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ {} }} }}"#,
            crate::session::canonical_rows::AGENT_OUTPUT_SEGMENT_FIELDS,
        )).await;
        anyhow::ensure!(
            !segments.has_errors(),
            "read recovered segments: {:?}",
            segments.errors
        );
        for row in segments
            .data
            .as_ref()
            .and_then(|data| data.get("AgentOutputSegment"))
            .and_then(serde_json::Value::as_array)
            .context("recovered segment query omitted rows")?
        {
            let row = crate::session::canonical_rows::decode_output_segment_row(row)?;
            if self.segment_ids.contains_key(&row.doc_id) {
                continue;
            }
            let matches = items
                .iter()
                .filter(|item| {
                    self.provider_segment(&item.closing, expected_generation)
                        .is_ok_and(|closing| {
                            closing.source == row.segment.source && row.segment.close.is_some()
                        })
                })
                .collect::<Vec<_>>();
            let [item] = matches.as_slice() else {
                anyhow::bail!("native recovery produced an unmapped or ambiguous closure")
            };
            self.segment_ids.insert(row.doc_id, item.closing.id);
        }
        let messages = self.node.execute(&format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ {} }} }}"#,
            crate::session::canonical_rows::AGENT_MESSAGE_FIELDS,
        )).await;
        anyhow::ensure!(
            !messages.has_errors(),
            "read recovered headers: {:?}",
            messages.errors
        );
        for row in messages
            .data
            .as_ref()
            .and_then(|data| data.get("AgentMessage"))
            .and_then(serde_json::Value::as_array)
            .context("recovered header query omitted rows")?
        {
            let row = crate::session::canonical_rows::decode_transcript_message_row(row)?;
            if self.message_ids.contains_key(&row.doc_id) {
                continue;
            }
            let matching = items
                .iter()
                .filter_map(|item| {
                    let message = item.message.as_ref()?;
                    let source = self
                        .provider_segment(&item.closing, expected_generation)
                        .ok()?
                        .source;
                    let key = crate::streaming::canonical::partial_message_key(
                        &self.request_doc_id,
                        &source,
                    )
                    .ok()?;
                    (key == row.message.message_key).then_some(message)
                })
                .collect::<Vec<_>>();
            let [message] = matching.as_slice() else {
                anyhow::bail!("native recovery produced an unmapped or ambiguous header")
            };
            self.message_ids
                .insert(row.doc_id.clone(), message.header.id);
            self.modeled_message_keys
                .insert(row.doc_id.clone(), message.key.clone());
            self.canonical_message_keys
                .insert(row.doc_id, row.message.message_key);
        }
        self.observe(true).await
    }

    fn physical_tool(&self, symbolic: u64) -> Result<&str> {
        self.tool_ids
            .iter()
            .find_map(|(physical, mapped)| (*mapped == symbolic).then_some(physical.as_str()))
            .context("modeled tool has no accepted physical lifecycle")
    }

    async fn refresh_output(&mut self) -> Result<()> {
        let request = crate::graphql::escape_graphql_string(&self.request_doc_id);
        let segments = self.node.execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ {} }} }}"#,
            crate::session::canonical_rows::AGENT_OUTPUT_SEGMENT_FIELDS,
        )).await;
        anyhow::ensure!(
            !segments.has_errors(),
            "read native output segments: {:?}",
            segments.errors
        );
        let mut decoded_segments = segments
            .data
            .as_ref()
            .and_then(|data| data.get("AgentOutputSegment"))
            .and_then(serde_json::Value::as_array)
            .context("native segment query omitted rows")?
            .iter()
            .map(crate::session::canonical_rows::decode_output_segment_row)
            .collect::<Result<Vec<_>>>()?;
        decoded_segments.sort_by_key(|row| {
            self.segment_ids
                .get(&row.doc_id)
                .copied()
                .unwrap_or(u64::MAX)
        });
        self.segments = decoded_segments
            .iter()
            .map(|row| {
                let id = self
                    .segment_ids
                    .get(&row.doc_id)
                    .copied()
                    .context("native segment has no symbolic identity")?;
                self.lean_segment(id, &row.segment)
            })
            .collect::<Result<Vec<_>>>()?;

        let messages = self.node.execute(&format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ {} }} }}"#,
            crate::session::canonical_rows::AGENT_MESSAGE_FIELDS,
        )).await;
        anyhow::ensure!(
            !messages.has_errors(),
            "read native message headers: {:?}",
            messages.errors
        );
        let mut decoded_messages = messages
            .data
            .as_ref()
            .and_then(|data| data.get("AgentMessage"))
            .and_then(serde_json::Value::as_array)
            .context("native message query omitted rows")?
            .iter()
            .map(crate::session::canonical_rows::decode_transcript_message_row)
            .collect::<Result<Vec<_>>>()?;
        decoded_messages.sort_by_key(|row| row.message.sequence);
        self.messages = decoded_messages
            .iter()
            .map(|row| {
                let id = self
                    .message_ids
                    .get(&row.doc_id)
                    .copied()
                    .context("native header has no symbolic identity")?;
                let mut message = self.lean_message(id, &row.message)?;
                if let Some(modeled_key) = self.modeled_message_keys.get(&row.doc_id) {
                    message.key.clone_from(modeled_key);
                }
                Ok(message)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(())
    }

    fn lean_segment(
        &self,
        id: u64,
        segment: &gents_protocol::output::OutputSegment,
    ) -> Result<LeanCanonicalSegment> {
        let source = match &segment.source {
            gents_protocol::output::OutputSource::ProviderTurn {
                scope,
                turn_index,
                attempt,
            } => {
                let scope = scope
                    .to_string()
                    .strip_prefix("inference.")
                    .context("native provider scope is not modeled")?
                    .parse()?;
                LeanCanonicalSource::Provider {
                    scope,
                    turn: u64::from(*turn_index),
                    attempt: u64::from(*attempt),
                }
            }
            gents_protocol::output::OutputSource::ToolCall { tool_call_doc_id } => {
                LeanCanonicalSource::Tool {
                    call: self
                        .tool_ids
                        .get(tool_call_doc_id)
                        .copied()
                        .context("native tool source has no symbolic identity")?,
                }
            }
            _ => anyhow::bail!("native adapter observed unsupported segment source"),
        };
        let writer = match &segment.writer {
            gents_protocol::output::OutputWriter::RequestExecution {
                execution_generation,
            } => LeanCanonicalWriter::Request {
                generation: parse_generation(Some(execution_generation))
                    .context("native segment generation is not symbolic")?,
            },
            gents_protocol::output::OutputWriter::ToolExecution { tool_call_doc_id } => {
                LeanCanonicalWriter::Tool {
                    call: self
                        .tool_ids
                        .get(tool_call_doc_id)
                        .copied()
                        .context("native tool writer has no symbolic identity")?,
                }
            }
        };
        let flush = segment
            .ordinal
            .map(|ordinal| -> Result<_> {
                Ok(crate::lean_vocab_test::LeanCanonicalFlush {
                    ordinal: u64::from(ordinal),
                    runs: segment
                        .runs
                        .iter()
                        .map(|run| {
                            Ok(crate::lean_vocab_test::LeanCanonicalRun {
                                stream: u64::from(run.stream),
                                bytes: u64::from(run.bytes),
                                declaration: run
                                    .declaration
                                    .as_ref()
                                    .map(|declaration| {
                                        let (kind, tool) = match &declaration.payload {
                                        gents_protocol::output::StreamPayload::Text => {
                                            (LeanPayloadKind::Text, None)
                                        }
                                        gents_protocol::output::StreamPayload::Reasoning => {
                                            (LeanPayloadKind::Reasoning, None)
                                        }
                                        gents_protocol::output::StreamPayload::ReasoningSummary => {
                                            (LeanPayloadKind::Summary, None)
                                        }
                                        gents_protocol::output::StreamPayload::ReasoningOpaque => {
                                            (LeanPayloadKind::Opaque, None)
                                        }
                                        gents_protocol::output::StreamPayload::ToolArguments {
                                            id,
                                            call_id,
                                            name,
                                        } => (
                                            LeanPayloadKind::Arguments,
                                            Some(crate::lean_vocab_test::LeanToolIdentity {
                                                id: id.clone(),
                                                call_id: call_id.clone(),
                                                name: name.clone(),
                                            }),
                                        ),
                                        gents_protocol::output::StreamPayload::ToolOutput => {
                                            (LeanPayloadKind::ToolOutput, None)
                                        }
                                        _ => anyhow::bail!(
                                            "native adapter observed unsupported stream payload"
                                        ),
                                    };
                                        Ok(crate::lean_vocab_test::LeanCanonicalDeclaration {
                                            block: u64::from(declaration.block_index),
                                            part: u64::from(declaration.part_index),
                                            kind,
                                            tool,
                                            media_kind: None,
                                        })
                                    })
                                    .transpose()?,
                            })
                        })
                        .collect::<Result<Vec<_>>>()?,
                    payload: segment.payload.as_bytes().to_vec(),
                })
            })
            .transpose()?;
        let close = segment.close.as_ref().map(|close| match close {
            SourceClose::Closed {
                outcome,
                segments,
                stream_bytes,
            } => crate::lean_vocab_test::LeanCanonicalClosure::Closed {
                outcome: match outcome {
                    OutputOutcome::Complete => LeanOutcome::Complete,
                    OutputOutcome::Partial => LeanOutcome::Partial,
                },
                segments: u64::from(*segments),
                stream_bytes: stream_bytes.clone(),
            },
            SourceClose::Retracted => crate::lean_vocab_test::LeanCanonicalClosure::Retracted,
        });
        Ok(LeanCanonicalSegment {
            id,
            coordinate: crate::lean_vocab_test::LeanCanonicalCoordinate {
                request: self.symbolic_request()?,
                source,
            },
            writer,
            flush,
            close,
            created_at: modeled_time(&segment.created_at)?,
        })
    }

    fn symbolic_request(&self) -> Result<u64> {
        Ok(self.request_id)
    }

    fn lean_message(
        &self,
        id: u64,
        message: &TranscriptMessage,
    ) -> Result<LeanCanonicalMessage<LeanPayloadSpec>> {
        let payload_ref = |reference: &PayloadRef| -> Result<LeanPayloadSpec> {
            Ok(LeanPayloadSpec {
                reference: crate::lean_vocab_test::LeanPayloadRef {
                    close_id: self
                        .segment_ids
                        .get(&reference.close_doc_id)
                        .copied()
                        .context("native payload reference has no symbolic closure identity")?,
                    stream: u64::from(reference.stream),
                },
                presentation: LeanPresentation::Full,
            })
        };
        let presented_payload = |value: &PresentedPayload| -> Result<LeanPayloadSpec> {
            let mut payload = payload_ref(&value.output)?;
            payload.presentation = match &value.presentation {
                PayloadPresentation::Full => LeanPresentation::Full,
                PayloadPresentation::Composed { parts } => LeanPresentation::Composed {
                    parts: parts
                        .iter()
                        .map(|part| match part {
                            gents_protocol::output::PresentationPart::OutputRange {
                                start_byte,
                                end_byte,
                            } => LeanPresentationPart::Range {
                                start: *start_byte,
                                end: *end_byte,
                            },
                            gents_protocol::output::PresentationPart::Literal { text } => {
                                LeanPresentationPart::Literal {
                                    bytes: text.as_bytes().to_vec(),
                                }
                            }
                        })
                        .collect(),
                },
            };
            Ok(payload)
        };
        let blocks = message
            .blocks
            .iter()
            .map(|block| -> Result<LeanMessageBlock<LeanPayloadSpec>> {
                Ok(match block {
                    MessageBlock::Text { text } => {
                        anyhow::ensure!(
                            matches!(text.presentation, PayloadPresentation::Full),
                            "native text presentation is not modeled"
                        );
                        LeanMessageBlock::Text {
                            payload: payload_ref(&text.output)?,
                        }
                    }
                    MessageBlock::ToolCall {
                        tool_call_doc_id,
                        id,
                        call_id,
                        name,
                        arguments,
                        signature,
                        additional_params,
                    } => LeanMessageBlock::ToolCall {
                        doc_id: self
                            .tool_ids
                            .get(tool_call_doc_id)
                            .copied()
                            .context("native tool call has no symbolic identity")?,
                        id: id.clone(),
                        call_id: call_id.clone(),
                        name: name.clone(),
                        arguments: payload_ref(arguments)?,
                        signature: signature.clone(),
                        additional_params: additional_params
                            .as_ref()
                            .map(serde_json::to_string)
                            .transpose()?,
                    },
                    MessageBlock::ToolResult {
                        tool_call_doc_id,
                        id,
                        call_id,
                        parts,
                    } => LeanMessageBlock::ToolResult {
                        doc_id: self
                            .tool_ids
                            .get(tool_call_doc_id)
                            .copied()
                            .context("native result has no symbolic tool identity")?,
                        id: id.clone(),
                        call_id: call_id.clone(),
                        parts: parts
                            .iter()
                            .map(|part| match part {
                                gents_protocol::output::ToolResultPart::Text { text } => {
                                    Ok(LeanResultPart::Text {
                                        payload: presented_payload(text)?,
                                    })
                                }
                                gents_protocol::output::ToolResultPart::Media(_) => {
                                    anyhow::bail!("native result media is not in this fixture")
                                }
                            })
                            .collect::<Result<Vec<_>>>()?,
                    },
                    _ => anyhow::bail!("native adapter observed unsupported message block"),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(LeanCanonicalMessage {
            header: crate::lean_vocab_test::LeanCanonicalHeader {
                id,
                session: self.transcript_session_id,
                request: message.request_doc_id.as_ref().map(|_| self.request_id),
                origin: None,
                refs: message
                    .payload_references()
                    .into_iter()
                    .map(|reference| {
                        Ok(crate::lean_vocab_test::LeanPayloadRef {
                            close_id: self
                                .segment_ids
                                .get(&reference.close_doc_id)
                                .copied()
                                .context(
                                    "native header reference has no symbolic closure identity",
                                )?,
                            stream: u64::from(reference.stream),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
                outcome: match message.outcome {
                    OutputOutcome::Complete => LeanOutcome::Complete,
                    OutputOutcome::Partial => LeanOutcome::Partial,
                },
                role: match message.role {
                    MessageRole::System => LeanMessageRole::System,
                    MessageRole::User => LeanMessageRole::User,
                    MessageRole::Assistant => LeanMessageRole::Assistant,
                },
                publication: match &message.publication {
                    MessagePublication::RequestExecution {
                        execution_generation,
                    } => LeanMessagePublication::RequestExecution {
                        generation: parse_generation(Some(execution_generation))
                            .context("native header generation is not symbolic")?,
                    },
                    MessagePublication::RequestRecovery {
                        execution_generation,
                    } => LeanMessagePublication::RequestRecovery {
                        generation: parse_generation(Some(execution_generation))
                            .context("native recovery header generation is not symbolic")?,
                    },
                    MessagePublication::ToolDelivery { tool_call_doc_id } => {
                        LeanMessagePublication::ToolDelivery {
                            call: self
                                .tool_ids
                                .get(tool_call_doc_id)
                                .copied()
                                .context("native delivery has no symbolic tool identity")?,
                        }
                    }
                    _ => anyhow::bail!("native adapter observed unsupported publication"),
                },
            },
            key: message.message_key.clone(),
            sequence: modeled_sequence(u64::from(message.sequence))?,
            native_id: message.native_id.clone(),
            blocks,
            created_at: modeled_time(&message.created_at)?,
        })
    }

    async fn observe(&mut self, accepted: bool) -> Result<LeanCanonicalExecutionObservation> {
        self.refresh_output().await?;
        let response = self
            .node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ {} lifecycle_state }} }}"#,
                crate::graphql::escape_graphql_string(&self.request_doc_id),
                crate::watcher::AGENT_REQUEST_FIELDS,
            ))
            .await;
        anyhow::ensure!(
            !response.has_errors(),
            "observe native request: {:?}",
            response.errors
        );
        let row: AgentRequestRow = crate::graphql::first_row(&response, "AgentRequest")?
            .context("native request disappeared")?;
        let state = row
            .lifecycle_state
            .context("native request omitted lifecycle")?;
        let persisted_generation = parse_generation(row.execution_generation.as_deref());
        let active_lease = !state.is_terminal();
        let lease_deadline = row
            .execution_lease_expires_at
            .as_deref()
            .map(DateTime::parse_from_rfc3339)
            .transpose()?
            .and_then(|deadline| u64::try_from(deadline.timestamp() - FIXTURE_EPOCH_SECONDS).ok());
        let request = crate::graphql::escape_graphql_string(&self.request_doc_id);
        let tools = self.node.execute(&format!(
            r#"{{ AgentToolCall(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ _docID request_doc_id lifecycle_state message_sequence await_mode stuck_since cancel_cascade_intent_at }} }}"#,
        )).await;
        anyhow::ensure!(
            !tools.has_errors(),
            "observe native tools: {:?}",
            tools.errors
        );
        let tool_rows = tools
            .data
            .as_ref()
            .and_then(|data| data.get("AgentToolCall"))
            .and_then(serde_json::Value::as_array)
            .context("native tool observation omitted rows")?;
        anyhow::ensure!(
            tool_rows.len() == self.tool_ids.len(),
            "native tool observation contains unmapped physical rows"
        );
        let mut tool_state = None;
        let mut tool_stuck_since = None;
        let mut tool_cancel_intent_at = None;
        let mut in_flight = false;
        let mut physical_tool_request = None;
        let mut accepted_sequence = None;
        let mut reserved_max = None::<u64>;
        for tool in tool_rows {
            let doc_id = tool
                .get("_docID")
                .and_then(serde_json::Value::as_str)
                .context("native tool omitted physical identity")?;
            let symbolic = self
                .tool_ids
                .get(doc_id)
                .context("native tool has no symbolic identity")?;
            let state = tool
                .get("lifecycle_state")
                .and_then(serde_json::Value::as_str)
                .context("native tool omitted lifecycle")?;
            anyhow::ensure!(
                tool.get("request_doc_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(self.request_doc_id.as_str()),
                "native tool changed physical request binding"
            );
            let sequence = tool
                .get("message_sequence")
                .and_then(serde_json::Value::as_u64)
                .context("native tool omitted reserved sequence")?;
            let sequence = modeled_sequence(sequence)?;
            reserved_max = Some(reserved_max.map_or(sequence, |current| current.max(sequence)));
            if *symbolic == self.query_document {
                anyhow::ensure!(
                    tool_state.replace(state.to_owned()).is_none(),
                    "queried symbolic tool has multiple physical rows"
                );
                let await_mode = tool
                    .get("await_mode")
                    .and_then(serde_json::Value::as_str)
                    .context("native tool omitted await mode")?;
                tool_stuck_since = tool
                    .get("stuck_since")
                    .and_then(serde_json::Value::as_str)
                    .map(modeled_time)
                    .transpose()?;
                // Lean's transcript inFlight tracks the accepted parent's
                // unsettled foreground ownership, not a still-running physical
                // executor. The native terminal owner records handoff in
                // `stuck_since`; retaining a running row after that is valid.
                in_flight =
                    state == "running" && await_mode == "foreground" && tool_stuck_since.is_none();
                tool_cancel_intent_at = tool
                    .get("cancel_cascade_intent_at")
                    .and_then(serde_json::Value::as_str)
                    .map(modeled_time)
                    .transpose()?;
                physical_tool_request = Some(self.request_id);
                accepted_sequence = Some(sequence);
            }
        }
        // Initialization admits no seeded transcript/tool facts and creates a
        // fresh isolated session, so these two authoritative collections are
        // the complete native sequence reservation set for this adapter.
        let message_max = self.messages.iter().map(|message| message.sequence).max();
        self.next_sequence = message_max
            .into_iter()
            .chain(reserved_max)
            .max()
            .map_or(0, |value| value + 1);
        self.tool_state = tool_state;
        self.physical_tool_request = physical_tool_request;
        Ok(LeanCanonicalExecutionObservation {
            accepted,
            generation: active_lease.then_some(persisted_generation).flatten(),
            terminal_generation: state
                .is_terminal()
                .then_some(persisted_generation)
                .flatten(),
            request_state: state.as_str().to_owned(),
            tool_state: self.tool_state.clone(),
            tool_stuck_since,
            tool_cancel_intent_at,
            in_flight,
            next_sequence: self.next_sequence,
            accepted_sequence,
            physical_tool_request: self.physical_tool_request,
            lease_deadline: active_lease.then_some(lease_deadline).flatten(),
            compaction_cursor: None,
            segments: self.segments.clone(),
            messages: self.messages.clone(),
        })
    }
}

impl CanonicalExecutionAdapter for NativeCanonicalExecutionAdapter {
    type Error = anyhow::Error;
    type Native = NativeCanonicalExecution;

    fn initialize<'a>(
        &'a mut self,
        seed: &'a LeanCanonicalExecutionSeed,
    ) -> ExecutionFuture<'a, Result<Self::Native>> {
        Box::pin(async move {
            anyhow::ensure!(
                seed.segments.is_empty() && seed.messages.is_empty(),
                "seeded durable output is not implemented by the native adapter"
            );
            anyhow::ensure!(
                seed.tool_calls.is_empty() && seed.in_flight.is_empty(),
                "seeded tool execution is not implemented by the native adapter"
            );
            anyhow::ensure!(
                seed.next_sequence == 0,
                "nonzero sequence without seeded durable facts is not implemented"
            );
            let node = Arc::new(EmbeddedNode::builder().build().await?);
            crate::ensure_runtime_schemas(&node).await?;
            let generation = seed
                .lease
                .lease
                .generation
                .context("native fixture requires an active generation")?;
            let duration = seed
                .lease
                .lease
                .duration
                .context("native fixture requires a lease duration")?;
            let expiry = fixture_time(seed.lease.effective_expiry)?;
            let now = fixture_time(seed.lease.now)?;
            let request_id = format!("lean-request-{}", seed.request_id);
            let session_id = format!("lean-session-{}", seed.session_id);
            let principal = format!("did:test:lean:principal-{}", seed.principal);
            crate::session::create_session_with_behavior_id(
                &node,
                &session_id,
                "general",
                &principal,
                "general",
            )
            .await?;
            let response = node.execute(&format!(r#"mutation {{ create_AgentRequest(input: {{ request_id: "{}", agent_did: "{}", behavior_id: "general", session_id: "{}", retry_parent_request: "", retry_root_request: "{}", superseded_by_request: "", content: "lean native execution", lifecycle_state: "{}", backend_id: "", execution_origin: "interactive", execution_generation: "{}", execution_lease_expires_at: "{}", execution_lease_secs: {}, created_at: "{}", retry_count: 0, max_retries: 3, subagent_depth: 0 }}) {{ _docID }} }}"#,
                crate::graphql::escape_graphql_string(&request_id), crate::graphql::escape_graphql_string(&principal), crate::graphql::escape_graphql_string(&session_id), crate::graphql::escape_graphql_string(&request_id), seed.lease.request.as_str(), symbolic_generation(generation), expiry.to_rfc3339(), duration, now.to_rfc3339())).await;
            anyhow::ensure!(
                !response.has_errors(),
                "initialize native request: {:?}",
                response.errors
            );
            let lookup = node.execute(&format!(r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{ {} }} }}"#, crate::graphql::escape_graphql_string(&request_id), crate::watcher::AGENT_REQUEST_FIELDS)).await;
            let request_doc_id =
                crate::graphql::first_row::<AgentRequestRow>(&lookup, "AgentRequest")?
                    .and_then(|row| row.doc_id)
                    .context("native request omitted physical identity")?;
            Ok(NativeCanonicalExecution {
                node,
                request_doc_id,
                request_id: seed.request_id,
                principal_id: seed.principal,
                query_document: 0,
                remote_routes: seed.remote_routes.clone(),
                transcript_session_id: seed.transcript_session_id,
                next_sequence: seed.next_sequence,
                session_id,
                principal,
                segments: Vec::new(),
                messages: Vec::new(),
                tool_state: None,
                physical_tool_request: None,
                segment_ids: HashMap::new(),
                message_ids: HashMap::new(),
                modeled_message_keys: HashMap::new(),
                canonical_message_keys: HashMap::new(),
                tool_ids: HashMap::new(),
                accepted_spawns: HashMap::new(),
            })
        })
    }

    fn apply<'a>(
        &'a mut self,
        native: &'a mut Self::Native,
        query_document: u64,
        operation: &'a LeanCanonicalExecutionOperation,
    ) -> ExecutionFuture<'a, Result<LeanCanonicalExecutionObservation>> {
        Box::pin(async move {
            native.query_document = query_document;
            match operation {
                LeanCanonicalExecutionOperation::RenewLease {
                    now,
                    generation,
                    expected_deadline,
                    ..
                } => {
                    let outcome = crate::lifecycle::renew_execution_lease_once_at(
                        &native.node,
                        &native.request_doc_id,
                        &symbolic_generation(*generation),
                        fixture_time(*expected_deadline)?,
                        fixture_time(*now)?,
                    )
                    .await?;
                    native
                        .observe(matches!(
                            outcome,
                            crate::lifecycle::RenewalAttemptOutcome::Committed
                        ))
                        .await
                }
                LeanCanonicalExecutionOperation::AppendOutput {
                    now,
                    generation,
                    record,
                    ..
                } => {
                    anyhow::ensure!(
                        record.close.is_none(),
                        "append output cannot close a source"
                    );
                    let prepared = native.provider_segment(record, *generation)?;
                    let doc_id = crate::streaming::canonical::append_provider_segment_at(
                        &native.node,
                        &symbolic_generation(*generation),
                        &prepared,
                        fixture_time(*now)?,
                    )
                    .await;
                    let doc_id = match doc_id {
                        Ok(value) => value,
                        Err(error)
                            if error
                                .downcast_ref::<crate::streaming::canonical::ProviderAppendRejection>()
                                .is_some() =>
                        {
                            return native.observe(false).await;
                        }
                        Err(error) => return Err(error),
                    };
                    native.segment_ids.insert(doc_id, record.id);
                    native.observe(true).await
                }
                LeanCanonicalExecutionOperation::AppendOutputWhileSiblingWaits {
                    now,
                    generation,
                    record,
                    ..
                } => {
                    let prepared = native.provider_segment(record, *generation)?;
                    let node = Arc::clone(&native.node);
                    let nested_node = Arc::clone(&node);
                    let generation = symbolic_generation(*generation);
                    let now = fixture_time(*now)?;
                    let attempted = crate::config_client::ConfigAccess::transact_local(
                        &node,
                        None,
                        "lean.same_task_holder",
                        |_| {
                            let prepared = prepared.clone();
                            let generation = generation.clone();
                            let node = Arc::clone(&nested_node);
                            let now = now.clone();
                            Box::pin(async move {
                                crate::streaming::canonical::append_provider_segment_at(
                                    &node,
                                    &generation,
                                    &prepared,
                                    now,
                                )
                                .await
                                .map(|_| ())
                            })
                        },
                    )
                    .await;
                    match attempted {
                        Err(error)
                            if error
                                .downcast_ref::<crate::config_client::ReentrantEmbeddedWrite>()
                                .is_some() =>
                        {
                            native.observe(false).await
                        }
                        Err(error) => {
                            Err(error.context("same-task guarded append failed unexpectedly"))
                        }
                        Ok(()) => anyhow::bail!("same-task guarded append bypassed mutation gate"),
                    }
                }
                LeanCanonicalExecutionOperation::ClosePartial {
                    now,
                    generation,
                    item,
                    ..
                } => native.close_partial(*now, *generation, item).await,
                LeanCanonicalExecutionOperation::RecoverExpiredTerminal {
                    now,
                    expected_generation,
                    fresh_generation,
                    outcome,
                    selection,
                    items,
                    ..
                } => {
                    anyhow::ensure!(
                        matches!(outcome.as_str(), "failed" | "interrupted"),
                        "modeled terminal recovery outcome is not supported by the lease owner"
                    );
                    native
                        .recover_terminal(
                            *now,
                            *expected_generation,
                            *fresh_generation,
                            outcome,
                            selection,
                            items,
                        )
                        .await
                }
                LeanCanonicalExecutionOperation::Dispatch {
                    now,
                    generation,
                    call,
                    cancellation_allows,
                    tool_policy_allows,
                    ..
                } => {
                    anyhow::ensure!(
                        *cancellation_allows && *tool_policy_allows,
                        "native dispatch adapter has no external denial injection"
                    );
                    let physical = native.physical_tool(*call)?.to_owned();
                    let mut tool = if let Some((accepted, deadline, await_mode, cancel_policy)) =
                        native.accepted_spawns.get(call)
                    {
                        anyhow::ensure!(
                            accepted.tool_call_doc_id == physical,
                            "modeled spawn dispatch lost exact accepted row"
                        );
                        crate::tool_call_lifecycle::ToolCallLifecycle::from_accepted(
                            native.node.clone(),
                            native.principal.clone(),
                            None,
                            accepted.clone(),
                            fixture_time(*deadline)?,
                            *await_mode,
                            *cancel_policy,
                        )?
                    } else {
                        crate::tool_call_lifecycle::ToolCallLifecycle::load_by_doc_id(
                            native.node.clone(),
                            &physical,
                            &native.principal,
                            &native.session_id,
                            None,
                        )
                        .await?
                        .context("accepted physical tool disappeared before dispatch")?
                    };
                    match tool
                        .start_running_at(fixture_time(*now)?, &symbolic_generation(*generation))
                        .await
                    {
                        Ok(()) => native.observe(true).await,
                        Err(error)
                            if error
                                .downcast_ref::<crate::tool_call_lifecycle::delivery::ToolDispatchRejection>()
                                .is_some() =>
                        {
                            native.observe(false).await
                        }
                        Err(error) => Err(error.context("native tool dispatch owner failed")),
                    }
                }
                LeanCanonicalExecutionOperation::TerminalizeCompleted {
                    now,
                    generation,
                    outcome,
                    selection,
                    ..
                } => {
                    let outcome = match outcome.as_str() {
                        "completed" => crate::lifecycle::RequestTerminalOutcome::Completed,
                        "failed" => crate::lifecycle::RequestTerminalOutcome::Failed,
                        "interrupted" => crate::lifecycle::RequestTerminalOutcome::Interrupted,
                        "dead" => crate::lifecycle::RequestTerminalOutcome::Dead,
                        "superseded" => crate::lifecycle::RequestTerminalOutcome::Superseded,
                        other => anyhow::bail!("unknown modeled terminal outcome: {other}"),
                    };
                    let selection = match selection {
                        LeanTerminalSelection::NoMessage => {
                            gents_protocol::output::TerminalOutput::NoMessage
                        }
                        LeanTerminalSelection::Message { id } => {
                            let physical = native
                                .message_ids
                                .iter()
                                .find_map(|(physical, symbolic)| {
                                    (*symbolic == *id).then_some(physical.clone())
                                })
                                .context(
                                    "modeled terminal message has no published physical header",
                                )?;
                            gents_protocol::output::TerminalOutput::Message {
                                message_doc_id: physical,
                            }
                        }
                    };
                    let result = crate::lifecycle::terminalize_owned_at(
                        &native.node,
                        &native.request_doc_id,
                        &symbolic_generation(*generation),
                        outcome,
                        selection,
                        fixture_time(*now)?,
                    )
                    .await;
                    let result = match result {
                        Ok(value) => value,
                        Err(error)
                            if error
                                .downcast_ref::<crate::lifecycle::ToolAccountingRejection>()
                                .is_some() =>
                        {
                            return native.observe(false).await;
                        }
                        Err(error) => return Err(error),
                    };
                    native
                        .observe(!matches!(result, crate::lifecycle::TerminalizeResult::Lost))
                        .await
                }
                LeanCanonicalExecutionOperation::CompleteForegroundTool {
                    now,
                    document,
                    authority_outcome,
                    record,
                    message,
                    ..
                } => {
                    native
                        .complete_foreground(*now, *document, authority_outcome, record, message)
                        .await
                }
                LeanCanonicalExecutionOperation::RevokeCorrupt {
                    now,
                    expected_generation,
                    fresh_generation,
                    outcome,
                    selection,
                    ..
                } => {
                    native
                        .revoke_corrupt(
                            *now,
                            *expected_generation,
                            *fresh_generation,
                            outcome,
                            selection,
                        )
                        .await
                }
                LeanCanonicalExecutionOperation::AdmitSpawnedBackground {
                    now,
                    generation,
                    admission,
                    ..
                } => {
                    let physical_parent = native
                        .physical_tool(admission.parent_tool_document)?
                        .to_owned();
                    let mut parent = crate::tool_call_lifecycle::ToolCallLifecycle::load_by_doc_id(
                        Arc::clone(&native.node),
                        &physical_parent,
                        &native.principal,
                        &native.session_id,
                        None,
                    )
                    .await?
                    .context("spawned admission parent disappeared")?;
                    anyhow::ensure!(
                        parent.execution_generation()
                            == Some(symbolic_generation(*generation).as_str()),
                        "modeled spawned admission generation differs from physical parent"
                    );
                    let child = parent
                        .admit_spawned_background_child_at(
                            crate::tool_call_lifecycle::SpawnedBackgroundToolAdmission {
                                tool_name: admission.operation.clone(),
                                deadline_at: fixture_time(admission.deadline)?,
                            },
                            fixture_time(*now)?,
                        )
                        .await?;
                    let physical_child = child
                        .doc_id()
                        .context("spawned admission child omitted physical identity")?
                        .to_owned();
                    if let Some(existing_symbolic) = native.tool_ids.get(&physical_child) {
                        if *existing_symbolic != admission.document {
                            return native.observe(false).await;
                        }
                    } else {
                        native.tool_ids.insert(physical_child, admission.document);
                    }
                    native.observe(true).await
                }
                LeanCanonicalExecutionOperation::DeliverReplicatedSegment { record, .. } => {
                    let generation = match &record.writer {
                        LeanCanonicalWriter::Request { generation } => *generation,
                        _ => anyhow::bail!(
                            "replicated native fixture supports provider segments only"
                        ),
                    };
                    let segment = native.provider_segment(record, generation)?;
                    let response = native.node.execute_request_with_retry(
                        defra_node::QueryRequest::new(
                            crate::session::canonical_rows::CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                        ).with_variables(
                            crate::session::canonical_rows::output_segment_create_variables(&segment)?,
                        ),
                        defra_node::ExecuteRetryPolicy::default(),
                    ).await;
                    anyhow::ensure!(
                        !response.has_errors(),
                        "replicate native segment: {:?}",
                        response.errors
                    );
                    let doc_id = crate::graphql::created_doc_id(
                        &serde_json::json!({ "data": response.data }),
                        "AgentOutputSegment",
                    )?;
                    native.segment_ids.insert(doc_id, record.id);
                    native.observe(true).await
                }
                LeanCanonicalExecutionOperation::AcceptForeground {
                    now,
                    generation,
                    closing,
                    message,
                    targets,
                    admissions,
                    ..
                }
                | LeanCanonicalExecutionOperation::AcceptTurn {
                    now,
                    generation,
                    closing,
                    message,
                    targets,
                    admissions,
                    ..
                } => {
                    anyhow::ensure!(
                        targets.is_empty(),
                        "foreground acceptance unexpectedly carries remote targets"
                    );
                    native
                        .accept_provider_turn(
                            *now,
                            *generation,
                            closing,
                            message,
                            targets,
                            admissions,
                        )
                        .await
                }
                LeanCanonicalExecutionOperation::AcceptRemote {
                    now,
                    generation,
                    closing,
                    message,
                    targets,
                    admissions,
                    ..
                } => {
                    native
                        .accept_provider_turn(
                            *now,
                            *generation,
                            closing,
                            message,
                            targets,
                            admissions,
                        )
                        .await
                }
                other => anyhow::bail!(
                    "native canonical execution operation is not implemented yet: {other:?}"
                ),
            }
        })
    }
}

impl NativeCanonicalExecution {
    async fn close_partial(
        &mut self,
        now: u64,
        generation: u64,
        item: &crate::lean_vocab_test::LeanCanonicalRecoveryItem,
    ) -> Result<LeanCanonicalExecutionObservation> {
        // Preserve the candidate's writer independently of the actor's lease
        // generation so stale operations reach the native authorization owner.
        let LeanCanonicalWriter::Request {
            generation: writer_generation,
        } = &item.closing.writer
        else {
            anyhow::bail!("provider partial close requires a request writer")
        };
        let closing = self.provider_segment(&item.closing, *writer_generation)?;
        let mut header = item
            .message
            .as_ref()
            .map(|message| self.protocol_message(message, *writer_generation))
            .transpose()?;
        if let Some(header) = header.as_mut() {
            header.message_key = crate::streaming::canonical::partial_message_key(
                &self.request_doc_id,
                &closing.source,
            )?;
        }
        let symbolic_close = format!("lean-segment-{}", item.closing.id);
        let header_factory = header.map(|template| {
            let symbolic_close = symbolic_close.clone();
            Arc::new(move |physical_close: &str| {
                let mut message = template.clone();
                for block in &mut message.blocks {
                    match block {
                        MessageBlock::Text { text } => {
                            if text.output.close_doc_id == symbolic_close {
                                text.output.close_doc_id = physical_close.to_owned();
                            }
                        }
                        MessageBlock::ToolCall { arguments, .. } => {
                            if arguments.close_doc_id == symbolic_close {
                                arguments.close_doc_id = physical_close.to_owned();
                            }
                        }
                        // The bounded adapter currently cannot decode these
                        // modeled shapes, but identity remapping itself is not
                        // a policy gate. Canonical owner validation below owns
                        // whether any such block is legal for Partial output.
                        MessageBlock::Reasoning { .. }
                        | MessageBlock::ToolResult { .. }
                        | MessageBlock::Media(_) => {}
                    }
                }
                Ok(message)
            }) as crate::streaming::canonical::PartialHeaderFactory
        });
        let result = crate::streaming::canonical::close_provider_attempt_at(
            &self.node,
            &symbolic_generation(generation),
            &closing,
            crate::streaming::canonical::ProviderAttemptClose::Partial,
            Some(crate::streaming::canonical::ProviderPartialCandidate {
                closing: closing.clone(),
                header: header_factory,
            }),
            fixture_time(now)?,
        )
        .await;
        let message_doc_id = match result {
            Ok(message_doc_id) => message_doc_id,
            Err(error)
                if error
                    .downcast_ref::<crate::streaming::canonical::ProviderCloseRejection>()
                    .is_some()
                    || error
                        .downcast_ref::<gents_protocol::output::ReconstructionError>()
                        .is_some() =>
            {
                return self.observe(false).await;
            }
            Err(error) => return Err(error.context("native ClosePartial owner failed")),
        };

        let request = crate::graphql::escape_graphql_string(&self.request_doc_id);
        let response = self.node.execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ {} }} }}"#,
            crate::session::canonical_rows::AGENT_OUTPUT_SEGMENT_FIELDS,
        )).await;
        anyhow::ensure!(
            !response.has_errors(),
            "read partial closure: {:?}",
            response.errors
        );
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data["AgentOutputSegment"].as_array())
            .context("partial closure query omitted rows")?;
        let closing_row = rows
            .iter()
            .map(crate::session::canonical_rows::decode_output_segment_row)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .find(|row| row.segment.close.is_some())
            .context("accepted partial close did not persist its closure")?;
        self.segment_ids.insert(closing_row.doc_id, item.closing.id);
        if let (Some(doc_id), Some(message)) = (message_doc_id, item.message.as_ref()) {
            self.message_ids.insert(doc_id.clone(), message.header.id);
            self.modeled_message_keys
                .insert(doc_id.clone(), message.key.clone());
            self.canonical_message_keys.insert(
                doc_id,
                crate::streaming::canonical::partial_message_key(
                    &self.request_doc_id,
                    &closing.source,
                )?,
            );
        }
        self.observe(true).await
    }

    async fn accept_provider_turn(
        &mut self,
        now: u64,
        generation: u64,
        closing: &LeanCanonicalSegment,
        message: &LeanCanonicalMessage<LeanPayloadSpec>,
        targets: &[crate::lean_vocab_test::LeanCanonicalRemoteTarget],
        admissions: &[LeanCanonicalToolAdmission],
    ) -> Result<LeanCanonicalExecutionObservation> {
        anyhow::ensure!(
            matches!(
                closing.close,
                Some(crate::lean_vocab_test::LeanCanonicalClosure::Closed {
                    outcome: LeanOutcome::Complete,
                    ..
                })
            ),
            "native publication owner accepts only complete provider closures"
        );
        let sealed = self.provider_segment(closing, generation)?;
        let header = self.protocol_message(message, generation)?;
        // A replay reads the already committed close through `observe`. The
        // candidate is that same physical identity, not a second source row.
        // Verify its immutable facts before excluding it from the input extent
        // used to reconstruct the expected native message.
        for committed in self.segments.iter().filter(|row| row.id == closing.id) {
            anyhow::ensure!(
                committed == closing,
                "modeled replay close differs from its committed physical identity"
            );
        }
        let preceding = self
            .segments
            .iter()
            .filter(|row| row.id != closing.id)
            .collect::<Vec<_>>();
        let mut protocol_segments = preceding
            .iter()
            .map(|segment| self.provider_segment(segment, generation))
            .collect::<Result<Vec<_>>>()?;
        // Build the expected native message from the same committed source
        // bytes the publication owner will inspect. The supplied close remains
        // untouched in `final_flush` below, so an invalid modeled extent is
        // rejected by that owner instead of this transport encoder.
        let mut reconstruction_close = sealed.clone();
        reconstruction_close.close = None;
        let open_doc_ids = preceding
            .iter()
            .map(|segment| format!("lean-segment-{}", segment.id))
            .collect::<Vec<_>>();
        let mut open_observed = open_doc_ids
            .iter()
            .zip(&protocol_segments)
            .map(|(doc_id, segment)| ObservedSegment { doc_id, segment })
            .collect::<Vec<_>>();
        let final_doc_id = format!("lean-segment-{}", closing.id);
        if reconstruction_close.ordinal.is_some() {
            open_observed.push(ObservedSegment {
                doc_id: &final_doc_id,
                segment: &reconstruction_close,
            });
        }
        let extent = gents_protocol::output::extent::inspect_open_source(
            &open_observed,
            &reconstruction_close.request_doc_id,
            &reconstruction_close.source,
            &reconstruction_close.writer,
        )?;
        reconstruction_close.close = Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: extent.segments,
            stream_bytes: extent.stream_bytes,
        });
        protocol_segments.push(reconstruction_close);
        let doc_ids = preceding
            .iter()
            .map(|segment| format!("lean-segment-{}", segment.id))
            .chain(std::iter::once(format!("lean-segment-{}", closing.id)))
            .collect::<Vec<_>>();
        let observed = doc_ids
            .iter()
            .zip(&protocol_segments)
            .map(|(doc_id, segment)| ObservedSegment { doc_id, segment })
            .collect::<Vec<_>>();
        let expected = reconstruct_message(&observed, &[], &[], &header)?;
        let encoded = Arc::new(crate::streaming::native_encoding::encode_native_message(
            &expected,
        )?);
        let expected_native = expected.clone();
        let final_flush = sealed;
        let spawn_admissions = targets
            .iter()
            .map(|target| -> Result<crate::streaming::SpawnAdmissionPlan> {
                let route = self
                    .remote_routes
                    .iter()
                    .find(|route| {
                        route.call == target.call
                            && route.target == target.target
                            && route.behavior == target.behavior
                    })
                    .context("modeled remote target has no configured route fact")?;
                anyhow::ensure!(
                    target.coordinator == self.principal_id,
                    "modeled remote target coordinator differs from fixture principal"
                );
                let admission = admissions
                    .iter()
                    .find(|admission| admission.document == route.call)
                    .context("modeled remote target has no accepted physical admission")?;
                let call_id = message
                    .blocks
                    .iter()
                    .find_map(|block| match block {
                        LeanMessageBlock::ToolCall { doc_id, id, .. } if *doc_id == target.call => {
                            Some(id.clone())
                        }
                        _ => None,
                    })
                    .context("modeled remote target has no provider-native ToolCall")?;
                let child = admission
                    .child_request_id
                    .context("modeled remote admission omitted immutable child identity")?;
                let admitted_behavior = admission
                    .spawn_behavior_id
                    .context("modeled remote admission omitted immutable behavior")?;
                let await_mode = match admission.await_mode.as_str() {
                    "background" => crate::tool_call_lifecycle::AwaitMode::Background,
                    "foreground" => crate::tool_call_lifecycle::AwaitMode::Foreground,
                    other => anyhow::bail!("unknown modeled await mode: {other}"),
                };
                Ok(crate::streaming::SpawnAdmissionPlan {
                    tool_call_id: call_id,
                    child_request_id: format!("lean-child-{child}"),
                    spawn_target_did: format!("did:test:lean:principal-{}", route.target),
                    spawn_behavior_id: format!("lean-behavior-{admitted_behavior}"),
                    delegated_workspace: admission.delegated_workspace.as_ref().map(|workspace| {
                        gents_protocol::output::DelegatedWorkspace {
                            workspace_id: format!("lean-workspace-{}", workspace.workspace_id),
                            workspace_owner_agent_did: format!(
                                "did:test:lean:principal-{}",
                                workspace.workspace_owner_agent_did
                            ),
                            workspace_authority: workspace.workspace_authority.clone(),
                            workspace_seal_hash: workspace
                                .workspace_seal_hash
                                .map(|seal| format!("lean-seal-{seal}")),
                        }
                    }),
                    await_mode,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let tool_deadline_at = fixture_time(
            admissions
                .first()
                .map_or(now, |admission| admission.deadline),
        )?
        .to_rfc3339();
        let published = crate::streaming::canonical::publish_provider_turn_at(
            &self.node,
            &symbolic_generation(generation),
            crate::streaming::canonical::ProviderPublicationPlan {
                final_flush: Some(final_flush),
                message_key: message.key.clone(),
                encoded,
                expected: Arc::new(expected),
                tool_deadline_at,
                spawn_admissions: spawn_admissions.clone(),
            },
            fixture_time(now)?,
        )
        .await;
        let published = match published {
            Ok(value) => value,
            Err(error)
                if error
                    .downcast_ref::<crate::streaming::canonical::ProviderCloseRejection>()
                    .is_some() =>
            {
                return self.observe(false).await;
            }
            Err(error)
                if error
                    .downcast_ref::<crate::streaming::canonical::ProviderReplayRejection>()
                    .is_some() =>
            {
                return self.observe(false).await;
            }
            Err(error) => {
                let detail = format!("native provider publication owner failed: {error:#}");
                return Err(error.context(detail));
            }
        };
        let (stored_header, stored_native) = crate::session::load_canonical_message_from_node(
            &self.node,
            &published.message_doc_id,
            &self.principal,
            None,
        )
        .await?;
        anyhow::ensure!(
            stored_native == expected_native,
            "native publication content differs: expected {expected_native:?}, got {stored_native:?}"
        );
        anyhow::ensure!(
            stored_header.message_key == message.key,
            "native publication key differs: expected {:?}, got {:?}",
            message.key,
            stored_header.message_key
        );
        anyhow::ensure!(
            stored_header.sequence == native_sequence(message.sequence)?,
            "native publication sequence differs: expected {}, got {}",
            message.sequence,
            stored_header.sequence
        );
        anyhow::ensure!(
            stored_header.request_doc_id.as_deref() == Some(self.request_doc_id.as_str()),
            "native publication request binding differs: expected {:?}, got {:?}",
            self.request_doc_id,
            stored_header.request_doc_id
        );
        let expected_publication = MessagePublication::RequestExecution {
            execution_generation: symbolic_generation(generation),
        };
        anyhow::ensure!(
            stored_header.publication == expected_publication,
            "native publication authority differs: expected {expected_publication:?}, got {:?}",
            stored_header.publication
        );
        self.message_ids
            .insert(published.message_doc_id.clone(), message.header.id);
        self.modeled_message_keys
            .insert(published.message_doc_id.clone(), message.key.clone());
        self.canonical_message_keys.insert(
            published.message_doc_id.clone(),
            stored_header.message_key.clone(),
        );
        for reference in stored_header.payload_references() {
            self.segment_ids
                .insert(reference.close_doc_id.clone(), closing.id);
        }
        anyhow::ensure!(
            published.accepted_tools.len() == admissions.len(),
            "native publication admission count differs from modeled acceptance"
        );
        for (accepted, admission) in published.accepted_tools.iter().zip(admissions) {
            self.tool_ids
                .insert(accepted.tool_call_doc_id.clone(), admission.document);
            let response = self
                .node
                .execute(&format!(
                    r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID request_doc_id lifecycle_state child_request_id spawn_target_did spawn_behavior_id delegated_workspace await_mode }} }}"#,
                    crate::graphql::escape_graphql_string(&accepted.tool_call_doc_id),
                ))
                .await;
            anyhow::ensure!(
                !response.has_errors(),
                "read accepted native tool: {:?}",
                response.errors
            );
            let rows = response
                .data
                .as_ref()
                .and_then(|data| data.get("AgentToolCall"))
                .and_then(serde_json::Value::as_array)
                .context("accepted native tool query omitted rows")?;
            anyhow::ensure!(
                rows.len() == 1,
                "accepted native tool is not physically unique"
            );
            anyhow::ensure!(
                rows[0]
                    .get("request_doc_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(self.request_doc_id.as_str())
                    && rows[0]
                        .get("lifecycle_state")
                        .and_then(serde_json::Value::as_str)
                        == Some(admission.state.as_str()),
                "accepted native tool readback differs from modeled admission"
            );
            if let Some(plan) = spawn_admissions
                .iter()
                .find(|plan| plan.tool_call_id == accepted.id)
            {
                let stored = &rows[0];
                anyhow::ensure!(
                    stored["child_request_id"].as_str() == Some(plan.child_request_id.as_str())
                        && stored["spawn_target_did"].as_str()
                            == Some(plan.spawn_target_did.as_str())
                        && stored["spawn_behavior_id"].as_str()
                            == Some(plan.spawn_behavior_id.as_str())
                        && stored["delegated_workspace"]
                            == serde_json::to_value(&plan.delegated_workspace)?
                        && stored["await_mode"].as_str() == Some(plan.await_mode.as_str()),
                    "accepted native spawn provenance differs from modeled route and admission"
                );
                self.accepted_spawns.insert(
                    admission.document,
                    (
                        accepted.clone(),
                        admission.deadline,
                        plan.await_mode,
                        crate::tool_call_lifecycle::CancelPolicy::from_persisted(
                            &admission.cancel_policy,
                        )
                        .context("modeled spawn admission has invalid cancel policy")?,
                    ),
                );
            }
        }
        self.observe(true).await
    }

    fn protocol_message(
        &self,
        message: &LeanCanonicalMessage<LeanPayloadSpec>,
        generation: u64,
    ) -> Result<TranscriptMessage> {
        anyhow::ensure!(message.header.session > 0, "message session is blank");
        anyhow::ensure!(
            message.header.request.is_some(),
            "provider header omitted request"
        );
        anyhow::ensure!(
            message.header.origin.is_none(),
            "provider header has fork origin"
        );
        anyhow::ensure!(
            matches!(message.header.publication,
                LeanMessagePublication::RequestExecution { generation: owner }
                | LeanMessagePublication::RequestRecovery { generation: owner }
                if owner == generation),
            "provider header generation conflicts with operation"
        );
        let payload = |value: &LeanPayloadSpec| -> Result<PresentedPayload> {
            Ok(PresentedPayload {
                output: PayloadRef {
                    close_doc_id: format!("lean-segment-{}", value.reference.close_id),
                    stream: u32::try_from(value.reference.stream)?,
                },
                presentation: match &value.presentation {
                    LeanPresentation::Full => PayloadPresentation::Full,
                    LeanPresentation::Composed { .. } => anyhow::bail!(
                        "composed presentation is not implemented by the bounded adapter"
                    ),
                },
            })
        };
        let blocks = message
            .blocks
            .iter()
            .map(|block| -> Result<MessageBlock> {
                Ok(match block {
                    LeanMessageBlock::Text { payload: value } => MessageBlock::Text {
                        text: payload(value)?,
                    },
                    LeanMessageBlock::ToolCall {
                        doc_id,
                        id,
                        call_id,
                        name,
                        arguments,
                        signature,
                        additional_params,
                    } => MessageBlock::ToolCall {
                        tool_call_doc_id: format!("lean-tool-{doc_id}"),
                        id: id.clone(),
                        call_id: call_id.clone(),
                        name: name.clone(),
                        arguments: payload(arguments)?.output,
                        signature: signature.clone(),
                        additional_params: additional_params
                            .as_deref()
                            .map(serde_json::from_str)
                            .transpose()?,
                    },
                    _ => anyhow::bail!(
                        "message block is not implemented by the bounded acceptance adapter"
                    ),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(TranscriptMessage {
            message_key: message.key.clone(),
            session_id: self.session_id.clone(),
            agent_did: self.principal.clone(),
            requester_did: None,
            request_doc_id: Some(self.request_doc_id.clone()),
            publication: match message.header.publication {
                LeanMessagePublication::RequestExecution { .. } => {
                    MessagePublication::RequestExecution {
                        execution_generation: symbolic_generation(generation),
                    }
                }
                LeanMessagePublication::RequestRecovery { .. } => {
                    MessagePublication::RequestRecovery {
                        execution_generation: symbolic_generation(generation),
                    }
                }
                _ => anyhow::bail!("provider header has unsupported publication authority"),
            },
            outcome: match message.header.outcome {
                LeanOutcome::Complete => OutputOutcome::Complete,
                LeanOutcome::Partial => OutputOutcome::Partial,
            },
            sequence: native_sequence(message.sequence)?,
            role: match message.header.role {
                LeanMessageRole::System => MessageRole::System,
                LeanMessageRole::User => MessageRole::User,
                LeanMessageRole::Assistant => MessageRole::Assistant,
            },
            native_id: message.native_id.clone(),
            blocks,
            created_at: fixture_time(message.created_at)?.to_rfc3339(),
        })
    }

    fn provider_segment(
        &self,
        record: &LeanCanonicalSegment,
        generation: u64,
    ) -> Result<gents_protocol::output::OutputSegment> {
        let LeanCanonicalSource::Provider {
            scope,
            turn,
            attempt,
        } = &record.coordinate.source
        else {
            anyhow::bail!("append output currently supports provider sources only")
        };
        anyhow::ensure!(record.coordinate.request > 0, "symbolic request is blank");
        anyhow::ensure!(
            matches!(&record.writer, LeanCanonicalWriter::Request { generation: writer } if *writer == generation),
            "append output writer generation conflicts with operation"
        );
        let runs = record
            .flush
            .as_ref()
            .into_iter()
            .flat_map(|flush| &flush.runs)
            .map(|run| {
                let declaration = run
                    .declaration
                    .as_ref()
                    .map(|declaration| {
                        let payload = match declaration.kind {
                            LeanPayloadKind::Text => gents_protocol::output::StreamPayload::Text,
                            LeanPayloadKind::Reasoning => {
                                gents_protocol::output::StreamPayload::Reasoning
                            }
                            LeanPayloadKind::Summary => {
                                gents_protocol::output::StreamPayload::ReasoningSummary
                            }
                            LeanPayloadKind::Opaque => {
                                gents_protocol::output::StreamPayload::ReasoningOpaque
                            }
                            LeanPayloadKind::Arguments => {
                                let tool = declaration
                                    .tool
                                    .as_ref()
                                    .context("argument stream omitted tool identity")?;
                                gents_protocol::output::StreamPayload::ToolArguments {
                                    id: tool.id.clone(),
                                    call_id: tool.call_id.clone(),
                                    name: tool.name.clone(),
                                }
                            }
                            LeanPayloadKind::ToolOutput => {
                                gents_protocol::output::StreamPayload::ToolOutput
                            }
                            LeanPayloadKind::Media => anyhow::bail!(
                                "media append conversion is not implemented in the bounded adapter"
                            ),
                        };
                        Ok(gents_protocol::output::StreamDeclaration {
                            block_index: u32::try_from(declaration.block)?,
                            part_index: u32::try_from(declaration.part)?,
                            payload,
                        })
                    })
                    .transpose()?;
                Ok(gents_protocol::output::SegmentRun {
                    stream: u32::try_from(run.stream)?,
                    bytes: u32::try_from(run.bytes)?,
                    declaration,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(gents_protocol::output::OutputSegment {
            agent_did: self.principal.clone(),
            requester_did: None,
            session_id: self.session_id.clone(),
            request_doc_id: self.request_doc_id.clone(),
            source: gents_protocol::output::OutputSource::ProviderTurn {
                scope: format!("inference.{scope}").parse()?,
                turn_index: u32::try_from(*turn)?,
                attempt: u32::try_from(*attempt)?,
            },
            writer: gents_protocol::output::OutputWriter::RequestExecution {
                execution_generation: symbolic_generation(generation),
            },
            ordinal: record
                .flush
                .as_ref()
                .map(|flush| u32::try_from(flush.ordinal))
                .transpose()?,
            runs,
            payload: record
                .flush
                .as_ref()
                .map(|flush| String::from_utf8(flush.payload.clone()))
                .transpose()?
                .unwrap_or_default(),
            close: record.close.as_ref().map(|close| match close {
                crate::lean_vocab_test::LeanCanonicalClosure::Closed {
                    outcome,
                    segments,
                    stream_bytes,
                } => SourceClose::Closed {
                    outcome: match outcome {
                        LeanOutcome::Complete => OutputOutcome::Complete,
                        LeanOutcome::Partial => OutputOutcome::Partial,
                    },
                    segments: u32::try_from(*segments).expect("modeled segment count exceeds u32"),
                    stream_bytes: stream_bytes.clone(),
                },
                crate::lean_vocab_test::LeanCanonicalClosure::Retracted => SourceClose::Retracted,
            }),
            created_at: fixture_time(record.created_at)?.to_rfc3339(),
        })
    }

    fn tool_output_segment(
        &self,
        record: &LeanCanonicalSegment,
        symbolic_tool: u64,
        physical_tool: &str,
    ) -> Result<gents_protocol::output::OutputSegment> {
        anyhow::ensure!(
            record.coordinate.request == self.request_id,
            "tool output segment belongs to another request"
        );
        anyhow::ensure!(
            matches!(&record.coordinate.source, LeanCanonicalSource::Tool { call } if *call == symbolic_tool),
            "tool output segment belongs to another source"
        );
        anyhow::ensure!(
            matches!(&record.writer, LeanCanonicalWriter::Tool { call } if *call == symbolic_tool),
            "tool output segment has another writer"
        );
        let runs = record
            .flush
            .as_ref()
            .map(|flush| {
                flush
                    .runs
                    .iter()
                    .map(|run| {
                        let declaration = run
                            .declaration
                            .as_ref()
                            .map(|declaration| {
                                anyhow::ensure!(
                                    declaration.kind == LeanPayloadKind::ToolOutput,
                                    "modeled tool source declared a non-tool payload"
                                );
                                Ok(gents_protocol::output::StreamDeclaration {
                                    block_index: u32::try_from(declaration.block)?,
                                    part_index: u32::try_from(declaration.part)?,
                                    payload: gents_protocol::output::StreamPayload::ToolOutput,
                                })
                            })
                            .transpose()?;
                        Ok(gents_protocol::output::SegmentRun {
                            stream: u32::try_from(run.stream)?,
                            bytes: u32::try_from(run.bytes)?,
                            declaration,
                        })
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        Ok(gents_protocol::output::OutputSegment {
            agent_did: self.principal.clone(),
            requester_did: None,
            session_id: self.session_id.clone(),
            request_doc_id: self.request_doc_id.clone(),
            source: gents_protocol::output::OutputSource::ToolCall {
                tool_call_doc_id: physical_tool.to_owned(),
            },
            writer: gents_protocol::output::OutputWriter::ToolExecution {
                tool_call_doc_id: physical_tool.to_owned(),
            },
            ordinal: record
                .flush
                .as_ref()
                .map(|flush| u32::try_from(flush.ordinal))
                .transpose()?,
            runs,
            payload: record
                .flush
                .as_ref()
                .map(|flush| String::from_utf8(flush.payload.clone()))
                .transpose()?
                .unwrap_or_default(),
            close: record.close.as_ref().map(|close| match close {
                crate::lean_vocab_test::LeanCanonicalClosure::Closed {
                    outcome,
                    segments,
                    stream_bytes,
                } => SourceClose::Closed {
                    outcome: match outcome {
                        LeanOutcome::Complete => OutputOutcome::Complete,
                        LeanOutcome::Partial => OutputOutcome::Partial,
                    },
                    segments: u32::try_from(*segments).expect("modeled segment count exceeds u32"),
                    stream_bytes: stream_bytes.clone(),
                },
                crate::lean_vocab_test::LeanCanonicalClosure::Retracted => SourceClose::Retracted,
            }),
            created_at: fixture_time(record.created_at)?.to_rfc3339(),
        })
    }
}

#[tokio::test]
async fn every_generated_native_execution_script_runs_to_completion() {
    let cases = crate::lean_vocab_test::lean_contract_snapshot()
        .canonical_execution_gate_cases
        .iter()
        .filter(|case| {
            matches!(
                case,
                crate::lean_vocab_test::LeanCanonicalExecutionCase::NativeExecution { .. }
            )
        })
        .collect::<Vec<_>>();
    assert!(
        !cases.is_empty(),
        "Lean exported no native execution scripts"
    );
    let mut failures = Vec::new();
    for case in cases {
        let mut adapter = NativeCanonicalExecutionAdapter;
        if let Err(error) =
            crate::lean_vocab_test::assert_native_execution_case(case, &mut adapter).await
        {
            failures.push(error);
        }
    }
    assert!(
        failures.is_empty(),
        "native execution contract gaps:\n{}",
        failures.join("\n")
    );
}

#[tokio::test]
async fn terminal_request_without_tool_handoff_still_reports_in_flight() {
    let case = crate::lean_vocab_test::lean_contract_snapshot()
        .canonical_execution_gate_cases
        .iter()
        .find(|case| {
            matches!(case,
            crate::lean_vocab_test::LeanCanonicalExecutionCase::NativeExecution { name, .. }
                if name == "running_foreground_terminal_recovery_records_handoff")
        })
        .expect("Lean exports the running foreground terminal-recovery case");
    let crate::lean_vocab_test::LeanCanonicalExecutionCase::NativeExecution {
        seed,
        operations,
        ..
    } = case
    else {
        unreachable!()
    };
    let mut adapter = NativeCanonicalExecutionAdapter;
    let mut native = adapter.initialize(seed).await.unwrap();
    for operation in operations.iter().take(2) {
        let observed = adapter.apply(&mut native, 600, operation).await.unwrap();
        assert!(observed.accepted);
    }
    let before = native.observe(true).await.unwrap();
    assert!(before.in_flight);
    assert_eq!(before.tool_stuck_since, None);

    // Deliberately inject a terminal request without the terminal owner's
    // tool-accounting write. This is a negative control for the observation:
    // request terminality alone must not conceal an unreleased foreground tool.
    let doc_id = crate::graphql::escape_graphql_string(&native.request_doc_id);
    let mutation = format!(
        r#"mutation {{ update_AgentRequest(
        filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
        input: {{ lifecycle_state: "failed" }}) {{ _docID }} }}"#
    );
    crate::config_client::ConfigAccess::write_local(
        &native.node,
        "test.native_in_flight_negative_control",
        &mutation,
    )
    .await
    .unwrap();
    let after = native.observe(true).await.unwrap();
    assert_eq!(after.request_state, "failed");
    assert_eq!(after.tool_state.as_deref(), Some("running"));
    assert_eq!(after.tool_stuck_since, None);
    assert!(after.in_flight);
    native.node.shutdown().await;
}

#[tokio::test]
async fn canonical_tool_output_uses_modeled_physical_source_facts() {
    use crate::session::canonical_rows::{
        output_segment_create_variables, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };

    let witness = crate::lean_vocab_test::lean_r4c_background_work_case(
        "r4c.read_tool_output.canonical_source_reconstruction",
    );
    let crate::lean_vocab_test::LeanR4cBackgroundWorkCase::ReadToolOutputCanonicalSourceReconstruction {
        cases, ..
    } = witness else { unreachable!() };
    assert_eq!(
        cases.len(),
        5,
        "Lean must export every canonical source case"
    );

    let acceptance = crate::lean_vocab_test::lean_contract_snapshot()
        .canonical_execution_gate_cases
        .iter()
        .find(|case| matches!(case, crate::lean_vocab_test::LeanCanonicalExecutionCase::NativeExecution { name, .. } if name == "running_foreground_terminal_recovery_records_handoff"))
        .expect("foreground acceptance case");
    let crate::lean_vocab_test::LeanCanonicalExecutionCase::NativeExecution {
        seed,
        operations,
        ..
    } = acceptance
    else {
        unreachable!()
    };

    for case in cases {
        let mut adapter = NativeCanonicalExecutionAdapter;
        let mut native = adapter.initialize(seed).await.unwrap();
        adapter
            .apply(&mut native, 600, &operations[0])
            .await
            .unwrap();
        let physical_tool = native.tool_ids.iter().find_map(|(physical, symbolic)| {
            (*symbolic == case.document).then_some(physical.clone())
        });
        if physical_tool.is_none() {
            assert!(
                case.expected_state.is_none(),
                "{} lost its physical tool",
                case.name
            );
            continue;
        }
        let physical_tool = physical_tool.expect("modeled source lacks accepted physical tool");
        for record in &case.segments {
            if !matches!(&record.coordinate.source, LeanCanonicalSource::Tool { call } if *call == case.document)
            {
                continue;
            }
            let segment = native
                .tool_output_segment(record, case.document, &physical_tool)
                .unwrap();
            let response = native
                .node
                .execute_request_with_retry(
                    defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                        .with_variables(output_segment_create_variables(&segment).unwrap()),
                    defra_node::ExecuteRetryPolicy::default(),
                )
                .await;
            assert!(
                !response.has_errors(),
                "{}: {:?}",
                case.name,
                response.errors
            );
        }
        let actual = crate::background_tools::canonical_tool_output(
            &native.node,
            &physical_tool,
            &native.request_doc_id,
            &native.session_id,
            &native.principal,
            None,
        )
        .await;
        match &case.expected_payload {
            Some(expected) => assert_eq!(
                actual.unwrap().as_bytes(),
                expected,
                "{} canonical output drifted",
                case.name
            ),
            None => assert!(actual.is_err(), "{} must fail closed", case.name),
        }
    }
}
