//! Eager materialization driver; advances document blobs, stored version keys,
//! and secondary indexes without creating CRDT commits or replication events.

use defra_node::Node as EmbeddedNode;
use tracing::{debug, info, warn};

use crate::error::{Error, Result};
use crate::report::{MaterializationStats, ParkedUniqueConflict, ParkedUniqueIndex};

/// Materialize the named collections using DefraDB's datastore-only primitive.
///
/// Any collection-level failure is a hard error — after lineage verification
/// every managed collection is expected to exist — with one deliberate
/// exception: a unique-index violation raised by a document a P2P merge
/// already parked unindexed (#984). The merge path resolves live unique
/// conflicts deterministically (smallest public docID keeps the index entry;
/// the loser persists unindexed), so the same document re-raising the
/// violation during the boot-time reindex is expected residue, not
/// corruption. The failed reindex transaction rolls back, preserving the
/// merge-time winner; boot skips the collection's eager pass, records the
/// parked state in the report, and continues. A foreign stale doc must never
/// be a poison pill.
pub async fn materialize_collections(
    node: &EmbeddedNode,
    collections: &[&str],
) -> Result<MaterializationStats> {
    let mut stats = MaterializationStats {
        collections_attempted: collections.len(),
        documents_materialized: 0,
        collections_failed: 0,
        parked_unique_conflicts: Vec::new(),
    };

    let mut failures: Vec<String> = Vec::new();

    for name in collections {
        match node.materialize_collection(name).await {
            Ok(n) => {
                stats.documents_materialized += n;
                debug!(collection = %name, documents = n, "materialized collection");
            }
            Err(e) if is_unique_index_violation(&e) => {
                let conflict = parked_unique_conflict(node, name, &e);
                warn!(
                    collection = %name,
                    detail = %conflict.detail,
                    "eager materialization skipped: a merge-parked unique-index \
                     conflict makes a full reindex impossible; the merge-time \
                     deterministic winner keeps the index entry and boot continues"
                );
                stats.parked_unique_conflicts.push(conflict);
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

fn is_unique_index_violation(error: &defra_node::Error) -> bool {
    matches!(error, defra_node::Error::Db(error) if error.is_unique_constraint_violation())
}

fn parked_unique_conflict(
    node: &EmbeddedNode,
    collection: &str,
    error: &defra_node::Error,
) -> ParkedUniqueConflict {
    let unique_indexes = match node.get_collection(collection) {
        Ok(Some(version)) => version
            .indexes
            .into_iter()
            .filter(|index| index.unique)
            .map(|index| ParkedUniqueIndex {
                name: index.name,
                fields: index.fields.into_iter().map(|field| field.name).collect(),
            })
            .collect(),
        Ok(None) => Vec::new(),
        Err(e) => {
            warn!(collection = %collection, error = %e, "parked-doc diagnostics: get_collection failed");
            Vec::new()
        }
    };
    ParkedUniqueConflict {
        collection: collection.to_owned(),
        detail: format!(
            "{collection}: eager materialization skipped ({error}); \
             consistent with a P2P merge having parked a unique-index-conflicting \
             document unindexed (#984)"
        ),
        unique_indexes,
    }
}
