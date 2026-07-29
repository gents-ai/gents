//! Outcome of a single `ensure_migrations` pass.

/// Stats from eager materialization.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaterializationStats {
    /// Collections for which materialization was attempted.
    pub collections_attempted: usize,
    /// Documents seen / restamped.
    ///
    /// Until upstream write-back lands this is the count of documents
    /// observed by the read-through scan (not durable restamps).
    pub documents_materialized: usize,
    /// True when the upstream `materialize_collection` API was unavailable.
    pub skipped_upstream_missing: bool,
    /// Collections successfully scanned via GraphQL read-through.
    pub read_through_scans: usize,
}

/// Summary returned by [`crate::ensure_migrations`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationReport {
    /// Baseline collections registered (or already present).
    pub baseline_registered: usize,
    /// Baseline collections already present (no-op add_schema).
    pub baseline_already_present: usize,
    /// Versioned / in-place steps newly applied.
    pub steps_applied: usize,
    /// Steps already at the expected state (verified, not re-applied).
    pub steps_already_current: usize,
    /// Edges re-attached via `set_migration` repair.
    pub edges_repaired: usize,
    /// Materialization stats.
    pub materialization: MaterializationStats,
    /// Non-fatal notes (never state verification failures).
    pub warnings: Vec<String>,
}
