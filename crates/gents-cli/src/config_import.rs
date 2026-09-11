use anyhow::Result;
use gents::config_client::{
    apply_desired_state_plan, read_desired_state_record_in_txn, DesiredStateApplyDocument,
    DesiredStateApplyPlan,
};
use gents::Collection;
use serde_json::Value;

use crate::config_bundle::select_apply_collection_docs;
#[cfg(test)]
use crate::config_writes::ConfigAccess;
use crate::config_writes::ConfigApplyTxn;
use crate::desired_state::{self, DesiredApplyBundle};
use crate::shared::{ConfigApplyCounts, ConfigExportBundle};

pub(crate) async fn apply_delete_collection(
    txn: &ConfigApplyTxn<'_>,
    collection: Collection,
    agent_did: &str,
    ids: &[String],
) -> Result<usize> {
    let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(
        ids.iter()
            .map(|id| (collection, agent_did.to_owned(), id.clone()))
            .collect(),
    )?;
    Ok(apply_selected_plan(txn, &plan).await?.get(collection))
}

pub(crate) fn diff_has_pending_apply(
    counts: &desired_state::DesiredStateDiffCollectionsCounts,
) -> bool {
    counts.has_pending_apply()
}

pub(crate) fn config_apply_counts_changed(counts: &ConfigApplyCounts) -> bool {
    counts.changed()
}

pub(crate) async fn apply_desired_state_changes(
    txn: &ConfigApplyTxn<'_>,
    desired_bundle: &DesiredApplyBundle,
    planned: &desired_state::DesiredStateDiffReport,
) -> Result<ConfigApplyCounts> {
    let bundle = desired_bundle.as_bundle();
    // Validate the complete input's owner, including unselected authoring roots.
    desired_state::manifest_from_export_bundle(bundle)?;
    anyhow::ensure!(
        planned.agent_did == bundle.agent_did,
        "apply plan owner differs from desired configuration"
    );
    anyhow::ensure!(
        planned.live_validation_errors.is_empty(),
        "cannot apply invalid configuration diff: {:?}",
        planned.live_validation_errors
    );
    let mut documents = Vec::new();
    let mut removals = Vec::new();
    for collection in Collection::ALL {
        for document in select_apply_docs_for_collection(bundle, planned, collection)? {
            documents.push(DesiredStateApplyDocument {
                collection,
                add: document.clone(),
                update: document,
            });
        }
        removals.extend(
            planned
                .collections
                .get(collection)
                .delete
                .iter()
                .map(|id| (collection, bundle.agent_did.clone(), id.clone())),
        );
    }
    let plan = DesiredStateApplyPlan::new(documents)?.with_removals(removals)?;
    apply_selected_plan(txn, &plan).await
}

async fn apply_selected_plan(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
) -> Result<ConfigApplyCounts> {
    let mut counts = ConfigApplyCounts::default();
    for (collection, owner, id) in plan.removals() {
        if read_desired_state_record_in_txn(txn, *collection, owner, id)
            .await?
            .is_some()
        {
            counts.add(*collection, 1);
        }
    }
    // One retained-candidate validation and publication for the complete change.
    // Enumeration order is deterministic; it does not establish reference safety.
    let applied = apply_desired_state_plan(txn, plan).await?;
    for collection in Collection::ALL {
        counts.add(collection, applied.get(collection));
    }
    Ok(counts)
}

fn select_apply_docs_for_collection(
    desired_bundle: &ConfigExportBundle,
    planned: &desired_state::DesiredStateDiffReport,
    collection: Collection,
) -> Result<Vec<Value>> {
    let diff = planned.collections.get(collection);
    let docs = desired_bundle.docs_for_collection(collection)?;

    select_apply_collection_docs(
        &docs,
        collection.unique_field(),
        collection.graphql_type(),
        diff,
    )
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod transaction_tests;
