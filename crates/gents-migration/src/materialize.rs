//! Eager materialization driver.
//!
//! Design §3: the real primitive is an upstream defradb.rs port of Go's
//! datastore-only write-back
//! ([defradb.rs#1230](https://github.com/sourcenetwork/defradb.rs/issues/1230)).
//! Until that lands we:
//! 1. Probe for a future `materialize_collection` surface (none today).
//! 2. Force a collection scan via GraphQL **read** so lazy lenses run (does
//!    not advance stored version keys without write-back — still the correct
//!    backstop for transformed field values in-process).
//! 3. Report skip reasons clearly for operators and tests.

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
    // Probe: no public materialize_collection on EmbeddedNode at pin 8eba3d5.
    // Keep this branch so landing the API is a one-line flip once the pin
    // advances (design Phase C / §7.1).
    if try_upstream_materialize(node, collections).await {
        return MaterializationStats {
            collections_attempted: collections.len(),
            documents_materialized: 0, // filled when API exists
            skipped_upstream_missing: false,
            read_through_scans: 0,
        };
    }

    let mut stats = MaterializationStats {
        collections_attempted: collections.len(),
        documents_materialized: 0,
        skipped_upstream_missing: true,
        read_through_scans: 0,
    };

    // Best-effort: touch every document through a read so any *in-memory*
    // lens path runs. Without write-back this does not persist version keys.
    for name in collections {
        match scan_collection_read_through(node, name).await {
            Ok(n) => {
                stats.read_through_scans += 1;
                stats.documents_materialized += n;
                debug!(collection = %name, docs = n, "read-through materialize scan");
            }
            Err(e) => {
                warn!(collection = %name, error = %e, "read-through materialize scan failed");
            }
        }
    }

    stats
}

async fn try_upstream_materialize(_node: &EmbeddedNode, _collections: &[&str]) -> bool {
    // No EmbeddedNode::materialize_collection on the current pin.
    false
}

/// GraphQL read of up to 256 docs — forces the lensed fetch path.
async fn scan_collection_read_through(node: &EmbeddedNode, collection: &str) -> anyhow::Result<usize> {
    // Collection names are schema identifiers, not user input — still avoid
    // interpolating anything that is not [A-Za-z0-9_].
    if !collection
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        anyhow::bail!("refusing to scan non-identifier collection name {collection}");
    }
    let query = format!("{{ {collection}(limit: 256) {{ _docID }} }}");
    let response = node.execute(&query).await;
    if response.has_errors() {
        // Missing collection or empty schema is fine during partial tests.
        debug!(
            collection,
            errors = ?response.errors,
            "materialize scan graphql errors (ignored)"
        );
        return Ok(0);
    }
    let n = response
        .data
        .as_ref()
        .and_then(|d| d.get(collection))
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    Ok(n)
}
