#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaterializationStats {
    pub collections_attempted: usize,
    pub documents_materialized: usize,
    pub collections_failed: usize,
    pub parked_unique_conflicts: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationReport {
    pub baseline_registered: usize,
    pub baseline_already_present: usize,
    pub steps_applied: usize,
    pub steps_already_current: usize,
    pub edges_repaired: usize,
    pub materialization: MaterializationStats,
    pub warnings: Vec<String>,
}
