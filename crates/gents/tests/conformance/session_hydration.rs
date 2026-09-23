//! Conformance fence for `Proofs/SessionHydration`.

use std::collections::BTreeSet;

use gents::agent::p2p_reconcile::session_hydration::{
    apply_hydration_delivery, begin_hydration_request, can_retry_hydration, decide_hydration,
    observe_hydration_progress, AppliedPairingRoute, ClientHydrationPhase, ClientHydrationProgress,
    HydrationApplyOutcome, HydrationCatalog, HydrationDeliveryResult, HydrationDocument,
    HydrationRequest, HydrationTerminalWriteResult, HydrationVerdict, SessionHydrationCollection,
    SessionHydrationDocumentKey, SessionOwner, VerifiedActiveMembership,
};

use crate::lean_vocab_test::{
    lean_session_hydration_apply_cases, lean_session_hydration_durable_cases,
    lean_session_hydration_progress_cases,
};

fn request() -> HydrationRequest {
    HydrationRequest::from_row(
        "peer-1:session-1".into(),
        "did:key:requester-1".into(),
        "did:key:agent-1".into(),
        "session-1".into(),
    )
    .expect("valid hydration key")
}

fn hydration_keys(count: usize, exact: bool) -> BTreeSet<SessionHydrationDocumentKey> {
    let stem = if exact { "doc-" } else { "foreign-" };
    (0..count)
        .map(|index| SessionHydrationDocumentKey {
            collection: SessionHydrationCollection::AgentMessage,
            doc_id: format!("{stem}{index}"),
        })
        .collect()
}

#[test]
fn generated_session_hydration_durable_cases_match_storage_projection() {
    use gents::agent::p2p_reconcile::session_hydration::{
        project_durable_hydration_progress, ClientHydrationRequestState,
    };

    let cases = lean_session_hydration_durable_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let request = match case.status.as_str() {
            "pending" => ClientHydrationRequestState::Pending,
            "served" => ClientHydrationRequestState::Served(hydration_keys(
                case.served.unwrap_or(0),
                case.served_matches,
            )),
            "rejected" => ClientHydrationRequestState::Rejected(
                case.served
                    .map(|count| hydration_keys(count, case.served_matches)),
            ),
            _ => ClientHydrationRequestState::Missing,
        };
        let progress = project_durable_hydration_progress(
            "session-exact",
            "agent-exact",
            hydration_keys(case.merged, true),
            request,
        );
        assert_eq!(progress.session_id, "session-exact", "{}", case.name);
        assert_eq!(progress.agent_did, "agent-exact", "{}", case.name);
        assert_eq!(
            progress.phase.as_str(),
            case.expected_phase,
            "{}",
            case.name
        );
        assert_eq!(progress.merged_count, case.expected_merged, "{}", case.name);
        assert_eq!(
            progress.covered_count, case.expected_covered,
            "{} covered count",
            case.name
        );
    }
}

fn document(
    collection: SessionHydrationCollection,
    id: &str,
    requester: &str,
    agent: &str,
    session: &str,
) -> HydrationDocument {
    HydrationDocument {
        collection,
        doc_id: id.into(),
        requester_did: requester.into(),
        agent_did: agent.into(),
        session_id: session.into(),
    }
}

fn admitted_catalog() -> HydrationCatalog {
    HydrationCatalog {
        applied_pairing_routes: BTreeSet::from([AppliedPairingRoute {
            peer_id: "peer-1".into(),
            requester_did: "did:key:requester-1".into(),
            agent_did: "did:key:agent-1".into(),
        }]),
        selected_network_id: "network-1".into(),
        verified_active_memberships: BTreeSet::from([VerifiedActiveMembership {
            network_id: "network-1".into(),
            member_did: "did:key:requester-1".into(),
        }]),
        sessions: BTreeSet::from([SessionOwner {
            session_id: "session-1".into(),
            requester_did: "did:key:requester-1".into(),
            agent_did: "did:key:agent-1".into(),
        }]),
        // Cross-session dependencies are never inferred from an origin scan:
        // the base catalog grants no reference closure, so only the request's
        // own session documents are admitted.
        authorized_reference_closure: BTreeSet::new(),
        documents: BTreeSet::from([
            document(
                SessionHydrationCollection::AgentMessage,
                "owned",
                "did:key:requester-1",
                "did:key:agent-1",
                "session-1",
            ),
            document(
                SessionHydrationCollection::AgentMessage,
                "foreign-requester",
                "did:key:requester-2",
                "did:key:agent-1",
                "session-1",
            ),
            document(
                SessionHydrationCollection::AgentMessage,
                "foreign-session",
                "did:key:requester-1",
                "did:key:agent-1",
                "session-2",
            ),
            document(
                SessionHydrationCollection::AgentMessage,
                "foreign-agent",
                "did:key:requester-1",
                "did:key:agent-2",
                "session-1",
            ),
        ]),
    }
}

#[test]
fn generated_session_hydration_apply_cases_match_terminal_delivery_core() {
    let cases = lean_session_hydration_apply_cases();
    assert_eq!(cases.len(), 6);
    for case in cases {
        // This group starts at the delivery owner's verdict boundary. Its
        // exact input set comes from the modeled admission/closure owner;
        // closure-to-admission itself is exercised by the native closure tests.
        let verdict = match (case.admitted, &case.input_documents) {
            (true, Some(documents)) => HydrationVerdict::Admit(
                documents
                    .iter()
                    .map(|key| {
                        let collection = match key.collection.as_str() {
                            "AgentRequest" => SessionHydrationCollection::AgentRequest,
                            "AgentMessage" => SessionHydrationCollection::AgentMessage,
                            "AgentToolCall" => SessionHydrationCollection::AgentToolCall,
                            "AgentOutputSegment" => SessionHydrationCollection::AgentOutputSegment,
                            "CompactionEntry" => SessionHydrationCollection::CompactionEntry,
                            other => panic!("unknown modeled hydration collection {other}"),
                        };
                        document(
                            collection,
                            &key.id.to_string(),
                            &case.request.requester,
                            &case.request.agent,
                            &case.request.session,
                        )
                    })
                    .collect(),
            ),
            _ => HydrationVerdict::Reject("modeled admission rejected"),
        };
        let delivery = if case.delivery_confirmed {
            HydrationDeliveryResult::Confirmed
        } else {
            HydrationDeliveryResult::Indeterminate
        };
        let terminal_write = match case.terminal_write.as_str() {
            "committed" => HydrationTerminalWriteResult::Committed,
            "failed" => HydrationTerminalWriteResult::Failed,
            "not_attempted" => HydrationTerminalWriteResult::NotAttempted,
            value => panic!("unknown terminal write result {value:?}"),
        };
        let input_documents = match &verdict {
            HydrationVerdict::Admit(documents) => documents.clone(),
            HydrationVerdict::Reject(_) => BTreeSet::new(),
        };
        let outcome = apply_hydration_delivery(verdict, delivery, terminal_write);
        let attempted = match &outcome {
            HydrationApplyOutcome::Served(documents) => documents,
            HydrationApplyOutcome::Rejected {
                attempted_documents,
                ..
            }
            | HydrationApplyOutcome::PendingAfterTerminalWriteFailure {
                attempted_documents,
                ..
            }
            | HydrationApplyOutcome::PendingAfterIndeterminateDelivery {
                attempted_documents,
            } => attempted_documents,
        };
        assert_eq!(
            attempted, &input_documents,
            "{} exact delivery input",
            case.name
        );
        let (served, rejected, attempted_count, confirmed_count) = match outcome {
            HydrationApplyOutcome::Served(documents) => {
                (true, false, documents.len(), documents.len())
            }
            HydrationApplyOutcome::Rejected {
                attempted_documents,
                ..
            } => (false, true, attempted_documents.len(), 0),
            HydrationApplyOutcome::PendingAfterTerminalWriteFailure {
                attempted_documents,
                confirmed_documents,
            } => (
                false,
                false,
                attempted_documents.len(),
                confirmed_documents.len(),
            ),
            HydrationApplyOutcome::PendingAfterIndeterminateDelivery {
                attempted_documents,
            } => (false, false, attempted_documents.len(), 0),
        };
        assert_eq!(served, case.expected_served, "{}", case.name);
        assert_eq!(rejected, case.expected_rejected, "{}", case.name);
        assert_eq!(
            attempted_count, case.expected_attempted_count,
            "{} attempted documents",
            case.name
        );
        assert_eq!(
            confirmed_count, case.expected_confirmed_count,
            "{} confirmed documents",
            case.name
        );
    }
}

/// Mirrors Lean `selected_tenancy_sound` and `selected_session_sound`.
#[test]
fn admitted_selection_is_exactly_requester_agent_session_scoped() {
    let req = request();
    let HydrationVerdict::Admit(documents) = decide_hydration(&req, &admitted_catalog()) else {
        panic!("request should be admitted");
    };
    assert_eq!(
        documents,
        BTreeSet::from([document(
            SessionHydrationCollection::AgentMessage,
            "owned",
            "did:key:requester-1",
            "did:key:agent-1",
            "session-1",
        )])
    );
}

#[test]
fn request_key_binds_peer_and_session() {
    assert!(HydrationRequest::from_row(
        "peer-1:other-session".into(),
        "did:key:requester-1".into(),
        "did:key:agent-1".into(),
        "session-1".into(),
    )
    .is_err());
}

#[test]
fn generated_session_hydration_progress_cases_match_observe() {
    let cases = lean_session_hydration_progress_cases();
    assert!(!cases.is_empty());
    assert!(
        cases
            .iter()
            .any(|case| case.name == "failed_stays_failed_without_retry"),
        "Lean contract must include the passive-observation terminality witness"
    );
    for case in cases {
        let prev = ClientHydrationProgress {
            session_id: case.prev_session.clone(),
            agent_did: case.prev_agent.clone(),
            phase: ClientHydrationPhase::parse(&case.prev_phase),
            merged_count: case.prev_merged,
            covered_count: case.prev_merged,
            served_count: case.prev_served,
            merged_documents: hydration_keys(case.prev_merged, true),
            served_documents: case.prev_served.map(|count| hydration_keys(count, true)),
        };
        let observed = observe_hydration_progress(
            &prev,
            &case.session,
            &case.agent,
            hydration_keys(case.merged, true),
            case.served
                .map(|count| hydration_keys(count, case.served_matches)),
            case.failed,
        );
        assert_eq!(
            can_retry_hydration(&observed, &case.session, &case.agent),
            case.expected_retry_admit,
            "{} retry admission after observation",
            case.name
        );
        let next = if case.begin_request {
            begin_hydration_request(&case.session, &case.agent)
        } else {
            observed
        };
        assert_eq!(next.phase.as_str(), case.expected_phase, "{}", case.name);
        assert_eq!(next.merged_count, case.expected_merged, "{}", case.name);
        assert_eq!(
            next.covered_count, case.expected_covered,
            "{} covered count",
            case.name
        );
        if let Some(served_count) = next.served_count {
            assert!(
                next.covered_count <= served_count,
                "{} covered count must be bounded by the manifest",
                case.name
            );
        }
        assert_eq!(
            next.phase == ClientHydrationPhase::Complete,
            case.expected_complete,
            "{}",
            case.name
        );
        if !case.begin_request && case.prev_session == case.session && case.prev_agent == case.agent
        {
            assert!(
                next.merged_count >= prev.merged_count,
                "{} merged count must be monotone within one target",
                case.name
            );
        } else {
            assert_eq!(next.session_id, case.session, "{}", case.name);
            assert_eq!(next.agent_did, case.agent, "{}", case.name);
        }
        if case.expected_complete {
            assert!(
                next.served_documents
                    .as_ref()
                    .is_some_and(|served| served.is_subset(&next.merged_documents)),
                "{} completed without covering the exact served manifest",
                case.name
            );
        }
    }
}

#[test]
fn hydration_coverage_distinguishes_same_doc_id_in_different_collections() {
    use gents::agent::p2p_reconcile::session_hydration::{
        project_durable_hydration_progress, ClientHydrationRequestState,
    };
    // Lean document references are interned (collection, _docID) pairs. Matching
    // an ID from another collection must not satisfy the served manifest.
    let message = SessionHydrationDocumentKey {
        collection: SessionHydrationCollection::AgentMessage,
        doc_id: "shared-id".into(),
    };
    let segment = SessionHydrationDocumentKey {
        collection: SessionHydrationCollection::AgentOutputSegment,
        doc_id: "shared-id".into(),
    };
    let served = BTreeSet::from([message.clone()]);
    let incomplete = project_durable_hydration_progress(
        "session",
        "agent",
        BTreeSet::from([segment.clone()]),
        ClientHydrationRequestState::Served(served.clone()),
    );
    assert_eq!(incomplete.phase, ClientHydrationPhase::Serving);
    assert_eq!(incomplete.covered_count, 0);
    let complete = project_durable_hydration_progress(
        "session",
        "agent",
        BTreeSet::from([segment, message]),
        ClientHydrationRequestState::Served(served),
    );
    assert_eq!(complete.phase, ClientHydrationPhase::Complete);
    assert_eq!(complete.covered_count, 1);
    assert_eq!(complete.merged_count, 2);
}

/// The hydration manifest has a closed collection vocabulary. Neither a session
/// control document nor the retired response collection is a transcript dependency.
#[test]
fn retired_collection_names_are_rejected_by_serde() {
    use gents_protocol::session_hydration::SessionHydrationDocumentKey as ProtocolKey;

    for retired in ["AgentSession", "AgentResponse"] {
        let raw = format!(r#"{{"collection": "{retired}", "doc_id": "doc-1"}}"#);
        let error = serde_json::from_str::<ProtocolKey>(&raw)
            .expect_err("retired collection must not deserialize");
        assert!(
            error.to_string().contains("unknown variant"),
            "{retired} must be rejected as an unknown collection variant: {error}"
        );
    }
}
