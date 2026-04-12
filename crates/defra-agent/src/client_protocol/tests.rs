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

// ── Monotonicity spot checks (T2) ───────────────────────────────

/// All valid server lifecycle transition pairs and their expected
/// rank relationship.
const LIFECYCLE_TRANSITIONS: &[(&str, &str)] = &[
    ("pending", "claimed"),       // claim
    ("pending", "superseded"),    // dedup_lose
    ("claimed", "processing"),    // begin_inference
    ("processing", "completed"),  // finish
    ("processing", "failed"),     // fail
    ("claimed", "failed"),        // fail_before_stream
    ("processing", "dead"),       // deadline_expire
    ("failed", "dead"),           // exhaust
];

#[test]
fn monotonicity_no_response() {
    for (pre, post) in LIFECYCLE_TRANSITIONS {
        let pre_state = derive_attempt(&attempt(pre, None));
        let post_state = derive_attempt(&attempt(post, None));
        assert!(
            post_state.rank() >= pre_state.rank(),
            "rank decreased: {pre} ({}) → {post} ({})",
            pre_state.rank(),
            post_state.rank()
        );
    }
}

#[test]
fn monotonicity_with_streaming_response() {
    for (pre, post) in LIFECYCLE_TRANSITIONS {
        let r = resp(ResponseStatus::Streaming);
        let pre_state = derive_attempt(&AttemptView {
            request: req(pre),
            response: r.clone(),
        });
        let post_state = derive_attempt(&AttemptView {
            request: req(post),
            response: r,
        });
        assert!(
            post_state.rank() >= pre_state.rank(),
            "rank decreased with streaming resp: {pre} ({}) → {post} ({})",
            pre_state.rank(),
            post_state.rank()
        );
    }
}

#[test]
fn monotonicity_response_none_to_streaming() {
    for lifecycle in &["pending", "claimed", "processing", "inputRequired"] {
        let pre = derive_attempt(&attempt(lifecycle, None));
        let post = derive_attempt(&attempt(lifecycle, resp(ResponseStatus::Streaming)));
        assert!(
            post.rank() >= pre.rank(),
            "response none→streaming decreased rank for {lifecycle}"
        );
    }
}

#[test]
fn monotonicity_response_streaming_to_complete() {
    for lifecycle in &["pending", "claimed", "processing", "inputRequired"] {
        let pre = derive_attempt(&attempt(lifecycle, resp(ResponseStatus::Streaming)));
        let post = derive_attempt(&attempt(lifecycle, resp(ResponseStatus::Complete)));
        assert!(
            post.rank() >= pre.rank(),
            "response streaming→complete decreased rank for {lifecycle}"
        );
    }
}

#[test]
fn monotonicity_response_streaming_to_error() {
    for lifecycle in &["pending", "claimed", "processing", "inputRequired"] {
        let pre = derive_attempt(&attempt(lifecycle, resp(ResponseStatus::Streaming)));
        let post = derive_attempt(&attempt(lifecycle, resp(ResponseStatus::Error)));
        assert!(
            post.rank() >= pre.rank(),
            "response streaming→error decreased rank for {lifecycle}"
        );
    }
}

// ── Terminal coherence spot checks (T3) ─────────────────────────

#[test]
fn terminal_coherence_terminal_lifecycle_states() {
    for lifecycle in &["completed", "failed", "dead", "superseded"] {
        let state = derive_attempt(&attempt(lifecycle, None));
        assert!(
            state.is_terminal(),
            "terminal lifecycle {lifecycle} did not produce terminal client state"
        );
    }
}

#[test]
fn terminal_coherence_nonterminal_lifecycle_no_response() {
    for lifecycle in &["pending", "claimed", "processing", "inputRequired"] {
        let state = derive_attempt(&attempt(lifecycle, None));
        assert!(
            !state.is_terminal(),
            "non-terminal lifecycle {lifecycle} with no response produced terminal client state"
        );
    }
}

#[test]
fn terminal_coherence_nonterminal_lifecycle_streaming_response() {
    for lifecycle in &["pending", "claimed", "processing", "inputRequired"] {
        let state = derive_attempt(&attempt(lifecycle, resp(ResponseStatus::Streaming)));
        assert!(
            !state.is_terminal(),
            "non-terminal lifecycle {lifecycle} with streaming response produced terminal client state"
        );
    }
}

#[test]
fn terminal_coherence_nonterminal_lifecycle_complete_response() {
    for lifecycle in &["pending", "claimed", "processing", "inputRequired"] {
        let state = derive_attempt(&attempt(lifecycle, resp(ResponseStatus::Complete)));
        assert!(
            state.is_terminal(),
            "non-terminal {lifecycle} + complete response should be effectively terminal"
        );
    }
}

#[test]
fn terminal_coherence_superseded_flag() {
    let view = AttemptView {
        request: req_superseded("pending"),
        response: None,
    };
    assert!(derive_attempt(&view).is_terminal());
}

// ── Turn replacement (T5) ───────────────────────────────────────

#[test]
fn turn_replacement_retry_restart() {
    let old_tip = attempt("failed", None);
    let new_tip = attempt("pending", None);
    let old_state = derive_attempt(&old_tip);
    let new_state = derive_attempt(&new_tip);

    assert_eq!(old_state, ClientTurnState::Failed);
    assert_eq!(new_state, ClientTurnState::WaitingForClaim);
    // This is the one allowed rank decrease
    assert!(new_state.rank() < old_state.rank());
}

#[test]
fn turn_replacement_supersession_rank() {
    let view = AttemptView {
        request: req_superseded("processing"),
        response: resp(ResponseStatus::Streaming),
    };
    assert_eq!(derive_attempt(&view).rank(), 2);
}
