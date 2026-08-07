//! Conformance fence for the abstract signed-ingest provenance decision.
//!
//! This does not implement DefraDB signature retrieval. It checks the generated
//! Lean decision table at the seam the later runtime integration must satisfy:
//! expected source authorship, an unambiguous exact source CID, and a target
//! agent claim bound to the same parent and payload.

use crate::lean_vocab_test::{lean_request_ingest_cases, LeanRequestIngestCase};

fn source_is_admitted(case: &LeanRequestIngestCase) -> bool {
    case.source_signature_valid
        && case.source_signer_did == case.expected_source_signer_did
        && case.source_head_count == 1
        && case.observed_source_cid == case.source_cid
}

fn evaluate(case: &LeanRequestIngestCase) -> &'static str {
    if !source_is_admitted(case) {
        return "sourceRejected";
    }

    if case.claim_signature_valid
        && case.claim_signer_did == case.target_agent_did
        && case.claim_parent_cid == case.source_cid
        && case.claim_payload == case.source_payload
    {
        "admitted"
    } else {
        "claimRejected"
    }
}

#[test]
fn generated_request_ingest_cases_fence_provenance_invariants() {
    let cases = lean_request_ingest_cases();
    assert_eq!(cases.len(), 12, "the signed-ingest decision table drifted");

    for case in cases {
        assert!(
            matches!(case.origin.as_str(), "external" | "internal"),
            "{} emitted an unknown origin",
            case.name
        );
        assert_eq!(
            source_is_admitted(case),
            case.source_admitted,
            "{} drifted from Lean source admission",
            case.name
        );
        assert_eq!(
            evaluate(case),
            case.outcome,
            "{} drifted from the Lean ingest outcome",
            case.name
        );
    }
}

#[test]
fn generated_internal_request_keeps_requester_distinct_from_author() {
    let case = lean_request_ingest_cases()
        .iter()
        .find(|case| case.name == "valid_internal_request_with_distinct_requester")
        .expect("Lean must emit the internal attribution witness");

    assert_eq!(case.origin, "internal");
    assert_ne!(case.requester_did, case.source_author_did);
    assert_eq!(case.source_signer_did, case.source_author_did);
    assert_eq!(case.expected_source_signer_did, case.source_author_did);
    assert_eq!(case.claim_signer_did, case.target_agent_did);
    assert!(case.source_admitted);
    assert_eq!(case.outcome, "admitted");
}
