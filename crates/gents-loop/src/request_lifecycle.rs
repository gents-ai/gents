//! The request-lifecycle seam `StreamProcessor` needs; `gents`'s native
//! `RequestLifecycle` owns the full claim/generation/terminal state machine
//! over DefraDB.

#[async_trait::async_trait]
pub trait RequestLifecycleControl: Send + Sync {
    /// Confirm this execution still owns its claim; called between provider
    /// turns so a superseded execution stops before writing anything else.
    async fn validate_owned_execution(&self) -> anyhow::Result<()>;
}
