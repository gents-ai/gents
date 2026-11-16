pub trait RuntimeSnapshotObserver: Send + Sync {
    fn on_generation_published(
        &self,
        generation: u64,
        configuration_fingerprint: &str,
        runnable_agent_dids: &[String],
    );

    /// Success is not a group-recovery/drain guarantee; errors do not authorize
    /// reseeding.
    fn on_event_sources_reconciled(
        &self,
        generation: u64,
        configuration_fingerprint: &str,
        result: Result<(), &str>,
    );

    /// The request router has durably acknowledged this generation; callers
    /// that submit work need the router, not merely a resolved snapshot.
    fn on_router_generation_activated(&self, _generation: u64, _configuration_fingerprint: &str) {}
}
