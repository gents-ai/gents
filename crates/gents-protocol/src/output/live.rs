//! Tolerant dense-prefix reconstruction for live views, refining
//! `Proofs/StreamingResponse` (`reconstructPrefix`/`contiguousFlushes`,
//! `PrefixGrowth`, `Executable`) and `Proofs/CanonicalOutput/Reconstruction`
//! (`flushAt`, `consumeRuns`).
//!
//! This module also owns the **full live eligibility classifier** — closure
//! observation, owner liveness, denial, retraction, terminal selection,
//! retained Partial diagnostics, and the `LiveView` projection — as the exact
//! Rust projection of Lean's `StreamingResponse.project`. Nothing outside
//! this module decides whether a source may be shown live; the prefix
//! primitive reconstructs the exact contiguous prefix and the evidence the
//! classifier consumes.
//!
//! Behavior, exactly as the Lean owners model it:
//!
//! - Selects the longest valid contiguous ordinal prefix of one
//!   `(request_doc_id, source)` scope, stopping at the first missing ordinal.
//!   A later ordinal arriving before an intermediate one never erases the
//!   earlier preview; benign arrivals only extend it.
//! - Rejects visible coordinate twins (distinct records at one ordinal) and
//!   physical twins (one immutable identity carrying conflicting facts)
//!   instead of choosing a winner. This does not weaken the closed
//!   reconstruction: `reconstruction::reconstruct_stream` keeps its strict
//!   conflict handling, and this primitive reuses its stream type rather than
//!   reimplementing closed reconstruction.
//! - Consumes runs with the strict native shape: runs partition the payload
//!   at UTF-8 boundaries, streams open densely from zero, every native block
//!   field position is declared exactly once, and a zero-byte continuation is not a
//!   flush.
//! - Never reads a closing record for content. Closure observation, extent
//!   accounting and the sealed path stay with `reconstruction` and `extent`.
//!
//! Evidence preserved for the later classifier: every reconstructed stream
//! keeps its declaration (including hidden reasoning fields, which only the
//! rendering step drops, as Lean's `visibleStreams` does);
//! `highest_visible_ordinal` exposes bytes beyond the contiguous prefix.
//!
//! The bounded form (`limit`) mirrors Lean's `reconstructPrefix (some count)`
//! used for previews bounded by a known closure: flushes at or beyond the
//! bound are excluded before any check, so a superseded writer's malformed
//! late record stays inert for the bounded preview while remaining preserved
//! evidence for the unbounded one.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, FixedOffset};

use super::origin::{self, ObservedMessage, OriginError};
use super::reconstruction::{self, DependencyDenial, ObservedSegment, ReconstructedStream};
use super::{
    LiveStream, LiveStreamState, MessagePublication, MessageRole, OutputOutcome, OutputSource,
    OutputWriter, PayloadRef, ReconstructionError, SourceClose, StreamPayload, TerminalOutput,
    TranscriptMessage,
};
use crate::message::Message;

/// Why no prefix is returned. This mirrors Lean's `OpenError` exactly; the
/// later classifier maps these onto its own views.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DensePrefixError {
    /// No contiguous prefix is reconstructable yet: the scope has no visible
    /// records, its first ordinal is missing, or every visible record carries
    /// closure only (Lean `OpenError.loading`). Expected while replication is
    /// behind; never a shortened answer.
    Loading,
    /// Visible coordinate twins at an ordinal disagree. No winner is selected
    /// (Lean `OpenError.conflicted`).
    Conflicted { ordinal: u32 },
    /// Malformed native shape, writer disagreement, non-monotonic timestamps,
    /// or a physical identity carrying conflicting facts (Lean
    /// `OpenError.invalid`).
    Invalid { detail: String },
}

impl std::fmt::Display for DensePrefixError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Loading => f.write_str("no contiguous output prefix is visible yet"),
            Self::Conflicted { ordinal } => {
                write!(f, "conflicting output records at ordinal {ordinal}")
            }
            Self::Invalid { detail } => write!(f, "invalid output prefix: {detail}"),
        }
    }
}

impl std::error::Error for DensePrefixError {}

/// The exact contiguous prefix of one source, plus the observation evidence
/// the later live classifier needs.
#[derive(Clone, Debug, PartialEq)]
pub struct DensePrefix {
    /// Contiguous ordinals `0..segments` consumed from the source's visible
    /// flushes. The walk stopped here at the first missing ordinal.
    pub segments: u32,
    /// Streams assembled from those ordinals, in declaration order. Includes
    /// opaque declarations; only the later classifier's rendering step drops
    /// them, as Lean's `visibleStreams` does.
    pub streams: Vec<ReconstructedStream>,
    /// Largest visible flush ordinal in scope, even when it lies beyond the
    /// contiguous prefix. `None` when the scope has no visible flushes.
    pub highest_visible_ordinal: Option<u32>,
}

/// Same predicate as Lean's `writerMatchesSource` and `extent::writer_matches`:
/// provider output is request-owned, tool output is owned by that exact tool
/// call, and authored output may use either existing owner.
/// `pub(crate)` so the live classifier in this module and other projection
/// owners reuse the one shared predicate instead of reimplementing it.
pub(crate) fn writer_matches_source(source: &OutputSource, writer: &OutputWriter) -> bool {
    match (source, writer) {
        (OutputSource::ProviderTurn { .. }, OutputWriter::RequestExecution { .. }) => true,
        (
            OutputSource::ToolCall {
                tool_call_doc_id: a,
            },
            OutputWriter::ToolExecution {
                tool_call_doc_id: b,
            },
        ) => a == b,
        (OutputSource::Authored { .. }, _) => true,
        _ => false,
    }
}

fn invalid(detail: impl std::fmt::Display) -> DensePrefixError {
    DensePrefixError::Invalid {
        detail: detail.to_string(),
    }
}

/// Reconstruct the tolerant dense prefix of one source scope.
///
/// `records` must be the authorized observation supplied by the caller;
/// authorization/denial classification stays with the later owner. The
/// `expected_writer` must bind the `source`, and every visible record in
/// scope must name it, so a superseded generation can never contribute bytes.
pub fn reconstruct_dense_prefix(
    records: &[ObservedSegment<'_>],
    request_doc_id: &str,
    source: &OutputSource,
    expected_writer: &OutputWriter,
    limit: Option<u32>,
) -> Result<DensePrefix, DensePrefixError> {
    if !writer_matches_source(source, expected_writer) {
        return Err(invalid("prefix writer does not bind its source"));
    }
    let scoped = records.iter().copied().filter(|record| {
        record.segment.request_doc_id == request_doc_id && record.segment.source == *source
    });
    let data: Vec<ObservedSegment<'_>> = match limit {
        None => scoped.collect(),
        // Bounded form: select flushes inside the closed extent before any
        // check, so records beyond the committed extent stay inert for the
        // bounded preview. Terminal-only records have no flush and drop out,
        // exactly as Lean's filter on `record.flush.any` does.
        Some(count) => scoped
            .filter(|record| {
                record
                    .segment
                    .ordinal
                    .is_some_and(|ordinal| ordinal < count)
            })
            .collect(),
    };
    if data.is_empty() {
        return Err(DensePrefixError::Loading);
    }
    if data
        .iter()
        .any(|record| record.segment.writer != *expected_writer)
    {
        return Err(invalid("prefix records disagree on writer"));
    }

    // Lean's `timestampsNondecreasing`: ordinal order implies timestamp order,
    // and equal ordinals (visible twins) must carry equal timestamps.
    //
    // O(n log n): sort by ordinal, then check adjacent pairs. Equal ordinals
    // must imply equal timestamps; strictly increasing ordinals must never
    // regress timestamps. Classification order is preserved: this runs before
    // the coordinate-twin (`Conflicted`) check below, so timestamp-disagreeing
    // twins are `Invalid`, not `Conflicted`.
    let mut stamped: Vec<(u32, DateTime<FixedOffset>)> = Vec::new();
    for record in &data {
        if let Some(ordinal) = record.segment.ordinal {
            let time = DateTime::parse_from_rfc3339(&record.segment.created_at)
                .map_err(|_| invalid("prefix flush has malformed timestamp"))?;
            stamped.push((ordinal, time));
        }
    }
    stamped.sort_by_key(|(ordinal, _)| *ordinal);
    for pair in stamped.windows(2) {
        let (left_ordinal, left_time) = &pair[0];
        let (right_ordinal, right_time) = &pair[1];
        let nondecreasing = if left_ordinal == right_ordinal {
            // Equal ordinals must carry equal timestamps: any disagreement
            // between visible twins is invalid, not a winner to select.
            left_time == right_time
        } else {
            left_time <= right_time
        };
        if !nondecreasing {
            return Err(invalid("prefix flush timestamps decrease"));
        }
    }

    // One immutable identity cannot carry conflicting facts. Exact replay of
    // the same fact is idempotent; anything else is rejected, not chosen.
    let mut identities: BTreeMap<&str, ObservedSegment<'_>> = BTreeMap::new();
    let mut slots: BTreeMap<u32, Vec<ObservedSegment<'_>>> = BTreeMap::new();
    for record in &data {
        if let Some(existing) = identities.insert(record.doc_id, *record) {
            if existing != *record {
                return Err(invalid(format!(
                    "physical segment identity {} has conflicting facts",
                    record.doc_id
                )));
            }
        }
        if let Some(ordinal) = record.segment.ordinal {
            slots.entry(ordinal).or_default().push(*record);
        }
    }
    // Lean's `Nodup` check: any duplicate ordinal anywhere in the visible data
    // is a conflict, before any bytes are consumed.
    for (ordinal, slot) in &slots {
        let first = &slot[0];
        if slot.iter().any(|other| other != first) {
            return Err(DensePrefixError::Conflicted { ordinal: *ordinal });
        }
    }

    // `contiguousFlushes`: walk from ordinal zero and stop at the first
    // missing ordinal. `consumeRuns`' strict shape applies only to the
    // ordinals actually consumed, so malformed data beyond a gap is not
    // reached (and is inert for a bounded preview that excludes it).
    let mut streams: Vec<ReconstructedStream> = Vec::new();
    let mut positions: BTreeSet<(u32, u32, bool)> = BTreeSet::new();
    let mut consumed: u32 = 0;
    for (ordinal, slot) in &slots {
        if *ordinal != consumed {
            break;
        }
        let record = slot[0];
        if record.segment.runs.is_empty() {
            return Err(invalid("prefix flush has no runs"));
        }
        let mut offset = 0usize;
        for run in &record.segment.runs {
            if run.bytes == 0 && run.declaration.is_none() {
                return Err(invalid("zero-byte continuation is not a flush"));
            }
            let Some(end) = offset.checked_add(
                usize::try_from(run.bytes).map_err(|_| invalid("run length overflows"))?,
            ) else {
                return Err(invalid("run length overflows the payload"));
            };
            // str::get rejects both out-of-bounds runs and split UTF-8 scalars.
            let Some(part) = record.segment.payload.get(offset..end) else {
                return Err(invalid("runs split UTF-8 or exceed the payload"));
            };
            if let Some(declaration) = &run.declaration {
                if run.stream as usize != streams.len()
                    || !positions.insert((
                        declaration.block_index,
                        declaration.part_index,
                        matches!(declaration.payload, StreamPayload::ReasoningSignature),
                    ))
                {
                    return Err(invalid(
                        "stream declarations are not dense or positions conflict",
                    ));
                }
                streams.push(ReconstructedStream {
                    declaration: declaration.clone(),
                    text: part.to_owned(),
                });
            } else {
                let Some(existing) = streams.get_mut(run.stream as usize) else {
                    return Err(invalid("continuation before declaration"));
                };
                existing.text.push_str(part);
            }
            offset = end;
        }
        if offset != record.segment.payload.len() {
            return Err(invalid("runs do not partition the payload"));
        }
        consumed += 1;
    }
    // Lean: `if flushes.isEmpty then .error .loading`.
    if consumed == 0 {
        return Err(DensePrefixError::Loading);
    }
    let highest_visible_ordinal = slots.keys().next_back().copied();
    Ok(DensePrefix {
        segments: consumed,
        streams,
        highest_visible_ordinal,
    })
}

/// The full live eligibility classifier: the Rust projection of Lean's
/// `StreamingResponse.project` (`Proofs/StreamingResponse/State.lean`) over the
/// protocol `LiveOutput`/`LiveStreamState` owner types.
///
/// One owner, no client-local substitute. Every branch is an exact replay of
/// the Lean decision sequence; classification order (`Loading` before
/// `Conflicted` before `Invalid`) matches `State.lean` exactly, including the
/// prefix ordering where a timestamp-regressing twin classifies `Invalid`
/// before the coordinate-twin check can classify `Conflicted`.
///
/// The Rust `LiveOutput`/`LiveStreamState` types are built by the caller from
/// the classifier's returned streams and states: `Live`/`Settling`/
/// `RetainedPartial` streams carry `Unclosed`/`PendingPublication`/`RetainedPartial`
/// respectively, and this module owns every eligibility decision behind them.
#[derive(Clone, Debug, PartialEq)]
pub enum LiveView {
    /// Lean `View.absent`: no open-source prefix may be shown. No local
    /// terminal fact proves remote liveness; only `owner_live` does.
    Absent,
    /// Lean `View.live`: the exact contiguous open prefix of an eligible owner.
    Live { streams: Vec<LiveStream> },
    /// Lean `View.settling`: closure is known but publication is not yet
    /// reconstructable; a validated contiguous preview is preserved.
    Settling { streams: Vec<LiveStream> },
    /// Lean `View.loading`: not reconstructable yet, never a shortened answer.
    Loading,
    /// Lean `View.denied`: the authorization owner established the denial.
    Denied,
    /// Lean `View.conflicted`: visible twins disagree; no winner is selected.
    Conflicted,
    /// Lean `View.invalid`: malformed shape, wrong writer, or a physical
    /// identity carrying conflicting facts.
    Invalid,
    /// Lean `View.retracted`: the attempt was abandoned before retry backoff.
    Retracted,
    /// Lean `View.retainedPartial`: unreferenced streams of a closed Partial
    /// provider source after terminal selection.
    RetainedPartial { streams: Vec<LiveStream> },
    /// Lean `View.published`: the exact terminal message and native message.
    Published {
        message: TranscriptMessage,
        native: Message,
    },
}

impl LiveView {
    /// Lean's `visibleStreams`: hidden reasoning fields never render,
    /// including in diagnostics and retained partials.
    fn visible_streams(streams: Vec<LiveStream>) -> Vec<LiveStream> {
        streams
            .into_iter()
            .filter(|stream| {
                !matches!(
                    stream.declaration.payload,
                    StreamPayload::ReasoningEncrypted
                        | StreamPayload::ReasoningRedacted
                        | StreamPayload::ReasoningSignature
                )
            })
            .collect()
    }
}

/// Exact target coordinate supplied by the observation owner. The target's
/// request is intentionally independent from the observation request so a
/// cross-request coordinate is rejected rather than silently re-scoped.
pub struct LiveTarget<'a> {
    pub request_doc_id: &'a str,
    pub source: &'a OutputSource,
    pub writer: &'a OutputWriter,
    /// Exact physical header identity. Header contents are resolved only from
    /// `LiveObservation::messages`; callers cannot provide a second copy.
    pub message_id: Option<&'a str>,
}

/// Result of selecting the one current provider target which may be passed to
/// [`project_live`]. Selection resolves identity only; the live classifier
/// remains the sole owner of reconstruction, closure and liveness policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveTargetSelection {
    Absent,
    Selected {
        source: OutputSource,
        writer: OutputWriter,
        message_id: Option<String>,
    },
    /// More than one physical message claims the selected source. Never pick a
    /// first/last winner from an unordered replica observation.
    Conflicted,
}

/// Rust projection of Lean `StreamingResponse.selectTarget`.
///
/// Only exact-request, current-generation inference provider coordinates are
/// candidates. Stale, foreign and non-inference facts are inert. The greatest
/// provider `(scope, turn, attempt)` coordinate is the intended current
/// attempt; exact record replay is harmless. A header is attached only when a
/// unique physical header references a closing record for that coordinate.
pub fn select_live_target(
    request_doc_id: &str,
    execution_generation: &str,
    records: &[ObservedSegment<'_>],
    messages: &[(&str, &TranscriptMessage)],
) -> LiveTargetSelection {
    use crate::rendered_request::CaptureScopeKind;

    let selected = records
        .iter()
        .filter_map(|observed| {
            let OutputSource::ProviderTurn {
                scope,
                turn_index,
                attempt,
            } = &observed.segment.source
            else {
                return None;
            };
            let OutputWriter::RequestExecution {
                execution_generation: writer_generation,
            } = &observed.segment.writer
            else {
                return None;
            };
            (observed.segment.request_doc_id == request_doc_id
                && writer_generation == execution_generation
                && scope.kind == CaptureScopeKind::Inference)
                .then_some((
                    (*scope, *turn_index, *attempt),
                    &observed.segment.source,
                    &observed.segment.writer,
                ))
        })
        .max_by_key(|(coordinate, _, _)| *coordinate);
    let Some((_, source, writer)) = selected else {
        return LiveTargetSelection::Absent;
    };

    let ids = messages
        .iter()
        .filter_map(|(doc_id, message)| {
            (message.request_doc_id.as_deref() == Some(request_doc_id)
                && message.payload_references().iter().any(|reference| {
                    records.iter().any(|observed| {
                        observed.doc_id == reference.close_doc_id
                            && observed.segment.request_doc_id == request_doc_id
                            && observed.segment.source == *source
                            && observed.segment.writer == *writer
                            && observed.segment.close.is_some()
                    })
                }))
            .then_some((*doc_id).to_owned())
        })
        .collect::<std::collections::BTreeSet<_>>();
    if ids.len() > 1 {
        return LiveTargetSelection::Conflicted;
    }
    LiveTargetSelection::Selected {
        source: source.clone(),
        writer: writer.clone(),
        message_id: ids.into_iter().next(),
    }
}

/// What the caller must observe about the target. Mirrors Lean's
/// `Observation`/`Target`/`OwnerLiveness` fields that the classifier consumes;
/// the exact message and native reconstruction stay behind this boundary.
pub struct LiveObservation<'a> {
    pub request_doc_id: &'a str,
    pub session_id: &'a str,
    /// The one target coordinate: the target source and its declared writer
    /// (Lean `Target.coordinate`/`Target.writer`). `project_live` classifies
    /// exactly one target per call; the caller groups per-source views into a
    /// `LiveOutput` row set.
    pub target: LiveTarget<'a>,
    /// Visible messages in scope for origin lookup and terminal resolution.
    pub messages: &'a [(&'a str, &'a TranscriptMessage)],
    /// The observation's authorized scope: the agent DID and the optional
    /// requester DID the view is served under. Every header and origin lookup
    /// validates against this scope through the origin owner
    /// (`origin::lookup_message`); ACP authorization itself stays with its
    /// existing owner, and the classifier never fabricates authority.
    pub agent_did: &'a str,
    pub requester_did: Option<&'a str>,
    /// Visible segments, replicating in any order.
    pub records: &'a [ObservedSegment<'a>],
    /// Explicit header denial evidence from the ACP/hydration owner; never
    /// inferred from absence. Exactly Lean's `Observation.deniedHeaders`: a
    /// namespace disjoint from segments, so an identical ID in both namespaces
    /// never leaks a denial across collections.
    pub denied_headers: &'a [String],
    /// Explicit segment/closure denial evidence (Lean
    /// `Observation.deniedSegments`), as the reconstruction owner's newtype so
    /// the reconstruction primitives can consume it directly.
    pub denied_segments: &'a [String],
    pub dependency_denials: &'a [DependencyDenial],
    /// Durable owner liveness (Lean `OwnerLiveness`): the request's current
    /// `(request_doc_id, execution_generation)` when the writer is a request
    /// execution, and every live tool call's doc ID.
    pub owner: OwnerLiveness<'a>,
    /// Whether the durable request row has reached terminal lifecycle (Lean
    /// `requestTerminal`), observed independently of the selection payload. A
    /// terminal request without a selection is `missingSelection` → loading
    /// through the terminal resolver, never a nonterminal settle.
    pub request_terminal: bool,
    /// Durable `AgentRequest.terminal_output` selection (Lean
    /// `terminalSelection`). `None` under a terminal request resolves as
    /// `missingSelection`.
    pub terminal_selection: Option<TerminalOutput>,
}

/// Durable owner-liveness observation, exactly Lean's `OwnerLiveness`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnerLiveness<'a> {
    /// `(request_doc_id, execution_generation)` currently claiming the request.
    pub current_request: Option<(&'a str, &'a str)>,
    /// Doc IDs of tool calls whose tool lifecycle is currently live.
    pub live_tools: Vec<&'a str>,
}

/// Decode the durable active-generation observation carried by a request.
/// This is not write or renewal authorization and deliberately performs no
/// UI-local wall-clock comparison; lifecycle recovery owns lease expiry.
pub fn observed_request_execution_owner(
    request: &crate::row::AgentRequestRow,
) -> Option<(&str, &str)> {
    let request_doc_id = request.doc_id.as_deref()?.trim();
    let generation = request.execution_generation.as_deref()?.trim();
    let deadline = request.execution_lease_expires_at.as_deref()?.trim();
    (matches!(
        request.lifecycle_state?,
        crate::request_lifecycle::RequestLifecycleState::Claimed
            | crate::request_lifecycle::RequestLifecycleState::Processing
    ) && !request_doc_id.is_empty()
        && !generation.is_empty()
        && request
            .execution_lease_secs
            .is_some_and(|seconds| seconds > 0)
        && !deadline.is_empty())
    .then_some((request_doc_id, generation))
}

/// Lean's `ownerLive` for the target writer: a request writer is live exactly
/// when the owner's current request matches `(request, generation)`; a tool
/// writer is live exactly when the source call is the writer call and is in
/// the owner's live set. Authored output has no live path at all.
fn owner_live(observation: &LiveObservation<'_>) -> bool {
    match (observation.target.source, observation.target.writer) {
        (
            OutputSource::ProviderTurn { .. },
            OutputWriter::RequestExecution {
                execution_generation,
            },
        ) => {
            observation.owner.current_request
                == Some((observation.request_doc_id, execution_generation.as_str()))
        }
        (
            OutputSource::ToolCall {
                tool_call_doc_id: source,
            },
            OutputWriter::ToolExecution {
                tool_call_doc_id: writer,
            },
        ) => source == writer && observation.owner.live_tools.contains(&source.as_str()),
        _ => false,
    }
}

/// Lean `sourceDenied`: any visible record of the target source denied.
fn source_denied(observation: &LiveObservation<'_>) -> bool {
    observation.records.iter().any(|record| {
        record.segment.request_doc_id == observation.request_doc_id
            && record.segment.source == *observation.target.source
            && observation
                .denied_segments
                .iter()
                .any(|denied| denied == record.doc_id)
    })
}

/// Lean `targetScoped`: the target coordinate's request equals the
/// observation's request. In this projection the target coordinate carries the
/// observation's `request_doc_id` directly — the caller binds one request
/// boundary per observation and `project_live` re-scopes every record and
/// lookup by it — so the check is structural and cannot fail. Kept as its own
/// step to mirror the Lean decision sequence exactly.
fn target_scoped(observation: &LiveObservation<'_>) -> bool {
    observation.target.request_doc_id == observation.request_doc_id
}

/// One header lookup through the shared origin owner (`Hydration.lookupMessage`
/// as implemented by `origin::lookup_message`): denial, exact-duplicate
/// dedup, the physical/coordinate twin sweep and agent/requester scope —
/// never a weaker classifier-local copy. Origin-owner error classes map onto
/// the views exactly as Lean's callers map them.
fn lookup_observed<'a>(
    observation: &'a LiveObservation<'a>,
    doc_id: &str,
) -> Result<ObservedMessage<'a>, LiveView> {
    let facts: Vec<ObservedMessage<'_>> = observation
        .messages
        .iter()
        .map(|(id, message)| ObservedMessage {
            doc_id: id,
            message,
        })
        .collect();
    origin::lookup_message(
        &facts,
        observation.denied_headers,
        doc_id,
        observation.agent_did,
        observation.requester_did,
    )
    .map_err(|error| match error {
        OriginError::Unavailable { .. } => LiveView::Loading,
        OriginError::Denied { .. } => LiveView::Denied,
        OriginError::Conflict { .. } => LiveView::Conflicted,
        OriginError::ScopeMismatch { .. }
        | OriginError::InvalidOrigin { .. }
        | OriginError::MetadataMismatch { .. } => LiveView::Invalid,
    })
}

/// Why a source's closure classifies. Exactly the observable facts of Lean's
/// `CloseObservation`; derived from the records, never supplied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseObservation_ {
    Open,
    Closed { outcome: OutputOutcome },
    Retracted,
    Conflict,
}

/// Exact replay of Lean's `observeClose`: the unique closure in scope wins;
/// multiple distinct closures conflict; none means open. Deduplication first,
/// so a replicated fact never conflicts with itself (`uniqueRecord`).
fn observe_close<'a>(
    observation: &'a LiveObservation<'a>,
) -> Result<(Option<&'a ObservedSegment<'a>>, CloseObservation_), ()> {
    let mut closures: Vec<&ObservedSegment<'_>> = Vec::new();
    for record in observation.records {
        if record.segment.request_doc_id != observation.request_doc_id
            || record.segment.source != *observation.target.source
            || record.segment.close.is_none()
        {
            continue;
        }
        // `uniqueRecord` dedups exact facts before deciding; replicated
        // delivery of the same document is not a twin.
        if !closures
            .iter()
            .any(|existing| existing.doc_id == record.doc_id && existing.segment == record.segment)
        {
            closures.push(record);
        }
    }
    match closures.as_slice() {
        [] => Ok((None, CloseObservation_::Open)),
        [only] => match &only.segment.close {
            Some(SourceClose::Closed { outcome, .. }) => {
                Ok((Some(only), CloseObservation_::Closed { outcome: *outcome }))
            }
            Some(SourceClose::Retracted) => Ok((Some(only), CloseObservation_::Retracted)),
            None => Ok((None, CloseObservation_::Conflict)),
        },
        // Distinct closures of one source: conflict, never a winner.
        _ => Err(()),
    }
}

/// How a stream's bytes will be surfaced in a message header. Exactly Lean's
/// `streamReferenced` reachability, over the protocol `PayloadRef` types.
fn stream_referenced(message: &TranscriptMessage, close_doc_id: &str, stream: u32) -> bool {
    message
        .payload_references()
        .iter()
        .any(|reference| reference.close_doc_id == close_doc_id && reference.stream == stream)
}

/// Lean `classifyMessageError`: a `ReconstructionError` onto the `View`
/// classes, enumerated against the owner enum so a future variant forces the
/// mapping to be reconsidered. `UnresolvedClose`/`MissingSegment` may be
/// replication lag → loading; `AccessDenied` is an access error → denied;
/// segment/closure/message disagreements → conflicted; every structural
/// integrity failure (invalid reference, extent, writer, terminal, structure,
/// presentation or payload) → invalid.
fn classify_message_error(error: &ReconstructionError) -> LiveView {
    match error {
        ReconstructionError::UnresolvedClose { .. }
        | ReconstructionError::MissingSegment { .. } => LiveView::Loading,
        ReconstructionError::AccessDenied { .. } => LiveView::Denied,
        ReconstructionError::ConflictingClosures { .. }
        | ReconstructionError::ConflictingSegments { .. }
        | ReconstructionError::ConflictingMessages { .. } => LiveView::Conflicted,
        ReconstructionError::InvalidReference { .. }
        | ReconstructionError::ExtentMismatch { .. }
        | ReconstructionError::InvalidWriter { .. }
        | ReconstructionError::UnresolvedTerminalOutput { .. }
        | ReconstructionError::InvalidStructure { .. }
        | ReconstructionError::InvalidPresentation { .. }
        | ReconstructionError::InvalidPayload { .. } => LiveView::Invalid,
    }
}

/// Lean `Terminal.resolveTerminal` over the protocol `TerminalOutput`
/// selection and `TranscriptMessage` headers, through the shared origin
/// owner: denial, exact-duplicate dedup, the coordinate sweep and
/// agent/requester scope come from `lookup_observed`; request/session
/// membership and role/publication eligibility stay here. Returns the
/// selected message (or `Ok(None)` for explicit `NoMessage`).
fn resolve_terminal<'a>(
    observation: &'a LiveObservation<'a>,
) -> Result<Option<&'a TranscriptMessage>, LiveView> {
    // Lean `.missingSelection`: a terminal request without an observed
    // selection is incomplete, not `NoMessage`.
    let Some(selection) = &observation.terminal_selection else {
        return Err(LiveView::Loading);
    };
    let message_doc_id = match selection {
        // Lean `.ok none`: an explicit absence of an answer, not loading.
        TerminalOutput::NoMessage => return Ok(None),
        TerminalOutput::Message { message_doc_id } => message_doc_id,
    };
    let message = lookup_observed(observation, message_doc_id)?.message;
    // Lean `.wrongScope`: exact physical request and session membership.
    if message.request_doc_id.as_deref() != Some(observation.request_doc_id)
        || message.session_id != observation.session_id
    {
        return Err(LiveView::Invalid);
    }
    // Lean `.ineligibleHeader`: assistant role and request-owned publication.
    if message.role != MessageRole::Assistant
        || !matches!(
            message.publication,
            MessagePublication::RequestExecution { .. }
                | MessagePublication::RequestRecovery { .. }
        )
    {
        return Err(LiveView::Invalid);
    }
    Ok(Some(message))
}

/// Lean `TerminalPayload.resolveTerminalPayload` over protocol types:
/// selection resolution, denial, dependency-denial membership, then full
/// reconstruction of the selected header.
fn resolve_terminal_payload(
    observation: &LiveObservation<'_>,
) -> Result<Option<Message>, LiveView> {
    let Some(message) = resolve_terminal(observation)? else {
        return Ok(None);
    };
    // Lean `resolveTerminalPayload`'s dependency-denial membership rule, before
    // reconstruction.
    if observation.dependency_denials.iter().any(|denial| {
        message
            .payload_references()
            .iter()
            .any(|r| r.close_doc_id == denial.root_close_id)
    }) {
        return Err(LiveView::Denied);
    }
    reconstruction::reconstruct_message(
        observation.records,
        observation.denied_segments,
        observation.dependency_denials,
        message,
    )
    .map(Some)
    .map_err(|error| classify_message_error(&error))
}

/// The classifier entry point: the protocol `LiveOutput`/`LiveStreamState`
/// projection of one target's live view, over the Lean `StreamingResponse.project`
/// owner's exact decision sequence.
///
/// Returns the `LiveView` for the one target coordinate in the observation.
/// The caller builds the protocol `LiveOutput`/`LiveStreamState` rows from the
/// streams; the classifier owns every eligibility decision.
///
/// Exact branch order mirrors `project`:
/// 1. Header target (`message_id`): `project_published` — denial, header
///    uniqueness/scope, dependency denials, reconstruction (with the
///    missing-extent settling path).
/// 2. Unheaded target: scope, then `observe_close` — open sources gate on
///    denial and `owner_live` before the prefix reconstructs (`Absent` without
///    a live owner), retracted and conflicting closures short-circuit, closed
///    sources run `project_unheaded_closed`.
pub fn project_live(observation: &LiveObservation<'_>) -> LiveView {
    if observation.target.source.is_auxiliary_audit() {
        return LiveView::Absent;
    }
    // Lean `project`: a header target goes straight to `projectPublished`.
    if observation.target.message_id.is_some() {
        return project_published(observation);
    }
    // Lean `project`'s `targetScoped` gate: structural invalidity, not loading.
    if !target_scoped(observation) {
        return LiveView::Invalid;
    }
    // Lean `observeClose`.
    let (closing, classification) = match observe_close(observation) {
        Ok(result) => result,
        Err(()) => return LiveView::Conflicted,
    };
    match classification {
        CloseObservation_::Open => {
            if source_denied(observation) {
                return LiveView::Denied;
            }
            if owner_live(observation) {
                match reconstruct_open(observation) {
                    Ok(streams) => LiveView::Live { streams },
                    Err(DensePrefixError::Loading) => LiveView::Loading,
                    Err(DensePrefixError::Conflicted { .. }) => LiveView::Conflicted,
                    Err(DensePrefixError::Invalid { .. }) => LiveView::Invalid,
                }
            } else {
                LiveView::Absent
            }
        }
        CloseObservation_::Retracted => LiveView::Retracted,
        CloseObservation_::Conflict => LiveView::Conflicted,
        CloseObservation_::Closed { outcome } => {
            let closing = closing.expect("closed classification carries its closing record");
            project_unheaded_closed(observation, closing, outcome)
        }
    }
}

/// Lean `reconstructOpen` over protocol types: the tolerant dense prefix,
/// then visible streams only (Lean `visibleStreams`).
fn reconstruct_open(
    observation: &LiveObservation<'_>,
) -> Result<Vec<LiveStream>, DensePrefixError> {
    reconstruct_prefix(observation, None)
}

/// Lean `reconstructPrefix` over protocol types: the tolerant dense prefix
/// primitive (bounded when closing) with visible streams only. The caller's
/// `records` must already be the target coordinate's scope; the primitive
/// re-scopes exactly as Lean's `sourceData` does.
fn reconstruct_prefix(
    observation: &LiveObservation<'_>,
    limit: Option<u32>,
) -> Result<Vec<LiveStream>, DensePrefixError> {
    let prefix = reconstruct_dense_prefix(
        observation.records,
        observation.request_doc_id,
        observation.target.source,
        observation.target.writer,
        limit,
    )?;
    // Lean `visibleStreams` filters after the streams are indexed: the
    // LiveStream index is the native stream position, before opaque streams
    // are dropped for rendering.
    Ok(LiveView::visible_streams(
        prefix
            .streams
            .into_iter()
            .enumerate()
            .map(|(stream_index, reconstructed)| LiveStream {
                source: observation.target.source.clone(),
                stream: stream_index as u32,
                declaration: reconstructed.declaration,
                state: LiveStreamState::Unclosed,
                text: reconstructed.text,
            })
            .collect(),
    ))
}

/// Lean `reconstructBeforeClose`: the bounded prefix within the closed
/// extent. Every closed caller here has already resolved the close.
fn reconstruct_before_close(
    observation: &LiveObservation<'_>,
    closing: &ObservedSegment<'_>,
) -> Result<Vec<LiveStream>, DensePrefixError> {
    match &closing.segment.close {
        Some(SourceClose::Closed { segments, .. }) => {
            reconstruct_prefix(observation, Some(*segments))
        }
        _ => Err(DensePrefixError::Invalid {
            detail: "bounded prefix without a closed extent".to_owned(),
        }),
    }
}

/// Lean `loadingOrSettling`/`loadingOrSettlingBeforeClose` over protocol
/// types: a reconstructable bounded prefix is `Settling`; otherwise the
/// prefix error classifies. Streams keep `PendingPublication` states: closure
/// is known, publication is not yet reconstructable.
fn loading_or_settling(
    observation: &LiveObservation<'_>,
    closing: Option<&ObservedSegment<'_>>,
) -> LiveView {
    let reconstructed = match closing {
        Some(closing) => reconstruct_before_close(observation, closing),
        None => reconstruct_open(observation),
    };
    // The settled streams keep their closure's outcome: Lean's
    // `loadingOrSettlingBeforeClose` always has a closed extent, so the
    // `Complete` fallback below is unreachable through `project`.
    let outcome = closing.and_then(|closing| match &closing.segment.close {
        Some(SourceClose::Closed { outcome, .. }) => Some(*outcome),
        _ => None,
    });
    match reconstructed {
        Ok(mut streams) => {
            for stream in &mut streams {
                // The unclosed prefix state becomes PendingPublication once
                // closure is known: bytes are kept, never labelled as live
                // activity.
                if stream.state == LiveStreamState::Unclosed {
                    stream.state = LiveStreamState::PendingPublication {
                        outcome: outcome.unwrap_or(OutputOutcome::Complete),
                    };
                }
            }
            LiveView::Settling { streams }
        }
        Err(DensePrefixError::Loading) => LiveView::Loading,
        Err(DensePrefixError::Conflicted { .. }) => LiveView::Conflicted,
        Err(DensePrefixError::Invalid { .. }) => LiveView::Invalid,
    }
}

/// Lean `referencedTargetClose?`: the first header reference resolving to a
/// closed record at the target coordinate with the target writer wins.
/// Resolution reuses the reconstruction owner's own validation (`resolveClose`):
/// visible, not denied, closed, stream index in the sealed extent.
fn referenced_target_close<'a>(
    observation: &LiveObservation<'a>,
    message: &TranscriptMessage,
) -> Option<&'a ObservedSegment<'a>> {
    message.payload_references().iter().find_map(|reference| {
        resolve_close_reference(observation, reference)
            .ok()
            .filter(|closing| {
                closing.segment.source == *observation.target.source
                    && closing.segment.writer == *observation.target.writer
            })
    })
}

/// Resolve one close reference (Lean `resolveClose`, minus the
/// reconstruction owner's duplicate-closure sweep, which `project` performs
/// through `observe_close`): the referenced record must be visible, not
/// denied, a closed one, and the stream index must be in the sealed extent.
fn resolve_close_reference<'a>(
    observation: &LiveObservation<'a>,
    reference: &PayloadRef,
) -> Result<&'a ObservedSegment<'a>, ()> {
    let mut matching = observation
        .records
        .iter()
        .filter(|r| r.doc_id == reference.close_doc_id);
    let record = matching.next().ok_or(())?;
    if matching.any(|other| other != record) {
        return Err(());
    }
    if observation
        .denied_segments
        .iter()
        .any(|denied| denied == record.doc_id)
    {
        return Err(());
    }
    match &record.segment.close {
        Some(SourceClose::Closed { stream_bytes, .. }) => {
            if (reference.stream as usize) >= stream_bytes.len() {
                return Err(());
            }
            // Lean `resolveClose` also requires the referenced record to be
            // the unique closure of its coordinate: conflicting closures fail
            // the reference (the caller's next reference is then tried).
            let mut closures: Vec<&ObservedSegment<'_>> = Vec::new();
            for candidate in observation.records {
                if candidate.segment.request_doc_id == record.segment.request_doc_id
                    && candidate.segment.source == record.segment.source
                    && candidate.segment.close.is_some()
                    && !closures.iter().any(|existing| {
                        existing.doc_id == candidate.doc_id && existing.segment == candidate.segment
                    })
                {
                    closures.push(candidate);
                }
            }
            if closures.len() != 1 || *closures[0] != *record {
                return Err(());
            }
            Ok(record)
        }
        _ => Err(()),
    }
}

/// Lean `settlingForReferencedTarget`: denial first, then the referenced
/// target close, then the bounded settling classification.
fn settling_for_referenced_target(
    observation: &LiveObservation<'_>,
    message: &TranscriptMessage,
) -> LiveView {
    if source_denied(observation) {
        return LiveView::Denied;
    }
    match referenced_target_close(observation, message) {
        Some(closing) => loading_or_settling(observation, Some(closing)),
        None => LiveView::Loading,
    }
}

/// Resolve a header's closure references without exposing bytes (Lean
/// `resolvedReferencingHeaders`). Every visible header that references this
/// source must fully reconstruct before its refs may be subtracted from
/// retained diagnostics.
#[derive(Clone, Debug, PartialEq)]
pub enum ReferencedHeaderResolution<'a> {
    /// Every visible header referencing this source fully reconstructs; their
    /// refs are subtracted from the retained diagnostics.
    Resolved { headers: Vec<&'a TranscriptMessage> },
    /// No visible header references this source yet.
    None,
    /// Candidate headers conflict, are denied, or fail reconstruction.
    Failed(LiveView),
}

/// Exact replay of `resolvedReferencingHeaders`: dedup candidates by header
/// identity (Lean `.dedup`), require request membership (or tool delivery),
/// check session scope, then resolve dependency denials and full
/// reconstruction per candidate.
fn resolved_referencing_headers<'a>(
    observation: &'a LiveObservation<'a>,
    closing: &ObservedSegment<'_>,
) -> ReferencedHeaderResolution<'a> {
    let mut candidates: Vec<(&'a str, &'a TranscriptMessage)> = Vec::new();
    for (doc_id, message) in observation.messages {
        let references_close = message
            .payload_references()
            .iter()
            .any(|r| r.close_doc_id == closing.doc_id);
        if !references_close {
            continue;
        }
        let request_member = match &message.publication {
            MessagePublication::ToolDelivery { .. } => true,
            _ => message.request_doc_id.as_deref() == Some(observation.request_doc_id),
        };
        if !request_member || message.session_id != observation.session_id {
            continue;
        }
        // Lean `.dedup` on the candidate envelope list.
        if !candidates
            .iter()
            .any(|(existing_id, existing)| *existing_id == *doc_id && **existing == **message)
        {
            candidates.push((doc_id, message));
        }
    }
    if candidates.is_empty() {
        return ReferencedHeaderResolution::None;
    }
    let mut headers = Vec::new();
    for (doc_id, candidate) in candidates {
        // Lean `messageAt observation.messages candidate.header.id`: the full
        // visible set may hold a distinct twin under the same identity.
        let mut matching: Vec<&'a TranscriptMessage> = observation
            .messages
            .iter()
            .filter_map(|(id, message)| (*id == doc_id).then_some(*message))
            .collect();
        matching.dedup();
        if matching.len() != 1 || matching[0] != candidate {
            return ReferencedHeaderResolution::Failed(LiveView::Conflicted);
        }
        if observation.dependency_denials.iter().any(|denial| {
            candidate
                .payload_references()
                .iter()
                .any(|r| r.close_doc_id == denial.root_close_id)
        }) {
            return ReferencedHeaderResolution::Failed(LiveView::Denied);
        }
        // Full reconstruction is the header owner's validation; the live
        // classifier reuses it (one owner), because a candidate that cannot
        // reconstruct changes the retained-diagnostics view.
        match reconstruction::reconstruct_message(
            observation.records,
            observation.denied_segments,
            observation.dependency_denials,
            candidate,
        ) {
            Ok(_) => headers.push(candidate),
            Err(error) => {
                return ReferencedHeaderResolution::Failed(classify_message_error(&error));
            }
        }
    }
    ReferencedHeaderResolution::Resolved { headers }
}

/// Build the retained-diagnostics streams: every visible stream of the closed
/// Partial provider source not referenced by any fully reconstructed header
/// (Lean `retainedStreams` over `visibleStreams`), with `RetainedPartial`
/// states.
fn retained_streams(
    observation: &LiveObservation<'_>,
    closing: &ObservedSegment<'_>,
    headers: &[&TranscriptMessage],
    streams: Vec<ReconstructedStream>,
) -> Vec<LiveStream> {
    // Lean `retainedStreams`: unreferenced streams are selected by the
    // original stream index, then `visibleStreams` drops opaque declarations.
    LiveView::visible_streams(
        streams
            .into_iter()
            .enumerate()
            .filter(|(stream_index, _)| {
                !headers
                    .iter()
                    .any(|header| stream_referenced(header, closing.doc_id, *stream_index as u32))
            })
            .map(|(stream_index, reconstructed)| LiveStream {
                source: observation.target.source.clone(),
                stream: stream_index as u32,
                declaration: reconstructed.declaration,
                state: LiveStreamState::RetainedPartial,
                text: reconstructed.text,
            })
            .collect(),
    )
}

/// Lean `projectUnheadedClosed` over protocol types. Terminal resolution is
/// performed before classifying unreferenced Partial streams.
fn project_unheaded_closed(
    observation: &LiveObservation<'_>,
    closing: &ObservedSegment<'_>,
    outcome: OutputOutcome,
) -> LiveView {
    // Lean `projectUnheadedClosed`'s `targetScoped` gate.
    if !target_scoped(observation) {
        return LiveView::Invalid;
    }
    // Lean: source denial or a dependency denial rooted at this close is denied.
    if source_denied(observation)
        || observation
            .dependency_denials
            .iter()
            .any(|denial| denial.root_close_id == closing.doc_id)
    {
        return LiveView::Denied;
    }
    // Lean: a terminal request resolves its selection first — `missingSelection`
    // (terminal without an observed selection) loads, it never settles as a
    // nonterminal. `request_terminal` is observed independently of the
    // selection payload.
    if observation.request_terminal {
        let resolved_terminal = match resolve_terminal_payload(observation) {
            Ok(resolved) => resolved,
            Err(view) => return view,
        };
        let _ = resolved_terminal;
    }
    // Lean: a nonterminal request classifies the bounded prefix.
    if !observation.request_terminal {
        return loading_or_settling(observation, Some(closing));
    }
    // Lean: only a closed Partial provider source has retained diagnostics.
    if matches!(observation.target.source, OutputSource::ProviderTurn { .. })
        && outcome == OutputOutcome::Partial
    {
        let headers = match resolved_referencing_headers(observation, closing) {
            ReferencedHeaderResolution::Failed(view) => return view,
            ReferencedHeaderResolution::Resolved { headers } => headers,
            ReferencedHeaderResolution::None => Vec::new(),
        };
        // Lean `reconstructExtent`: bounded to the sealed count, with its
        // exact error classes. The shared all-stream extent owner
        // (`reconstruct_extent_streams`) applies the unique closure sweep,
        // writer agreement, the closing flush, dense openings, strict runs
        // and sealed byte accounting — one owner, no second dense validator.
        // `missingSegment` (a gap inside the sealed extent) falls back to the
        // bounded settling view; segment/closure conflicts conflict and every
        // integrity failure is invalid.
        match reconstruct_extent_streams(observation, *closing) {
            Err(ReconstructionError::MissingSegment { .. }) => {
                loading_or_settling(observation, Some(closing))
            }
            Err(
                ReconstructionError::ConflictingSegments { .. }
                | ReconstructionError::ConflictingClosures { .. },
            ) => LiveView::Conflicted,
            Err(_) => LiveView::Invalid,
            Ok(streams) => LiveView::RetainedPartial {
                streams: retained_streams(observation, closing, &headers, streams),
            },
        }
    } else {
        loading_or_settling(observation, Some(closing))
    }
}

/// Reconstruct every stream declared by one sealed extent through the shared
/// strict stream owner. The close owns the stream count; this live projection
/// does not retain another byte or native-shape validator.
fn reconstruct_extent_streams(
    observation: &LiveObservation<'_>,
    closing: ObservedSegment<'_>,
) -> Result<Vec<ReconstructedStream>, ReconstructionError> {
    let Some(SourceClose::Closed { stream_bytes, .. }) = &closing.segment.close else {
        return Err(ReconstructionError::InvalidStructure {
            detail: "live retained extent is not closed".to_owned(),
        });
    };
    (0..stream_bytes.len())
        .map(|stream| {
            reconstruction::reconstruct_stream(
                observation.records,
                observation.denied_segments,
                observation.dependency_denials,
                &PayloadRef {
                    close_doc_id: closing.doc_id.to_owned(),
                    stream: u32::try_from(stream).map_err(|_| {
                        ReconstructionError::InvalidStructure {
                            detail: "sealed stream count exceeds protocol index".to_owned(),
                        }
                    })?,
                },
            )
        })
        .collect()
}

/// Lean `projectPublished` over protocol types. The target envelope is
/// resolved by exact doc ID through the shared origin owner (`lookup_observed`:
/// denial, exact-duplicate dedup, the coordinate sweep and agent/requester
/// scope — never a weaker classifier-local copy); dependency denials and
/// reconstruction follow the Lean order. The target argument mirrors the Lean
/// call shape — the envelope identity comes from the observation's
/// `target_message` coordinate.
fn project_published<'a>(observation: &'a LiveObservation<'a>) -> LiveView {
    let Some(target_doc_id) = observation.target.message_id else {
        // Lean `messageAt` unavailable: no target coordinate observed yet.
        return LiveView::Loading;
    };
    let message = match lookup_observed(observation, target_doc_id) {
        Ok(observed) => observed.message,
        Err(view) => return view,
    };
    // Lean `messageScopeResult`: session and request membership are
    // structural; a fork follows the origin lookup path.
    if message.session_id != observation.session_id {
        return LiveView::Invalid;
    }
    match &message.publication {
        MessagePublication::Fork {
            origin_message_doc_id,
        } => {
            // Lean: forks carry no request membership by design; the origin
            // header is looked up and its metadata must match exactly. The
            // fork child itself is looked up through the same owner, so the
            // coordinate sweep and scope apply to it too.
            if message.request_doc_id.is_some() {
                return LiveView::Invalid;
            }
            let fork_observed = match lookup_observed(observation, target_doc_id) {
                Ok(observed) => observed,
                Err(view) => return view,
            };
            if let Err(view) = project_fork(observation, fork_observed, origin_message_doc_id) {
                return view;
            }
        }
        _ => {
            if message.request_doc_id.as_deref() != Some(observation.request_doc_id) {
                return LiveView::Invalid;
            }
        }
    }
    // Lean: dependency denials before reconstruction.
    if observation.dependency_denials.iter().any(|denial| {
        message
            .payload_references()
            .iter()
            .any(|r| r.close_doc_id == denial.root_close_id)
    }) {
        return LiveView::Denied;
    }
    match reconstruction::reconstruct_message(
        observation.records,
        observation.denied_segments,
        observation.dependency_denials,
        message,
    ) {
        Ok(native) => LiveView::Published {
            message: message.clone(),
            native,
        },
        Err(error) => match &error {
            // Lean `projectPublished`: lookup .unavailable is .loading, and
            // a missing extent in the sealed reference runs the referenced-
            // target settling path.
            ReconstructionError::UnresolvedClose { .. } => LiveView::Loading,
            ReconstructionError::MissingSegment { .. } => {
                settling_for_referenced_target(observation, message)
            }
            _ => classify_message_error(&error),
        },
    }
}

/// Lean `messageScopeResult`'s fork path over protocol types: the origin
/// header is resolved through the shared origin owner (`lookup_observed` —
/// denial, exact-duplicate dedup, the coordinate sweep and agent/requester
/// scope, never a weaker classifier-local copy), then `forkMetadataMatches`
/// must hold exactly. The caller continues with the shared dependency-denial
/// and reconstruction flow on the fork child itself.
fn project_fork(
    observation: &LiveObservation<'_>,
    fork: ObservedMessage<'_>,
    origin_message_doc_id: &str,
) -> Result<(), LiveView> {
    let origin = lookup_observed(observation, origin_message_doc_id)?;
    // Lean `forkMetadataMatches`: the fork retains the origin's native
    // identity, blocks, creation provenance, sequence, outcome and role; only
    // its own id, session, key and request may change. Lean forkHeader
    // preserves the fork child's own payload references, so they are not
    // compared here. Any metadata failure is invalid.
    origin::validate_fork_metadata(fork, origin).map_err(|_| LiveView::Invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{
        OutputOutcome, OutputSegment, SegmentRun, SourceClose, StreamDeclaration, StreamPayload,
    };
    use crate::rendered_request::{CaptureScope, CaptureScopeKind};

    fn source() -> OutputSource {
        OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: CaptureScopeKind::Inference,
                seq: 1,
            },
            turn_index: 0,
            attempt: 0,
        }
    }

    fn writer() -> OutputWriter {
        OutputWriter::RequestExecution {
            execution_generation: "gen-7".to_string(),
        }
    }

    fn text_declaration(block_index: u32) -> StreamDeclaration {
        StreamDeclaration {
            block_index,
            part_index: 0,
            payload: StreamPayload::Text,
        }
    }

    fn opaque_declaration(block_index: u32) -> StreamDeclaration {
        StreamDeclaration {
            block_index,
            part_index: 0,
            payload: StreamPayload::ReasoningEncrypted,
        }
    }

    fn run(stream: u32, bytes: u32, declaration: Option<StreamDeclaration>) -> SegmentRun {
        SegmentRun {
            stream,
            bytes,
            declaration,
        }
    }

    fn stamp(ordinal: u32) -> String {
        format!("2026-01-01T00:{ordinal:02}:00Z")
    }

    fn segment(
        doc_id: &str,
        ordinal: Option<u32>,
        runs: Vec<SegmentRun>,
        payload: &str,
        close: Option<SourceClose>,
        created_at: String,
    ) -> (String, OutputSegment) {
        (
            doc_id.to_string(),
            OutputSegment {
                agent_did: "did:key:z6MkAgent".to_string(),
                requester_did: None,
                session_id: "session-1".to_string(),
                request_doc_id: "request-1".to_string(),
                source: source(),
                writer: writer(),
                ordinal,
                runs,
                payload: payload.to_string(),
                close,
                created_at,
            },
        )
    }

    fn observed(records: &[(String, OutputSegment)]) -> Vec<ObservedSegment<'_>> {
        records
            .iter()
            .map(|(doc_id, segment)| ObservedSegment { doc_id, segment })
            .collect()
    }

    fn prefix(
        records: &[(String, OutputSegment)],
        limit: Option<u32>,
    ) -> Result<DensePrefix, DensePrefixError> {
        reconstruct_dense_prefix(&observed(records), "request-1", &source(), &writer(), limit)
    }

    /// Lean `sampleOpen`: one flush opening a Text stream (65 = "A") and an
    /// opaque reasoning stream (66 = "B"), with no closing record.
    fn sample_open() -> Vec<(String, OutputSegment)> {
        vec![segment(
            "flush-0",
            Some(0),
            vec![
                run(0, 1, Some(text_declaration(0))),
                run(1, 1, Some(opaque_declaration(1))),
            ],
            "AB",
            None,
            stamp(0),
        )]
    }

    #[test]
    fn audit_prefix_keeps_hidden_fields_while_live_presentation_filters_them() {
        let records = vec![segment(
            "flush-0",
            Some(0),
            vec![
                run(
                    0,
                    1,
                    Some(StreamDeclaration {
                        block_index: 0,
                        part_index: 0,
                        payload: StreamPayload::Reasoning,
                    }),
                ),
                run(
                    1,
                    1,
                    Some(StreamDeclaration {
                        block_index: 0,
                        part_index: 0,
                        payload: StreamPayload::ReasoningSignature,
                    }),
                ),
                run(
                    2,
                    1,
                    Some(StreamDeclaration {
                        block_index: 0,
                        part_index: 1,
                        payload: StreamPayload::ReasoningEncrypted,
                    }),
                ),
                run(
                    3,
                    1,
                    Some(StreamDeclaration {
                        block_index: 0,
                        part_index: 2,
                        payload: StreamPayload::ReasoningRedacted,
                    }),
                ),
            ],
            "BSER",
            None,
            stamp(0),
        )];
        let audit =
            reconstruct_dense_prefix(&observed(&records), "request-1", &source(), &writer(), None)
                .unwrap();
        assert_eq!(audit.streams.len(), 4);
        assert_eq!(audit.streams[1].text, "S");
        let presented = LiveView::visible_streams(
            audit
                .streams
                .into_iter()
                .enumerate()
                .map(|(stream, value)| LiveStream {
                    source: source(),
                    stream: stream as u32,
                    declaration: value.declaration,
                    state: LiveStreamState::Unclosed,
                    text: value.text,
                })
                .collect(),
        );
        assert_eq!(presented.len(), 1);
        assert_eq!(presented[0].text, "B");
    }

    /// Lean `sampleOpenContinuation`: continues both streams with 67, 68.
    fn sample_open_continuation() -> (String, OutputSegment) {
        segment(
            "flush-1",
            Some(1),
            vec![run(0, 1, None), run(1, 1, None)],
            "CD",
            None,
            stamp(1),
        )
    }

    /// Lean `sampleComplete`: the same flush, closed Complete with a
    /// one-segment extent.
    fn sample_complete() -> Vec<(String, OutputSegment)> {
        vec![segment(
            "flush-0",
            Some(0),
            vec![
                run(0, 1, Some(text_declaration(0))),
                run(1, 1, Some(opaque_declaration(1))),
            ],
            "AB",
            Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 1,
                stream_bytes: vec![1, 1],
            }),
            stamp(0),
        )]
    }

    /// Lean `sampleLateBeyondExtent`: a distinct, closureless flush at ordinal
    /// 1, beyond the committed one-segment extent, from `writer_gen`.
    fn sample_late_beyond_extent(writer_gen: &str) -> (String, OutputSegment) {
        let (doc_id, mut segment) = segment(
            "late-1",
            Some(1),
            vec![run(0, 1, None)],
            "Z",
            None,
            stamp(1),
        );
        segment.writer = OutputWriter::RequestExecution {
            execution_generation: writer_gen.to_string(),
        };
        (doc_id, segment)
    }

    #[test]
    fn contiguous_open_source_reconstructs_prefix_with_opaque_evidence() {
        let records = sample_open();
        let prefix = prefix(&records, None).expect("reconstructs");
        assert_eq!(prefix.segments, 1);
        assert_eq!(prefix.highest_visible_ordinal, Some(0));
        assert_eq!(prefix.streams.len(), 2);
        assert_eq!(prefix.streams[0].declaration, text_declaration(0));
        assert_eq!(prefix.streams[0].text, "A");
        // Opaque declarations are evidence for the later classifier; only the
        // rendering step drops them, as Lean's `visibleStreams` does.
        assert_eq!(prefix.streams[1].declaration, opaque_declaration(1));
        assert_eq!(prefix.streams[1].text, "B");
    }

    /// Lean `valid_open_append_extends_live_preview`: a valid contiguous
    /// append extends the preview, and the earlier preview is a byte prefix
    /// of the extension.
    #[test]
    fn valid_open_append_extends_live_preview() {
        let before = sample_open();
        let mut after = before.clone();
        after.push(sample_open_continuation());
        let initial = prefix(&before, None).expect("initial prefix");
        let extended = prefix(&after, None).expect("extended prefix");
        assert_eq!(initial.streams[0].text, "A");
        assert_eq!(extended.streams[0].text, "AC");
        assert_eq!(extended.streams[1].text, "BD");
        assert_eq!(extended.segments, 2);
        assert!(extended.streams[0]
            .text
            .as_bytes()
            .starts_with(initial.streams[0].text.as_bytes()));
        assert!(extended.streams[1]
            .text
            .as_bytes()
            .starts_with(initial.streams[1].text.as_bytes()));
    }

    /// A later ordinal arriving before an intermediate one must not erase the
    /// earlier preview; the gap only fills forward.
    #[test]
    fn later_ordinal_arriving_before_intermediate_preserves_prefix() {
        let mut records = sample_open();
        records.push(segment(
            "flush-2",
            Some(2),
            vec![run(0, 1, None)],
            "E",
            None,
            stamp(2),
        ));
        let gapped = prefix(&records, None).expect("prefix stops at the gap");
        assert_eq!(gapped.segments, 1);
        assert_eq!(gapped.streams[0].text, "A");
        assert_eq!(gapped.highest_visible_ordinal, Some(2));
        records.push(sample_open_continuation());
        let filled = prefix(&records, None).expect("gap filled");
        assert_eq!(filled.segments, 3);
        assert_eq!(filled.streams[0].text, "ACE");
        assert_eq!(filled.streams[1].text, "BD");
    }

    /// Lean `missing_open_prefix_is_loading` and the empty-flushes rule: no
    /// contiguous prefix yet is `Loading`, never a shortened answer.
    #[test]
    fn missing_open_prefix_is_loading() {
        assert_eq!(prefix(&[], None), Err(DensePrefixError::Loading));
        let only_later = vec![segment(
            "flush-1",
            Some(1),
            vec![run(0, 1, Some(text_declaration(0)))],
            "A",
            None,
            stamp(1),
        )];
        assert_eq!(prefix(&only_later, None), Err(DensePrefixError::Loading));
        let closure_only = vec![segment(
            "close-0",
            None,
            Vec::new(),
            "",
            Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 1,
                stream_bytes: vec![1],
            }),
            stamp(0),
        )];
        assert_eq!(prefix(&closure_only, None), Err(DensePrefixError::Loading));
    }

    /// Lean `late_in_extent_twin_is_a_conflict` (Executable.lean L292): the
    /// witness twin carries `createdAt := 6` against `5` at the same ordinal.
    /// The pinned `.conflicted` view is produced by the *closed* path
    /// (`projectPublished` → `reconstructExtent` → `flushAt`/`uniqueRecord`),
    /// which never applies the timestamp rule. On the *open* prefix path
    /// `reconstructPrefix` (State.lean L182) classifies the same records
    /// `.invalid` because `timestampsNondecreasing` demands equal timestamps
    /// for equal ordinals. Either way no winner is selected and the
    /// disagreeing evidence is preserved for the classifier.
    #[test]
    fn late_in_extent_twin_with_later_timestamp_is_invalid_on_the_open_path() {
        let mut records = sample_complete();
        records.push(segment(
            "twin-0",
            Some(0),
            vec![run(0, 1, Some(text_declaration(0)))],
            "Z",
            None,
            stamp(1),
        ));
        assert!(matches!(
            prefix(&records, Some(1)),
            Err(DensePrefixError::Invalid { .. })
        ));
        assert!(matches!(
            prefix(&records, None),
            Err(DensePrefixError::Invalid { .. })
        ));
    }

    /// Lean's `Nodup` branch of `reconstructPrefix`: visible coordinate twins
    /// at one ordinal with non-regressing (here equal) timestamps are
    /// `.conflicted`, bounded or not. No winner is chosen.
    #[test]
    fn coordinate_twins_at_one_ordinal_are_conflicted() {
        let mut records = sample_complete();
        records.push(segment(
            "twin-0",
            Some(0),
            vec![run(0, 1, Some(text_declaration(0)))],
            "Z",
            None,
            stamp(0),
        ));
        assert_eq!(
            prefix(&records, Some(1)),
            Err(DensePrefixError::Conflicted { ordinal: 0 })
        );
        assert_eq!(
            prefix(&records, None),
            Err(DensePrefixError::Conflicted { ordinal: 0 })
        );
    }

    /// Lean `benign_late_record_beyond_extent_preserves_publication`: a
    /// distinct late record beyond the committed extent leaves the bounded
    /// preview exactly as it was.
    #[test]
    fn benign_late_record_beyond_extent_is_inert_for_the_bounded_preview() {
        let mut records = sample_complete();
        records.push(sample_late_beyond_extent("gen-7"));
        assert_eq!(
            prefix(&records, Some(1)),
            prefix(&sample_complete(), Some(1))
        );
    }

    /// Lean `closed_preview_ignores_malformed_data_beyond_committed_extent`:
    /// the bounded form excludes records at or beyond the extent before any
    /// check, while the unbounded form preserves the disagreement as evidence.
    #[test]
    fn malformed_records_beyond_committed_extent_are_inert_only_bounded() {
        let mut records = sample_complete();
        records.push(sample_late_beyond_extent("gen-99"));
        assert_eq!(
            prefix(&records, Some(1)),
            prefix(&sample_complete(), Some(1))
        );
        assert!(matches!(
            prefix(&records, None),
            Err(DensePrefixError::Invalid { .. })
        ));
    }

    /// Lean `stale_generation_open_source_is_not_live`: a superseded writer
    /// never contributes prefix bytes; the disagreement is invalid evidence.
    #[test]
    fn stale_generation_prefix_records_are_invalid_evidence() {
        let mut records = sample_open();
        records[0].1.writer = OutputWriter::RequestExecution {
            execution_generation: "gen-8".to_string(),
        };
        assert!(matches!(
            prefix(&records, None),
            Err(DensePrefixError::Invalid { .. })
        ));
        let mut mixed = sample_open();
        mixed.push(sample_open_continuation());
        mixed.last_mut().unwrap().1.writer = OutputWriter::RequestExecution {
            execution_generation: "gen-8".to_string(),
        };
        assert!(matches!(
            prefix(&mixed, None),
            Err(DensePrefixError::Invalid { .. })
        ));
    }

    /// Lean `malformed_sealed_payload_is_invalid`: declared run lengths that
    /// exceed the immutable payload are invalid.
    #[test]
    fn malformed_sealed_run_lengths_are_invalid() {
        let records = vec![segment(
            "flush-0",
            Some(0),
            vec![
                run(0, 2, Some(text_declaration(0))),
                run(1, 1, Some(opaque_declaration(1))),
            ],
            "AB",
            Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 1,
                stream_bytes: vec![1, 1],
            }),
            stamp(0),
        )];
        assert!(matches!(
            prefix(&records, None),
            Err(DensePrefixError::Invalid { .. })
        ));
    }

    #[test]
    fn zero_byte_continuation_is_invalid() {
        let records = vec![
            segment(
                "flush-0",
                Some(0),
                vec![run(0, 1, Some(text_declaration(0)))],
                "A",
                None,
                stamp(0),
            ),
            segment(
                "flush-1",
                Some(1),
                vec![run(0, 0, None)],
                "",
                None,
                stamp(1),
            ),
        ];
        assert!(matches!(
            prefix(&records, None),
            Err(DensePrefixError::Invalid { .. })
        ));
    }

    /// An empty native string is an opening run of zero bytes.
    #[test]
    fn explicitly_declared_empty_stream_is_preserved() {
        let records = vec![segment(
            "flush-0",
            Some(0),
            vec![run(0, 0, Some(text_declaration(0)))],
            "",
            None,
            stamp(0),
        )];
        let prefix = prefix(&records, None).expect("empty opening run");
        assert_eq!(prefix.segments, 1);
        assert_eq!(prefix.streams.len(), 1);
        assert!(prefix.streams[0].text.is_empty());
    }

    #[test]
    fn run_split_inside_utf8_sequence_is_invalid() {
        let records = vec![segment(
            "flush-0",
            Some(0),
            vec![run(0, 1, Some(text_declaration(0)))],
            "é",
            None,
            stamp(0),
        )];
        assert!(matches!(
            prefix(&records, None),
            Err(DensePrefixError::Invalid { .. })
        ));
    }

    #[test]
    fn duplicate_native_declaration_position_is_invalid() {
        let records = vec![segment(
            "flush-0",
            Some(0),
            vec![
                run(0, 1, Some(text_declaration(0))),
                run(1, 1, Some(text_declaration(0))),
            ],
            "ab",
            None,
            stamp(0),
        )];
        assert!(matches!(
            prefix(&records, None),
            Err(DensePrefixError::Invalid { .. })
        ));
    }

    #[test]
    fn streams_open_densely_from_zero() {
        let records = vec![segment(
            "flush-0",
            Some(0),
            vec![run(1, 1, Some(text_declaration(0)))],
            "A",
            None,
            stamp(0),
        )];
        assert!(matches!(
            prefix(&records, None),
            Err(DensePrefixError::Invalid { .. })
        ));
    }

    #[test]
    fn continuation_before_declaration_is_invalid() {
        let records = vec![segment(
            "flush-0",
            Some(0),
            vec![run(2, 1, None)],
            "A",
            None,
            stamp(0),
        )];
        assert!(matches!(
            prefix(&records, None),
            Err(DensePrefixError::Invalid { .. })
        ));
    }

    #[test]
    fn runs_must_partition_payload_exactly() {
        let records = vec![segment(
            "flush-0",
            Some(0),
            vec![run(0, 1, Some(text_declaration(0)))],
            "AB",
            None,
            stamp(0),
        )];
        assert!(matches!(
            prefix(&records, None),
            Err(DensePrefixError::Invalid { .. })
        ));
    }

    #[test]
    fn prefix_flush_timestamps_must_not_decrease() {
        let mut records = sample_open();
        records.push(segment(
            "flush-1",
            Some(1),
            vec![run(0, 1, None)],
            "C",
            None,
            "2025-12-31T23:59:59Z".to_string(),
        ));
        assert!(matches!(
            prefix(&records, None),
            Err(DensePrefixError::Invalid { .. })
        ));
    }

    /// The timestamp check is order-insensitive: any permutation of the same
    /// visible records yields the same classification. Here the regressing
    /// record arrives first, before the earlier ordinal, and the check must
    /// still classify invalid.
    #[test]
    fn timestamp_regression_is_invalid_in_any_arrival_order() {
        let regressing = |records: &mut Vec<(String, OutputSegment)>| {
            records.push(segment(
                "flush-1",
                Some(1),
                vec![run(0, 1, None)],
                "C",
                None,
                "2025-12-31T23:59:59Z".to_string(),
            ));
        };
        let mut late_first = sample_open();
        regressing(&mut late_first);
        assert!(matches!(
            prefix(&late_first, None),
            Err(DensePrefixError::Invalid { .. })
        ));

        let mut interleaved = sample_open();
        interleaved.push(segment(
            "flush-2",
            Some(2),
            vec![run(0, 1, None)],
            "E",
            None,
            stamp(2),
        ));
        regressing(&mut interleaved);
        assert!(matches!(
            prefix(&interleaved, None),
            Err(DensePrefixError::Invalid { .. })
        ));
    }

    /// Equal ordinals (visible twins) must carry equal timestamps: a
    /// disagreement in either direction is invalid, never a winner. The
    /// sorted-ordinal adjacent check must catch both directions.
    #[test]
    fn equal_ordinal_twins_with_differing_timestamps_are_invalid_both_directions() {
        let twin = |created_at: String| {
            segment(
                "twin-0",
                Some(0),
                vec![run(0, 1, Some(text_declaration(0)))],
                "Z",
                None,
                created_at,
            )
        };
        // Twin timestamp LATER than the original ordinal-0 timestamp.
        let mut later_twin = sample_open();
        later_twin.push(twin(stamp(1)));
        assert!(matches!(
            prefix(&later_twin, None),
            Err(DensePrefixError::Invalid { .. })
        ));
        // Twin timestamp EARLIER than the original ordinal-0 timestamp.
        let mut earlier_twin = sample_open();
        earlier_twin.push(twin("2025-12-31T23:59:59Z".to_string()));
        assert!(matches!(
            prefix(&earlier_twin, None),
            Err(DensePrefixError::Invalid { .. })
        ));
    }

    /// Classification order is preserved: a timestamp-disagreeing twin is
    /// `Invalid` even though it is also a coordinate twin that would
    /// otherwise classify `Conflicted`. Shuffling the arrival order must not
    /// change which error wins.
    #[test]
    fn timestamp_disagreement_classifies_invalid_before_conflicted() {
        let mut records = sample_open();
        records.push(segment(
            "twin-0",
            Some(0),
            vec![run(0, 1, Some(text_declaration(0)))],
            "Z",
            None,
            stamp(3),
        ));
        assert!(matches!(
            prefix(&records, None),
            Err(DensePrefixError::Invalid { .. })
        ));
        // Same records, twin listed before the original flush.
        let mut reversed = records;
        reversed.reverse();
        assert!(matches!(
            prefix(&reversed, None),
            Err(DensePrefixError::Invalid { .. })
        ));
    }

    #[test]
    fn one_physical_identity_claiming_two_ordinals_is_invalid() {
        let mut records = sample_open();
        records.push(segment(
            "flush-0",
            Some(1),
            vec![run(0, 1, None)],
            "C",
            None,
            stamp(1),
        ));
        assert!(matches!(
            prefix(&records, None),
            Err(DensePrefixError::Invalid { .. })
        ));
    }

    /// Lean `raw_exact_record_replay_is_idempotent`: the same fact delivered
    /// twice is a replay, not a twin.
    #[test]
    fn raw_exact_record_replay_is_idempotent() {
        let mut records = sample_complete();
        records.push(records[0].clone());
        assert_eq!(prefix(&records, None), prefix(&sample_complete(), None));
    }

    /// Lean `header_arrival_preserves_valid_open_prefix_as_settling`: a
    /// visible closure bounds the preview; the validated contiguous prefix is
    /// preserved as evidence for the classifier's settling view.
    #[test]
    fn bounded_by_visible_closure_preserves_valid_open_prefix() {
        let mut records = sample_open();
        records.push(segment(
            "close-1",
            None,
            Vec::new(),
            "",
            Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 2,
                stream_bytes: vec![1, 1],
            }),
            stamp(1),
        ));
        let bounded = prefix(&records, Some(2)).expect("bounded by the closed extent");
        assert_eq!(bounded.segments, 1);
        assert_eq!(bounded.streams[0].text, "A");
        assert_eq!(bounded.highest_visible_ordinal, Some(0));
    }

    /// Writer binding follows the source, as `writerMatchesSource` models it.
    /// The later classifier decides what an authored prefix may be shown as;
    /// the primitive only reconstructs it as evidence.
    #[test]
    fn writer_binding_follows_the_source() {
        assert!(matches!(
            reconstruct_dense_prefix(
                &observed(&sample_open()),
                "request-1",
                &source(),
                &OutputWriter::ToolExecution {
                    tool_call_doc_id: "tool-1".to_string(),
                },
                None,
            ),
            Err(DensePrefixError::Invalid { .. })
        ));

        let mut tool_records = sample_open();
        tool_records[0].1.source = OutputSource::ToolCall {
            tool_call_doc_id: "tool-1".to_string(),
        };
        tool_records[0].1.writer = OutputWriter::ToolExecution {
            tool_call_doc_id: "tool-1".to_string(),
        };
        assert!(reconstruct_dense_prefix(
            &observed(&tool_records),
            "request-1",
            &OutputSource::ToolCall {
                tool_call_doc_id: "tool-1".to_string(),
            },
            &OutputWriter::ToolExecution {
                tool_call_doc_id: "tool-1".to_string(),
            },
            None,
        )
        .is_ok());

        let mut authored_records = sample_open();
        authored_records[0].1.source = OutputSource::Authored {
            key: "authored-1".to_string(),
        };
        assert!(reconstruct_dense_prefix(
            &observed(&authored_records),
            "request-1",
            &OutputSource::Authored {
                key: "authored-1".to_string(),
            },
            &writer(),
            None,
        )
        .is_ok());
    }

    // ==== Stage 2: live eligibility classifier (Lean `StreamingResponse.project`) ====
    //
    // All fixtures live in one `Fixture` struct so every borrow passed to
    // `project_live` outlives the call inside a single test function.

    use crate::message::Message;
    use crate::output::reconstruction::DependencyDenial;
    use crate::output::{
        MessageBlock, MessagePublication, MessageRole, PayloadPresentation, PayloadRef,
        PresentedPayload, TerminalOutput,
    };

    struct Fixture {
        records: Vec<(String, OutputSegment)>,
        messages: Vec<(String, TranscriptMessage)>,
        /// Header-ID denials (Lean `deniedHeaders`).
        denied_headers: Vec<String>,
        /// Segment/closure denials (Lean `deniedSegments`).
        denied_segments: Vec<String>,
        dependency_denials: Vec<DependencyDenial>,
        owner: OwnerLiveness<'static>,
        requester_did: Option<&'static str>,
        request_terminal: bool,
        terminal_selection: Option<TerminalOutput>,
        /// Target header doc ID: `Some` projects the headed path.
        target_message: Option<String>,
        target_request_doc_id: &'static str,
        target_source: OutputSource,
        target_writer: OutputWriter,
    }

    /// The observation's authorized agent DID for every fixture message.
    const AGENT_DID: &str = "did:key:z6MkAgent";

    impl Default for Fixture {
        fn default() -> Self {
            Self {
                records: sample_open(),
                messages: Vec::new(),
                denied_headers: Vec::new(),
                denied_segments: Vec::new(),
                dependency_denials: Vec::new(),
                owner: OwnerLiveness {
                    current_request: Some(("request-1", "gen-7")),
                    live_tools: Vec::new(),
                },
                requester_did: None,
                request_terminal: false,
                terminal_selection: None,
                target_message: None,
                target_request_doc_id: "request-1",
                target_source: source(),
                target_writer: writer(),
            }
        }
    }

    impl Fixture {
        fn partial_close(&mut self, segments: u32, stream_bytes: Vec<u64>) {
            self.records.push(segment(
                "close-1",
                None,
                Vec::new(),
                "",
                Some(SourceClose::Closed {
                    outcome: OutputOutcome::Partial,
                    segments,
                    stream_bytes,
                }),
                stamp(1),
            ));
        }

        fn complete_close(&mut self) {
            self.records.push(segment(
                "close-1",
                None,
                Vec::new(),
                "",
                Some(SourceClose::Closed {
                    outcome: OutputOutcome::Complete,
                    segments: 1,
                    stream_bytes: vec![1, 1],
                }),
                stamp(1),
            ));
        }

        fn retracted_close(&mut self) {
            self.records.push(segment(
                "close-1",
                None,
                Vec::new(),
                "",
                Some(SourceClose::Retracted),
                stamp(1),
            ));
        }

        fn assistant_header(&mut self, id: &str, stream: u32) {
            self.messages.push((
                id.to_string(),
                TranscriptMessage {
                    message_key: format!("key-{id}"),
                    session_id: "session-1".to_string(),
                    agent_did: "did:key:z6MkAgent".to_string(),
                    requester_did: None,
                    request_doc_id: Some("request-1".to_string()),
                    publication: MessagePublication::RequestExecution {
                        execution_generation: "gen-7".to_string(),
                    },
                    outcome: OutputOutcome::Complete,
                    sequence: 1,
                    role: MessageRole::Assistant,
                    native_id: Some("native-1".to_string()),
                    blocks: vec![MessageBlock::Text {
                        text: PresentedPayload {
                            output: PayloadRef {
                                close_doc_id: "close-1".to_string(),
                                stream,
                            },
                            presentation: PayloadPresentation::Full,
                        },
                    }],
                    created_at: stamp(9),
                },
            ));
        }

        /// The Lean `StreamingResponse.project` decision sequence over the
        /// fixture's borrows.
        fn view(&self) -> LiveView {
            let observed_records: Vec<ObservedSegment<'_>> = observed(&self.records);
            let message_refs: Vec<(&str, &TranscriptMessage)> = self
                .messages
                .iter()
                .map(|(id, message)| (id.as_str(), message))
                .collect();
            let observation = LiveObservation {
                request_doc_id: "request-1",
                session_id: "session-1",
                agent_did: AGENT_DID,
                requester_did: self.requester_did,
                target: LiveTarget {
                    request_doc_id: self.target_request_doc_id,
                    source: &self.target_source,
                    writer: &self.target_writer,
                    message_id: self.target_message.as_deref(),
                },
                messages: &message_refs,
                records: &observed_records,
                denied_headers: &self.denied_headers,
                denied_segments: &self.denied_segments,
                dependency_denials: &self.dependency_denials,
                owner: self.owner.clone(),
                request_terminal: self.request_terminal,
                terminal_selection: self.terminal_selection.clone(),
            };
            project_live(&observation)
        }
    }

    #[test]
    fn live_open_source_with_live_owner() {
        let fixture = Fixture::default();
        assert!(matches!(fixture.view(), LiveView::Live { .. }));
    }

    /// Lean `targetScoped`: the target coordinate cannot be projected through
    /// an observation for another physical request.
    #[test]
    fn mismatched_target_request_is_invalid() {
        let fixture = Fixture {
            target_request_doc_id: "request-2",
            ..Default::default()
        };
        assert_eq!(fixture.view(), LiveView::Invalid);
    }

    /// Lean `authored_without_header_is_not_live`: authored bytes become
    /// visible only through their immutable header, never as an open preview.
    #[test]
    fn authored_without_header_is_not_live() {
        let authored = OutputSource::Authored {
            key: "authored-1".to_owned(),
        };
        let mut fixture = Fixture {
            target_source: authored.clone(),
            ..Default::default()
        };
        for (_, record) in &mut fixture.records {
            record.source = authored.clone();
        }
        assert_eq!(fixture.view(), LiveView::Absent);
    }

    /// Lean `tool_partial_awaits_delivery`: terminal request selection does
    /// not turn a closed tool source into retained provider diagnostics.
    #[test]
    fn tool_partial_awaits_delivery() {
        let tool_source = OutputSource::ToolCall {
            tool_call_doc_id: "tool-1".to_owned(),
        };
        let tool_writer = OutputWriter::ToolExecution {
            tool_call_doc_id: "tool-1".to_owned(),
        };
        let mut fixture = Fixture {
            target_source: tool_source.clone(),
            target_writer: tool_writer.clone(),
            request_terminal: true,
            terminal_selection: Some(TerminalOutput::NoMessage),
            ..Default::default()
        };
        fixture.partial_close(1, vec![1, 1]);
        for (_, record) in &mut fixture.records {
            record.source = tool_source.clone();
            record.writer = tool_writer.clone();
        }
        assert!(matches!(fixture.view(), LiveView::Settling { .. }));
    }

    /// Lean `.absent`: a live-shaped source without a live owner is absent —
    /// never `Loading`.
    #[test]
    fn open_source_without_live_owner_is_absent() {
        let fixture = Fixture {
            owner: OwnerLiveness::default(),
            ..Default::default()
        };
        assert_eq!(fixture.view(), LiveView::Absent);
    }

    /// Lean `.denied` gate before owner liveness on the open path. The
    /// flushed record is a segment: only the segment namespace denies it.
    #[test]
    fn denied_open_source_is_denied_even_when_owner_live() {
        let fixture = Fixture {
            denied_segments: vec!["flush-0".to_string()],
            ..Default::default()
        };
        assert_eq!(fixture.view(), LiveView::Denied);
    }

    /// Lean namespace isolation: a header-ID denial never denies a segment
    /// with an identical ID, and a segment denial never denies a header. The
    /// open source's flush is observed, so a header-namespace denial of the
    /// same ID cannot turn it `Denied` — the classification must be exactly
    /// what the un-denied observation produces (`Live`: observed open source
    /// with a live owner).
    #[test]
    fn header_denial_does_not_leak_into_segment_namespace() {
        let fixture = Fixture {
            denied_headers: vec!["flush-0".to_string()],
            ..Default::default()
        };
        assert_eq!(fixture.view(), Fixture::default().view());

        // And the converse: a segment denial of a *header* ID does not make
        // the headed target `Denied` — the header publishes if it otherwise
        // reconstructs.
        let mut headed = Fixture::default();
        headed.complete_close();
        headed.assistant_header("header-1", 0);
        headed.target_message = Some("header-1".to_string());
        headed.denied_segments = vec!["header-1".to_string()];
        assert!(matches!(headed.view(), LiveView::Published { .. }));
    }

    /// Lean `.retracted`: abandonment before retry backoff.
    #[test]
    fn retracted_closure_classifies_retracted() {
        let mut fixture = Fixture::default();
        fixture.retracted_close();
        assert_eq!(fixture.view(), LiveView::Retracted);
    }

    /// Lean `.conflict` from `observeClose`: two distinct closures of one
    /// source conflict; a replicated identical closure never does.
    #[test]
    fn conflicting_closures_conflict_but_replication_does_not() {
        let mut fixture = Fixture::default();
        fixture.complete_close();
        fixture.records.push(segment(
            "close-2",
            None,
            Vec::new(),
            "",
            Some(SourceClose::Closed {
                outcome: OutputOutcome::Partial,
                segments: 1,
                stream_bytes: vec![1, 1],
            }),
            stamp(2),
        ));
        assert_eq!(fixture.view(), LiveView::Conflicted);

        // Exact replication of the same closure document is not a twin.
        let mut replicated = Fixture::default();
        replicated.complete_close();
        replicated.records.push(replicated.records[1].clone());
        assert_ne!(replicated.view(), LiveView::Conflicted);
    }

    /// Lean `.settling`: closure is known, the bounded prefix reconstructs,
    /// and unclosed states become `PendingPublication` with the closure's
    /// outcome.
    #[test]
    fn closed_complete_source_settles_with_pending_publication() {
        let mut fixture = Fixture::default();
        fixture.complete_close();
        let view = fixture.view();
        match view {
            LiveView::Settling { streams } => {
                assert_eq!(streams.len(), 1, "opaque dropped for rendering");
                assert_eq!(streams[0].text, "A");
                assert_eq!(
                    streams[0].state,
                    LiveStreamState::PendingPublication {
                        outcome: OutputOutcome::Complete
                    }
                );
            }
            other => panic!("expected Settling, got {other:?}"),
        }
    }

    /// Lean `reconstructExtent` + `retainedStreams`: a closed Partial provider
    /// source under terminal selection with no referencing headers keeps
    /// unreferenced, non-opaque streams as `RetainedPartial` diagnostics.
    #[test]
    fn closed_partial_without_headers_retains_unreferenced_streams() {
        let mut fixture = Fixture::default();
        fixture.partial_close(1, vec![1, 1]);
        fixture.request_terminal = true;
        fixture.terminal_selection = Some(TerminalOutput::NoMessage);
        let view = fixture.view();
        match view {
            LiveView::RetainedPartial { streams } => {
                assert_eq!(streams.len(), 1, "opaque stream stays dropped");
                assert_eq!(streams[0].text, "A");
            }
            other => panic!("expected RetainedPartial, got {other:?}"),
        }
    }

    /// Lean `retainedStreams`: a stream referenced by a resolved header is not
    /// retained as a diagnostic.
    #[test]
    fn closed_partial_referenced_streams_are_not_retained() {
        let mut fixture = Fixture::default();
        fixture.partial_close(1, vec![1, 1]);
        fixture.assistant_header("header-1", 0);
        fixture.request_terminal = true;
        fixture.terminal_selection = Some(TerminalOutput::NoMessage);
        let view = fixture.view();
        match view {
            LiveView::RetainedPartial { streams } => {
                assert!(
                    streams.is_empty(),
                    "referenced stream is published through the header, not retained"
                );
            }
            other => panic!("expected RetainedPartial, got {other:?}"),
        }
    }

    /// Lean `projectUnheadedClosed`: without a terminal row the closed source
    /// settles regardless of outcome — no retained diagnostics before the
    /// terminalization owner has spoken.
    #[test]
    fn closed_partial_without_terminal_settles() {
        let mut fixture = Fixture::default();
        fixture.partial_close(1, vec![1, 1]);
        assert!(matches!(fixture.view(), LiveView::Settling { .. }));
    }

    /// Lean `reconstructExtent` byte accounting: a sealed stream length that
    /// disagrees with the assembled extent is `Invalid`, never a shorter
    /// partial.
    #[test]
    fn extent_byte_mismatch_is_invalid() {
        let mut fixture = Fixture::default();
        fixture.partial_close(1, vec![9, 1]);
        fixture.request_terminal = true;
        fixture.terminal_selection = Some(TerminalOutput::NoMessage);
        assert_eq!(fixture.view(), LiveView::Invalid);
    }

    /// Lean `.missingSelection`: a terminal request with no observed selection
    /// is loading — it never settles as a nonterminal, even over a closed
    /// partial source.
    #[test]
    fn terminal_without_selection_is_loading() {
        let mut fixture = Fixture::default();
        fixture.partial_close(1, vec![1, 1]);
        fixture.request_terminal = true;
        fixture.terminal_selection = None;
        assert_eq!(fixture.view(), LiveView::Loading);
    }

    /// Lean `reconstructExtent` `.missingOrdinal` fallback: a gap inside the
    /// sealed extent falls back to the bounded settling view, never `Invalid`.
    #[test]
    fn gap_inside_sealed_extent_falls_back_to_settling() {
        let mut fixture = Fixture::default();
        // Sealed extent of two with only ordinal 0 visible: ordinal 1 is a
        // gap inside the sealed extent.
        fixture.partial_close(2, vec![2, 1]);
        fixture.terminal_selection = Some(TerminalOutput::NoMessage);
        let view = fixture.view();
        assert!(
            matches!(view, LiveView::Settling { .. }),
            "missingOrdinal must fall back to settling, got {view:?}"
        );
    }

    /// Lean `reconstructExtent` `.conflictingOrdinal`: a twin inside the
    /// sealed extent conflicts, not settles.
    #[test]
    fn twin_inside_sealed_extent_conflicts() {
        let mut fixture = Fixture::default();
        fixture.records.push(segment(
            "flush-1",
            Some(0),
            vec![run(0, 1, Some(text_declaration(0)))],
            "Z",
            None,
            stamp(1),
        ));
        fixture.partial_close(1, vec![1, 1]);
        fixture.request_terminal = true;
        fixture.terminal_selection = Some(TerminalOutput::NoMessage);
        assert_eq!(fixture.view(), LiveView::Conflicted);
    }

    /// Lean `projectPublished`: a target header whose payload references a
    /// reconstructable closed source publishes the exact native message.
    #[test]
    fn headed_target_with_reconstructable_source_publishes() {
        let mut fixture = Fixture::default();
        fixture.complete_close();
        fixture.assistant_header("header-1", 0);
        fixture.target_message = Some("header-1".to_string());
        let view = fixture.view();
        match view {
            LiveView::Published { message, native } => {
                assert_eq!(message, fixture.messages[0].1);
                assert_eq!(
                    native,
                    Message::Assistant {
                        id: Some("native-1".to_string()),
                        content: vec![crate::message::AssistantContent::text("A")],
                    }
                );
            }
            other => panic!("expected Published, got {other:?}"),
        }
    }

    /// Lean `resolveTerminal`'s ineligibility rule over the target header: a
    /// user-role target cannot publish.
    #[test]
    fn non_assistant_target_header_is_invalid() {
        let mut fixture = Fixture::default();
        fixture.complete_close();
        fixture.assistant_header("header-1", 0);
        fixture.messages[0].1.role = MessageRole::User;
        fixture.target_message = Some("header-1".to_string());
        assert_eq!(fixture.view(), LiveView::Invalid);
    }

    /// Lean `lookupMessage`'s coordinate sweep: a distinct message sharing the
    /// target's session with its key or sequence conflicts.
    #[test]
    fn message_coordinate_conflict_conflicts() {
        let mut fixture = Fixture::default();
        fixture.complete_close();
        fixture.assistant_header("header-1", 0);
        let mut twin = fixture.messages[0].1.clone();
        twin.sequence = 2;
        twin.message_key = fixture.messages[0].1.message_key.clone();
        fixture.messages.push(("header-2".to_string(), twin));
        fixture.target_message = Some("header-1".to_string());
        assert_eq!(fixture.view(), LiveView::Conflicted);
    }

    /// Lean `messageCoordinateConflict`'s sequence clause: a distinct physical
    /// header ID at the same session and sequence conflicts even when the
    /// content is equal — only exact replication of the same envelope (same
    /// header ID) dedups, so equal content does not paper over a twin.
    #[test]
    fn same_sequence_equal_content_twin_with_distinct_header_conflicts() {
        let mut fixture = Fixture::default();
        fixture.complete_close();
        fixture.assistant_header("header-1", 0);
        let twin = fixture.messages[0].1.clone();
        fixture.messages.push(("header-2".to_string(), twin));
        fixture.target_message = Some("header-1".to_string());
        assert_eq!(fixture.view(), LiveView::Conflicted);
    }

    /// Lean `.wrongScope`: a target header bound to another request cannot
    /// publish.
    #[test]
    fn target_header_wrong_request_scope_is_invalid() {
        let mut fixture = Fixture::default();
        fixture.complete_close();
        fixture.assistant_header("header-1", 0);
        fixture.messages[0].1.request_doc_id = Some("request-2".to_string());
        fixture.target_message = Some("header-1".to_string());
        assert_eq!(fixture.view(), LiveView::Invalid);
    }

    /// Lean `resolveTerminalPayload`'s dependency-denial rule on the target
    /// header: a referenced root close under an established denial is
    /// `Denied`, never loading or partial.
    #[test]
    fn target_header_dependency_denial_is_denied() {
        let mut fixture = Fixture::default();
        fixture.complete_close();
        fixture.assistant_header("header-1", 0);
        fixture.target_message = Some("header-1".to_string());
        fixture.dependency_denials = vec![DependencyDenial {
            root_close_id: "close-1".to_string(),
            denied_doc_id: "flush-0".to_string(),
        }];
        assert_eq!(fixture.view(), LiveView::Denied);
    }

    /// Lean `messageAt`'s conflicting-envelope rule: two distinct messages
    /// under one target doc ID conflict; replicated delivery does not.
    #[test]
    fn target_envelope_twin_conflicts_but_replication_does_not() {
        let mut fixture = Fixture::default();
        fixture.complete_close();
        fixture.assistant_header("header-1", 0);
        let mut twin = fixture.messages[0].1.clone();
        twin.native_id = Some("native-2".to_string());
        fixture.messages.push(("header-1".to_string(), twin));
        fixture.target_message = Some("header-1".to_string());
        assert_eq!(fixture.view(), LiveView::Conflicted);

        // Exact replication of the same envelope: dedup, no conflict.
        let mut replicated = Fixture::default();
        replicated.complete_close();
        replicated.assistant_header("header-1", 0);
        replicated.messages.push(replicated.messages[0].clone());
        replicated.target_message = Some("header-1".to_string());
        assert_ne!(replicated.view(), LiveView::Conflicted);
    }

    #[test]
    fn target_selector_uses_latest_inference_scope_and_unique_header() {
        let mut fixture = Fixture::default();
        fixture.complete_close();
        fixture.assistant_header("header-1", 0);
        let mut later = fixture.records[0].1.clone();
        later.source = OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: CaptureScopeKind::Inference,
                seq: 2,
            },
            turn_index: 0,
            attempt: 0,
        };
        fixture.records.push(("later-flush".to_string(), later));
        let records = observed(&fixture.records);
        let messages = fixture
            .messages
            .iter()
            .map(|(id, message)| (id.as_str(), message))
            .collect::<Vec<_>>();
        assert!(matches!(
            select_live_target("request-1", "gen-7", &records, &messages),
            LiveTargetSelection::Selected {
                source: OutputSource::ProviderTurn {
                    scope: CaptureScope { seq: 2, .. },
                    ..
                },
                message_id: None,
                ..
            }
        ));
    }

    #[test]
    fn target_selector_rejects_header_ambiguity_and_stale_writer_reference() {
        let mut fixture = Fixture::default();
        fixture.complete_close();
        fixture.assistant_header("header-1", 0);
        fixture.assistant_header("header-2", 0);
        let records = observed(&fixture.records);
        let messages = fixture
            .messages
            .iter()
            .map(|(id, message)| (id.as_str(), message))
            .collect::<Vec<_>>();
        assert_eq!(
            select_live_target("request-1", "gen-7", &records, &messages),
            LiveTargetSelection::Conflicted
        );

        fixture.messages.truncate(1);
        fixture.records.last_mut().unwrap().1.writer = OutputWriter::RequestExecution {
            execution_generation: "gen-8".to_string(),
        };
        let records = observed(&fixture.records);
        let messages = fixture
            .messages
            .iter()
            .map(|(id, message)| (id.as_str(), message))
            .collect::<Vec<_>>();
        assert!(matches!(
            select_live_target("request-1", "gen-7", &records, &messages),
            LiveTargetSelection::Selected {
                message_id: None,
                ..
            }
        ));
    }

    #[test]
    fn durable_owner_observation_rejects_pending_with_stray_generation() {
        use crate::request_lifecycle::RequestLifecycleState;
        let mut request = crate::row::AgentRequestRow {
            doc_id: Some("request-1".to_string()),
            request_id: "logical-1".to_string(),
            lifecycle_state: Some(RequestLifecycleState::Pending),
            execution_generation: Some("gen-7".to_string()),
            execution_lease_secs: Some(30),
            execution_lease_expires_at: Some("2026-01-01T00:00:30Z".to_string()),
            ..Default::default()
        };
        assert_eq!(observed_request_execution_owner(&request), None);
        request.lifecycle_state = Some(RequestLifecycleState::Processing);
        assert_eq!(
            observed_request_execution_owner(&request),
            Some(("request-1", "gen-7"))
        );
        request.execution_lease_expires_at = None;
        assert_eq!(observed_request_execution_owner(&request), None);
    }
}
