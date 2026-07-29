//! Eager materialization driver.
//!
//! Uses DefraDB's datastore-only materialization primitive, which advances
//! document blobs, stored version keys, and secondary indexes without creating
//! CRDT commits or replication events.

use defra_node::EmbeddedNode;
use tracing::{debug, warn};

use crate::registry::Registry;
use crate::report::MaterializationStats;

/// Attempt eager materialization for every managed collection name.
pub async fn materialize_all(node: &EmbeddedNode, registry: &Registry<'_>) -> MaterializationStats {
    let names: Vec<&str> = registry.managed_names().collect();
    materialize_collections(node, &names).await
}

/// Materialize the given collection names.
pub async fn materialize_collections(
    node: &EmbeddedNode,
    collections: &[&str],
) -> MaterializationStats {
    let mut stats = MaterializationStats {
        collections_attempted: collections.len(),
        documents_materialized: 0,
        skipped_upstream_missing: false,
        read_through_scans: 0,
    };

    for name in collections {
        match node.materialize_collection(name).await {
            Ok(n) => {
                stats.documents_materialized += n;
                debug!(collection = %name, documents = n, "materialized collection");
            }
            Err(e) => {
                warn!(collection = %name, error = %e, "collection materialization failed");
            }
        }
    }

    stats
}
