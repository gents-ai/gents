use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::output::{
    MessagePublication, MessageRole, OutputOutcome, OutputSource, OutputWriter, SourceClose,
};
use std::collections::{BTreeMap, BTreeSet};

use super::super::canonical_rows::{decode_transcript_message_row, AGENT_MESSAGE_FIELDS};
use super::super::query::session_scope_filter;
use super::access::{rows_value, ReadAccess, ReadCache};
use super::reconstruct::{preload_request_output_segments, reconstruct_scoped_message_with_facts};
use super::replay_capture::{
    load_replay_capture_rows, preload_replay_capture_blocks, replay_capture_from_row,
    replay_capture_key,
};
use super::replay_request::{
    validate_replay_request_facts, validated_canonical_replay_boundary, ReplayRequestReader,
};
use super::{
    replay_violation, CanonicalAssistantCandidate, CanonicalProviderCoordinate,
    CanonicalReplayScope, ReconstructedScopedMessage,
};

/// Resolve physical assistant origins reachable from the bounded session view.
/// Forks retain their origin's request/session scope; an arbitrary historical
/// request supplied by the caller is never a lookup root.
#[cfg(test)]
pub(crate) async fn load_canonical_assistant_candidates(
    node: &EmbeddedNode,
    scope: CanonicalReplayScope<'_>,
    boundary: &crate::turn::provider_context_reduction::SourceBoundary,
) -> Result<Vec<CanonicalAssistantCandidate>> {
    load_canonical_assistant_candidates_with(node, node, scope, boundary, None).await
}

pub(crate) async fn load_canonical_assistant_candidates_with(
    node: &EmbeddedNode,
    requests: &impl ReplayRequestReader,
    scope: CanonicalReplayScope<'_>,
    boundary: &crate::turn::provider_context_reduction::SourceBoundary,
    selected_tags: Option<&[gents_loop::claude_messages_body::ReplayTag]>,
) -> Result<Vec<CanonicalAssistantCandidate>> {
    let (request_commits, high_water) =
        validated_canonical_replay_boundary(node, requests, scope, boundary).await?;
    // None records the empty canonical view at capture time. A later current
    // read cannot turn that historical empty boundary into an unbounded scan.
    let Some(high_water) = high_water else {
        return Ok(Vec::new());
    };
    let mut cache = ReadCache {
        reuse_node_scans: true,
        ..ReadCache::default()
    };
    let mut request_facts = BTreeMap::from([(
        (
            scope.request_doc_id.to_owned(),
            scope.node_did.to_owned(),
            scope.requester_did.map(str::to_owned),
            scope.session_id.to_owned(),
        ),
        (scope.request_id.to_owned(), request_commits),
    )]);
    let mut capture_cache = crate::inference::rendered_request::CaptureReadCache::default();

    let session_filter =
        session_scope_filter(scope.node_did, scope.session_id, scope.requester_did);
    let query = format!(
        r#"{{ AgentMessage(filter: {{ {session_filter}, sequence: {{ _le: {} }} }}, order: {{ sequence: ASC }}) {{ {AGENT_MESSAGE_FIELDS} }} }}"#,
        high_water.sequence
    );
    let response = ReadAccess::Node(node)
        .query(&query, "load_canonical_replay_candidates")
        .await?;
    let headers = rows_value(&response, "AgentMessage")?
        .iter()
        .map(decode_transcript_message_row)
        .collect::<Result<Vec<_>>>()?;
    let mut ids = BTreeSet::new();
    let mut sequences = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for row in &headers {
        replay_ensure!(
            row.message.node_did == scope.node_did
                && row.message.requester_did.as_deref() == scope.requester_did
                && row.message.session_id == scope.session_id
                && i64::from(row.message.sequence) <= high_water.sequence,
            "canonical replay header crossed its scoped high-water view"
        );
        replay_ensure!(
            ids.insert(row.doc_id.clone())
                && sequences.insert(row.message.sequence)
                && keys.insert(row.message.message_key.clone()),
            "canonical replay header has a physical, sequence, or key twin"
        );
        cache.headers.insert(row.doc_id.clone(), row.clone());
    }
    replay_ensure!(
        ids.contains(&high_water.doc_id),
        "canonical replay high-water header is absent from its bounded view"
    );

    let replay_output_requests = headers
        .iter()
        .filter(|row| {
            row.message.role == MessageRole::Assistant
                && !row.message.payload_references().is_empty()
        })
        .filter_map(|row| row.message.request_doc_id.clone())
        .collect::<Vec<_>>();
    preload_request_output_segments(
        node,
        &replay_output_requests,
        scope.node_did,
        scope.requester_did,
        &mut cache,
    )
    .await?;

    let mut pending = Vec::new();
    for row in headers
        .into_iter()
        .filter(|row| row.message.role == MessageRole::Assistant)
    {
        let reconstructed = reconstruct_scoped_message_with_facts(
            ReadAccess::Node(node),
            &row.doc_id,
            scope.node_did,
            scope.requester_did,
            &mut cache,
        )
        .await?;
        let origin_header = &reconstructed.origin.message;
        let complete = origin_header.outcome == OutputOutcome::Complete
            && matches!(
                origin_header.publication,
                MessagePublication::RequestExecution { .. }
            );
        let provenance = if complete {
            provider_coordinate_for_candidate(&reconstructed, scope.expected_scope_kind).map(
                |value| {
                    value.map(|coordinate| (coordinate, (0..origin_header.blocks.len()).collect()))
                },
            )
        } else {
            recovered_provider_provenance(&reconstructed, scope.expected_scope_kind)
        };
        let provenance = match provenance {
            Ok(provenance) => provenance,
            Err(error) => {
                tracing::debug!(header_doc_id = %row.doc_id, error = %error,
                    "historical assistant has no usable provider replay coordinate");
                continue;
            }
        };
        let Some((coordinate, block_indices)) = provenance else {
            // An authored assistant belongs in permissive history, but is not
            // a provider continuation candidate merely because its role and
            // publication match a provider-owned assistant.
            continue;
        };
        let request_doc_id = origin_header
            .request_doc_id
            .as_deref()
            .context("canonical provider header has no physical request")?;
        // Filter only after bounded-header and physical-coordinate validation
        // so unrequested history cannot bypass canonical-view checks.
        if let Some(tags) = selected_tags {
            let selected = tags.iter().any(|tag| {
                if tag.request_doc_id != request_doc_id {
                    return false;
                }
                matches!(
                    &tag.source,
                    OutputSource::ProviderTurn {
                        scope: tag_scope,
                        turn_index,
                        attempt,
                    } if tag_scope == &coordinate.scope
                        && *turn_index == coordinate.turn_index
                        && *attempt == coordinate.attempt
                )
            });
            if !selected {
                continue;
            }
        }
        pending.push(PendingReplayCandidate {
            capture_key: replay_capture_key(
                &origin_header.node_did,
                &origin_header.session_id,
                request_doc_id,
                coordinate,
            )?,
            header_doc_id: reconstructed.origin.doc_id,
            request_doc_id: request_doc_id.to_owned(),
            node_did: origin_header.node_did.clone(),
            requester_did: origin_header.requester_did.clone(),
            session_id: origin_header.session_id.clone(),
            coordinate,
            block_indices,
            complete,
            #[cfg(test)]
            sequence: row.message.sequence,
            message: reconstructed.message,
        });
    }
    let missing_request_ids = pending
        .iter()
        .filter(|candidate| {
            !request_facts.contains_key(&(
                candidate.request_doc_id.clone(),
                candidate.node_did.clone(),
                candidate.requester_did.clone(),
                candidate.session_id.clone(),
            ))
        })
        .map(|candidate| candidate.request_doc_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let loaded_request_facts = requests.request_facts(&missing_request_ids).await?;
    let mut selected = Vec::new();
    for candidate in pending {
        let request_scope = (
            candidate.request_doc_id.clone(),
            candidate.node_did.clone(),
            candidate.requester_did.clone(),
            candidate.session_id.clone(),
        );
        let loaded = match request_facts.get(&request_scope) {
            Some(facts) => Ok(facts.clone()),
            None => match loaded_request_facts.get(&candidate.request_doc_id) {
                Some(facts) => validate_replay_request_facts(
                    &candidate.request_doc_id,
                    &candidate.node_did,
                    candidate.requester_did.as_deref(),
                    &candidate.session_id,
                    facts,
                ),
                None => Err(anyhow::anyhow!(
                    "canonical replay request batch omitted {}",
                    candidate.request_doc_id
                )),
            },
        };
        let (request_id, request_commits) = match loaded {
            Ok(request) => request,
            // A historical request that is verifiably missing or out of scope
            // makes only its turn non-replayable; the current request is
            // validated above. A failed store read says nothing about the
            // request and must not silently strip reasoning, so it propagates.
            Err(error)
                if candidate.request_doc_id != scope.request_doc_id
                    && error
                        .downcast_ref::<gents_loop::loop_stream::ReplayEvidenceViolation>()
                        .is_some() =>
            {
                tracing::warn!(
                    header_doc_id = %candidate.header_doc_id,
                    error = %format!("{error:#}"),
                    "accepted turn request is unverifiable; its reasoning is not replayed"
                );
                continue;
            }
            Err(error) => return Err(error),
        };
        request_facts
            .entry(request_scope)
            .or_insert_with(|| (request_id.clone(), request_commits.clone()));
        selected.push(SelectedReplayCandidate {
            capture_key: candidate.capture_key,
            header_doc_id: candidate.header_doc_id,
            request_doc_id: candidate.request_doc_id,
            node_did: candidate.node_did,
            requester_did: candidate.requester_did,
            session_id: candidate.session_id,
            request_id,
            request_commits,
            coordinate: candidate.coordinate,
            block_indices: candidate.block_indices,
            complete: candidate.complete,
            #[cfg(test)]
            sequence: candidate.sequence,
            message: candidate.message,
        });
    }
    let capture_rows = load_replay_capture_rows(node, &selected).await?;
    preload_replay_capture_blocks(node, &capture_rows, &mut capture_cache).await?;

    let mut candidates = Vec::new();
    for item in selected {
        let capture = replay_capture_from_row(
            node,
            &capture_rows,
            &item.capture_key,
            &item.node_did,
            item.requester_did.as_deref(),
            &item.session_id,
            &item.request_id,
            &item.request_doc_id,
            &item.request_commits,
            item.coordinate,
            &mut capture_cache,
        )
        .await?;
        candidates.push(CanonicalAssistantCandidate {
            header_doc_id: item.header_doc_id,
            request_doc_id: item.request_doc_id,
            #[cfg(test)]
            sequence: item.sequence,
            message: item.message,
            coordinate: item.coordinate,
            block_indices: item.block_indices,
            complete: item.complete,
            capture,
        });
    }
    Ok(candidates)
}

struct PendingReplayCandidate {
    capture_key: String,
    header_doc_id: String,
    request_doc_id: String,
    node_did: String,
    requester_did: Option<String>,
    session_id: String,
    coordinate: CanonicalProviderCoordinate,
    block_indices: Vec<usize>,
    complete: bool,
    #[cfg(test)]
    sequence: u32,
    message: gents_protocol::message::Message,
}

/// One assistant of the bounded canonical view that passed every check a
/// capture read does not participate in: bounded-header, coordinate, tag
/// selection and physical-request validation.
pub(super) struct SelectedReplayCandidate {
    pub(super) capture_key: String,
    header_doc_id: String,
    request_doc_id: String,
    node_did: String,
    requester_did: Option<String>,
    session_id: String,
    request_id: String,
    request_commits: BTreeSet<String>,
    coordinate: CanonicalProviderCoordinate,
    block_indices: Vec<usize>,
    complete: bool,
    #[cfg(test)]
    sequence: u32,
    message: gents_protocol::message::Message,
}

pub(super) fn provider_coordinate_for_candidate(
    reconstructed: &ReconstructedScopedMessage,
    expected_scope_kind: gents_protocol::rendered_request::CaptureScopeKind,
) -> Result<Option<CanonicalProviderCoordinate>> {
    provider_coordinate_for_candidate_inner(reconstructed, expected_scope_kind)
        .map_err(|error| replay_violation(error.to_string()))
}

fn provider_coordinate_for_candidate_inner(
    reconstructed: &ReconstructedScopedMessage,
    expected_scope_kind: gents_protocol::rendered_request::CaptureScopeKind,
) -> Result<Option<CanonicalProviderCoordinate>> {
    let header = &reconstructed.origin.message;
    let request_doc_id = header
        .request_doc_id
        .as_deref()
        .context("canonical replay execution assistant has no physical request")?;
    let MessagePublication::RequestExecution {
        execution_generation,
    } = &header.publication
    else {
        anyhow::bail!("canonical replay candidate is not an execution publication");
    };
    let close_ids = header
        .payload_references()
        .into_iter()
        .map(|reference| reference.close_doc_id.clone())
        .collect::<BTreeSet<_>>();
    // An accepted empty assistant has no payload reference to a close. Its
    // message key is not independent provider evidence; never parse it to
    // manufacture an issuer or select a capture.
    if close_ids.is_empty() {
        return Ok(None);
    }
    let mut provider = None;
    let mut authored = false;
    for close_id in close_ids {
        let matches = reconstructed
            .segments
            .iter()
            .filter(|record| record.doc_id == close_id)
            .collect::<Vec<_>>();
        anyhow::ensure!(
            matches.len() == 1,
            "canonical replay reference has a missing or conflicting physical close {close_id}"
        );
        let close = &matches[0].segment;
        anyhow::ensure!(
            close.node_did == header.node_did
                && close.requester_did == header.requester_did
                && close.session_id == header.session_id
                && close.request_doc_id == request_doc_id
                && matches!(&close.writer, OutputWriter::RequestExecution { execution_generation: writer } if writer == execution_generation)
                && matches!(
                    &close.close,
                    Some(SourceClose::Closed {
                        outcome: OutputOutcome::Complete,
                        ..
                    })
                ),
            "canonical replay close crossed its request execution or is not complete"
        );
        match &close.source {
            OutputSource::Authored { .. } => authored = true,
            OutputSource::ProviderTurn {
                scope: capture_scope,
                turn_index,
                attempt,
            } => {
                anyhow::ensure!(
                    capture_scope.kind == expected_scope_kind,
                    "canonical replay assistant references a different provider loop scope"
                );
                let coordinate = CanonicalProviderCoordinate {
                    scope: *capture_scope,
                    turn_index: *turn_index,
                    attempt: *attempt,
                };
                match provider {
                    None => provider = Some(coordinate),
                    Some(expected) => anyhow::ensure!(
                        expected == coordinate,
                        "canonical replay assistant combines different provider turns"
                    ),
                }
            }
            _ => anyhow::bail!("canonical replay assistant references an invalid close source"),
        }
    }
    anyhow::ensure!(
        !(authored && provider.is_some()),
        "canonical replay assistant combines authored and provider sources"
    );
    Ok(provider)
}

/// Recovery must carry the source's native positions, including gaps for
/// omitted blocks. Only the exact sealed, stopped recovery projection is
/// eligible to become a replay witness.
pub(super) fn recovered_provider_provenance(
    reconstructed: &ReconstructedScopedMessage,
    expected_scope_kind: gents_protocol::rendered_request::CaptureScopeKind,
) -> Result<Option<(CanonicalProviderCoordinate, Vec<usize>)>> {
    use gents_protocol::output::reconstruction::ObservedSegment;
    let header = &reconstructed.origin.message;
    if !matches!(
        header.publication,
        MessagePublication::RequestRecovery { .. }
    ) || header.outcome != OutputOutcome::Partial
    {
        return Ok(None);
    }
    let request_doc_id = header
        .request_doc_id
        .as_deref()
        .context("recovery has no request")?;
    let close_ids = header
        .payload_references()
        .into_iter()
        .map(|reference| reference.close_doc_id.as_str())
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        close_ids.len() == 1,
        "recovery must reference one physical close"
    );
    let close_id = *close_ids.iter().next().expect("checked singleton");
    let closes = reconstructed
        .segments
        .iter()
        .filter(|row| row.doc_id == close_id)
        .collect::<Vec<_>>();
    anyhow::ensure!(closes.len() == 1, "recovery close is missing or ambiguous");
    let close = &closes[0].segment;
    let OutputSource::ProviderTurn {
        scope,
        turn_index,
        attempt,
    } = &close.source
    else {
        anyhow::bail!("recovery source is not a provider turn");
    };
    anyhow::ensure!(
        scope.kind == expected_scope_kind,
        "recovery has a different provider scope"
    );
    let Some(SourceClose::Closed {
        outcome: OutputOutcome::Partial,
        segments,
        stream_bytes,
    }) = &close.close
    else {
        anyhow::bail!("recovery source is not partially closed");
    };
    anyhow::ensure!(
        matches!(close.writer, OutputWriter::RequestExecution { .. }),
        "recovery source is not request-owned"
    );
    let scoped = reconstructed
        .segments
        .iter()
        .filter(|row| {
            row.segment.request_doc_id == request_doc_id && row.segment.source == close.source
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        scoped
            .iter()
            .all(|row| row.segment.close.is_none() || row.doc_id == close_id),
        "recovery source has conflicting closure"
    );
    let bounded = scoped
        .iter()
        .filter(|row| {
            row.segment
                .ordinal
                .is_some_and(|ordinal| ordinal < *segments)
                || row.doc_id == close_id
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        bounded
            .iter()
            .all(|row| row.segment.node_did == header.node_did
                && row.segment.requester_did == header.requester_did
                && row.segment.session_id == header.session_id
                && row.segment.writer == close.writer),
        "recovery source has conflicting scope or writer"
    );
    let observed = bounded
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    let extent = gents_protocol::output::extent::inspect_open_source(
        &observed,
        request_doc_id,
        &close.source,
        &close.writer,
    )?;
    anyhow::ensure!(
        extent.segments == *segments && extent.stream_bytes == *stream_bytes,
        "recovery source does not match its sealed extent"
    );
    let blocks =
        gents_protocol::output::recovery::retained_blocks_from_validated_extent(&extent, close_id)?;
    anyhow::ensure!(
        blocks == header.blocks,
        "recovery header is not the exact stopped projection"
    );
    let indices =
        gents_protocol::output::recovery::retained_block_indices_from_validated_extent(&extent)
            .into_iter()
            .map(|index| index as usize)
            .collect::<Vec<_>>();
    anyhow::ensure!(
        indices.len() == blocks.len() && indices.windows(2).all(|pair| pair[0] < pair[1]),
        "recovery block positions are invalid"
    );
    Ok(Some((
        CanonicalProviderCoordinate {
            scope: *scope,
            turn_index: *turn_index,
            attempt: *attempt,
        },
        indices,
    )))
}

#[cfg(test)]
mod recovered_provenance_tests {
    use super::*;
    use crate::session::canonical_rows::{OutputSegmentRow, TranscriptMessageRow};
    use gents_protocol::output::{
        OutputSegment, SegmentRun, StreamDeclaration, StreamPayload, TranscriptMessage,
    };

    fn fixture() -> ReconstructedScopedMessage {
        let declaration = |stream, bytes, payload| SegmentRun {
            stream,
            bytes,
            declaration: Some(StreamDeclaration {
                block_index: 2,
                part_index: 0,
                payload,
            }),
        };
        let source = OutputSource::ProviderTurn {
            scope: "inference.1".parse().unwrap(),
            turn_index: 0,
            attempt: 0,
        };
        let segment = OutputSegment {
            node_did: "node".into(),
            requester_did: None,
            session_id: "session".into(),
            request_doc_id: "request".into(),
            source: source.clone(),
            writer: OutputWriter::RequestExecution {
                execution_generation: "producer".into(),
            },
            ordinal: Some(0),
            runs: vec![
                declaration(0, 1, StreamPayload::Reasoning),
                declaration(1, 1, StreamPayload::ReasoningSignature),
                declaration(2, 0, StreamPayload::ReasoningStop),
            ],
            payload: "AS".into(),
            close: Some(SourceClose::Closed {
                outcome: OutputOutcome::Partial,
                segments: 1,
                stream_bytes: vec![1, 1, 0],
            }),
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        let observations = [gents_protocol::output::reconstruction::ObservedSegment {
            doc_id: "close",
            segment: &segment,
        }];
        let extent = gents_protocol::output::extent::inspect_open_source(
            &observations,
            "request",
            &source,
            &segment.writer,
        )
        .unwrap();
        let blocks = gents_protocol::output::recovery::retained_blocks_from_validated_extent(
            &extent, "close",
        )
        .unwrap();
        let header = TranscriptMessageRow {
            doc_id: "header".into(),
            message: TranscriptMessage {
                message_key: "recovered".into(),
                session_id: "session".into(),
                node_did: "node".into(),
                requester_did: None,
                request_doc_id: Some("request".into()),
                publication: MessagePublication::RequestRecovery {
                    execution_generation: "recovery".into(),
                },
                outcome: OutputOutcome::Partial,
                sequence: 1,
                role: MessageRole::Assistant,
                native_id: None,
                native_request_id: None,
                blocks,
                created_at: "2026-01-01T00:00:00Z".into(),
            },
        };
        ReconstructedScopedMessage {
            header: header.clone(),
            origin: header,
            message: gents_protocol::message::Message::Assistant {
                id: None,
                request_id: None,
                content: vec![],
            },
            segments: vec![OutputSegmentRow {
                doc_id: "close".into(),
                segment,
            }],
        }
    }

    #[test]
    fn recovered_witness_requires_exact_scoped_sealed_projection() {
        let mut restored = fixture();
        let kind = gents_protocol::rendered_request::CaptureScopeKind::Inference;
        assert_eq!(
            recovered_provider_provenance(&restored, kind)
                .unwrap()
                .unwrap()
                .1,
            [2]
        );
        restored.origin.message.blocks.clear();
        assert!(recovered_provider_provenance(&restored, kind).is_err());
        let mut restored = fixture();
        restored.segments[0].segment.session_id = "foreign".into();
        assert!(recovered_provider_provenance(&restored, kind).is_err());
        let mut restored = fixture();
        restored.segments[0].segment.runs.pop();
        let Some(SourceClose::Closed { stream_bytes, .. }) =
            &mut restored.segments[0].segment.close
        else {
            panic!("expected a source close");
        };
        stream_bytes.pop();
        assert!(recovered_provider_provenance(&restored, kind)
            .unwrap_err()
            .to_string()
            .contains("exact stopped projection"));
    }
}
