//! Eager materialization driver.
//!
//! Until defradb.rs exposes `materialize_collection` (design §3 / §7.1), this
//! is a no-op: chain + verification run, and reads pay the lazy lens cost.

use defra_node::EmbeddedNode;

use crate::report::MaterializationStats;
use crate::registry::Registry;

/// Attempt eager materialization for every managed collection.
///
/// Phase A: always reports `skipped_upstream_missing = true`.
pub async fn materialize_all(
    _node: &EmbeddedNode,
    registry: &Registry,
) -> MaterializationStats {
    MaterializationStats {
        collections_attempted: registry.baseline.len(),
        documents_materialized: 0,
        skipped_upstream_missing: true,
    }
}
