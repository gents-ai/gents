//! Rolling-upgrade / mixed-version fleet policy (Phase D).
//!
//! Until defradb.rs passes unknown document versions through unchanged
//! ([defradb.rs#1231](https://github.com/sourcenetwork/defradb.rs/issues/1231);
//! design §7.2; Go does, Rust pin errors), a node running an *older* binary
//! can hard-fail reads when a newer peer replicates docs stamped past its
//! known chain.
//!
//! Gents policy:
//! 1. Promote older nodes promptly during rolling upgrades.
//! 2. Schema DAG foreign versions are still rejected at `ensure_migrations`
//!    (no silent limping).
//! 3. Document-level unknown-version errors from the database surface as
//!    operator-visible failures — we do not swallow them.

/// Operator-facing guidance string for mixed-binary fleets.
pub const ROLLING_UPGRADE_GUIDANCE: &str = "\
Rolling upgrade policy (lens-first migrations): \
promote every node to the newest binary before relying on cross-version \
document replication. Older Rust DefraDB pins error on documents whose \
stored collection version is unknown to the local history (Go passes them \
through). Schema DAGs with foreign versions are rejected at ensure_migrations \
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
