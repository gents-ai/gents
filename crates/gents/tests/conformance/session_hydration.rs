//! Conformance fence for `Proofs/SessionHydration`.

use std::collections::BTreeSet;

use gents::agent::p2p_reconcile::session_hydration::{
    begin_hydration_request, can_retry_hydration, decide_hydration, observe_hydration_progress,
    AppliedPairingRoute, ClientHydrationPhase, ClientHydrationProgress, HydrationCatalog,
    HydrationDocument, HydrationRequest, HydrationVerdict, SessionOwner, VerifiedActiveMembership,
};

use crate::lean_vocab_test::{
    lean_session_hydration_decision_cases, lean_session_hydration_progress_cases,
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

fn document(id: &str, requester: &str, agent: &str, session: &str) -> HydrationDocument {
    HydrationDocument {
        collection: "AgentMessage".into(),
        doc_id: id.into(),
        requester_did: requester.into(),
        agent_did: agent.into(),
        session_id: session.into(),
    }
}

fn document_in_collection(
    collection: &str,
    id: &str,
    requester: &str,
    agent: &str,
    session: &str,
) -> HydrationDocument {
    HydrationDocument {
        collection: collection.into(),
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
        documents: BTreeSet::from([
            document(
                "owned",
                "did:key:requester-1",
                "did:key:agent-1",
                "session-1",
            ),
            document(
                "foreign-requester",
                "did:key:requester-2",
                "did:key:agent-1",
                "session-1",
            ),
            document(
                "foreign-session",
                "did:key:requester-1",
                "did:key:agent-1",
                "session-2",
            ),
            document_in_collection(
                "AgentSession",
                "wrong-collection",
                "did:key:requester-1",
                "did:key:agent-1",
                "session-1",
            ),
        ]),
    }
}

#[test]
fn generated_session_hydration_cases_match_decision_core() {
    let req = request();
    let base = admitted_catalog();
    let cases = lean_session_hydration_decision_cases();
    assert_eq!(cases.len(), 7);

    for case in cases {
        let mut catalog = base.clone();
        if !case.paired {
            catalog.applied_pairing_routes.clear();
        } else if !case.pairing_requester_matches || !case.pairing_agent_matches {
            catalog.applied_pairing_routes = BTreeSet::from([AppliedPairingRoute {
                peer_id: req.peer_id.clone(),
                requester_did: if case.pairing_requester_matches {
                    req.requester_did.clone()
                } else {
                    "did:key:requester-2".into()
                },
                agent_did: if case.pairing_agent_matches {
                    req.agent_did.clone()
                } else {
                    "did:key:agent-2".into()
                },
            }]);
        }
        if !case.active_member {
            catalog.verified_active_memberships.clear();
        } else if !case.membership_network_matches {
            catalog.verified_active_memberships = BTreeSet::from([VerifiedActiveMembership {
                network_id: "network-2".into(),
                member_did: req.requester_did.clone(),
            }]);
        }
        if !case.owns_session {
            catalog.sessions.clear();
        }
        match decide_hydration(&req, &catalog) {
            HydrationVerdict::Admit(documents) => {
                assert!(case.expected_admit, "{} unexpectedly admitted", case.name);
                assert_eq!(
                    documents.len(),
                    case.expected_selected_count,
                    "{}",
                    case.name
                );
            }
            HydrationVerdict::Reject(_) => {
                assert!(!case.expected_admit, "{} unexpectedly rejected", case.name);
            }
        }
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
            served_count: case.prev_served,
        };
        let observed = observe_hydration_progress(
            &prev,
            &case.session,
            &case.agent,
            case.merged,
            case.served,
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
                next.served_count
                    .is_some_and(|served| next.merged_count >= served),
                "{} completed without covering served_doc_count",
                case.name
            );
        }
    }
}
