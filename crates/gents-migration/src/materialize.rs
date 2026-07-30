//! Eager materialization driver.
//!
//! Uses DefraDB's datastore-only materialization primitive
//! ([defradb.rs#1230](https://github.com/sourcenetwork/defradb.rs/issues/1230),
//! merged in #1232): advances document blobs, stored version keys, and
//! secondary indexes without creating CRDT commits or replication events.

use defra_node::EmbeddedNode;
use tracing::{debug, info};

use crate::error::{Error, Result};
use crate::registry::Registry;
use crate::report::MaterializationStats;

/// Eagerly materialize every managed collection.
///
/// Returns stats on success. Any collection-level failure is a hard error —
/// after lineage verification every managed collection is expected to exist.
pub async fn materialize_all(
    node: &EmbeddedNode,
    registry: &Registry<'_>,
) -> Result<MaterializationStats> {
    let names: Vec<&str> = registry.managed_names().collect();
    materialize_collections(node, &names).await
}

/// Materialize the given collection names via `EmbeddedNode::materialize_collection`.
pub async fn materialize_collections(
    node: &EmbeddedNode,
    collections: &[&str],
) -> Result<MaterializationStats> {
    let mut stats = MaterializationStats {
        collections_attempted: collections.len(),
        documents_materialized: 0,
        collections_failed: 0,
        skipped_upstream_missing: false,
        read_through_scans: 0,
    };

    let mut failures: Vec<String> = Vec::new();

    for name in collections {
        match node.materialize_collection(name).await {
            Ok(n) => {
                stats.documents_materialized += n;
                debug!(collection = %name, documents = n, "materialized collection");
            }
            Err(e) => {
                stats.collections_failed += 1;
                failures.push(format!("{name}: {e}"));
            }
        }
    }

    if !failures.is_empty() {
        return Err(Error::MaterializeFailed {
            detail: failures.join("; "),
        });
    }

    if stats.documents_materialized > 0 {
        info!(
            collections = stats.collections_attempted,
            documents = stats.documents_materialized,
            "eager materialization complete"
        );
    }

    Ok(stats)
}
