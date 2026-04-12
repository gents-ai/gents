use super::*;

fn req(lifecycle: &str) -> RequestSnapshot {
    RequestSnapshot {
        lifecycle_state: lifecycle.to_string(),
        is_superseded: false,
    }
}

fn req_superseded(lifecycle: &str) -> RequestSnapshot {
    RequestSnapshot {
        lifecycle_state: lifecycle.to_string(),
        is_superseded: true,
    }
}

fn resp(status: ResponseStatus) -> Option<ResponseSnapshot> {
    Some(ResponseSnapshot { status })
}

fn attempt(lifecycle: &str, response: Option<ResponseSnapshot>) -> AttemptView {
    AttemptView {
        request: req(lifecycle),
        response,
    }
}

// ── Derivation table coverage (T4) ──────────────────────────────

#[test]
fn pending_no_response() {
    assert_eq!(
        derive_attempt(&attempt("pending", None)),
        ClientTurnState::WaitingForClaim
    );
}

#[test]
fn claimed_no_response() {
    assert_eq!(
        derive_attempt(&attempt("claimed", None)),
        ClientTurnState::WaitingForClaim
    );
}

#[test]
fn processing_no_response() {
    assert_eq!(
        derive_attempt(&attempt("processing", None)),
        ClientTurnState::WaitingForClaim
    );
}

#[test]
fn input_required_no_response() {
    assert_eq!(
        derive_attempt(&attempt("inputRequired", None)),
        ClientTurnState::WaitingForClaim
    );
}

#[test]
fn processing_streaming_response() {
    assert_eq!(
        derive_attempt(&attempt("processing", resp(ResponseStatus::Streaming))),
        ClientTurnState::Streaming
    );
}

#[test]
fn processing_complete_response() {
    assert_eq!(
        derive_attempt(&attempt("processing", resp(ResponseStatus::Complete))),
        ClientTurnState::Completed
    );
}

#[test]
fn processing_error_response() {
    assert_eq!(
        derive_attempt(&attempt("processing", resp(ResponseStatus::Error))),
        ClientTurnState::Failed
    );
}

#[test]
fn completed_lifecycle() {
    assert_eq!(
        derive_attempt(&attempt("completed", None)),
        ClientTurnState::Completed
    );
}

#[test]
fn completed_lifecycle_ignores_stale_streaming() {
    assert_eq!(
        derive_attempt(&attempt("completed", resp(ResponseStatus::Streaming))),
        ClientTurnState::Completed
    );
}

#[test]
fn failed_lifecycle() {
    assert_eq!(
        derive_attempt(&attempt("failed", None)),
        ClientTurnState::Failed
    );
}

#[test]
fn failed_lifecycle_ignores_stale_streaming() {
    assert_eq!(
        derive_attempt(&attempt("failed", resp(ResponseStatus::Streaming))),
        ClientTurnState::Failed
    );
}

#[test]
fn dead_lifecycle() {
    assert_eq!(
        derive_attempt(&attempt("dead", None)),
        ClientTurnState::Failed
    );
}

#[test]
fn superseded_lifecycle() {
    assert_eq!(
        derive_attempt(&attempt("superseded", None)),
        ClientTurnState::Superseded
    );
}

#[test]
fn superseded_flag_overrides_everything() {
    let view = AttemptView {
        request: req_superseded("processing"),
        response: resp(ResponseStatus::Streaming),
    };
    assert_eq!(derive_attempt(&view), ClientTurnState::Superseded);
}

// ── Response-before-request replication lag ──────────────────────

#[test]
fn pending_with_complete_response_trusts_response() {
    assert_eq!(
        derive_attempt(&attempt("pending", resp(ResponseStatus::Complete))),
        ClientTurnState::Completed
    );
}

#[test]
fn claimed_with_streaming_response_trusts_response() {
    assert_eq!(
        derive_attempt(&attempt("claimed", resp(ResponseStatus::Streaming))),
        ClientTurnState::Streaming
    );
}

// ── deriveTurn: retry chain ─────────────────────────────────────

#[test]
fn derive_turn_empty() {
    assert_eq!(derive_turn(&[]), None);
}

#[test]
fn derive_turn_single() {
    let chain = vec![attempt("processing", resp(ResponseStatus::Streaming))];
    assert_eq!(derive_turn(&chain), Some(ClientTurnState::Streaming));
}

#[test]
fn derive_turn_uses_tip() {
    let chain = vec![
        attempt("failed", None),
        attempt("pending", None),
    ];
    assert_eq!(derive_turn(&chain), Some(ClientTurnState::WaitingForClaim));
}

#[test]
fn derive_turn_three_attempt_chain() {
    let chain = vec![
        attempt("failed", None),
        attempt("failed", None),
        attempt("processing", resp(ResponseStatus::Streaming)),
    ];
    assert_eq!(derive_turn(&chain), Some(ClientTurnState::Streaming));
}
