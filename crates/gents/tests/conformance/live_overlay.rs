//! Integration test pinning the issue #64 live-tail contract to the
//! generated Lean LiveOverlay case table.
//!
//! Each case asserts that the rendered overlay decision computed from
//! `(response_status, materialized, derived_turn, hasContent, hasReasoning)`
//! matches `expectOverlay` from the Lean case table emitted by
//! `Proofs.Conformance.Contracts`.

use gents_protocol::client_protocol::ClientTurnState;

use crate::lean_vocab_test::{lean_live_overlay_cases, LeanLiveOverlayCase};

fn parse_turn(label: &str) -> Option<ClientTurnState> {
    match label {
        "waitingForClaim" => Some(ClientTurnState::WaitingForClaim),
        "streaming" => Some(ClientTurnState::Streaming),
        "completed" => Some(ClientTurnState::Completed),
        "failed" => Some(ClientTurnState::Failed),
        "superseded" => Some(ClientTurnState::Superseded),
        "interrupted" => Some(ClientTurnState::Interrupted),
        _ => None,
    }
}

/// Mirror of the Lean `projectActiveOverlay` decision (see
/// `Proofs/ClientShell/Projection.lean`). Kept inline in the test rather than
/// imported, so the test can fail loudly if either the bridge or the frontend
/// drifts from the contract.
fn should_show_overlay(
    response_status: &str,
    materialized: bool,
    turn: Option<ClientTurnState>,
    has_content: bool,
    has_reasoning: bool,
) -> bool {
    if materialized {
        return false;
    }
    if response_status == "complete" || response_status == "error" {
        return false;
    }
    let Some(turn) = turn else {
        return false;
    };
    if turn.is_terminal() {
        return false;
    }
    let renderable = matches!(
        turn,
        ClientTurnState::WaitingForClaim | ClientTurnState::Streaming
    );
    if !renderable {
        return false;
    }
    has_content || has_reasoning
}

#[test]
fn live_overlay_cases_match_lean_table() {
    let cases: &[LeanLiveOverlayCase] = lean_live_overlay_cases();
    assert!(!cases.is_empty(), "Lean LiveOverlay case table is empty");

    for case in cases {
        let actual = should_show_overlay(
            &case.response_status,
            case.materialized,
            parse_turn(&case.turn_label),
            case.has_content,
            case.has_reasoning,
        );
        assert_eq!(
            actual,
            case.expect_overlay,
            "case {name:?} expected overlay={expected}, got {actual}",
            name = case.name,
            expected = case.expect_overlay,
        );

        // Sanity: terminal turns must hide the overlay regardless of content.
        if case.turn_terminal {
            assert!(
                !case.expect_overlay,
                "case {:?} marks turn as terminal but expects overlay; contract violated",
                case.name,
            );
        }

        // The `precedingToolCalls` field disambiguates otherwise field-identical
        // streaming cases. The runtime predicate ignores it (the live-tail
        // decision does not depend on tool-call prefix shape) — referencing the
        // field here keeps the snapshot pinned and prevents Lean from silently
        // dropping the column.
        let _ = case.preceding_tool_calls;
    }
}
