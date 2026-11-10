use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use serde_json::Value;

use super::read::read_record;
use super::ConfigApplyTxn;
use crate::documents::references_of;
use crate::store::config_client::ConfigClientError;
use crate::Collection;

pub(super) struct Change {
    pub(super) collection: Collection,
    pub(super) node_did: String,
    pub(super) doc_id: String,
    pub(super) before: Option<Value>,
    pub(super) after: Value,
}

/// Selected IDs per target collection, from the canonical reference owner.
fn selected_ids(
    collection: Collection,
    node_did: &str,
    value: Option<&Value>,
) -> Result<BTreeMap<Collection, BTreeSet<String>>> {
    let mut selected: BTreeMap<Collection, BTreeSet<String>> = BTreeMap::new();
    if let Some(value) = value {
        for reference in references_of(collection, value, node_did)? {
            selected
                .entry(reference.collection)
                .or_default()
                .insert(reference.id);
        }
    }
    Ok(selected)
}

/// Documents whose references grant `selected_by` on the selected document.
fn selects_configuration(collection: Collection) -> bool {
    matches!(
        collection,
        Collection::Agent
            | Collection::AgentContext
            | Collection::Compaction
            | Collection::InferenceProfile
            | Collection::InferenceExecution
            | Collection::Tools
    )
}

pub(super) fn context_hierarchy_enabled(bound: fn(Collection) -> bool) -> bool {
    [
        Collection::Skill,
        Collection::Compaction,
        Collection::AgentTarget,
        Collection::ToolServiceRegistry,
        Collection::DatastoreToolSurface,
    ]
    .into_iter()
    .any(bound)
}

/// Only a changed selection may add or remove an edge. Native relation
/// management and all document writes share the caller's transaction; an
/// inaccessible source or rejected edge aborts the complete publication.
/// `repair` is the explicit dangling-selection repair
/// (`Identity.Configuration.repairUsersetSelection`), a destructive admin
/// decision: it may drop a removed source only when
/// `DestructiveAuthority::decide` allows it, which needs a complete inventory
/// no Gents read provides, so it is refused with a typed reason.
pub(super) async fn apply_selection_changes(
    txn: &ConfigApplyTxn<'_>,
    changes: &[Change],
    bound: fn(Collection) -> bool,
    repair: bool,
) -> Result<()> {
    let mut repair_authority = None;
    for change in changes
        .iter()
        .filter(|change| selects_configuration(change.collection))
    {
        let (target_resource, target_relation) = if change.collection == Collection::Agent {
            ("agents", "executor")
        } else {
            ("configuration", "selected_by")
        };
        let before = selected_ids(change.collection, &change.node_did, change.before.as_ref())?;
        let after = selected_ids(change.collection, &change.node_did, Some(&change.after))?;
        let sources = before
            .keys()
            .chain(after.keys())
            .copied()
            .collect::<BTreeSet<_>>();
        let unselected = BTreeSet::new();
        for source in sources {
            if !bound(source)
                || (source == Collection::InferenceBackend
                    && source.graphql_type() != "InferenceBackendMetadata")
                || (change.collection != Collection::Agent && !bound(change.collection))
                || (matches!(source, Collection::AgentContext | Collection::Tools)
                    && !context_hierarchy_enabled(bound))
            {
                continue;
            }
            let before = before.get(&source).unwrap_or(&unselected);
            let after = after.get(&source).unwrap_or(&unselected);
            for (ids, add) in [
                (before.difference(after), false),
                (after.difference(before), true),
            ] {
                for id in ids {
                    let Some((source_doc, _)) =
                        read_record(txn, source, &change.node_did, id).await?
                    else {
                        let selection = format!("{} {id:?}", source.graphql_type());
                        if add || !repair {
                            anyhow::bail!("selected {selection} is unavailable");
                        }
                        let authority = match repair_authority {
                            Some(authority) => authority,
                            None => *repair_authority.insert(txn.destructive_authority().await?),
                        };
                        if let Err(refusal) = authority.decide() {
                            return Err(ConfigClientError::DanglingSelectionRepairRefused {
                                selection,
                                refusal,
                            }
                            .into());
                        }
                        continue;
                    };
                    if add {
                        txn.add_entity_set_relationship(
                            source.graphql_type(),
                            &source_doc,
                            "selected_by",
                            target_resource,
                            &change.doc_id,
                            target_relation,
                        )
                        .await?;
                    } else {
                        txn.delete_entity_set_relationship(
                            source.graphql_type(),
                            &source_doc,
                            "selected_by",
                            target_resource,
                            &change.doc_id,
                            target_relation,
                        )
                        .await?;
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod compaction_tests;
#[cfg(test)]
pub(super) mod tests;
