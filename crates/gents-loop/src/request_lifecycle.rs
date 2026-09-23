//! The request-lifecycle seam `StreamProcessor` needs.
//!
//! `gents`'s `RequestLifecycle` (native) owns the full claim/generation/
//! terminal state machine over DefraDB; `StreamProcessor` only ever calls
//! ownership validation on it, so only that operation crosses this seam.

#[async_trait::async_trait]
pub trait RequestLifecycleControl: Send + Sync {
    /// Confirm this execution still owns its claim (lease, generation).
    /// Called between provider turns so a superseded execution stops before
    /// it writes anything else.
    async fn validate_owned_execution(&self) -> anyhow::Result<()>;

}
