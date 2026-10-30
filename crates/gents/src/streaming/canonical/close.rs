use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents_protocol::output::reconstruction::ObservedSegment;
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadPresentation, ReconstructionError, SourceClose, StreamPayload,
    TranscriptMessage,
};

use crate::config_client::{ConfigAccess, ConfigApplyTxn, IdempotentTransactionRetry};
use crate::graphql::created_doc_id;
use crate::lifecycle::execution_policy::{authorize_output_append, LeaseObservation};
use crate::session::canonical_rows::{
    output_segment_create_variables, transcript_message_create_variables, OutputSegmentRow,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};

use super::{
    partial_message_key,
    source::{
        load_request_in_txn, load_source_in_txn, retained_partial_blocks,
        validate_closed_source_extent, validate_source_physical_identities,
    },
    PartialHeaderFactory, ProviderAttemptClose, ProviderCloseRejection, ProviderPartialCandidate,
};

pub(crate) async fn close_provider_attempt(
    node: &std::sync::Arc<EmbeddedNode>,
    generation: &str,
    prepared: &OutputSegment,
    close: ProviderAttemptClose,
) -> Result<Option<String>> {
    close_provider_attempt_at(node, generation, prepared, close, None, Utc::now()).await
}

/// Fenced to the writer generation, except a changed generation may acknowledge
/// an already committed, exact auxiliary closure without authorizing another write.
pub(crate) async fn close_provider_attempt_at(
    node: &std::sync::Arc<EmbeddedNode>,
    generation: &str,
    prepared: &OutputSegment,
    close: ProviderAttemptClose,
    candidate: Option<ProviderPartialCandidate>,
    now: DateTime<Utc>,
) -> Result<Option<String>> {
    let writer_matches_generation = matches!(
        &prepared.writer,
        OutputWriter::RequestExecution { execution_generation }
            if execution_generation == generation
    );
    if !matches!(&prepared.source, OutputSource::ProviderTurn { .. })
        || !matches!(&prepared.writer, OutputWriter::RequestExecution { .. })
        || (!writer_matches_generation && !prepared.source.is_auxiliary_audit())
    {
        return Err(ProviderCloseRejection::WriterMismatch.into());
    }
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
                let request = load_request_in_txn(txn, prepared).await?;
                let records = load_source_in_txn(txn, prepared).await?;
                let closures = records
                    .iter()
                    .filter(|row| row.segment.close.is_some())
                    .collect::<Vec<_>>();
                if let [existing] = closures.as_slice() {
                    if existing.segment.writer != prepared.writer {
                        return Err(ProviderCloseRejection::WriterMismatch.into());
                    }
                    validate_closing_shape(prepared, &existing.segment)?;
                    if !writer_matches_generation {
                        let proposed = candidate.as_ref().map_or(prepared, |value| &value.closing);
                        if proposed != &existing.segment {
                            return Err(ProviderCloseRejection::InvalidExtent.into());
                        }
                        validate_closed_source_extent(
                            &records,
                            &prepared.request_doc_id,
                            &prepared.source,
                            existing,
                        )?;
                    }
                    let matches = matches!(
                        (&existing.segment.close, close),
                        (
                            Some(SourceClose::Retracted),
                            ProviderAttemptClose::Retracted
                        ) | (
                            Some(SourceClose::Closed {
                                outcome: OutputOutcome::Partial,
                                ..
                            }),
                            ProviderAttemptClose::Partial,
                        ) | (
                            Some(SourceClose::Closed {
                                outcome: OutputOutcome::Complete,
                                ..
                            }),
                            ProviderAttemptClose::AuxiliaryComplete,
                        )
                    );
                    anyhow::ensure!(matches, "provider close replay changed outcome");
                    if let Some(candidate) = candidate.as_ref() {
                        validate_closing_shape(prepared, &candidate.closing)?;
                        anyhow::ensure!(
                            candidate.closing == existing.segment,
                            "partial close replay changed its closure candidate"
                        );
                    }
                    if request.purpose.is_some_and(
                        gents_protocol::request_admission::RequestPurpose::is_auxiliary,
                    ) {
                        match existing.segment.close.as_ref() {
                            Some(SourceClose::Closed { .. }) => validate_closed_source_extent(
                                &records,
                                &prepared.request_doc_id,
                                &prepared.source,
                                existing,
                            )?,
                            Some(SourceClose::Retracted) => validate_source_physical_identities(
                                &records,
                                &prepared.request_doc_id,
                                &prepared.source,
                            )?,
                            None => unreachable!("closure row selected"),
                        }
                    }
                    return partial_header_in_txn(
                        txn,
                        PartialHeaderInputs {
                            prepared,
                            generation,
                            closing: existing,
                            records: &records,
                            close,
                            header_factory: candidate
                                .as_ref()
                                .and_then(|value| value.header.as_ref()),
                            now,
                            replay: true,
                        },
                    )
                    .await;
                }
                if request
                    .purpose
                    .is_some_and(gents_protocol::request_admission::RequestPurpose::is_auxiliary)
                    && closures.len() > 1
                {
                    return Err(ReconstructionError::ConflictingClosures {
                        request_doc_id: prepared.request_doc_id.clone(),
                        source: prepared.source.clone(),
                    }
                    .into());
                }
                anyhow::ensure!(
                    closures.is_empty(),
                    "provider source has multiple closing records"
                );
                if !writer_matches_generation {
                    return Err(ProviderCloseRejection::WriterMismatch.into());
                }
                let observations = records
                    .iter()
                    .map(|row| ObservedSegment {
                        doc_id: &row.doc_id,
                        segment: &row.segment,
                    })
                    .collect::<Vec<_>>();
                let extent = gents_protocol::output::extent::inspect_open_source(
                    &observations,
                    &prepared.request_doc_id,
                    &prepared.source,
                    &prepared.writer,
                )?;
                if !matches!(close, ProviderAttemptClose::Retracted) {
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
                }
                let derived_close = match close {
                    ProviderAttemptClose::Retracted => SourceClose::Retracted,
                    ProviderAttemptClose::Partial | ProviderAttemptClose::AuxiliaryComplete => {
                        SourceClose::Closed {
                            outcome: if matches!(close, ProviderAttemptClose::AuxiliaryComplete) {
                                OutputOutcome::Complete
                            } else {
                                OutputOutcome::Partial
                            },
                            segments: extent.segments,
                            stream_bytes: extent.stream_bytes,
                        }
                    }
                };
                let closure = candidate
                    .as_ref()
                    .map(|value| value.closing.clone())
                    .unwrap_or_else(|| {
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
                let owner = request
                    .execution_generation
                    .as_deref()
                    .context("request generation missing")?;
                let state = request
                    .lifecycle_state
                    .context("request lifecycle missing")?;
                let expiry = request
                    .execution_lease_expires_at
                    .as_deref()
                    .context("request expiry missing")?;
                let deadline = DateTime::parse_from_rfc3339(expiry)?.timestamp_millis();
                if !authorize_output_append(
                    LeaseObservation {
                        request: state,
                        generation: owner,
                        deadline_ms: deadline,
                    },
                    generation,
                    now.timestamp_millis(),
                ) {
                    return Err(ProviderCloseRejection::LostLease.into());
                }
                let response = txn
                    .execute_with_variables(
                        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                        &output_segment_create_variables(&closure)?,
                    )
                    .await?;
                let close_doc_id = created_doc_id(&response, "AgentOutputSegment")?;
                let closing = OutputSegmentRow {
                    doc_id: close_doc_id,
                    segment: closure,
                };
                partial_header_in_txn(
                    txn,
                    PartialHeaderInputs {
                        prepared,
                        generation,
                        closing: &closing,
                        records: &records,
                        close,
                        header_factory: candidate.as_ref().and_then(|value| value.header.as_ref()),
                        now,
                        replay: false,
                    },
                )
                .await
            })
        },
    )
    .await
}

fn validate_closing_shape(prepared: &OutputSegment, closing: &OutputSegment) -> Result<()> {
    anyhow::ensure!(
        closing.node_did == prepared.node_did
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

struct PartialHeaderInputs<'a> {
    prepared: &'a OutputSegment,
    generation: &'a str,
    closing: &'a OutputSegmentRow,
    records: &'a [OutputSegmentRow],
    close: ProviderAttemptClose,
    header_factory: Option<&'a PartialHeaderFactory>,
    now: DateTime<Utc>,
    replay: bool,
}

async fn partial_header_in_txn(
    txn: &ConfigApplyTxn<'_>,
    inputs: PartialHeaderInputs<'_>,
) -> Result<Option<String>> {
    let PartialHeaderInputs {
        prepared,
        generation,
        closing,
        records,
        close,
        header_factory,
        now,
        replay,
    } = inputs;
    if !matches!(close, ProviderAttemptClose::Partial) || prepared.source.is_auxiliary_audit() {
        return Ok(None);
    }
    let message_key = partial_message_key(&prepared.request_doc_id, &prepared.source)?;
    let headers = crate::session::load_request_headers_in_txn(
        txn,
        &prepared.session_id,
        &prepared.node_did,
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
            &prepared.node_did,
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
        &prepared.node_did,
        &prepared.session_id,
    )
    .await?;
    let derived = TranscriptMessage {
        message_key,
        session_id: prepared.session_id.clone(),
        node_did: prepared.node_did.clone(),
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
        &prepared.node_did,
        prepared.requester_did.as_deref(),
    )
    .await?;
    Ok(Some(message_doc_id))
}
