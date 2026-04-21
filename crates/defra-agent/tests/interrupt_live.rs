//! Live end-to-end interrupt test. Requires a reachable inference backend.
//!
//! Set `MINIMAX_LIVE=1` and `MINIMAX_API_KEY` to run. See the live-test
//! pattern in `backend_auth_live.rs::live_openrouter_oneshot_succeeds` for
//! backend wiring; this test differs in that it needs the full
//! `BehaviorDaemon` (not just a one-shot inference call) so we can observe
//! the mid-stream interrupt path in a real environment.

/// Mid-stream interrupt against MiniMax:
///   1. Spin up `EmbeddedNode` + `BehaviorDaemon` wired to the MiniMax backend.
///   2. Submit a request whose prompt expects >=100 output tokens.
///   3. Poll `AgentResponse.content` until `len() >= 20` (stream is flowing).
///   4. Write `interrupt_requested_at` on the `AgentRequest`.
///   5. Within 3 seconds, assert `AgentRequest.lifecycle_state == "interrupted"`
///      and `AgentResponse.interrupted_at` is non-null, and observe that no
///      further tokens land after the interrupt fires.
///
/// Left as a `todo!()` stub until the full-daemon test fixture exists; the
/// env-gate shape is shipped so the follow-up test can be plugged in without
/// re-plumbing the skeleton.
#[tokio::test]
#[ignore = "live: requires MINIMAX_LIVE=1 and MINIMAX_API_KEY"]
async fn live_interrupt_mid_stream_on_minimax() {
    if std::env::var("MINIMAX_LIVE").is_err() {
        return;
    }
    if std::env::var("MINIMAX_API_KEY").is_err() {
        eprintln!("MINIMAX_LIVE set but MINIMAX_API_KEY missing; skipping");
        return;
    }
    todo!("needs full BehaviorDaemon fixture + MiniMax credentials");
}
