use super::*;
use crate::identity::NodeIdentity;
use gents_protocol::session::{SessionCreationCause, SessionCreationReceipt};

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
    let identity =
        crate::KeyIdentity::load_or_create(directory.path().join("node.key"), None).unwrap();
    let did = identity.did();
    for case in &crate::lean_vocab_test::lean_causal_hop_contract().creation_cases {
        let mut receipt = SessionCreationReceipt {
            node_did: did.into(),
            session_id: if case.scope_matches {
                "session"
            } else {
                "different"
            }
            .into(),
            requester_did: Some(did.into()),
            session_doc_id: if case.physical_matches {
                "physical"
            } else {
                "recreated"
            }
            .into(),
            agent_id: "agent".into(),
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
        receipt.signature = identity.sign(&receipt.signing_payload()).await.unwrap();
        if !case.signature_valid {
            receipt.signature[0] ^= 1;
        }
        let session: AgentSession = serde_json::from_value(serde_json::json!({
            "node_did": did, "session_id": "session", "requester_did": did,
            "agent_id": "agent", "created_at": "2026-01-01T00:00:00Z",
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
    }
}
