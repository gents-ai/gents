//! Request-owned immutable output appends, under the existing mutation gate.
//!
//! The supplied record is a prepared fact: retries retain its timestamp and
//! bytes. No caller buffer or ordinal advances inside the transaction. A caller
//! acknowledges its batch only after this owner returns a committed document ID.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents_protocol::output::reconstruction::ObservedSegment;
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SourceClose, StreamPayload,
    TranscriptMessage,
};
use gents_protocol::row::AgentRequestRow;

use crate::config_client::{ConfigAccess, ConfigApplyTxn, IdempotentTransactionRetry};
use crate::graphql::escape_graphql_string;
use crate::lifecycle::execution_policy::{authorize_output_append, LeaseObservation};
use crate::session::canonical_rows::{
    decode_output_segment_row, decode_transcript_message_row, output_segment_create_variables,
    transcript_message_create_variables, OutputSegmentRow, AGENT_MESSAGE_FIELDS,
    AGENT_OUTPUT_SEGMENT_FIELDS, CREATE_AGENT_MESSAGE_MUTATION,
    CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};

pub(crate) struct ProviderPublicationPlan {
    pub(crate) final_flush: Option<OutputSegment>,
    pub(crate) message_key: String,
    pub(crate) encoded: std::sync::Arc<super::native_encoding::EncodedNativeMessage>,
    pub(crate) expected: std::sync::Arc<gents_protocol::message::Message>,
    pub(crate) tool_deadline_at: String,
    pub(crate) spawn_admissions: Vec<super::SpawnAdmissionPlan>,
}

#[derive(Clone, Debug)]
pub(crate) struct PublishedProviderTurn {
    pub(crate) message_doc_id: String,
    pub(crate) sequence: u32,
    pub(crate) accepted_tools: Vec<super::AcceptedToolCall>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderAttemptClose {
    Retracted,
    Partial,
    AuxiliaryComplete,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProviderCloseRejection {
    #[error("provider close candidate disagrees with committed extent")]
    InvalidExtent,
    #[error("provider close lost its live lease")]
    LostLease,
    #[error("provider closure timestamp precedes committed source data")]
    RegressedTimestamp,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProviderReplayRejection {
    #[error("publication replay spawn admission route changed")]
    SpawnRouteChanged,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProviderWorkspaceRejection {
    #[error("remote spawn workspace differs from accepted parent request")]
    ParentStampChanged,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProviderAppendRejection {
    #[error("cannot append to a closed provider source")]
    ClosedSource,
    #[error("provider append lost its live processing lease")]
    LostLease,
}

pub(crate) type PartialHeaderFactory =
    std::sync::Arc<dyn Fn(&str) -> Result<TranscriptMessage> + Send + Sync>;

#[derive(Clone)]
pub(crate) struct ProviderPartialCandidate {
    pub(crate) closing: OutputSegment,
    pub(crate) header: Option<PartialHeaderFactory>,
}

pub(crate) fn partial_message_key(request_doc_id: &str, source: &OutputSource) -> Result<String> {
    Ok(format!(
        "request-recovery:{request_doc_id}:{}",
        serde_json::to_string(source)?
    ))
}

pub(crate) fn retained_partial_blocks(
    records: &[ObservedSegment<'_>],
    request_doc_id: &str,
    source: &OutputSource,
    writer: &OutputWriter,
    closing: &OutputSegmentRow,
) -> Result<Vec<MessageBlock>> {
    let Some(SourceClose::Closed {
        outcome: OutputOutcome::Partial,
        segments,
        stream_bytes,
    }) = closing.segment.close.as_ref()
    else {
        anyhow::bail!("reusable recovery closure is not Partial")
    };
    anyhow::ensure!(
        &closing.segment.writer == writer,
        "reusable recovery closure has wrong writer"
    );
    let extent = gents_protocol::output::extent::inspect_open_source(
        records,
        request_doc_id,
        source,
        writer,
    )?;
    anyhow::ensure!(
        &extent.segments == segments && &extent.stream_bytes == stream_bytes,
        "reusable recovery closure truncates or disagrees with committed extent"
    );
    if source.is_auxiliary_audit() {
        return Ok(Vec::new());
    }
    let mut retained = Vec::new();
    for (stream, reconstructed) in extent.streams.iter().enumerate() {
        if matches!(reconstructed.declaration.payload, StreamPayload::Text)
            && reconstructed.declaration.part_index == 0
        {
            retained.push((
                reconstructed.declaration.block_index,
                MessageBlock::Text {
                    text: PresentedPayload {
                        output: PayloadRef {
                            close_doc_id: closing.doc_id.clone(),
                            stream: u32::try_from(stream)?,
                        },
                        presentation: PayloadPresentation::Full,
                    },
                },
            ));
        }
    }
    retained.sort_by_key(|(block, _)| *block);
    Ok(retained.into_iter().map(|(_, block)| block).collect())
}

pub(crate) async fn close_provider_attempt(
    node: &EmbeddedNode,
    generation: &str,
    prepared: &OutputSegment,
    close: ProviderAttemptClose,
) -> Result<Option<String>> {
    close_provider_attempt_at(node, generation, prepared, close, None, Utc::now()).await
}

pub(crate) async fn close_provider_attempt_at(
    node: &EmbeddedNode,
    generation: &str,
    prepared: &OutputSegment,
    close: ProviderAttemptClose,
    candidate: Option<ProviderPartialCandidate>,
    now: DateTime<Utc>,
) -> Result<Option<String>> {
    anyhow::ensure!(
        matches!(&prepared.source, OutputSource::ProviderTurn { .. })
            && matches!(&prepared.writer, OutputWriter::RequestExecution { execution_generation }
                if execution_generation == generation),
        "provider close requires its exact request writer"
    );
    anyhow::ensure!(
        !matches!(close, ProviderAttemptClose::AuxiliaryComplete)
            || prepared.source.is_auxiliary_audit(),
        "headerless Complete closure requires an auxiliary audit source"
    );
    anyhow::ensure!(
        !prepared.source.is_auxiliary_audit()
            || candidate
                .as_ref()
                .is_none_or(|value| value.header.is_none()),
        "auxiliary audit closure cannot publish a transcript header"
    );
    ConfigAccess::transact_local_idempotent(
        node,
        None,
        IdempotentTransactionRetry::Standard,
        "streaming.close_provider_attempt",
        move |txn| {
            let candidate = candidate.clone();
            Box::pin(async move {
            let records = load_source_in_txn(txn, prepared).await?;
            let closures = records.iter().filter(|row| row.segment.close.is_some()).collect::<Vec<_>>();
            if let [existing] = closures.as_slice() {
                validate_closing_shape(prepared, &existing.segment)?;
                let matches = match (&existing.segment.close, close) {
                    (Some(SourceClose::Retracted), ProviderAttemptClose::Retracted) => true,
                    (Some(SourceClose::Closed { outcome: OutputOutcome::Partial, .. }), ProviderAttemptClose::Partial) => true,
                    (Some(SourceClose::Closed { outcome: OutputOutcome::Complete, .. }), ProviderAttemptClose::AuxiliaryComplete) => true,
                    _ => false,
                };
                anyhow::ensure!(matches, "provider close replay changed outcome");
                if let Some(candidate) = candidate.as_ref() {
                    validate_closing_shape(prepared, &candidate.closing)?;
                    anyhow::ensure!(candidate.closing == existing.segment, "partial close replay changed its closure candidate");
                }
                return partial_header_in_txn(txn, prepared, generation, existing, &records, close, candidate.as_ref().and_then(|value| value.header.as_ref()), now, true).await;
            }
            anyhow::ensure!(closures.is_empty(), "provider source has multiple closing records");
            let observations = records.iter().map(|row| ObservedSegment {
                doc_id: &row.doc_id,
                segment: &row.segment,
            }).collect::<Vec<_>>();
            let extent = gents_protocol::output::extent::inspect_open_source(
                &observations, &prepared.request_doc_id, &prepared.source, &prepared.writer,
            )?;
            if let Some(last_created_at) = extent.last_created_at.as_deref() {
                let closing = candidate.as_ref().map_or(prepared, |value| &value.closing);
                let last_created_at = DateTime::parse_from_rfc3339(last_created_at)
                    .context("provider source has an invalid committed timestamp")?;
                let closing_created_at = DateTime::parse_from_rfc3339(&closing.created_at)
                    .context("provider closure has an invalid created_at")?;
                if closing_created_at < last_created_at {
                    return Err(ProviderCloseRejection::RegressedTimestamp.into());
                }
            }
            let derived_close = match close {
                ProviderAttemptClose::Retracted => SourceClose::Retracted,
                ProviderAttemptClose::Partial | ProviderAttemptClose::AuxiliaryComplete => SourceClose::Closed {
                    outcome: if matches!(close, ProviderAttemptClose::AuxiliaryComplete) {
                        OutputOutcome::Complete
                    } else {
                        OutputOutcome::Partial
                    },
                    segments: extent.segments,
                    stream_bytes: extent.stream_bytes,
                },
            };
            let closure = candidate.as_ref().map(|value| value.closing.clone()).unwrap_or_else(|| {
                let mut closure = prepared.clone();
                closure.ordinal = None;
                closure.runs.clear();
                closure.payload.clear();
                closure.close = Some(derived_close.clone());
                closure
            });
            if closure.close.as_ref() != Some(&derived_close) {
                return Err(ProviderCloseRejection::InvalidExtent.into());
            }
            validate_closing_shape(prepared, &closure)?;
            let request = load_request_in_txn(txn, prepared).await?;
            let owner = request.execution_generation.as_deref().context("request generation missing")?;
            let state = request.lifecycle_state.context("request lifecycle missing")?;
            let expiry = request.execution_lease_expires_at.as_deref().context("request expiry missing")?;
            let deadline = DateTime::parse_from_rfc3339(expiry)?.timestamp_millis();
            if !authorize_output_append(
                LeaseObservation { request: state, generation: owner, deadline_ms: deadline },
                generation, now.timestamp_millis()) {
                return Err(ProviderCloseRejection::LostLease.into());
            }
            let response = txn.execute_with_variables(
                CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                &output_segment_create_variables(&closure)?,
            ).await?;
            let close_doc_id = created_doc_id(&response, "AgentOutputSegment")?;
            let closing = OutputSegmentRow { doc_id: close_doc_id, segment: closure };
            let request_id = escape_graphql_string(&prepared.request_doc_id);
            let escaped_generation = escape_graphql_string(generation);
            let expiry = escape_graphql_string(expiry);
            let state = escape_graphql_string(state.as_str());
            let response = txn.execute_local_response(&format!(r#"mutation {{ update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{request_id}" }}, lifecycle_state: {{ _eq: "{state}" }},
                    execution_generation: {{ _eq: "{escaped_generation}" }}, execution_lease_expires_at: {{ _eq: "{expiry}" }} }},
                input: {{ execution_generation: "{escaped_generation}" }}) {{ _docID }} }}"#)).await?;
            anyhow::ensure!(response.data.as_ref().and_then(|data| data.get("update_AgentRequest"))
                .is_some_and(crate::graphql::response_has_documents), "provider close lost request CAS");
            partial_header_in_txn(txn, prepared, generation, &closing, &records, close, candidate.as_ref().and_then(|value| value.header.as_ref()), now, false).await
        })},
    ).await
}

fn validate_closing_shape(prepared: &OutputSegment, closing: &OutputSegment) -> Result<()> {
    anyhow::ensure!(
        closing.agent_did == prepared.agent_did
            && closing.requester_did == prepared.requester_did
            && closing.session_id == prepared.session_id
            && closing.request_doc_id == prepared.request_doc_id
            && closing.source == prepared.source
            && closing.writer == prepared.writer,
        "provider close candidate changed immutable source identity"
    );
    anyhow::ensure!(
        closing.ordinal.is_none() && closing.runs.is_empty() && closing.payload.is_empty(),
        "provider close candidate must be terminal-only"
    );
    Ok(())
}

async fn partial_header_in_txn(
    txn: &ConfigApplyTxn<'_>,
    prepared: &OutputSegment,
    generation: &str,
    closing: &OutputSegmentRow,
    records: &[OutputSegmentRow],
    close: ProviderAttemptClose,
    header_factory: Option<&PartialHeaderFactory>,
    now: DateTime<Utc>,
    replay: bool,
) -> Result<Option<String>> {
    if !matches!(close, ProviderAttemptClose::Partial) || prepared.source.is_auxiliary_audit() {
        return Ok(None);
    }
    let message_key = partial_message_key(&prepared.request_doc_id, &prepared.source)?;
    let headers = crate::session::load_request_headers_in_txn(
        txn,
        &prepared.session_id,
        &prepared.agent_did,
        prepared.requester_did.as_deref(),
        &prepared.request_doc_id,
    )
    .await?;
    let matching = headers
        .iter()
        .filter(|row| row.message.message_key == message_key)
        .collect::<Vec<_>>();
    if let [existing] = matching.as_slice() {
        anyhow::ensure!(
            existing.message.request_doc_id.as_deref() == Some(prepared.request_doc_id.as_str())
                && existing.message.publication
                    == (MessagePublication::RequestRecovery {
                        execution_generation: generation.to_owned(),
                    })
                && existing.message.outcome == OutputOutcome::Partial
                && existing.message.role == MessageRole::Assistant
                && existing.message.native_id.is_none()
                && existing.message.blocks.iter().all(|block| matches!(block,
                    MessageBlock::Text { text }
                        if text.presentation == PayloadPresentation::Full
                            && text.output.close_doc_id == closing.doc_id)),
            "partial publication replay changed its exact header"
        );
        if let Some(factory) = header_factory {
            let proposed = factory(&closing.doc_id)?;
            anyhow::ensure!(
                proposed == existing.message,
                "partial publication replay changed its header candidate"
            );
        }
        crate::session::load_canonical_message_in_txn(
            txn,
            &existing.doc_id,
            &prepared.agent_did,
            prepared.requester_did.as_deref(),
        )
        .await?;
        return Ok(Some(existing.doc_id.clone()));
    }
    anyhow::ensure!(
        matching.is_empty(),
        "partial publication identity is ambiguous"
    );
    if replay {
        let Some(SourceClose::Closed { segments, .. }) = closing.segment.close.as_ref() else {
            anyhow::bail!("partial replay has no Partial closure")
        };
        let retains_text = records.iter().any(|row| {
            row.segment
                .ordinal
                .is_some_and(|ordinal| ordinal < *segments)
                && row.segment.runs.iter().any(|run| {
                    run.declaration.as_ref().is_some_and(|declaration| {
                        declaration.part_index == 0
                            && matches!(declaration.payload, StreamPayload::Text)
                    })
                })
        });
        anyhow::ensure!(
            !retains_text,
            "partial close replay is missing its committed publication header"
        );
        return Ok(None);
    }
    let observations = records
        .iter()
        .chain((!records.iter().any(|row| row.doc_id == closing.doc_id)).then_some(closing))
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    let blocks = retained_partial_blocks(
        &observations,
        &prepared.request_doc_id,
        &prepared.source,
        &prepared.writer,
        closing,
    )?;
    if blocks.is_empty() {
        return Ok(None);
    }
    let sequence = crate::lifecycle::queue::next_append_sequence_in_transaction(
        txn,
        &prepared.agent_did,
        &prepared.session_id,
    )
    .await?;
    let derived = TranscriptMessage {
        message_key,
        session_id: prepared.session_id.clone(),
        agent_did: prepared.agent_did.clone(),
        requester_did: prepared.requester_did.clone(),
        request_doc_id: Some(prepared.request_doc_id.clone()),
        publication: MessagePublication::RequestRecovery {
            execution_generation: generation.to_owned(),
        },
        outcome: OutputOutcome::Partial,
        sequence,
        role: MessageRole::Assistant,
        native_id: None,
        blocks,
        created_at: now.to_rfc3339(),
    };
    let message = if let Some(factory) = header_factory {
        let proposed = factory(&closing.doc_id)?;
        anyhow::ensure!(
            proposed == derived,
            "partial publication header candidate disagrees with canonical presentation"
        );
        proposed
    } else {
        derived
    };
    let response = txn
        .execute_with_variables(
            CREATE_AGENT_MESSAGE_MUTATION,
            &transcript_message_create_variables(&message)?,
        )
        .await?;
    let message_doc_id = created_doc_id(&response, "AgentMessage")?;
    crate::session::load_canonical_message_in_txn(
        txn,
        &message_doc_id,
        &prepared.agent_did,
        prepared.requester_did.as_deref(),
    )
    .await?;
    Ok(Some(message_doc_id))
}

use crate::graphql::created_doc_id;

/// Atomically seal and publish one accepted provider turn. Tool-bearing turns
/// are admitted by the extended path that also creates pending tool rows; this
/// entry deliberately rejects them rather than publishing executable intent
/// without its lifecycle documents.
pub(crate) async fn publish_provider_turn(
    node: &EmbeddedNode,
    generation: &str,
    plan: ProviderPublicationPlan,
) -> Result<PublishedProviderTurn> {
    publish_provider_turn_with_time(node, generation, plan, None).await
}

pub(crate) async fn publish_provider_turn_at(
    node: &EmbeddedNode,
    generation: &str,
    plan: ProviderPublicationPlan,
    now: DateTime<Utc>,
) -> Result<PublishedProviderTurn> {
    publish_provider_turn_with_time(node, generation, plan, Some(now)).await
}

async fn publish_provider_turn_with_time(
    node: &EmbeddedNode,
    generation: &str,
    plan: ProviderPublicationPlan,
    fixture_now: Option<DateTime<Utc>>,
) -> Result<PublishedProviderTurn> {
    ConfigAccess::transact_local_idempotent(
        node,
        None,
        IdempotentTransactionRetry::Standard,
        "streaming.publish_provider_turn",
        move |txn| {
            let mut final_flush = plan.final_flush.clone();
            let message_key = plan.message_key.clone();
            let encoded = std::sync::Arc::clone(&plan.encoded);
            let expected = std::sync::Arc::clone(&plan.expected);
            let tool_deadline_at = plan.tool_deadline_at.clone();
            let spawn_admissions = plan.spawn_admissions.clone();
            let fixture_now = fixture_now.clone();
            Box::pin(async move {
            let exemplar = final_flush.as_ref().context("provider publication has no output")?;
            anyhow::ensure!(
                matches!(&exemplar.source, OutputSource::ProviderTurn { .. } | OutputSource::Authored { .. })
                    && !exemplar.source.is_auxiliary_audit()
                    && matches!(&exemplar.writer, OutputWriter::RequestExecution { execution_generation }
                        if execution_generation == generation)
                    && matches!(&exemplar.close, None | Some(SourceClose::Closed { outcome: OutputOutcome::Complete, .. })),
                "provider publication does not bind its request generation"
            );
            anyhow::ensure!(matches!(exemplar.source, OutputSource::ProviderTurn { .. }) || encoded.tool_calls.is_empty(), "authored publication cannot create tool intent");
            let request = load_request_in_txn(txn, exemplar).await?;
            let parent_subagent_depth = u32::try_from(
                request
                    .subagent_depth
                    .context("accepted request omitted subagent depth")?,
            )
            .context("accepted request has invalid subagent depth")?;
            let owner = request.execution_generation.as_deref().context("request generation missing")?;
            let state = request.lifecycle_state.context("request lifecycle missing")?;
            let expiry = request.execution_lease_expires_at.as_deref().context("request expiry missing")?;
            let deadline = DateTime::parse_from_rfc3339(expiry)?.timestamp_millis();
            anyhow::ensure!(authorize_output_append(
                LeaseObservation { request: state, generation: owner, deadline_ms: deadline },
                generation, fixture_now.unwrap_or_else(Utc::now).timestamp_millis()), "provider publication lost its live lease");

            let records = load_source_in_txn(txn, exemplar).await?;
            if records.iter().any(|row| row.segment.close.is_some()) {
                if let Some(candidate) = exemplar.close.as_ref() {
                    let accepted = records
                        .iter()
                        .filter_map(|row| row.segment.close.as_ref())
                        .collect::<Vec<_>>();
                    if accepted.as_slice() != [candidate] {
                        return Err(ProviderCloseRejection::InvalidExtent.into());
                    }
                }
                return replay_publication_in_txn(
                    txn,
                    exemplar,
                    generation,
                    &message_key,
                    expected.as_ref(),
                    &spawn_admissions,
                )
                .await;
            }
            anyhow::ensure!(records.iter().all(|row| row.segment.close.is_none()), "provider source is already closed");
            let prepared = final_flush.as_mut().expect("checked");
            let provisional = OutputSegmentRow { doc_id: "<pending-close>".into(), segment: prepared.clone() };
            let mut observations = records.iter().map(|row| ObservedSegment { doc_id: &row.doc_id, segment: &row.segment }).collect::<Vec<_>>();
            if provisional.segment.ordinal.is_some() {
                observations.push(ObservedSegment { doc_id: &provisional.doc_id, segment: &provisional.segment });
            }
            let extent = gents_protocol::output::extent::inspect_open_source(
                &observations, &prepared.request_doc_id, &prepared.source, &prepared.writer)?;
            if let Some(last_created_at) = extent.last_created_at.as_deref() {
                let last_created_at = DateTime::parse_from_rfc3339(last_created_at)
                    .context("provider source has an invalid committed timestamp")?;
                let closing_created_at = DateTime::parse_from_rfc3339(&prepared.created_at)
                    .context("provider closure has an invalid created_at")?;
                if closing_created_at < last_created_at {
                    return Err(ProviderCloseRejection::RegressedTimestamp.into());
                }
            }
            let derived_close = SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: extent.segments,
                stream_bytes: extent.stream_bytes,
            };
            if prepared.close.as_ref().is_some_and(|candidate| candidate != &derived_close) {
                return Err(ProviderCloseRejection::InvalidExtent.into());
            }
            prepared.close = Some(derived_close);
            let created = txn.execute_with_variables(
                CREATE_AGENT_OUTPUT_SEGMENT_MUTATION, &output_segment_create_variables(prepared)?,
            ).await?;
            let close_doc_id = created_doc_id(&created, "AgentOutputSegment")?;
            let sequence = crate::lifecycle::queue::next_append_sequence_in_transaction(
                txn, &prepared.agent_did, &prepared.session_id,
            ).await?;
            let mut tool_doc_ids = Vec::with_capacity(encoded.tool_calls.len());
            for tool in &encoded.tool_calls {
                let admission = spawn_admissions.iter().find(|plan| plan.tool_call_id == tool.id);
                anyhow::ensure!(
                    admission.is_none() || tool.name == crate::toolset::SPAWN_SUBAGENT_TOOL_NAME,
                    "spawn admission names a non-spawn provider tool"
                );
                let arguments = PayloadRef {
                    close_doc_id: close_doc_id.clone(),
                    stream: tool.arguments_stream,
                };
                let arguments_text = encoded
                    .streams
                    .get(usize::try_from(tool.arguments_stream)?)
                    .context("encoded tool arguments stream is missing")?
                    .payload
                    .clone();
                let remote_admission = admission
                    .filter(|plan| plan.spawn_target_did != prepared.agent_did);
                if let Some(plan) = remote_admission {
                    if plan.delegated_workspace != accepted_parent_workspace(&request)? {
                        return Err(ProviderWorkspaceRejection::ParentStampChanged.into());
                    }
                }
                let delegated_input = remote_admission
                    .map(|_| gents_protocol::output::DelegatedToolInput {
                        source: arguments.clone(),
                        arguments: arguments_text,
                        parent_subagent_depth,
                    });
                let tool_call_key = format!("{}:{}:{}", prepared.request_doc_id, message_key, tool.native_index);
                let created = txn.execute_with_variables(
                    "mutation($input: AgentToolCallMutationInputArg!) { create_AgentToolCall(input: $input) { _docID } }",
                    &serde_json::json!({"input": {
                        "tool_call_key": tool_call_key,
                        "request_id": request.request_id,
                        "request_doc_id": prepared.request_doc_id,
                        "session_id": prepared.session_id,
                        "agent_did": prepared.agent_did,
                        "requester_did": prepared.requester_did,
                        "message_sequence": sequence,
                        "tool_name": tool.name,
                        "tool_call_id": tool.id,
                        "lifecycle_state": "pending",
                        "deadline_at": tool_deadline_at,
                        "await_mode": admission.map(|plan| plan.await_mode.as_str()).unwrap_or("foreground"),
                        "cancel_policy": "cascade",
                        "child_request_id": admission.map(|plan| plan.child_request_id.clone()),
                        "spawn_target_did": admission.map(|plan| plan.spawn_target_did.clone()),
                        "spawn_behavior_id": admission.map(|plan| plan.spawn_behavior_id.clone()),
                        "delegated_workspace": admission.and_then(|plan| plan.delegated_workspace.clone()),
                        "delegated_input": delegated_input
                    }}),
                ).await?;
                tool_doc_ids.push(created_doc_id(&created, "AgentToolCall")?);
            }
            let blocks = encoded.build_blocks(&close_doc_id, &tool_doc_ids)?;
            let message = TranscriptMessage {
                message_key,
                session_id: prepared.session_id.clone(),
                agent_did: prepared.agent_did.clone(),
                requester_did: prepared.requester_did.clone(),
                request_doc_id: Some(prepared.request_doc_id.clone()),
                publication: MessagePublication::RequestExecution { execution_generation: generation.to_owned() },
                outcome: OutputOutcome::Complete,
                sequence,
                role: encoded.role,
                native_id: encoded.native_id.clone(),
                blocks,
                created_at: prepared.created_at.clone(),
            };
            let created = txn.execute_with_variables(
                CREATE_AGENT_MESSAGE_MUTATION, &transcript_message_create_variables(&message)?,
            ).await?;
            let message_doc_id = created_doc_id(&created, "AgentMessage")?;
            let (_, reconstructed) = crate::session::load_canonical_message_in_txn(
                txn, &message_doc_id, &prepared.agent_did, prepared.requester_did.as_deref(),
            ).await?;
            anyhow::ensure!(
                &reconstructed == expected.as_ref(),
                "first publication native message changed"
            );

            // The matching-generation write is the ordering point against
            // cancellation/recovery. It changes no lease deadline.
            let request_id = escape_graphql_string(&prepared.request_doc_id);
            let escaped_generation = escape_graphql_string(generation);
            let expiry = escape_graphql_string(expiry);
            let state = escape_graphql_string(state.as_str());
            let response = txn.execute_local_response(&format!(r#"mutation {{ update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{request_id}" }}, lifecycle_state: {{ _eq: "{state}" }},
                    execution_generation: {{ _eq: "{escaped_generation}" }}, execution_lease_expires_at: {{ _eq: "{expiry}" }} }},
                input: {{ execution_generation: "{escaped_generation}" }}) {{ _docID }} }}"#)).await?;
            anyhow::ensure!(response.data.as_ref().and_then(|data| data.get("update_AgentRequest"))
                .is_some_and(crate::graphql::response_has_documents), "provider publication lost request CAS");
            let accepted_tools = encoded.tool_calls.iter().zip(tool_doc_ids).map(|(tool, tool_call_doc_id)| {
                super::AcceptedToolCall {
                    tool_call_doc_id,
                    request_doc_id: prepared.request_doc_id.clone(),
                    session_id: prepared.session_id.clone(),
                    accepted_header_doc_id: message_doc_id.clone(),
                    message_sequence: sequence,
                    id: tool.id.clone(),
                    call_id: tool.call_id.clone(),
                    tool_name: tool.name.clone(),
                    execution_generation: generation.to_owned(),
                    arguments: PayloadRef { close_doc_id: close_doc_id.clone(), stream: tool.arguments_stream },
                    // The coordinator dispatch object retains its private
                    // canonical argument reference. Delegated input is the
                    // persisted remote-host projection on AgentToolCall.
                    delegated_input: None,
                    spawn_admission: spawn_admissions.iter()
                        .find(|plan| plan.tool_call_id == tool.id)
                        .cloned(),
                }
            }).collect();
            Ok(PublishedProviderTurn { message_doc_id, sequence, accepted_tools })
        })},
    ).await
}

async fn replay_publication_in_txn(
    txn: &ConfigApplyTxn<'_>,
    exemplar: &OutputSegment,
    generation: &str,
    message_key: &str,
    expected: &gents_protocol::message::Message,
    spawn_admissions: &[super::SpawnAdmissionPlan],
) -> Result<PublishedProviderTurn> {
    let parent_request = load_request_in_txn(txn, exemplar).await?;
    let parent_subagent_depth = u32::try_from(
        parent_request
            .subagent_depth
            .context("accepted replay request omitted subagent depth")?,
    )
    .context("accepted replay request has invalid subagent depth")?;
    let requester = exemplar.requester_did.as_ref().map_or_else(
        || "null".to_owned(),
        |did| format!("\"{}\"", escape_graphql_string(did)),
    );
    let response = txn.execute_local_response(&format!(
        r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{}" }}, request_doc_id: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }}, requester_did: {{ _eq: {requester} }} }}) {{ {AGENT_MESSAGE_FIELDS} }} }}"#,
        escape_graphql_string(message_key),
        escape_graphql_string(&exemplar.request_doc_id),
        escape_graphql_string(&exemplar.agent_did),
    )).await?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(serde_json::Value::as_array)
        .context("publication replay omitted headers")?;
    anyhow::ensure!(
        rows.len() == 1,
        "closed provider source has no unique accepted header"
    );
    let row = decode_transcript_message_row(&rows[0])?;
    anyhow::ensure!(
        row.message.publication
            == MessagePublication::RequestExecution {
                execution_generation: generation.to_owned(),
            },
        "publication replay generation changed"
    );
    let (_, reconstructed) = crate::session::load_canonical_message_in_txn(
        txn,
        &row.doc_id,
        &exemplar.agent_did,
        exemplar.requester_did.as_deref(),
    )
    .await?;
    anyhow::ensure!(
        &reconstructed == expected,
        "publication replay native message changed"
    );
    let mut accepted_tools = Vec::new();
    for block in &row.message.blocks {
        let MessageBlock::ToolCall {
            tool_call_doc_id,
            id,
            call_id,
            name,
            arguments,
            ..
        } = block
        else {
            continue;
        };
        let response = txn.execute_local_response(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID request_doc_id session_id agent_did requester_did message_sequence tool_call_id tool_name lifecycle_state await_mode child_request_id spawn_target_did spawn_behavior_id delegated_workspace delegated_input }} }}"#,
            escape_graphql_string(tool_call_doc_id),
        )).await?;
        let tools = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentToolCall"))
            .and_then(serde_json::Value::as_array)
            .context("publication replay omitted pending tool row")?;
        anyhow::ensure!(
            tools.len() == 1,
            "publication replay has ambiguous pending tool row"
        );
        let tool = &tools[0];
        anyhow::ensure!(
            tool.get("request_doc_id")
                .and_then(serde_json::Value::as_str)
                == Some(exemplar.request_doc_id.as_str())
                && tool.get("session_id").and_then(serde_json::Value::as_str)
                    == Some(exemplar.session_id.as_str())
                && tool.get("agent_did").and_then(serde_json::Value::as_str)
                    == Some(exemplar.agent_did.as_str())
                && tool
                    .get("requester_did")
                    .and_then(serde_json::Value::as_str)
                    == exemplar.requester_did.as_deref()
                && tool
                    .get("message_sequence")
                    .and_then(serde_json::Value::as_u64)
                    == Some(u64::from(row.message.sequence))
                && tool.get("tool_call_id").and_then(serde_json::Value::as_str)
                    == Some(id.as_str())
                && tool.get("tool_name").and_then(serde_json::Value::as_str) == Some(name.as_str())
                && tool
                    .get("lifecycle_state")
                    .and_then(serde_json::Value::as_str)
                    == Some("pending"),
            "publication replay pending tool binding changed"
        );
        let persisted_admission = match (
            tool["child_request_id"].as_str(),
            tool["spawn_target_did"].as_str(),
            tool["spawn_behavior_id"].as_str(),
            serde_json::from_value::<Option<gents_protocol::output::DelegatedWorkspace>>(
                tool["delegated_workspace"].clone(),
            )
            .context("publication replay has malformed delegated workspace")?,
        ) {
            (None, None, None, None) => None,
            (
                Some(child_request_id),
                Some(spawn_target_did),
                Some(spawn_behavior_id),
                delegated_workspace,
            ) => Some(super::SpawnAdmissionPlan {
                tool_call_id: id.clone(),
                child_request_id: child_request_id.to_owned(),
                spawn_target_did: spawn_target_did.to_owned(),
                spawn_behavior_id: spawn_behavior_id.to_owned(),
                delegated_workspace,
                await_mode: crate::tool_call_lifecycle::AwaitMode::from_persisted(
                    tool["await_mode"]
                        .as_str()
                        .context("spawn admission replay lacks await mode")?,
                )
                .context("spawn admission replay has invalid await mode")?,
            }),
            _ => anyhow::bail!("publication replay has incomplete immutable spawn admission"),
        };
        let delegated_input = serde_json::from_value::<
            Option<gents_protocol::output::DelegatedToolInput>,
        >(tool["delegated_input"].clone())
        .context("publication replay has malformed delegated input")?;
        let should_delegate = persisted_admission
            .as_ref()
            .is_some_and(|plan| plan.spawn_target_did != exemplar.agent_did);
        if should_delegate {
            let source = accepted_parent_workspace(&parent_request)?;
            if !persisted_admission
                .as_ref()
                .is_some_and(|plan| plan.delegated_workspace == source)
            {
                return Err(ProviderWorkspaceRejection::ParentStampChanged.into());
            }
        }
        let expected_arguments = match expected {
            gents_protocol::message::Message::Assistant { content, .. } => content
                .iter()
                .find_map(|part| match part {
                    gents_protocol::message::AssistantContent::ToolCall(call) if call.id == *id => {
                        Some(&call.function.arguments)
                    }
                    _ => None,
                })
                .map(serde_json::to_string)
                .transpose()?,
            _ => None,
        };
        anyhow::ensure!(
            delegated_input_matches(
                should_delegate,
                delegated_input.as_ref(),
                arguments,
                expected_arguments.as_deref(),
                parent_subagent_depth,
            ),
            "publication replay delegated input changed"
        );
        // A lost acknowledgement reruns hook planning and therefore has a
        // fresh provisional UUID.  The committed accepted row is the genesis
        // authority: only target/mode may be corroborated here; its persisted
        // child identity is returned below for dispatch adoption.
        if let Some(supplied) = spawn_admissions
            .iter()
            .find(|plan| plan.tool_call_id == *id)
        {
            if !persisted_admission.as_ref().is_some_and(|persisted| {
                persisted.spawn_target_did == supplied.spawn_target_did
                    && persisted.spawn_behavior_id == supplied.spawn_behavior_id
                    && persisted.delegated_workspace == supplied.delegated_workspace
                    && persisted.await_mode == supplied.await_mode
            }) {
                return Err(ProviderReplayRejection::SpawnRouteChanged.into());
            }
        }
        accepted_tools.push(super::AcceptedToolCall {
            tool_call_doc_id: tool_call_doc_id.clone(),
            request_doc_id: exemplar.request_doc_id.clone(),
            session_id: exemplar.session_id.clone(),
            accepted_header_doc_id: row.doc_id.clone(),
            message_sequence: row.message.sequence,
            id: id.clone(),
            call_id: call_id.clone(),
            tool_name: name.clone(),
            execution_generation: generation.to_owned(),
            arguments: arguments.clone(),
            // Replay returns the coordinator-local dispatch object; the
            // validated delegated projection remains on the durable row.
            delegated_input: None,
            spawn_admission: persisted_admission,
        });
    }
    Ok(PublishedProviderTurn {
        message_doc_id: row.doc_id,
        sequence: row.message.sequence,
        accepted_tools,
    })
}

pub(super) fn delegated_input_matches(
    should_delegate: bool,
    input: Option<&gents_protocol::output::DelegatedToolInput>,
    source: &PayloadRef,
    arguments: Option<&str>,
    parent_subagent_depth: u32,
) -> bool {
    match (should_delegate, input) {
        (false, None) => true,
        (true, Some(input)) => {
            input.source == *source
                && arguments == Some(input.arguments.as_str())
                && input.parent_subagent_depth == parent_subagent_depth
        }
        (false, Some(_)) | (true, None) => false,
    }
}

/// Append exactly one provider flush. Replaying the same committed fact is a
/// read, even after expiry; it never renews or reopens its source.
pub(crate) async fn append_provider_segment(
    node: &EmbeddedNode,
    generation: &str,
    prepared: &OutputSegment,
) -> Result<String> {
    ConfigAccess::transact_local_idempotent(
        node,
        None,
        IdempotentTransactionRetry::Standard,
        "streaming.append_provider_segment",
        |txn| Box::pin(async move { append_in_txn(txn, generation, prepared).await }),
    )
    .await
}

pub(crate) async fn append_provider_segment_at(
    node: &EmbeddedNode,
    generation: &str,
    prepared: &OutputSegment,
    now: DateTime<Utc>,
) -> Result<String> {
    ConfigAccess::transact_local_idempotent(
        node,
        None,
        IdempotentTransactionRetry::Standard,
        "streaming.append_provider_segment",
        |txn| Box::pin(async move { append_in_txn_at(txn, generation, prepared, now).await }),
    )
    .await
}

pub(crate) async fn append_in_txn(
    txn: &ConfigApplyTxn<'_>,
    generation: &str,
    prepared: &OutputSegment,
) -> Result<String> {
    append_in_txn_with_time(txn, generation, prepared, None).await
}

pub(crate) async fn append_in_txn_at(
    txn: &ConfigApplyTxn<'_>,
    generation: &str,
    prepared: &OutputSegment,
    now: DateTime<Utc>,
) -> Result<String> {
    append_in_txn_with_time(txn, generation, prepared, Some(now)).await
}

async fn append_in_txn_with_time(
    txn: &ConfigApplyTxn<'_>,
    generation: &str,
    prepared: &OutputSegment,
    fixture_now: Option<DateTime<Utc>>,
) -> Result<String> {
    anyhow::ensure!(
        matches!(&prepared.source, OutputSource::ProviderTurn { .. })
            && matches!(&prepared.writer, OutputWriter::RequestExecution { execution_generation }
                if execution_generation == generation)
            && prepared.ordinal.is_some()
            && prepared.close.is_none(),
        "raw append requires an open provider flush and its exact request writer"
    );
    let request = load_request_in_txn(txn, prepared).await?;
    let mut records = load_source_in_txn(txn, prepared).await?;
    let replays = records
        .iter()
        .filter(|row| row.segment == *prepared)
        .collect::<Vec<_>>();
    if !replays.is_empty() {
        anyhow::ensure!(replays.len() == 1, "ambiguous immutable segment replay");
        return Ok(replays[0].doc_id.clone());
    }
    if records.iter().any(|row| row.segment.close.is_some()) {
        return Err(ProviderAppendRejection::ClosedSource.into());
    }
    let owner = request
        .execution_generation
        .as_deref()
        .context("request generation missing")?;
    let deadline = DateTime::parse_from_rfc3339(
        request
            .execution_lease_expires_at
            .as_deref()
            .context("request lease deadline missing")?,
    )?
    .timestamp_millis();
    if !authorize_output_append(
        LeaseObservation {
            request: request
                .lifecycle_state
                .context("request lifecycle missing")?,
            generation: owner,
            deadline_ms: deadline,
        },
        generation,
        fixture_now.unwrap_or_else(Utc::now).timestamp_millis(),
    ) {
        return Err(ProviderAppendRejection::LostLease.into());
    }

    // Obtain the actual genesis-derived ID inside the transaction. Validation
    // failure rolls this create back; no invented ID participates in validation.
    let result = txn
        .execute_with_variables(
            CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
            &output_segment_create_variables(prepared)?,
        )
        .await?;
    let doc_id = created_doc_id(&result, "AgentOutputSegment")?;
    records.push(OutputSegmentRow {
        doc_id: doc_id.clone(),
        segment: prepared.clone(),
    });
    let observations = records
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    gents_protocol::output::extent::inspect_open_source(
        &observations,
        &prepared.request_doc_id,
        &prepared.source,
        &prepared.writer,
    )?;
    Ok(doc_id)
}

/// Read the existing authorized request owner, not a parallel principal check.
async fn load_request_in_txn(
    txn: &ConfigApplyTxn<'_>,
    prepared: &OutputSegment,
) -> Result<AgentRequestRow> {
    let id = escape_graphql_string(&prepared.request_doc_id);
    let response = txn
        .execute_local_response(&format!(
            r#"{{ AgentRequest(
        filter: {{ _docID: {{ _eq: "{id}" }} }}) {{
        _docID request_id agent_did requester_did session_id lifecycle_state
        execution_generation execution_lease_expires_at subagent_depth
        workspace_id workspace_owner_agent_did workspace_authority workspace_seal_hash
    }} }}"#
        ))
        .await?;
    let mut rows: Vec<AgentRequestRow> = crate::graphql::rows(&response, "AgentRequest")?;
    anyhow::ensure!(
        rows.len() == 1,
        "provider output request is missing or ambiguous"
    );
    let row = rows.pop().expect("one request checked");
    anyhow::ensure!(
        row.doc_id.as_deref() == Some(prepared.request_doc_id.as_str())
            && row.agent_did.as_deref() == Some(prepared.agent_did.as_str())
            && row.requester_did == prepared.requester_did
            && row.session_id.as_deref() == Some(prepared.session_id.as_str()),
        "provider segment crossed its request owner or session"
    );
    Ok(row)
}

/// Reconstruct the existing signed workspace-reference shape from the exact
/// parent row read in the publication transaction. This is bridge provenance,
/// not a new workspace grant or an ACP decision.
fn accepted_parent_workspace(
    request: &AgentRequestRow,
) -> Result<Option<gents_protocol::output::DelegatedWorkspace>> {
    let lineage = crate::lifecycle::WorkspaceLineage {
        workspace_id: request.workspace_id.clone(),
        workspace_owner_agent_did: request.workspace_owner_agent_did.clone(),
        workspace_authority: request.workspace_authority.clone(),
        workspace_seal_hash: request.workspace_seal_hash.clone(),
    };
    lineage.require_authority_if_workspace_id()?;
    Ok(lineage
        .workspace_id
        .map(|workspace_id| gents_protocol::output::DelegatedWorkspace {
            workspace_id,
            workspace_owner_agent_did: lineage
                .workspace_owner_agent_did
                .expect("validated workspace owner"),
            workspace_authority: lineage
                .workspace_authority
                .expect("validated workspace authority"),
            workspace_seal_hash: lineage.workspace_seal_hash,
        }))
}

async fn load_source_in_txn(
    txn: &ConfigApplyTxn<'_>,
    prepared: &OutputSegment,
) -> Result<Vec<OutputSegmentRow>> {
    let id = escape_graphql_string(&prepared.request_doc_id);
    let scope = crate::session::session_scope_filter(
        &prepared.agent_did,
        &prepared.session_id,
        prepared.requester_did.as_deref(),
    );
    let response = txn
        .execute_local_response(&format!(
            r#"{{ AgentOutputSegment(
        filter: {{ {scope}, request_doc_id: {{ _eq: "{id}" }} }}) {{
        {AGENT_OUTPUT_SEGMENT_FIELDS}
    }} }}"#
        ))
        .await?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentOutputSegment"))
        .and_then(serde_json::Value::as_array)
        .context("source query omitted rows")?;
    rows.iter()
        .map(decode_output_segment_row)
        .collect::<Result<Vec<_>>>()
        .map(|rows| {
            rows.into_iter()
                .filter(|row| row.segment.source == prepared.source)
                .collect()
        })
}

#[cfg(test)]
mod workspace_guard_tests {
    use super::*;

    #[test]
    fn malformed_parent_provenance_is_not_a_workspace_stamp_rejection() {
        let request = AgentRequestRow {
            request_id: "injected-malformed-parent".to_owned(),
            workspace_id: Some("workspace-without-authority".to_owned()),
            ..Default::default()
        };
        let error = accepted_parent_workspace(&request)
            .expect_err("incomplete parent provenance must fail validation");
        assert!(
            error.downcast_ref::<ProviderWorkspaceRejection>().is_none(),
            "only an authenticated stamp mismatch is a modeled rejection: {error:#}"
        );
    }
}
