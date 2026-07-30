//! Rolling-upgrade / mixed-version fleet policy (Phase D).
//!
//! The pinned DefraDB passes unknown document versions through unchanged,
//! matching Go query behavior while preserving the foreign version marker.
//!
//! Gents policy:
//! 1. Promote older nodes promptly during rolling upgrades.
//! 2. Schema DAG foreign versions are still rejected at `ensure_migrations`
//!    (no silent limping).
//! 3. Document-level unknown-version errors from legacy database pins remain
//!    operator-visible failures — we do not swallow them.

/// Operator-facing guidance string for mixed-binary fleets.
pub const ROLLING_UPGRADE_GUIDANCE: &str = "\
Rolling upgrade policy (lens-first migrations): \
promote every node to the newest binary before relying on cross-version \
document replication. The pinned Rust DefraDB passes documents whose stored \
collection version is unknown to the local history through without restamping. \
Schema DAGs with foreign versions are rejected at ensure_migrations \
with UnknownLineage/ForeignVersion — export/import is required for \
pre-baseline stores.";

/// Classify a defradb query/runtime error message as an unknown-version read
/// failure (Phase D observability helper).
pub fn is_unknown_version_read_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("no migration path found")
        || lower.contains("unknown collection version")
        || lower.contains("unknown version")
        || (lower.contains("from version") && lower.contains("to version"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_rust_migration_path_error() {
        assert!(is_unknown_version_read_error(
            "no migration path found for document abc from version v1 to v2"
        ));
    }

    #[test]
    fn ignores_unrelated_errors() {
        assert!(!is_unknown_version_read_error("connection reset by peer"));
    }
}
