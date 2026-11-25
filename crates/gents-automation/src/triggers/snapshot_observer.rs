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

    /// A retired Agent slot's task has joined: the calls it ran have
    /// finished and it holds no dispatcher, worker or backend client.
    fn on_retired_slot_joined(&self, _agent_did: &str, _generation: u64) {}
}
