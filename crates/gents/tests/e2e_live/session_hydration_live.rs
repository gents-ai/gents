//! Live two-node session hydration against real inference.
//!
//! Gated: `#[ignore]` + `GENTS_LIVE_SESSION_HYDRATION=1`.
//!
//! ```bash
//! GENTS_LIVE_SESSION_HYDRATION=1 \
//!   GENTS_LIVE_SESSION_HYDRATION_ENDPOINT=http://workstation-1:8000/v1 \
//!   GENTS_LIVE_SESSION_HYDRATION_MODEL=d4f \
//!   cargo test -p gents --features live-e2e --test e2e_live \
//!     live_session_hydration_replays_desktop_history_to_a_fresh_client \
//!     -- --ignored --nocapture --test-threads=1
//! ```

use gents::agent::p2p_reconcile::session_hydration::{
    observe_hydration_progress, ClientHydrationPhase, ClientHydrationProgress,
};

fn live_enabled() -> bool {
    std::env::var("GENTS_LIVE_SESSION_HYDRATION").as_deref() == Ok("1")
}

#[ignore = "live: set GENTS_LIVE_SESSION_HYDRATION=1 and pass --ignored"]
#[tokio::test]
async fn live_session_hydration_replays_desktop_history_to_a_fresh_client() {
    if !live_enabled() {
        eprintln!("set GENTS_LIVE_SESSION_HYDRATION=1 and pass --ignored to run live hydration");
        return;
    }

    // Receiver completeness is independent of transport: served_doc_count
    // without local coverage must not complete.
    let serving =
        observe_hydration_progress(&ClientHydrationProgress::default(), 0, Some(4), false);
    assert_eq!(serving.phase, ClientHydrationPhase::Serving);
    let complete = observe_hydration_progress(&serving, 4, Some(4), false);
    assert_eq!(complete.phase, ClientHydrationPhase::Complete);

    panic!(
        "full two-node live pairing body requires P2POperations::push_documents_to_peer on the DefraDB pin"
    );
}
