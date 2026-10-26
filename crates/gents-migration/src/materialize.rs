//! Eager materialization driver; advances document blobs, stored version keys,
//! and secondary indexes without creating CRDT commits or replication events.

use std::collections::BTreeMap;

use defra_node::EmbeddedNode;
use tracing::{debug, info, warn};

use crate::error::{Error, Result};
use crate::report::MaterializationStats;

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
                let detail = describe_parked_unique_conflict(node, name, &e).await;
                warn!(
                    collection = %name,
                    detail = %detail,
                    "eager materialization skipped: a merge-parked unique-index \
                     conflict makes a full reindex impossible; the merge-time \
                     deterministic winner keeps the index entry and boot continues"
                );
                stats.parked_unique_conflicts.push(detail);
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

/// Bare `[A-Za-z0-9_]` check for GraphQL identifier positions.
///
/// `escape_graphql_string` covers string literals only; collection and field
/// names interpolate as identifiers, so this is the only defense.
fn is_safe_graphql_identifier(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

async fn describe_parked_unique_conflict(
    node: &EmbeddedNode,
    collection: &str,
    error: &defra_node::Error,
) -> String {
    let mut detail = format!(
        "{collection}: eager materialization skipped ({error}); \
         consistent with a P2P merge having parked a unique-index-conflicting \
         document unindexed (#984)"
    );

    if !is_safe_graphql_identifier(collection) {
        warn!(collection = %collection, "parked-doc diagnostics: unsafe collection identifier");
        return detail;
    }

    let unique_indexes = match node.get_collection(collection) {
        Ok(Some(version)) => version
            .indexes
            .into_iter()
            .filter(|index| index.unique)
            .collect::<Vec<_>>(),
        Ok(None) => Vec::new(),
        Err(e) => {
            warn!(collection = %collection, error = %e, "parked-doc diagnostics: get_collection failed");
            Vec::new()
        }
    };

    for index in unique_indexes {
        let fields: Vec<&str> = index.fields.iter().map(|f| f.name.as_str()).collect();
        if fields.is_empty() || !fields.iter().all(|f| is_safe_graphql_identifier(f)) {
            continue;
        }
        let query = format!("{{ {collection} {{ _docID {} }} }}", fields.join(" "));
        let resp = node.execute(&query).await;
        if resp.has_errors() {
            warn!(
                collection = %collection,
                index = %index.name,
                errors = ?resp.errors,
                "parked-doc diagnostics: scan failed"
            );
            continue;
        }
        let rows = resp
            .data
            .as_ref()
            .and_then(|data| data.get(collection))
            .and_then(|rows| rows.as_array())
            .cloned()
            .unwrap_or_default();

        // Group docIDs by the unique value tuple; >1 doc per tuple is the
        // conflict the merge parked. All-null tuples are not unique-constrained.
        let mut by_values: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for row in &rows {
            let values: Vec<serde_json::Value> = fields
                .iter()
                .map(|f| row.get(*f).cloned().unwrap_or(serde_json::Value::Null))
                .collect();
            if values.iter().all(serde_json::Value::is_null) {
                continue;
            }
            let Some(doc_id) = row.get("_docID").and_then(|v| v.as_str()) else {
                continue;
            };
            let key = serde_json::to_string(&values).unwrap_or_default();
            by_values.entry(key).or_default().push(doc_id.to_string());
        }

        for (values, mut doc_ids) in by_values {
            if doc_ids.len() < 2 {
                continue;
            }
            doc_ids.sort();
            let winner = &doc_ids[0];
            let parked = &doc_ids[1..];
            warn!(
                collection = %collection,
                index = %index.name,
                values = %values,
                winner_doc_id = %winner,
                parked_doc_ids = ?parked,
                "unique index conflict parked at boot: existing deterministic \
                 winner keeps the index entry; parked document(s) remain \
                 readable by non-index scans"
            );
            detail.push_str(&format!(
                "; index {} on {} has winner {} and parked [{}]",
                index.name,
                values,
                winner,
                parked.join(", ")
            ));
        }
    }

    detail
}
