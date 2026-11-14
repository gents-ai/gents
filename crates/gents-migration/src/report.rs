#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaterializationStats {
    pub collections_attempted: usize,
    pub documents_materialized: usize,
    pub collections_failed: usize,
    pub parked_unique_conflicts: Vec<ParkedUniqueConflict>,
}

/// A collection whose eager pass was skipped over a merge-parked
/// unique-index conflict (#984). The store layer, which owns document reads,
/// names the winning and parked documents from `unique_indexes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParkedUniqueConflict {
    pub collection: String,
    pub detail: String,
    pub unique_indexes: Vec<ParkedUniqueIndex>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParkedUniqueIndex {
    pub name: String,
    pub fields: Vec<String>,
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
