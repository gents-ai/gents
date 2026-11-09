//! Lean `Identity.PackPlacementReaders` and `Identity.DeclaredAccess`: a
//! RepositoryPlacement's `readers` declaration is applied as exactly those
//! `reader` actor tuples, by the actor publishing the placement, in the
//! configuration transaction. Only a changed declaration grants or revokes; a
//! removed placement's tuples are revoked before it is deleted.

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use serde_json::Value;

use super::{plan::document_identity, read::read_record, ConfigApplyTxn, DesiredStateApplyPlan};
use crate::Collection;

const COLLECTION: Collection = Collection::RepositoryPlacement;

#[derive(Default)]
pub(super) struct Capture(Vec<(String, String, BTreeSet<String>)>);

fn declared(value: &Value) -> BTreeSet<String> {
    value["readers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|reader| reader["agent_did"].as_str())
        .map(str::to_owned)
        .collect()
}

async fn change(
    txn: &ConfigApplyTxn<'_>,
    doc: &str,
    readers: impl IntoIterator<Item = &String>,
    add: bool,
) -> Result<()> {
    for reader in readers {
        let reader = identity::Did::new(reader)?;
        if add {
            txn.add_actor_relationship(COLLECTION.graphql_type(), doc, "reader", &reader)
                .await?;
        } else {
            txn.delete_actor_relationship(COLLECTION.graphql_type(), doc, "reader", &reader)
                .await?;
        }
    }
    Ok(())
}

/// Records each published placement's prior declaration and revokes the
/// declaration of each placement the plan removes, while it still exists.
pub(super) async fn capture(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
) -> Result<Capture> {
    let mut before = Vec::new();
    for document in plan
        .documents()
        .iter()
        .filter(|document| document.collection == COLLECTION)
    {
        let (owner, id) = document_identity(document.collection, &document.add)?;
        let prior = read_record(txn, COLLECTION, owner, id)
            .await?
            .map(|(_, value)| declared(&value))
            .unwrap_or_default();
        before.push((owner.to_owned(), id.to_owned(), prior));
    }
    for (collection, owner, id) in plan.removals() {
        if *collection != COLLECTION {
            continue;
        }
        if let Some((doc, value)) = read_record(txn, COLLECTION, owner, id).await? {
            change(txn, &doc, &declared(&value), false).await?;
        }
    }
    Ok(Capture(before))
}

/// Grants added and revokes withdrawn readers of each published placement.
pub(super) async fn apply(txn: &ConfigApplyTxn<'_>, before: Capture) -> Result<()> {
    for (owner, id, prior) in before.0 {
        let (doc, value) = read_record(txn, COLLECTION, &owner, &id)
            .await?
            .with_context(|| format!("published RepositoryPlacement {id} is unavailable"))?;
        let next = declared(&value);
        change(txn, &doc, prior.difference(&next), false).await?;
        change(txn, &doc, next.difference(&prior), true).await?;
    }
    Ok(())
}
