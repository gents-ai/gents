#[async_trait::async_trait]
pub trait RequestLifecycleControl: Send + Sync {
    /// Revalidate between provider turns so a superseded execution stops
    /// before it writes further state.
    async fn validate_owned_execution(&self) -> anyhow::Result<()>;
}
