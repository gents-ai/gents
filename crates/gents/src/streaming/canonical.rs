//! Retries reuse prepared output facts; callers acknowledge only committed batches.

use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents_protocol::output::reconstruction::ObservedSegment;
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, ReconstructionError,
    SourceClose, StreamPayload, TranscriptMessage,
};
use gents_protocol::row::AgentRequestRow;

use crate::config_client::{ConfigAccess, ConfigApplyTxn, IdempotentTransactionRetry};
use crate::graphql::{escape_graphql_string, mutation_documents};
use crate::lifecycle::execution_policy::{authorize_output_append, LeaseObservation};
use crate::session::canonical_rows::{
    decode_transcript_message_row, output_segment_create_variables,
    transcript_message_create_variables, OutputSegmentRow, AGENT_MESSAGE_FIELDS,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};

pub(crate) struct ProviderPublicationPlan {
    pub(crate) final_flush: Option<OutputSegment>,
    pub(crate) message_key: String,
    pub(crate) encoded: std::sync::Arc<super::native_encoding::EncodedNativeMessage>,
    pub(crate) expected: std::sync::Arc<gents_protocol::message::Message>,
    pub(crate) tool_deadline_at: String,
    pub(crate) background_calls: Vec<String>,
    pub(crate) consumes_folded: Option<crate::lifecycle::queue::FoldedConsumption>,
}

#[derive(Debug, thiserror::Error)]
pub enum CanonicalStreamError {
    #[error(transparent)]
    GraphqlResponse(#[from] crate::graphql::GraphqlResponseError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Reconstruction(#[from] ReconstructionError),
    #[error(transparent)]
    StreamIndex(#[from] std::num::TryFromIntError),
    #[error("reusable recovery closure is not Partial")]
    RecoveryClosureNotPartial,
    #[error("reusable recovery closure has wrong writer")]
    RecoveryClosureWrongWriter,
    #[error("reusable recovery closure truncates or disagrees with committed extent")]
    RecoveryClosureTruncated,
    #[error("create AgentToolCall omitted response data")]
    ToolCallResponseDataMissing,
    #[error("create AgentToolCall omitted its mutation field")]
    ToolCallMutationMissing,
    #[error("create AgentToolCall returned {rows} rows for {expected} tool calls")]
    ToolCallRowCount { rows: usize, expected: usize },
    #[error("provider publication contains duplicate physical tool-call keys")]
    DuplicateToolCallKeys,
    #[error("create AgentToolCall omitted tool_call_key")]
    ToolCallKeyMissing,
    #[error("create AgentToolCall returned an unexpected tool_call_key")]
    UnexpectedToolCallKey,
    #[error("create AgentToolCall omitted its physical identity")]
    ToolCallIdentityMissing,
    #[error("create AgentToolCall returned a duplicate tool_call_key")]
    DuplicateToolCallKey,
    #[error("create AgentToolCall omitted tool_call_key {key:?}")]
    ToolCallKeyUnanswered { key: String },
    #[error("provider close candidate changed immutable source identity")]
    CloseCandidateIdentityChanged,
    #[error("provider close candidate must be terminal-only")]
    CloseCandidateNotTerminal,
    #[error("headerless Complete closure requires an auxiliary audit source")]
    HeaderlessCompleteClosureRequiresAuxiliaryAudit,
    #[error("auxiliary audit closure cannot publish a transcript header")]
    AuxiliaryAuditClosureCannotPublishTranscript,
    #[error("provider close replay changed outcome")]
    ProviderCloseReplayChangedOutcome,
    #[error("partial close replay changed its closure candidate")]
    PartialCloseReplayChangedClosureCandidate,
    #[error("provider source has multiple closing records")]
    ProviderSourceHasMultipleClosingRecords,
    #[error("partial publication replay changed its exact header")]
    PartialPublicationReplayChangedExactHeader,
    #[error("partial publication replay changed its header candidate")]
    PartialPublicationReplayChangedHeaderCandidate,
    #[error("partial publication identity is ambiguous")]
    PartialPublicationIdentityAmbiguous,
    #[error("partial replay has no Partial closure")]
    PartialReplayHasNoPartialClosure,
    #[error("partial close replay is missing its committed publication header")]
    PartialCloseReplayMissingCommittedPublication,
    #[error("partial publication header candidate disagrees with canonical presentation")]
    PartialPublicationHeaderCandidateDisagreesCanonical,
    #[error("provider publication does not bind its request generation")]
    ProviderPublicationDoesNotBindRequest,
    #[error("authored publication cannot create tool intent")]
    AuthoredPublicationCannotCreateToolIntent,
    #[error("provider publication lost its live lease")]
    ProviderPublicationLostLiveLease,
    #[error("provider source is already closed")]
    ProviderSourceAlreadyClosed,
    #[error("background publication names a tool call outside this turn")]
    BackgroundPublicationNamesToolCallOutside,
    #[error("first publication native message changed")]
    FirstPublicationNativeMessageChanged,
    #[error("request deadline expired before steering consumption")]
    RequestDeadlineExpiredBeforeSteeringConsumption,
    #[error("closed provider source has no unique accepted header")]
    ClosedProviderSourceHasNoUnique,
    #[error("publication replay generation changed")]
    PublicationReplayGenerationChanged,
    #[error("publication replay native message changed")]
    PublicationReplayNativeMessageChanged,
    #[error("publication replay has ambiguous pending tool row")]
    PublicationReplayHasAmbiguousPendingTool,
    #[error("publication replay pending tool binding changed")]
    PublicationReplayPendingToolBindingChanged,
    #[error("raw append requires an open provider flush and its exact request writer")]
    RawAppendRequiresOpenProviderFlush,
    #[error("ambiguous immutable segment replay")]
    AmbiguousImmutableSegmentReplay,
    #[error("provider output request is missing or ambiguous")]
    ProviderOutputRequestMissingAmbiguous,
    #[error("provider segment crossed its request owner or session")]
    ProviderSegmentCrossedRequestOwnerSession,
    #[error("output source does not match its request purpose")]
    OutputSourceDoesNotMatchRequest,
    #[error("provider source has an invalid committed timestamp")]
    ProviderSourceHasInvalidCommittedTimestamp(#[source] chrono::ParseError),
    #[error("provider closure has an invalid created_at")]
    ProviderClosureHasInvalidCreated(#[source] chrono::ParseError),
    #[error("request generation missing")]
    RequestGenerationMissing,
    #[error("request lifecycle missing")]
    RequestLifecycleMissing,
    #[error("request expiry missing")]
    RequestExpiryMissing,
    #[error("provider publication has no output")]
    ProviderPublicationHasNoOutput,
    #[error("publication replay omitted headers")]
    PublicationReplayOmittedHeaders,
    #[error("publication replay omitted pending tool row")]
    PublicationReplayOmittedPendingToolRow,
    #[error("publication replay lacks await mode")]
    PublicationReplayLacksAwaitMode,
    #[error("publication replay has invalid await mode")]
    PublicationReplayHasInvalidAwaitMode,
    #[error("request lease deadline missing")]
    RequestLeaseDeadlineMissing,
    #[error("output request purpose missing")]
    OutputRequestPurposeMissing,
    #[error("source query omitted rows")]
    SourceQueryOmittedRows,
    #[error(transparent)]
    Close(#[from] ProviderCloseRejection),
    #[error(transparent)]
    Replay(#[from] ProviderReplayRejection),
    #[error(transparent)]
    Append(#[from] ProviderAppendRejection),
    #[error(transparent)]
    CanonicalRows(#[from] crate::session::canonical_rows::CanonicalRowsError),
    #[error(transparent)]
    ConfigClient(#[from] crate::config_client::ConfigClientError),
    #[error(transparent)]
    SessionOutput(#[from] crate::session::SessionOutputError),
    #[error(transparent)]
    Parse(#[from] chrono::ParseError),
    #[error(transparent)]
    Queue(#[from] crate::lifecycle::QueueError),
    #[error(transparent)]
    Streaming(#[from] crate::streaming::StreamingError),
}

/// The provider rejection a failed canonical write carries, if any.
#[cfg(test)]
pub(crate) fn provider_rejection<'a, T>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a T>
where
    T: std::error::Error + 'static,
{
    std::iter::successors(Some(error), |cause| cause.source()).find_map(|cause| {
        cause.downcast_ref::<T>().or_else(|| {
            match cause.downcast_ref::<CanonicalStreamError>()? {
                CanonicalStreamError::Close(rejection) => {
                    (rejection as &dyn std::error::Error).downcast_ref::<T>()
                }
                CanonicalStreamError::Replay(rejection) => {
                    (rejection as &dyn std::error::Error).downcast_ref::<T>()
                }
                CanonicalStreamError::Append(rejection) => {
                    (rejection as &dyn std::error::Error).downcast_ref::<T>()
                }
                _ => None,
            }
        })
    })
}

type Result<T, E = CanonicalStreamError> = std::result::Result<T, E>;

impl crate::config_client::TransactionFailure for CanonicalStreamError {
    fn config_client_error(&self) -> Option<&crate::config_client::ConfigClientError> {
        match self {
            Self::GraphqlResponse(error) => {
                crate::config_client::TransactionFailure::config_client_error(error)
            }
            _ => None,
        }
    }
}

fn created_tool_call_doc_ids(
    response: &serde_json::Value,
    expected_keys: &[String],
) -> Result<Vec<String>, CanonicalStreamError> {
    let rows = mutation_documents(response, "create_AgentToolCall")?
        .ok_or(CanonicalStreamError::ToolCallResponseDataMissing)?;
    if rows.is_empty() {
        return Err(CanonicalStreamError::ToolCallMutationMissing);
    }
    if rows.len() != expected_keys.len() {
        return Err(CanonicalStreamError::ToolCallRowCount {
            rows: rows.len(),
            expected: expected_keys.len(),
        });
    }
    let expected = expected_keys
        .iter()
        .map(String::as_str)
        .collect::<rapidhash::RapidHashSet<_>>();
    if expected.len() != expected_keys.len() {
        return Err(CanonicalStreamError::DuplicateToolCallKeys);
    }
    let mut doc_ids =
        rapidhash::RapidHashMap::with_capacity_and_hasher(rows.len(), Default::default());
    for row in rows {
        let key = row
            .get("tool_call_key")
            .and_then(serde_json::Value::as_str)
            .ok_or(CanonicalStreamError::ToolCallKeyMissing)?;
        if !expected.contains(key) {
            return Err(CanonicalStreamError::UnexpectedToolCallKey);
        }
        let doc_id = row
            .get("_docID")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or(CanonicalStreamError::ToolCallIdentityMissing)?;
        if doc_ids.insert(key.to_owned(), doc_id.to_owned()).is_some() {
            return Err(CanonicalStreamError::DuplicateToolCallKey);
        }
    }
    expected_keys
        .iter()
        .map(|key| {
            doc_ids
                .remove(key)
                .ok_or_else(|| CanonicalStreamError::ToolCallKeyUnanswered { key: key.clone() })
        })
        .collect()
}

pub(crate) fn session_message_call_ids(message: &gents_protocol::message::Message) -> Vec<String> {
    match message {
        gents_protocol::message::Message::Assistant { content, .. } => content
            .iter()
            .filter_map(|part| match part {
                gents_protocol::message::AssistantContent::ToolCall(call)
                    if crate::toolset::is_session_message_tool(&call.function.name) =>
                {
                    Some(call.id.clone())
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PublishedProviderTurn {
    pub(crate) message_doc_id: String,
    #[cfg(test)]
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
pub enum ProviderCloseRejection {
    #[error("provider close requires its exact request writer")]
    WriterMismatch,
    #[error("provider close candidate disagrees with committed extent")]
    InvalidExtent,
    #[error("provider close lost its live lease")]
    LostLease,
    #[error("provider closure timestamp precedes committed source data")]
    RegressedTimestamp,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderReplayRejection {
    #[error("publication replay await mode changed")]
    AwaitModeChanged,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderAppendRejection {
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

pub(crate) fn partial_message_key(
    request_doc_id: &str,
    source: &OutputSource,
) -> Result<String, CanonicalStreamError> {
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
) -> Result<Vec<MessageBlock>, CanonicalStreamError> {
    let Some(SourceClose::Closed {
        outcome: OutputOutcome::Partial,
        segments,
        stream_bytes,
    }) = closing.segment.close.as_ref()
    else {
        return Err(CanonicalStreamError::RecoveryClosureNotPartial);
    };
    if &closing.segment.writer != writer {
        return Err(CanonicalStreamError::RecoveryClosureWrongWriter);
    }
    let extent = gents_protocol::output::extent::inspect_open_source(
        records,
        request_doc_id,
        source,
        writer,
    )?;
    if &extent.segments != segments || &extent.stream_bytes != stream_bytes {
        return Err(CanonicalStreamError::RecoveryClosureTruncated);
    }
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
    if !(!matches!(close, ProviderAttemptClose::AuxiliaryComplete)
        || prepared.source.is_auxiliary_audit())
    {
        return Err(CanonicalStreamError::HeaderlessCompleteClosureRequiresAuxiliaryAudit);
    }
    if !(!prepared.source.is_auxiliary_audit()
        || candidate
            .as_ref()
            .is_none_or(|value| value.header.is_none()))
    {
        return Err(CanonicalStreamError::AuxiliaryAuditClosureCannotPublishTranscript);
    }
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
                    if !matches {
                        return Err(CanonicalStreamError::ProviderCloseReplayChangedOutcome);
                    }
                    if let Some(candidate) = candidate.as_ref() {
                        validate_closing_shape(prepared, &candidate.closing)?;
                        if candidate.closing != existing.segment {
                            return Err(
                                CanonicalStreamError::PartialCloseReplayChangedClosureCandidate,
                            );
                        }
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
                if !closures.is_empty() {
                    return Err(CanonicalStreamError::ProviderSourceHasMultipleClosingRecords);
                }
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
                            .map_err(
                                CanonicalStreamError::ProviderSourceHasInvalidCommittedTimestamp,
                            )?;
                        let closing_created_at = DateTime::parse_from_rfc3339(&closing.created_at)
                            .map_err(CanonicalStreamError::ProviderClosureHasInvalidCreated)?;
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
                    .ok_or(CanonicalStreamError::RequestGenerationMissing)?;
                let state = request
                    .lifecycle_state
                    .ok_or(CanonicalStreamError::RequestLifecycleMissing)?;
                let expiry = request
                    .execution_lease_expires_at
                    .as_deref()
                    .ok_or(CanonicalStreamError::RequestExpiryMissing)?;
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

fn validate_closing_shape(
    prepared: &OutputSegment,
    closing: &OutputSegment,
) -> Result<(), CanonicalStreamError> {
    if !(closing.node_did == prepared.node_did
        && closing.requester_did == prepared.requester_did
        && closing.session_id == prepared.session_id
        && closing.request_doc_id == prepared.request_doc_id
        && closing.source == prepared.source
        && closing.writer == prepared.writer)
    {
        return Err(CanonicalStreamError::CloseCandidateIdentityChanged);
    }
    if !(closing.ordinal.is_none() && closing.runs.is_empty() && closing.payload.is_empty()) {
        return Err(CanonicalStreamError::CloseCandidateNotTerminal);
    }
    Ok(())
}

fn invalid_source(detail: impl Into<String>) -> ReconstructionError {
    ReconstructionError::InvalidStructure {
        detail: detail.into(),
    }
}

pub(crate) fn validate_source_physical_identities(
    records: &[OutputSegmentRow],
    request_doc_id: &str,
    source: &OutputSource,
) -> Result<(), ReconstructionError> {
    let mut physical = std::collections::BTreeMap::new();
    for record in records.iter().filter(|record| {
        record.segment.request_doc_id == request_doc_id && record.segment.source == *source
    }) {
        if let Some(previous) = physical.insert(&record.doc_id, &record.segment) {
            if previous != &record.segment {
                return Err(invalid_source(
                    "one source physical identity has conflicting facts",
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_closed_source_extent(
    records: &[OutputSegmentRow],
    request_doc_id: &str,
    source: &OutputSource,
    closing: &OutputSegmentRow,
) -> Result<(), ReconstructionError> {
    let Some(SourceClose::Closed {
        segments,
        stream_bytes,
        ..
    }) = closing.segment.close.as_ref()
    else {
        return Err(invalid_source("source closure is not Closed"));
    };
    if closing.segment.request_doc_id != request_doc_id || closing.segment.source != *source {
        return Err(invalid_source("Closed record has invalid source identity"));
    }
    let scoped = records
        .iter()
        .filter(|record| {
            record.segment.request_doc_id == request_doc_id && record.segment.source == *source
        })
        .collect::<Vec<_>>();
    let closures = scoped
        .iter()
        .filter(|record| record.segment.close.is_some())
        .collect::<Vec<_>>();
    if closures.len() != 1
        || closures[0].doc_id != closing.doc_id
        || closures[0].segment != closing.segment
    {
        return Err(invalid_source(
            "Closed source has conflicting closure facts",
        ));
    }
    let prefix = scoped
        .into_iter()
        .filter(|record| {
            record.doc_id == closing.doc_id
                || record
                    .segment
                    .ordinal
                    .is_some_and(|ordinal| ordinal < *segments)
        })
        .map(|record| ObservedSegment {
            doc_id: &record.doc_id,
            segment: &record.segment,
        })
        .collect::<Vec<_>>();
    let extent = gents_protocol::output::extent::inspect_open_source(
        &prefix,
        request_doc_id,
        source,
        &closing.segment.writer,
    )?;
    if extent.segments != *segments || extent.stream_bytes != *stream_bytes {
        return Err(invalid_source(
            "Closed source disagrees with its committed extent",
        ));
    }
    DateTime::parse_from_rfc3339(&closing.segment.created_at)
        .map_err(|_| invalid_source("Closed source has malformed closure timestamp"))?;
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
        if !(existing.message.request_doc_id.as_deref() == Some(prepared.request_doc_id.as_str())
            && existing.message.publication
                == (MessagePublication::RequestRecovery {
                    execution_generation: generation.to_owned(),
                })
            && existing.message.outcome == OutputOutcome::Partial
            && existing.message.role == MessageRole::Assistant
            && existing.message.native_id.is_none()
            && existing.message.blocks.iter().all(|block| {
                matches!(block,
                    MessageBlock::Text { text }
                        if text.presentation == PayloadPresentation::Full
                            && text.output.close_doc_id == closing.doc_id)
            }))
        {
            return Err(CanonicalStreamError::PartialPublicationReplayChangedExactHeader);
        }
        if let Some(factory) = header_factory {
            let proposed = factory(&closing.doc_id)?;
            if proposed != existing.message {
                return Err(CanonicalStreamError::PartialPublicationReplayChangedHeaderCandidate);
            }
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
    if !matching.is_empty() {
        return Err(CanonicalStreamError::PartialPublicationIdentityAmbiguous);
    }
    if replay {
        let Some(SourceClose::Closed { segments, .. }) = closing.segment.close.as_ref() else {
            return Err(CanonicalStreamError::PartialReplayHasNoPartialClosure);
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
        if retains_text {
            return Err(CanonicalStreamError::PartialCloseReplayMissingCommittedPublication);
        }
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
        if proposed != derived {
            return Err(CanonicalStreamError::PartialPublicationHeaderCandidateDisagreesCanonical);
        }
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

use crate::graphql::created_doc_id;

pub(crate) async fn publish_provider_turn(
    node: &std::sync::Arc<EmbeddedNode>,
    generation: &str,
    plan: ProviderPublicationPlan,
) -> Result<PublishedProviderTurn> {
    publish_provider_turn_with_time(node, generation, plan, None).await
}

#[cfg(test)]
pub(crate) async fn publish_provider_turn_at(
    node: &std::sync::Arc<EmbeddedNode>,
    generation: &str,
    plan: ProviderPublicationPlan,
    now: DateTime<Utc>,
) -> Result<PublishedProviderTurn> {
    publish_provider_turn_with_time(node, generation, plan, Some(now)).await
}

pub(super) async fn publish_provider_turn_with_time(
    node: &std::sync::Arc<EmbeddedNode>,
    generation: &str,
    plan: ProviderPublicationPlan,
    fixture_now: Option<DateTime<Utc>>,
) -> Result<PublishedProviderTurn> {
    let plan = std::sync::Arc::new(plan);
    ConfigAccess::transact_local_idempotent(
        node,
        None,
        IdempotentTransactionRetry::Standard,
        "streaming.publish_provider_turn",
        move |txn| {
            let plan = std::sync::Arc::clone(&plan);
            let message_key = plan.message_key.clone();
            let encoded = std::sync::Arc::clone(&plan.encoded);
            let expected = std::sync::Arc::clone(&plan.expected);
            let tool_deadline_at = plan.tool_deadline_at.clone();
            let background_calls = plan.background_calls.clone();
            Box::pin(async move {
            let exemplar = plan.final_flush.as_ref().ok_or(CanonicalStreamError::ProviderPublicationHasNoOutput)?;
            if !(matches!(&exemplar.source, OutputSource::ProviderTurn { .. } | OutputSource::Authored { .. })
                    && !exemplar.source.is_auxiliary_audit()
                    && matches!(&exemplar.writer, OutputWriter::RequestExecution { execution_generation }
                        if execution_generation == generation)
                    && matches!(&exemplar.close, None | Some(SourceClose::Closed { outcome: OutputOutcome::Complete, .. }))) {
 return Err(CanonicalStreamError::ProviderPublicationDoesNotBindRequest);
 }
            if !(matches!(exemplar.source, OutputSource::ProviderTurn { .. }) || encoded.tool_calls.is_empty()) {
 return Err(CanonicalStreamError::AuthoredPublicationCannotCreateToolIntent);
 }
            let request = load_request_in_txn(txn, exemplar).await?;
            let owner = request.execution_generation.as_deref().ok_or(CanonicalStreamError::RequestGenerationMissing)?;
            let state = request.lifecycle_state.ok_or(CanonicalStreamError::RequestLifecycleMissing)?;
            let expiry = request.execution_lease_expires_at.as_deref().ok_or(CanonicalStreamError::RequestExpiryMissing)?;
            let deadline = DateTime::parse_from_rfc3339(expiry)?.timestamp_millis();
            if !(authorize_output_append(
                LeaseObservation { request: state, generation: owner, deadline_ms: deadline },
                generation, fixture_now.unwrap_or_else(Utc::now).timestamp_millis())) {
 return Err(CanonicalStreamError::ProviderPublicationLostLiveLease);
 }

            let mut reader = crate::session::TxnCanonicalReader::new_single(
                txn, &exemplar.node_did, exemplar.requester_did.as_deref(),
            );
            let records = reader.load_source(
                &exemplar.request_doc_id, &exemplar.session_id, &exemplar.source,
            ).await?;
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
                let accepted_writer = records
                    .iter()
                    .find(|row| row.segment.close.is_some())
                    .map(|row| row.segment.writer.clone());
                let published = replay_publication_in_txn(txn, &mut reader, ReplayPublication { exemplar, generation, accepted_writer: accepted_writer.as_ref(), message_key: &message_key, expected: expected.as_ref(), background_calls: &background_calls })
                .await?;
                if let Some(consumption) = plan.consumes_folded.as_ref() {
                    crate::lifecycle::queue::ensure_folded_consumed_in_txn(txn, consumption).await?;
                }
                return Ok(published);
            }
            if !(records.iter().all(|row| row.segment.close.is_none())) {
 return Err(CanonicalStreamError::ProviderSourceAlreadyClosed);
 }

            let prepared = exemplar;
            let mut observations = records.iter().map(|row| ObservedSegment { doc_id: &row.doc_id, segment: &row.segment }).collect::<Vec<_>>();
            if prepared.ordinal.is_some() {
                observations.push(ObservedSegment { doc_id: "<pending-close>", segment: prepared });
            }
            let extent = gents_protocol::output::extent::inspect_open_source(
                &observations, &prepared.request_doc_id, &prepared.source, &prepared.writer)?;
            if let Some(last_created_at) = extent.last_created_at.as_deref() {
                let last_created_at = DateTime::parse_from_rfc3339(last_created_at)
                    .map_err(CanonicalStreamError::ProviderSourceHasInvalidCommittedTimestamp)?;
                let closing_created_at = DateTime::parse_from_rfc3339(&prepared.created_at)
                    .map_err(CanonicalStreamError::ProviderClosureHasInvalidCreated)?;
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
            let mut variables = output_segment_create_variables(prepared)?;
            variables["input"]["close"] = serde_json::to_value(&derived_close)?;
            let created = txn.execute_with_variables(
                CREATE_AGENT_OUTPUT_SEGMENT_MUTATION, &variables,
            ).await?;
            let close_doc_id = created_doc_id(&created, "AgentOutputSegment")?;
            let sequence = crate::lifecycle::queue::next_append_sequence_in_transaction(
                txn, &prepared.node_did, &prepared.session_id,
            ).await?;
            if !(background_calls
                    .iter()
                    .all(|id| encoded.tool_calls.iter().any(|tool| tool.id == *id))) {
 return Err(CanonicalStreamError::BackgroundPublicationNamesToolCallOutside);
 }
            let mut tool_call_keys = Vec::with_capacity(encoded.tool_calls.len());
            let mut tool_inputs = Vec::with_capacity(encoded.tool_calls.len());
            for tool in &encoded.tool_calls {
                let await_mode = if background_calls.contains(&tool.id) {
                    crate::tool_call_lifecycle::AwaitMode::Background
                } else {
                    crate::tool_call_lifecycle::AwaitMode::Foreground
                };
                let tool_call_key = format!("{}:{}:{}", prepared.request_doc_id, message_key, tool.native_index);
                tool_call_keys.push(tool_call_key.clone());
                tool_inputs.push(serde_json::json!({
                        "tool_call_key": tool_call_key,
                        "request_id": request.request_id,
                        "request_doc_id": prepared.request_doc_id,
                        "session_id": prepared.session_id,
                        "node_did": prepared.node_did,
                        "requester_did": prepared.requester_did,
                        "message_sequence": sequence,
                        "tool_name": tool.name,
                        "tool_call_id": tool.id,
                        "lifecycle_state": "pending",
                        "deadline_at": tool_deadline_at,
                        "await_mode": await_mode.as_str()
                    }));
            }
            let tool_doc_ids = if tool_inputs.is_empty() {
                Vec::new()
            } else {
                let created = txn.execute_with_variables(
                    "mutation($input: [AgentToolCallMutationInputArg!]!) { create_AgentToolCall(input: $input) { _docID tool_call_key } }",
                    &serde_json::json!({"input": tool_inputs}),
                ).await?;
                created_tool_call_doc_ids(&created, &tool_call_keys)?
            };
            let blocks = encoded.build_blocks(&close_doc_id, &tool_doc_ids)?;
            let message = TranscriptMessage {
                message_key,
                session_id: prepared.session_id.clone(),
                node_did: prepared.node_did.clone(),
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
            let (_, reconstructed) = reader.load_created_message(&message_doc_id, &close_doc_id).await?;
            if &reconstructed != expected.as_ref() {
 return Err(CanonicalStreamError::FirstPublicationNativeMessageChanged);
 }

            if let Some(consumption) = plan.consumes_folded.as_ref() {
                if !(crate::lifecycle::RequestLifecycle::input_publication_before_deadline(
                        fixture_now.unwrap_or_else(Utc::now),
                        Some(DateTime::parse_from_rfc3339(&tool_deadline_at)?.with_timezone(&Utc)),
                    )) {
 return Err(CanonicalStreamError::RequestDeadlineExpiredBeforeSteeringConsumption);
 }
                crate::lifecycle::queue::consume_folded_in_txn(txn, consumption, &prepared.created_at, expected.as_ref()).await?;
            }
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
                    await_mode: if background_calls.contains(&tool.id) {
                        crate::tool_call_lifecycle::AwaitMode::Background
                    } else {
                        crate::tool_call_lifecycle::AwaitMode::Foreground
                    },
                }
            }).collect();
            Ok(PublishedProviderTurn {
                message_doc_id,
                #[cfg(test)]
                sequence,
                accepted_tools,
            })
        })},
    ).await
}

struct ReplayPublication<'a> {
    exemplar: &'a OutputSegment,
    generation: &'a str,
    accepted_writer: Option<&'a OutputWriter>,
    message_key: &'a str,
    expected: &'a gents_protocol::message::Message,
    background_calls: &'a [String],
}

async fn replay_publication_in_txn(
    txn: &ConfigApplyTxn<'_>,
    reader: &mut crate::session::TxnCanonicalReader<'_, '_>,
    replay: ReplayPublication<'_>,
) -> Result<PublishedProviderTurn> {
    let ReplayPublication {
        exemplar,
        generation,
        accepted_writer,
        message_key,
        expected,
        background_calls,
    } = replay;
    let requester = exemplar.requester_did.as_ref().map_or_else(
        || "null".to_owned(),
        |did| format!("\"{}\"", escape_graphql_string(did)),
    );
    let response = txn.execute_local_response(&format!(
        r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{}" }}, request_doc_id: {{ _eq: "{}" }}, node_did: {{ _eq: "{}" }}, requester_did: {{ _eq: {requester} }} }}) {{ {AGENT_MESSAGE_FIELDS} }} }}"#,
        escape_graphql_string(message_key),
        escape_graphql_string(&exemplar.request_doc_id),
        escape_graphql_string(&exemplar.node_did),
    )).await?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(serde_json::Value::as_array)
        .ok_or(CanonicalStreamError::PublicationReplayOmittedHeaders)?;
    if rows.len() != 1 {
        return Err(CanonicalStreamError::ClosedProviderSourceHasNoUnique);
    }
    let row = decode_transcript_message_row(&rows[0])?;
    let MessagePublication::RequestExecution {
        execution_generation: accepted_generation,
    } = &row.message.publication
    else {
        return Err(CanonicalStreamError::PublicationReplayGenerationChanged);
    };
    if !(accepted_generation == generation
        || (matches!(exemplar.source, OutputSource::Authored { .. })
            && accepted_writer
                == Some(&OutputWriter::RequestExecution {
                    execution_generation: accepted_generation.clone(),
                })))
    {
        return Err(CanonicalStreamError::PublicationReplayGenerationChanged);
    }
    reader.observe_headers(std::slice::from_ref(&row));
    let (_, reconstructed) = reader.load_message(&row.doc_id).await?;
    if &reconstructed != expected {
        return Err(CanonicalStreamError::PublicationReplayNativeMessageChanged);
    }
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
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID request_doc_id session_id node_did requester_did message_sequence tool_call_id tool_name lifecycle_state await_mode }} }}"#,
            escape_graphql_string(tool_call_doc_id),
        )).await?;
        let tools = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentToolCall"))
            .and_then(serde_json::Value::as_array)
            .ok_or(CanonicalStreamError::PublicationReplayOmittedPendingToolRow)?;
        if tools.len() != 1 {
            return Err(CanonicalStreamError::PublicationReplayHasAmbiguousPendingTool);
        }
        let tool = &tools[0];
        if !(tool
            .get("request_doc_id")
            .and_then(serde_json::Value::as_str)
            == Some(exemplar.request_doc_id.as_str())
            && tool.get("session_id").and_then(serde_json::Value::as_str)
                == Some(exemplar.session_id.as_str())
            && tool.get("node_did").and_then(serde_json::Value::as_str)
                == Some(exemplar.node_did.as_str())
            && tool
                .get("requester_did")
                .and_then(serde_json::Value::as_str)
                == exemplar.requester_did.as_deref()
            && tool
                .get("message_sequence")
                .and_then(serde_json::Value::as_u64)
                == Some(u64::from(row.message.sequence))
            && tool.get("tool_call_id").and_then(serde_json::Value::as_str) == Some(id.as_str())
            && tool.get("tool_name").and_then(serde_json::Value::as_str) == Some(name.as_str())
            && tool
                .get("lifecycle_state")
                .and_then(serde_json::Value::as_str)
                == Some("pending"))
        {
            return Err(CanonicalStreamError::PublicationReplayPendingToolBindingChanged);
        }
        let await_mode = crate::tool_call_lifecycle::AwaitMode::from_persisted(
            tool["await_mode"]
                .as_str()
                .ok_or(CanonicalStreamError::PublicationReplayLacksAwaitMode)?,
        )
        .ok_or(CanonicalStreamError::PublicationReplayHasInvalidAwaitMode)?;
        let planned = if background_calls.contains(id) {
            crate::tool_call_lifecycle::AwaitMode::Background
        } else {
            crate::tool_call_lifecycle::AwaitMode::Foreground
        };
        if await_mode != planned {
            return Err(ProviderReplayRejection::AwaitModeChanged.into());
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
            await_mode,
        });
    }
    Ok(PublishedProviderTurn {
        message_doc_id: row.doc_id,
        #[cfg(test)]
        sequence: row.message.sequence,
        accepted_tools,
    })
}

pub(crate) async fn append_provider_segment(
    node: &std::sync::Arc<EmbeddedNode>,
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

#[cfg(test)]
pub(crate) async fn append_provider_segment_at(
    node: &std::sync::Arc<EmbeddedNode>,
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

#[cfg(test)]
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
    if !(matches!(&prepared.source, OutputSource::ProviderTurn { .. })
        && matches!(&prepared.writer, OutputWriter::RequestExecution { execution_generation }
                if execution_generation == generation)
        && prepared.ordinal.is_some()
        && prepared.close.is_none())
    {
        return Err(CanonicalStreamError::RawAppendRequiresOpenProviderFlush);
    }
    let request = load_request_in_txn(txn, prepared).await?;
    let mut records = load_source_in_txn(txn, prepared).await?;
    let replays = records
        .iter()
        .filter(|row| row.segment == *prepared)
        .collect::<Vec<_>>();
    if !replays.is_empty() {
        if replays.len() != 1 {
            return Err(CanonicalStreamError::AmbiguousImmutableSegmentReplay);
        }
        return Ok(replays[0].doc_id.clone());
    }
    if records.iter().any(|row| row.segment.close.is_some()) {
        return Err(ProviderAppendRejection::ClosedSource.into());
    }
    let owner = request
        .execution_generation
        .as_deref()
        .ok_or(CanonicalStreamError::RequestGenerationMissing)?;
    let deadline = DateTime::parse_from_rfc3339(
        request
            .execution_lease_expires_at
            .as_deref()
            .ok_or(CanonicalStreamError::RequestLeaseDeadlineMissing)?,
    )?
    .timestamp_millis();
    if !authorize_output_append(
        LeaseObservation {
            request: request
                .lifecycle_state
                .ok_or(CanonicalStreamError::RequestLifecycleMissing)?,
            generation: owner,
            deadline_ms: deadline,
        },
        generation,
        fixture_now.unwrap_or_else(Utc::now).timestamp_millis(),
    ) {
        return Err(ProviderAppendRejection::LostLease.into());
    }

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

/// Publication's fence against recovery/cancellation: native OCC validates
/// point-read keys at commit (RequestExecutionLease/ObservedTransaction.lean).
/// Keep this an in-transaction `_docID` point read; do not add a same-value
/// request write.
async fn load_request_in_txn(
    txn: &ConfigApplyTxn<'_>,
    prepared: &OutputSegment,
) -> Result<AgentRequestRow> {
    let id = escape_graphql_string(&prepared.request_doc_id);
    let response = txn
        .execute_local_response(&format!(
            r#"{{ AgentRequest(
        filter: {{ _docID: {{ _eq: "{id}" }} }}) {{
        _docID request_id purpose node_did requester_did session_id lifecycle_state
        execution_generation execution_lease_expires_at request_hop
        workspace_id workspace_owner_node_did workspace_authority workspace_seal_hash
    }} }}"#
        ))
        .await?;
    let mut rows: Vec<AgentRequestRow> = crate::graphql::rows(&response, "AgentRequest")?;
    if rows.len() != 1 {
        return Err(CanonicalStreamError::ProviderOutputRequestMissingAmbiguous);
    }
    let row = rows.pop().expect("one request checked");
    if !(row.doc_id.as_deref() == Some(prepared.request_doc_id.as_str())
        && row.node_did.as_deref() == Some(prepared.node_did.as_str())
        && row.requester_did == prepared.requester_did
        && row.session_id.as_deref() == Some(prepared.session_id.as_str()))
    {
        return Err(CanonicalStreamError::ProviderSegmentCrossedRequestOwnerSession);
    }
    validate_source_purpose(
        row.purpose
            .ok_or(CanonicalStreamError::OutputRequestPurposeMissing)?,
        &prepared.source,
    )?;
    Ok(row)
}

pub(crate) fn validate_source_purpose(
    purpose: gents_protocol::request_admission::RequestPurpose,
    source: &OutputSource,
) -> Result<()> {
    let expected = match source {
        OutputSource::ProviderTurn { scope, .. } => scope.kind.request_purpose(),
        _ => gents_protocol::request_admission::RequestPurpose::Normal,
    };
    if purpose != expected {
        return Err(CanonicalStreamError::OutputSourceDoesNotMatchRequest);
    }
    Ok(())
}

async fn load_source_in_txn(
    txn: &ConfigApplyTxn<'_>,
    prepared: &OutputSegment,
) -> Result<Vec<OutputSegmentRow>> {
    let response = txn
        .execute_local_response(
            &crate::session::canonical_rows::source_output_segments_query(
                &prepared.request_doc_id,
                &prepared.source,
            )?,
        )
        .await?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentOutputSegment"))
        .and_then(serde_json::Value::as_array)
        .ok_or(CanonicalStreamError::SourceQueryOmittedRows)?;
    crate::session::canonical_rows::decode_scoped_request_output_segments(
        rows,
        &prepared.node_did,
        Some(&prepared.session_id),
        prepared.requester_did.as_deref(),
    )
    .map(|rows| {
        rows.into_iter()
            .filter(|row| row.segment.source == prepared.source)
            .collect()
    })
    .map_err(Into::into)
}

#[cfg(test)]
mod tool_call_batch_tests {
    use super::*;

    #[test]
    fn batch_create_rows_bind_to_tool_keys_not_response_order() {
        let response = serde_json::json!({
            "data": {
                "create_AgentToolCall": [
                    {"_docID": "doc-b", "tool_call_key": "key-b"},
                    {"_docID": "doc-a", "tool_call_key": "key-a"}
                ]
            }
        });
        let keys = vec!["key-a".to_owned(), "key-b".to_owned()];

        assert_eq!(
            created_tool_call_doc_ids(&response, &keys).unwrap(),
            vec!["doc-a".to_owned(), "doc-b".to_owned()]
        );
    }

    #[test]
    fn batch_create_rejects_missing_or_duplicate_tool_keys() {
        let keys = vec!["key-a".to_owned(), "key-b".to_owned()];
        let missing = serde_json::json!({
            "data": {
                "create_AgentToolCall": [
                    {"_docID": "doc-a", "tool_call_key": "key-a"}
                ]
            }
        });
        assert!(created_tool_call_doc_ids(&missing, &keys).is_err());

        let duplicate = serde_json::json!({
            "data": {
                "create_AgentToolCall": [
                    {"_docID": "doc-a", "tool_call_key": "key-a"},
                    {"_docID": "doc-b", "tool_call_key": "key-a"}
                ]
            }
        });
        assert!(created_tool_call_doc_ids(&duplicate, &keys).is_err());
    }
}
