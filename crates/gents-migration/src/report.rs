/// Stats from eager materialization.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaterializationStats {
    pub collections_attempted: usize,
    pub documents_materialized: usize,
    pub collections_failed: usize,
    /// Human-readable details for collections whose eager materialization was
    /// skipped because a P2P merge parked a unique-index-conflicting document
    /// unindexed (#984). Boot proceeds; the merge-time deterministic winner
    /// keeps the index entry and parked documents remain readable by
    /// non-index scans.
    pub parked_unique_conflicts: Vec<String>,
}

/// Summary returned by [`crate::ensure_migrations`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationReport {
    pub baseline_registered: usize,
    pub baseline_already_present: usize,
    pub steps_applied: usize,
    pub steps_already_current: usize,
    pub edges_repaired: usize,
    pub materialization: MaterializationStats,
    /// Non-fatal notes (never state verification failures).
    pub warnings: Vec<String>,
}
