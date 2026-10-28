#[async_trait::async_trait]
pub trait RequestLifecycleControl: Send + Sync {
    type Error: Into<Box<dyn std::error::Error + Send + Sync>> + Send + Sync + 'static;

    /// Revalidate between provider turns so a superseded execution stops
    /// before it writes further state.
    async fn validate_owned_execution(&self) -> Result<(), Self::Error>;
}
