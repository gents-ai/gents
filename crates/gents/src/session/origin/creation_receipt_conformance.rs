use super::*;
use crate::store::identity::NodeIdentity;
use gents_protocol::session::{SessionCreationCause, SessionCreationReceipt};

#[tokio::test]
async fn verified_receipt_cannot_authorize_a_different_opening_request() {
    let directory = tempfile::tempdir().unwrap();
    let agent =
        crate::KeyIdentity::load_or_create(directory.path().join("agent.key"), None).unwrap();
    let mut receipt = SessionCreationReceipt {
        node_did: "did:example:node".into(),
        session_id: "same-session".into(),
        session_doc_id: "physical-session".into(),
        requester_did: Some(agent.did().into()),
        agent_did: agent.did().into(),
        opening_request_doc_id: "first-opening".into(),
        cause: SessionCreationCause::AgentNew,
        cause_binding: "tool-call".into(),
        starter: None,
        signature: Vec::new(),
    };
    receipt.signature = agent.sign(&receipt.signing_payload()).await.unwrap();
    let session: AgentSession = serde_json::from_value(serde_json::json!({
        "node_did": receipt.node_did, "session_id": receipt.session_id,
        "requester_did": receipt.requester_did, "agent_did": receipt.agent_did,
        "created_at": "2026-01-01T00:00:00Z", "provenance": {"creation_receipt": receipt}
    }))
    .unwrap();
    assert!(
        verified_agent_creation_receipt(&session, "physical-session", Some("first-opening"))
            .is_some()
    );
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().delegated_publication_cases;
    let mismatch = cases
        .iter()
        .find(|case| case["name"] == "mismatched-recovery-binding")
        .unwrap();
    assert_eq!(
        verified_agent_creation_receipt(&session, "physical-session", Some("second-opening"))
            .is_some(),
        mismatch["expected_target_read"].as_bool().unwrap()
    );
}

#[test]
fn job_creation_binding_matches_executable_model() {
    for case in &crate::lean_vocab_test::lean_causal_hop_contract().job_creation_cases {
        assert_eq!(
            super::job_creation_matches(case.parent, case.job, case.key, case.spawn),
            case.expected
        );
    }
}

#[test]
fn deferred_creation_matches_executable_model() {
    for case in &crate::lean_vocab_test::lean_causal_hop_contract().deferred_creation_cases {
        assert_eq!(
            super::deferred_creation_allowed(case.kind, case.receipt, case.opening, case.cause),
            case.expected
        );
    }
}

#[tokio::test]
async fn receipt_verification_matches_executable_creation_model() {
    let directory = tempfile::tempdir().unwrap();
    let node = crate::KeyIdentity::load_or_create(directory.path().join("node.key"), None).unwrap();
    let agent =
        crate::KeyIdentity::load_or_create(directory.path().join("agent.key"), None).unwrap();
    let user = crate::KeyIdentity::load_or_create(directory.path().join("user.key"), None).unwrap();
    assert_ne!(node.did(), agent.did());
    assert_ne!(node.did(), user.did());
    assert_ne!(agent.did(), user.did());
    for case in &crate::lean_vocab_test::lean_causal_hop_contract().creation_cases {
        let mut receipt = SessionCreationReceipt {
            node_did: node.did().into(),
            session_id: if case.scope_matches {
                "session"
            } else {
                "different"
            }
            .into(),
            requester_did: Some(user.did().into()),
            session_doc_id: if case.physical_matches {
                "physical"
            } else {
                "recreated"
            }
            .into(),
            agent_did: agent.did().into(),
            opening_request_doc_id: "opening".into(),
            cause: if case.cause_matches {
                SessionCreationCause::AgentNew
            } else {
                SessionCreationCause::TaskTrigger
            },
            cause_binding: "binding".into(),
            starter: None,
            signature: Vec::new(),
        };
        receipt.signature = user.sign(&receipt.signing_payload()).await.unwrap();
        if !case.signature_valid {
            receipt.signature[0] ^= 1;
        }
        let session: AgentSession = serde_json::from_value(serde_json::json!({
            "node_did": node.did(), "session_id": "session", "requester_did": user.did(),
            "agent_did": agent.did(), "created_at": "2026-01-01T00:00:00Z",
            "provenance": {"creation_receipt": receipt}
        }))
        .unwrap();
        assert_eq!(
            verified_creation_receipt(&session, "physical", SessionCreationCause::AgentNew)
                .is_some(),
            case.expected,
            "{}",
            case.name
        );
        if !case.signature_valid {
            for signer in [&node, &agent] {
                let mut forged = session.clone();
                let receipt = forged
                    .provenance
                    .as_mut()
                    .unwrap()
                    .creation_receipt
                    .as_mut()
                    .unwrap();
                receipt.signature = signer.sign(&receipt.signing_payload()).await.unwrap();
                assert_eq!(
                    verified_creation_receipt(&forged, "physical", SessionCreationCause::AgentNew)
                        .is_some(),
                    case.expected,
                    "{} signed by {}",
                    case.name,
                    signer.did()
                );
            }
        }
        if !case.scope_matches {
            for field in ["node", "requester", "agent"] {
                let mut foreign = session.clone();
                foreign.session_id = "different".into();
                match field {
                    "node" => foreign.node_did = user.did().into(),
                    "requester" => foreign.requester_did = Some(node.did().into()),
                    "agent" => foreign.agent_did = node.did().into(),
                    _ => unreachable!(),
                }
                assert_eq!(
                    verified_creation_receipt(&foreign, "physical", SessionCreationCause::AgentNew)
                        .is_some(),
                    case.expected,
                    "{} mismatched {field}",
                    case.name
                );
            }
        }
    }
}

#[tokio::test]
async fn receipt_signer_matches_creation_model() {
    let directory = tempfile::tempdir().unwrap();
    let creator =
        crate::KeyIdentity::load_or_create(directory.path().join("creator.key"), None).unwrap();
    let worker =
        crate::KeyIdentity::load_or_create(directory.path().join("worker.key"), None).unwrap();
    let other =
        crate::KeyIdentity::load_or_create(directory.path().join("other.key"), None).unwrap();
    for case in &crate::lean_vocab_test::lean_causal_hop_contract().creation_signer_cases {
        let cause = if case.agent_new {
            SessionCreationCause::AgentNew
        } else {
            SessionCreationCause::TaskTrigger
        };
        let mut receipt = SessionCreationReceipt {
            node_did: "node".into(),
            session_id: "session".into(),
            session_doc_id: "physical".into(),
            requester_did: case.has_requester.then(|| creator.did().to_owned()),
            agent_did: worker.did().into(),
            opening_request_doc_id: "opening".into(),
            cause,
            cause_binding: "binding".into(),
            starter: None,
            signature: Vec::new(),
        };
        let signer = match case.signer.as_str() {
            "creator" => &creator,
            "worker" => &worker,
            _ => &other,
        };
        receipt.signature = signer.sign(&receipt.signing_payload()).await.unwrap();
        let session: AgentSession = serde_json::from_value(serde_json::json!({
            "node_did": receipt.node_did, "session_id": receipt.session_id,
            "requester_did": receipt.requester_did, "agent_did": receipt.agent_did,
            "created_at": "2026-01-01T00:00:00Z", "provenance": {"creation_receipt": receipt}
        }))
        .unwrap();
        assert_eq!(
            verified_creation_receipt(&session, "physical", cause).is_some(),
            case.expected,
            "{case:?}"
        );
        if case.agent_new {
            let mut job_receipt = session
                .provenance
                .as_ref()
                .unwrap()
                .creation_receipt
                .clone()
                .unwrap();
            job_receipt.cause = SessionCreationCause::PluginJob;
            job_receipt.signature = signer.sign(&job_receipt.signing_payload()).await.unwrap();
            let mut job_session = session.clone();
            job_session.provenance.as_mut().unwrap().creation_receipt = Some(job_receipt);
            assert_eq!(
                verified_creation_receipt(
                    &job_session,
                    "physical",
                    SessionCreationCause::PluginJob
                )
                .is_some(),
                case.expected,
                "PluginJob: {case:?}"
            );
        }
    }
}
