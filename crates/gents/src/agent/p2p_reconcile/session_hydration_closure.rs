//! Exact canonical-output reference closure for session hydration.
//! Fork validation and payload reconstruction remain shared protocol owners.

use super::session_hydration::HydrationDocument;
use crate::session::canonical_rows::{OutputSegmentRow, TranscriptMessageRow};
use anyhow::anyhow;
use gents_protocol::output::origin::{lookup_message, resolve_origin, ObservedMessage};
use gents_protocol::output::reconstruction::{
    reconstruct_message, DependencyDenial, ObservedSegment,
};
use gents_protocol::output::{
    MessageBlock, MessagePublication, OutputSource, OutputWriter, ReconstructionError, SourceClose,
};
use gents_protocol::session_hydration::SessionHydrationCollection;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct ScopedDocument {
    pub collection: SessionHydrationCollection,
    pub doc_id: String,
    pub requester_did: String,
    pub agent_did: String,
    pub session_id: String,
}

impl ScopedDocument {
    fn into_hydration(self) -> HydrationDocument {
        HydrationDocument {
            collection: self.collection,
            doc_id: self.doc_id,
            requester_did: self.requester_did,
            agent_did: self.agent_did,
            session_id: self.session_id,
        }
    }
}

pub struct CanonicalClosureInput<'a> {
    pub root_header_ids: &'a [String],
    pub headers: &'a [TranscriptMessageRow],
    pub segments: &'a [OutputSegmentRow],
    pub base_documents: &'a [ScopedDocument],
    pub denied_headers: &'a [String],
    pub denied_segments: &'a [String],
    pub dependency_denials: &'a [DependencyDenial],
    pub agent_did: &'a str,
    pub requester_did: Option<&'a str>,
    pub session_id: &'a str,
}

#[derive(Debug)]
pub enum CanonicalClosureError {
    MissingObservation(anyhow::Error),
    StructuralConflict(anyhow::Error),
    Unclassified(anyhow::Error),
}

impl std::fmt::Display for CanonicalClosureError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingObservation(error) => write!(formatter, "missing observation: {error}"),
            Self::StructuralConflict(error) => write!(formatter, "structural conflict: {error}"),
            Self::Unclassified(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for CanonicalClosureError {}

type ClosureResult<T> = std::result::Result<T, CanonicalClosureError>;

fn origin_failure(error: gents_protocol::output::origin::OriginError) -> CanonicalClosureError {
    use gents_protocol::output::origin::OriginError;
    match error {
        error @ OriginError::Unavailable { .. } => {
            CanonicalClosureError::MissingObservation(anyhow::Error::new(error))
        }
        error @ OriginError::Conflict { .. } => {
            CanonicalClosureError::StructuralConflict(anyhow::Error::new(error))
        }
        error => CanonicalClosureError::Unclassified(anyhow::Error::new(error)),
    }
}

fn reconstruction_failure(error: ReconstructionError) -> CanonicalClosureError {
    if error.is_incomplete() {
        return CanonicalClosureError::MissingObservation(anyhow::Error::new(error));
    }
    match error {
        error @ (ReconstructionError::ConflictingSegments { .. }
        | ReconstructionError::ConflictingClosures { .. }
        | ReconstructionError::ConflictingMessages { .. }) => {
            CanonicalClosureError::StructuralConflict(anyhow::Error::new(error))
        }
        error => CanonicalClosureError::Unclassified(anyhow::Error::new(error)),
    }
}

pub fn build_canonical_closure(
    input: CanonicalClosureInput<'_>,
) -> ClosureResult<BTreeSet<HydrationDocument>> {
    let headers = input
        .headers
        .iter()
        .map(|row| ObservedMessage {
            doc_id: &row.doc_id,
            message: &row.message,
        })
        .collect::<Vec<_>>();
    let segments = input
        .segments
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    let bases = input
        .base_documents
        .iter()
        .map(|row| ((row.collection, row.doc_id.as_str()), row))
        .collect::<BTreeMap<_, _>>();
    let mut selected = BTreeSet::new();
    for root_id in input.root_header_ids {
        let root = lookup_message(
            &headers,
            input.denied_headers,
            root_id,
            input.agent_did,
            input.requester_did,
        )
        .map_err(origin_failure)?;
        if root.message.session_id != input.session_id {
            return Err(CanonicalClosureError::Unclassified(anyhow!(
                "hydration root is outside target session"
            )));
        }
        let origin = resolve_origin(
            &headers,
            input.denied_headers,
            root_id,
            input.agent_did,
            input.requester_did,
        )
        .map_err(origin_failure)?;
        reconstruct_message(
            &segments,
            input.denied_segments,
            input.dependency_denials,
            origin.message,
        )
        .map_err(reconstruction_failure)?;
        collect_origin_chain(
            &headers,
            input.denied_headers,
            root_id,
            input.agent_did,
            input.requester_did,
            &mut selected,
        )?;
        collect_message_dependencies(origin, &segments, &bases, &mut selected)?;
    }
    Ok(selected)
}

fn collect_origin_chain(
    headers: &[ObservedMessage<'_>],
    denied: &[String],
    root_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    selected: &mut BTreeSet<HydrationDocument>,
) -> ClosureResult<()> {
    let mut id = root_id.to_owned();
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(id.clone()) {
            return Err(CanonicalClosureError::Unclassified(anyhow!(
                "cyclic hydration origin"
            )));
        }
        let row = lookup_message(headers, denied, &id, agent_did, requester_did)
            .map_err(origin_failure)?;
        selected.insert(HydrationDocument {
            collection: SessionHydrationCollection::AgentMessage,
            doc_id: row.doc_id.to_owned(),
            requester_did: row.message.requester_did.clone().unwrap_or_default(),
            agent_did: row.message.agent_did.clone(),
            session_id: row.message.session_id.clone(),
        });
        match &row.message.publication {
            MessagePublication::Fork {
                origin_message_doc_id,
            } => id = origin_message_doc_id.clone(),
            _ => return Ok(()),
        }
    }
}

fn collect_message_dependencies(
    message: ObservedMessage<'_>,
    segments: &[ObservedSegment<'_>],
    bases: &BTreeMap<(SessionHydrationCollection, &str), &ScopedDocument>,
    selected: &mut BTreeSet<HydrationDocument>,
) -> ClosureResult<()> {
    if let Some(id) = message.message.request_doc_id.as_deref() {
        retain_base(
            SessionHydrationCollection::AgentRequest,
            id,
            bases,
            selected,
        )?;
    }
    if let MessagePublication::ToolDelivery { tool_call_doc_id } = &message.message.publication {
        retain_base(
            SessionHydrationCollection::AgentToolCall,
            tool_call_doc_id,
            bases,
            selected,
        )?;
    }
    for block in &message.message.blocks {
        match block {
            MessageBlock::ToolCall {
                tool_call_doc_id, ..
            }
            | MessageBlock::ToolResult {
                tool_call_doc_id, ..
            } => retain_base(
                SessionHydrationCollection::AgentToolCall,
                tool_call_doc_id,
                bases,
                selected,
            )?,
            _ => {}
        }
    }
    for reference in message.message.payload_references() {
        let closing = segments
            .iter()
            .copied()
            .find(|row| row.doc_id == reference.close_doc_id)
            .ok_or_else(|| {
                CanonicalClosureError::MissingObservation(anyhow!(
                    "validated closing segment disappeared"
                ))
            })?;
        let SourceClose::Closed {
            segments: extent, ..
        } = closing.segment.close.as_ref().ok_or_else(|| {
            CanonicalClosureError::Unclassified(anyhow!("validated closing segment is not closed"))
        })?
        else {
            return Err(CanonicalClosureError::Unclassified(anyhow!(
                "validated reference names a retraction"
            )));
        };
        retain_base(
            SessionHydrationCollection::AgentRequest,
            &closing.segment.request_doc_id,
            bases,
            selected,
        )?;
        for row in segments.iter().copied().filter(|row| {
            row.segment.request_doc_id == closing.segment.request_doc_id
                && row.segment.source == closing.segment.source
                && (row.doc_id == closing.doc_id
                    || row.segment.ordinal.is_some_and(|ordinal| ordinal < *extent))
        }) {
            selected.insert(HydrationDocument {
                collection: SessionHydrationCollection::AgentOutputSegment,
                doc_id: row.doc_id.to_owned(),
                requester_did: row.segment.requester_did.clone().unwrap_or_default(),
                agent_did: row.segment.agent_did.clone(),
                session_id: row.segment.session_id.clone(),
            });
        }
        // CanonicalOutput/Hydration.lean `requiredProvenance`: a ToolCall
        // source names its exact AgentToolCall base regardless of writer, and
        // authored delivery derives its tool owner from a ToolExecution
        // writer. Shared reconstruction already rejects incoherent
        // source/writer pairs, so this only retains the base it admitted.
        let tool_call_doc_id = match &closing.segment.source {
            OutputSource::ToolCall { tool_call_doc_id } => Some(tool_call_doc_id),
            OutputSource::Authored { .. } => match &closing.segment.writer {
                OutputWriter::ToolExecution { tool_call_doc_id } => Some(tool_call_doc_id),
                _ => None,
            },
            _ => None,
        };
        if let Some(tool_call_doc_id) = tool_call_doc_id {
            retain_base(
                SessionHydrationCollection::AgentToolCall,
                tool_call_doc_id,
                bases,
                selected,
            )?;
        }
    }
    Ok(())
}

fn retain_base(
    collection: SessionHydrationCollection,
    doc_id: &str,
    bases: &BTreeMap<(SessionHydrationCollection, &str), &ScopedDocument>,
    selected: &mut BTreeSet<HydrationDocument>,
) -> ClosureResult<()> {
    let document = bases.get(&(collection, doc_id)).ok_or_else(|| {
        CanonicalClosureError::MissingObservation(anyhow!(
            "missing authorized {collection:?} dependency {doc_id}"
        ))
    })?;
    selected.insert((*document).clone().into_hydration());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::output::{
        MessagePublication, MessageRole, OutputOutcome, OutputSegment, PayloadPresentation,
        PayloadRef, PresentedPayload, SegmentRun, StreamDeclaration, StreamPayload, ToolResultPart,
        TranscriptMessage,
    };
    use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

    fn message(
        session: &str,
        request: Option<&str>,
        publication: MessagePublication,
    ) -> TranscriptMessage {
        TranscriptMessage {
            message_key: "message-key".into(),
            session_id: session.into(),
            agent_did: "agent".into(),
            requester_did: Some("requester".into()),
            request_doc_id: request.map(str::to_owned),
            publication,
            outcome: OutputOutcome::Complete,
            sequence: 7,
            role: MessageRole::Assistant,
            native_id: Some("native".into()),
            blocks: Vec::new(),
            created_at: "2026-09-21T00:00:00Z".into(),
        }
    }

    fn base_request() -> ScopedDocument {
        ScopedDocument {
            collection: SessionHydrationCollection::AgentRequest,
            doc_id: "request".into(),
            requester_did: "requester".into(),
            agent_did: "agent".into(),
            session_id: "origin-session".into(),
        }
    }

    fn modeled_collection(name: &str) -> SessionHydrationCollection {
        match name {
            "AgentRequest" => SessionHydrationCollection::AgentRequest,
            "AgentMessage" => SessionHydrationCollection::AgentMessage,
            "AgentToolCall" => SessionHydrationCollection::AgentToolCall,
            "AgentOutputSegment" => SessionHydrationCollection::AgentOutputSegment,
            "CompactionEntry" => SessionHydrationCollection::CompactionEntry,
            other => panic!("unknown modeled hydration collection {other}"),
        }
    }

    #[test]
    fn generated_modeled_closure_input_selects_exact_native_manifest() {
        fn assert_input(
            input: &crate::lean_vocab_test::LeanSessionHydrationClosureInput,
            expected_documents: Option<
                &Vec<crate::lean_vocab_test::LeanSessionHydrationDocumentKey>,
            >,
        ) -> Option<BTreeSet<HydrationDocument>> {
            let request = &input.request;
            let target_session = request.session.as_str();
            let agent = request.agent.as_str();
            let requester = request.requester.as_str();
            let peer = request.peer.as_str();
            let session_for = |native: u64| {
                if native == request.native_session {
                    target_session.to_string()
                } else {
                    format!("native-session-{native}")
                }
            };
            let headers = input
                .messages
                .iter()
                .map(|modeled| TranscriptMessageRow {
                    doc_id: modeled.id.to_string(),
                    message: TranscriptMessage {
                        message_key: format!("modeled-message-{}", modeled.id),
                        session_id: session_for(modeled.session),
                        agent_did: agent.into(),
                        requester_did: Some(requester.into()),
                        request_doc_id: modeled.request.map(|id| id.to_string()),
                        publication: modeled.origin.map_or(
                            MessagePublication::RequestExecution {
                                execution_generation: "7".into(),
                            },
                            |origin| MessagePublication::Fork {
                                origin_message_doc_id: origin.to_string(),
                            },
                        ),
                        outcome: OutputOutcome::Complete,
                        sequence: 0,
                        role: MessageRole::Assistant,
                        native_id: Some("native".into()),
                        blocks: modeled
                            .refs
                            .iter()
                            .map(|reference| MessageBlock::Text {
                                text: PresentedPayload {
                                    output: PayloadRef {
                                        close_doc_id: reference.close_id.to_string(),
                                        stream: reference.stream.try_into().unwrap(),
                                    },
                                    presentation: PayloadPresentation::Full,
                                },
                            })
                            .collect(),
                        created_at: "2026-09-21T00:00:00Z".into(),
                    },
                })
                .collect::<Vec<_>>();
            let segments = input
                .segments
                .iter()
                .map(|modeled| {
                    assert_eq!(modeled.source.kind, "provider");
                    assert_eq!(modeled.writer.kind, "request");
                    OutputSegmentRow {
                        doc_id: modeled.id.to_string(),
                        segment: OutputSegment {
                            agent_did: agent.into(),
                            requester_did: Some(requester.into()),
                            session_id: input
                                .messages
                                .iter()
                                .find(|message| {
                                    message
                                        .refs
                                        .iter()
                                        .any(|reference| reference.close_id == modeled.id)
                                })
                                .map(|message| session_for(message.session))
                                .unwrap_or_else(|| target_session.to_string()),
                            request_doc_id: modeled.request.to_string(),
                            source: OutputSource::ProviderTurn {
                                scope: CaptureScope {
                                    kind: CaptureScopeKind::Inference,
                                    seq: modeled.source.scope.unwrap(),
                                },
                                turn_index: modeled.source.turn.unwrap().try_into().unwrap(),
                                attempt: modeled.source.attempt.unwrap().try_into().unwrap(),
                            },
                            writer: OutputWriter::RequestExecution {
                                execution_generation: modeled.writer.owner.unwrap().to_string(),
                            },
                            ordinal: Some(0),
                            runs: vec![SegmentRun {
                                stream: 0,
                                bytes: modeled.payload.len().try_into().unwrap(),
                                declaration: Some(StreamDeclaration {
                                    block_index: 0,
                                    part_index: 0,
                                    payload: StreamPayload::Text,
                                }),
                            }],
                            payload: String::from_utf8(modeled.payload.clone()).unwrap(),
                            close: Some(SourceClose::Closed {
                                outcome: OutputOutcome::Complete,
                                segments: 1,
                                stream_bytes: vec![modeled.payload.len() as u64],
                            }),
                            created_at: "2026-09-21T00:00:00Z".into(),
                        },
                    }
                })
                .collect::<Vec<_>>();
            let bases = input
                .access
                .iter()
                .filter(|access| {
                    access.peer == peer
                        && access.requester == requester
                        && access.agent == agent
                        && access.session == target_session
                        && access.native_session == request.native_session
                })
                .filter(|access| access.state == "authorized")
                .filter(|access| {
                    matches!(
                        access.key.collection.as_str(),
                        "AgentRequest" | "AgentToolCall" | "CompactionEntry"
                    )
                })
                .map(|access| ScopedDocument {
                    collection: modeled_collection(&access.key.collection),
                    doc_id: access.key.id.to_string(),
                    requester_did: requester.into(),
                    agent_did: agent.into(),
                    session_id: input
                        .messages
                        .iter()
                        .find(|message| message.request == Some(access.key.id))
                        .map(|message| session_for(message.session))
                        .unwrap_or_else(|| target_session.to_string()),
                })
                .collect::<Vec<_>>();
            let roots = input.roots.iter().map(u64::to_string).collect::<Vec<_>>();
            let mut denied_headers = input
                .denied_headers
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>();
            denied_headers.extend(input.access.iter().filter_map(|access| {
                (access.peer == peer
                    && access.requester == requester
                    && access.agent == agent
                    && access.session == target_session
                    && access.native_session == request.native_session
                    && access.state == "denied"
                    && access.key.collection == "AgentMessage")
                    .then(|| access.key.id.to_string())
            }));
            let mut denied_segments = input
                .denied_segments
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>();
            denied_segments.extend(input.access.iter().filter_map(|access| {
                (access.peer == peer
                    && access.requester == requester
                    && access.agent == agent
                    && access.session == target_session
                    && access.native_session == request.native_session
                    && access.state == "denied"
                    && access.key.collection == "AgentOutputSegment")
                    .then(|| access.key.id.to_string())
            }));
            let result = build_canonical_closure(CanonicalClosureInput {
                root_header_ids: &roots,
                headers: &headers,
                segments: &segments,
                base_documents: &bases,
                denied_headers: &denied_headers,
                denied_segments: &denied_segments,
                dependency_denials: &[],
                agent_did: agent,
                requester_did: Some(requester),
                session_id: target_session,
            });
            let Some(expected_documents) = expected_documents else {
                assert!(
                    result.is_err(),
                    "modeled rejected closure passed native owner"
                );
                return None;
            };
            let closure = result.expect("modeled closure must pass native owner");
            let selected = closure
                .iter()
                .map(|document| (document.collection, document.doc_id.clone()))
                .collect::<BTreeSet<_>>();
            let expected = expected_documents
                .iter()
                .map(|document| {
                    (
                        modeled_collection(&document.collection),
                        document.id.to_string(),
                    )
                })
                .collect::<BTreeSet<_>>();
            assert_eq!(selected, expected);
            Some(closure)
        }

        let case = crate::lean_vocab_test::lean_session_hydration_decision_cases()
            .iter()
            .find(|case| case.name == "admitted")
            .expect("modeled admitted hydration case");
        let closure = assert_input(
            &case.closure_input,
            case.expected_selected_documents.as_ref(),
        )
        .expect("admitted modeled closure");

        use super::super::session_hydration::{
            decide_hydration, AppliedPairingRoute, HydrationCatalog, HydrationRequest,
            HydrationVerdict, SessionOwner, VerifiedActiveMembership,
        };
        for case in crate::lean_vocab_test::lean_session_hydration_decision_cases() {
            let modeled_request = &case.closure_input.request;
            let request = HydrationRequest::from_row(
                modeled_request.key.clone(),
                modeled_request.requester.clone(),
                modeled_request.agent.clone(),
                modeled_request.session.clone(),
            )
            .expect("modeled hydration request key");
            let mut catalog = HydrationCatalog {
                applied_pairing_routes: BTreeSet::from([AppliedPairingRoute {
                    peer_id: request.peer_id.clone(),
                    requester_did: if case.pairing_requester_matches {
                        request.requester_did.clone()
                    } else {
                        "did:key:requester-2".into()
                    },
                    agent_did: if case.pairing_agent_matches {
                        request.agent_did.clone()
                    } else {
                        "did:key:agent-2".into()
                    },
                }]),
                selected_network_id: "network-1".into(),
                verified_active_memberships: BTreeSet::from([VerifiedActiveMembership {
                    network_id: if case.membership_network_matches {
                        "network-1".into()
                    } else {
                        "network-2".into()
                    },
                    member_did: request.requester_did.clone(),
                }]),
                sessions: BTreeSet::from([SessionOwner {
                    session_id: request.session_id.clone(),
                    requester_did: if case.owner_requester_matches {
                        request.requester_did.clone()
                    } else {
                        request.agent_did.clone()
                    },
                    agent_did: request.agent_did.clone(),
                }]),
                documents: closure.iter().cloned().collect(),
                authorized_reference_closure: closure
                    .iter()
                    .filter(|document| document.session_id != request.session_id)
                    .map(|document| {
                        gents_protocol::session_hydration::SessionHydrationDocumentKey {
                            collection: document.collection,
                            doc_id: document.doc_id.clone(),
                        }
                    })
                    .collect(),
            };
            if !case.paired {
                catalog.applied_pairing_routes.clear();
            }
            if !case.active_member {
                catalog.verified_active_memberships.clear();
            }
            if !case.owns_session {
                catalog.sessions.clear();
            }
            match decide_hydration(&request, &catalog) {
                HydrationVerdict::Admit(documents) => {
                    assert!(case.expected_admit, "{} unexpectedly admitted", case.name);
                    let actual = documents
                        .iter()
                        .map(|document| (document.collection, document.doc_id.clone()))
                        .collect::<BTreeSet<_>>();
                    let expected = case
                        .expected_selected_documents
                        .as_ref()
                        .expect("admitted modeled manifest")
                        .iter()
                        .map(|document| {
                            (
                                modeled_collection(&document.collection),
                                document.id.to_string(),
                            )
                        })
                        .collect::<BTreeSet<_>>();
                    assert_eq!(actual, expected, "{} composed manifest", case.name);
                }
                HydrationVerdict::Reject(_) => {
                    assert!(!case.expected_admit, "{} unexpectedly rejected", case.name);
                }
            }
        }

        for case in crate::lean_vocab_test::lean_session_hydration_closure_cases() {
            let Some(closure) = assert_input(
                &case.closure_input,
                case.expected_closure_documents.as_ref(),
            ) else {
                assert!(!case.expected_selected, "{}", case.name);
                continue;
            };
            let selection = &case.selection_request;
            let request = HydrationRequest::from_row(
                selection.key.clone(),
                selection.requester.clone(),
                selection.agent.clone(),
                selection.session.clone(),
            )
            .expect("modeled selection request key");
            let closure_request = &case.closure_input.request;
            let catalog = HydrationCatalog {
                applied_pairing_routes: BTreeSet::from([AppliedPairingRoute {
                    peer_id: closure_request.peer.clone(),
                    requester_did: closure_request.requester.clone(),
                    agent_did: closure_request.agent.clone(),
                }]),
                selected_network_id: "network-1".into(),
                verified_active_memberships: BTreeSet::from([VerifiedActiveMembership {
                    network_id: "network-1".into(),
                    member_did: closure_request.requester.clone(),
                }]),
                sessions: BTreeSet::from([SessionOwner {
                    session_id: closure_request.session.clone(),
                    requester_did: closure_request.requester.clone(),
                    agent_did: closure_request.agent.clone(),
                }]),
                documents: closure.iter().cloned().collect(),
                authorized_reference_closure: closure
                    .iter()
                    .filter(|document| document.session_id != closure_request.session)
                    .map(|document| {
                        gents_protocol::session_hydration::SessionHydrationDocumentKey {
                            collection: document.collection,
                            doc_id: document.doc_id.clone(),
                        }
                    })
                    .collect(),
            };
            match decide_hydration(&request, &catalog) {
                HydrationVerdict::Admit(documents) => {
                    assert!(
                        case.expected_selected,
                        "{} unexpectedly admitted",
                        case.name
                    );
                    let actual = documents
                        .iter()
                        .map(|document| (document.collection, document.doc_id.clone()))
                        .collect::<BTreeSet<_>>();
                    let expected = case
                        .expected_selected_documents
                        .as_ref()
                        .expect("selected modeled manifest")
                        .iter()
                        .map(|document| {
                            (
                                modeled_collection(&document.collection),
                                document.id.to_string(),
                            )
                        })
                        .collect::<BTreeSet<_>>();
                    assert_eq!(actual, expected, "{} selected manifest", case.name);
                }
                HydrationVerdict::Reject(_) => {
                    assert!(
                        !case.expected_selected,
                        "{} unexpectedly rejected",
                        case.name
                    );
                    assert!(case.expected_selected_documents.is_none(), "{}", case.name);
                }
            }
        }
    }

    #[test]
    fn fork_closure_retains_exact_origin_and_request_owner() {
        let origin = TranscriptMessageRow {
            doc_id: "origin".into(),
            message: message(
                "origin-session",
                Some("request"),
                MessagePublication::RequestExecution {
                    execution_generation: "generation".into(),
                },
            ),
        };
        let mut child_message = origin.message.clone();
        child_message.session_id = "child-session".into();
        child_message.request_doc_id = None;
        child_message.publication = MessagePublication::Fork {
            origin_message_doc_id: "origin".into(),
        };
        let headers = vec![
            TranscriptMessageRow {
                doc_id: "child".into(),
                message: child_message,
            },
            origin,
        ];
        let roots = vec!["child".into()];
        let bases = vec![base_request()];
        let closure = build_canonical_closure(CanonicalClosureInput {
            root_header_ids: &roots,
            headers: &headers,
            segments: &[],
            base_documents: &bases,
            denied_headers: &[],
            denied_segments: &[],
            dependency_denials: &[],
            agent_did: "agent",
            requester_did: Some("requester"),
            session_id: "child-session",
        })
        .expect("valid exact fork closure");
        assert!(closure.iter().any(|row| row.doc_id == "child"));
        assert!(closure.iter().any(|row| row.doc_id == "origin"));
        assert!(closure.iter().any(|row| row.doc_id == "request"));
        assert_eq!(closure.len(), 3);
    }

    #[test]
    fn absent_origin_is_incomplete_and_never_becomes_session_grant() {
        let child = TranscriptMessageRow {
            doc_id: "child".into(),
            message: message(
                "child-session",
                None,
                MessagePublication::Fork {
                    origin_message_doc_id: "missing".into(),
                },
            ),
        };
        let roots = vec!["child".into()];
        let result = build_canonical_closure(CanonicalClosureInput {
            root_header_ids: &roots,
            headers: &[child],
            segments: &[],
            base_documents: &[],
            denied_headers: &[],
            denied_segments: &[],
            dependency_denials: &[],
            agent_did: "agent",
            requester_did: Some("requester"),
            session_id: "child-session",
        });
        assert!(matches!(
            result,
            Err(CanonicalClosureError::MissingObservation(_))
        ));
    }

    #[test]
    fn explicit_origin_denial_fails_without_broadening_to_parent_session() {
        let child = TranscriptMessageRow {
            doc_id: "child".into(),
            message: message(
                "child-session",
                None,
                MessagePublication::Fork {
                    origin_message_doc_id: "denied".into(),
                },
            ),
        };
        let roots = vec!["child".into()];
        let denied = vec!["denied".into()];
        let result = build_canonical_closure(CanonicalClosureInput {
            root_header_ids: &roots,
            headers: &[child],
            segments: &[],
            base_documents: &[],
            denied_headers: &denied,
            denied_segments: &[],
            dependency_denials: &[],
            agent_did: "agent",
            requester_did: Some("requester"),
            session_id: "child-session",
        });
        let error = result.expect_err("explicit denial must fail hydration");
        assert!(matches!(&error, CanonicalClosureError::Unclassified(_)));
        assert!(error.to_string().contains("Denied"));
    }

    fn base_tool_call(doc_id: &str) -> ScopedDocument {
        ScopedDocument {
            collection: SessionHydrationCollection::AgentToolCall,
            doc_id: doc_id.into(),
            requester_did: "requester".into(),
            agent_did: "agent".into(),
            session_id: "origin-session".into(),
        }
    }

    fn tool_call_run(stream: u32, bytes: u32) -> SegmentRun {
        SegmentRun {
            stream,
            bytes,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::ToolOutput,
            }),
        }
    }

    fn closed_tool_output_segment(
        doc_id: &str,
        source: OutputSource,
        writer: OutputWriter,
    ) -> Vec<OutputSegmentRow> {
        let ordinal_zero = OutputSegment {
            agent_did: "agent".into(),
            requester_did: Some("requester".into()),
            session_id: "origin-session".into(),
            request_doc_id: "request".into(),
            source,
            writer,
            ordinal: Some(0),
            runs: vec![tool_call_run(0, 4)],
            payload: "data".into(),
            close: None,
            created_at: "2026-09-21T00:00:00Z".into(),
        };
        let closing = OutputSegment {
            ordinal: None,
            runs: Vec::new(),
            payload: String::new(),
            close: Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 1,
                stream_bytes: vec![4],
            }),
            ..ordinal_zero.clone()
        };
        vec![
            OutputSegmentRow {
                doc_id: format!("{doc_id}-flush"),
                segment: ordinal_zero,
            },
            OutputSegmentRow {
                doc_id: format!("{doc_id}-close"),
                segment: closing,
            },
        ]
    }

    fn tool_delivery_message(tool_call_doc_id: &str) -> TranscriptMessageRow {
        TranscriptMessageRow {
            doc_id: "delivered".into(),
            message: TranscriptMessage {
                message_key: "delivered-key".into(),
                session_id: "origin-session".into(),
                agent_did: "agent".into(),
                requester_did: Some("requester".into()),
                request_doc_id: Some("request".into()),
                publication: MessagePublication::ToolDelivery {
                    tool_call_doc_id: tool_call_doc_id.into(),
                },
                outcome: OutputOutcome::Complete,
                sequence: 7,
                role: MessageRole::User,
                native_id: None,
                blocks: vec![MessageBlock::ToolResult {
                    tool_call_doc_id: tool_call_doc_id.into(),
                    id: "tool-result".into(),
                    call_id: None,
                    parts: vec![ToolResultPart::Text {
                        text: PresentedPayload {
                            output: PayloadRef {
                                close_doc_id: "call-close".into(),
                                stream: 0,
                            },
                            presentation: PayloadPresentation::Full,
                        },
                    }],
                }],
                created_at: "2026-09-21T00:00:00Z".into(),
            },
        }
    }

    #[test]
    fn authored_delivery_written_by_tool_retains_its_exact_tool_call_base() {
        // Lean `requiredProvenance`: `.authored _, .tool call` requires
        // `[request, ⟨.agentToolCall, call⟩]` in the closure.
        let headers = vec![tool_delivery_message("call")];
        let segments = closed_tool_output_segment(
            "call",
            OutputSource::Authored {
                key: "background-receipt".into(),
            },
            OutputWriter::ToolExecution {
                tool_call_doc_id: "call".into(),
            },
        );
        let bases = vec![base_request(), base_tool_call("call")];
        let closure = build_canonical_closure(CanonicalClosureInput {
            root_header_ids: &["delivered".into()],
            headers: &headers,
            segments: &segments,
            base_documents: &bases,
            denied_headers: &[],
            denied_segments: &[],
            dependency_denials: &[],
            agent_did: "agent",
            requester_did: Some("requester"),
            session_id: "origin-session",
        })
        .expect("authored tool-writer delivery closure");
        assert!(
            closure.iter().any(
                |row| row.collection == SessionHydrationCollection::AgentToolCall
                    && row.doc_id == "call"
            ),
            "authored delivery must retain its exact tool call base"
        );
        assert!(closure.iter().any(|row| row.doc_id == "request"));
        assert!(closure.iter().any(|row| row.collection
            == SessionHydrationCollection::AgentOutputSegment
            && row.doc_id == "call-close"));
    }

    #[test]
    fn authored_delivery_missing_tool_writer_base_is_missing_observation() {
        // Regression: native collect must surface the exact missing
        // AgentToolCall base as MissingObservation, mirroring Lean
        // `validateProvenance .missing => Error.provenanceMissing`.
        let headers = vec![tool_delivery_message("call")];
        let segments = closed_tool_output_segment(
            "call",
            OutputSource::Authored {
                key: "background-receipt".into(),
            },
            OutputWriter::ToolExecution {
                tool_call_doc_id: "call".into(),
            },
        );
        let bases = vec![base_request()];
        let result = build_canonical_closure(CanonicalClosureInput {
            root_header_ids: &["delivered".into()],
            headers: &headers,
            segments: &segments,
            base_documents: &bases,
            denied_headers: &[],
            denied_segments: &[],
            dependency_denials: &[],
            agent_did: "agent",
            requester_did: Some("requester"),
            session_id: "origin-session",
        });
        let error = result.expect_err("missing tool call base must fail hydration");
        assert!(matches!(
            &error,
            CanonicalClosureError::MissingObservation(_)
        ));
        assert!(
            error.to_string().contains("AgentToolCall") && error.to_string().contains("call"),
            "error names the exact missing provenance: {error}"
        );
    }

    #[test]
    fn tool_call_source_retains_its_exact_tool_call_base() {
        // Lean `requiredProvenance`: `.tool call, _` requires the exact base
        // regardless of writer; this pins the pre-existing behavior.
        let headers = vec![tool_delivery_message("call")];
        let segments = closed_tool_output_segment(
            "call",
            OutputSource::ToolCall {
                tool_call_doc_id: "call".into(),
            },
            OutputWriter::ToolExecution {
                tool_call_doc_id: "call".into(),
            },
        );
        let bases = vec![base_request(), base_tool_call("call")];
        let closure = build_canonical_closure(CanonicalClosureInput {
            root_header_ids: &["delivered".into()],
            headers: &headers,
            segments: &segments,
            base_documents: &bases,
            denied_headers: &[],
            denied_segments: &[],
            dependency_denials: &[],
            agent_did: "agent",
            requester_did: Some("requester"),
            session_id: "origin-session",
        })
        .expect("tool call delivery closure");
        assert!(closure.iter().any(|row| row.collection
            == SessionHydrationCollection::AgentToolCall
            && row.doc_id == "call"));
    }

    #[test]
    fn immutable_header_twin_is_a_structural_conflict() {
        let first = TranscriptMessageRow {
            doc_id: "first".into(),
            message: message(
                "child-session",
                Some("request"),
                MessagePublication::RequestExecution {
                    execution_generation: "generation".into(),
                },
            ),
        };
        let mut twin = first.clone();
        twin.doc_id = "twin".into();
        let roots = vec!["first".into()];
        let result = build_canonical_closure(CanonicalClosureInput {
            root_header_ids: &roots,
            headers: &[first, twin],
            segments: &[],
            base_documents: &[base_request()],
            denied_headers: &[],
            denied_segments: &[],
            dependency_denials: &[],
            agent_did: "agent",
            requester_did: Some("requester"),
            session_id: "child-session",
        });
        assert!(matches!(
            result,
            Err(CanonicalClosureError::StructuralConflict(_))
        ));
    }
}
