use anyhow::{Context, Result};
use serde_json::Value;

use super::{read::read_record, selection::Change, ConfigApplyTxn};
use crate::documents::references_of;
use crate::Collection;

fn selected_agent(node_did: &str, value: Option<&Value>) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    Ok(references_of(Collection::Task, value, node_did)?
        .into_iter()
        .find(|reference| reference.collection == Collection::Agent && reference.field == "agent_did")
        .map(|reference| reference.id))
}

/// The Task owner manages its target Agent userset in the same transaction as
/// the reference. A replay must not restore a deliberately revoked relation.
pub(super) async fn apply_task_selections(
    txn: &ConfigApplyTxn<'_>,
    changes: &[Change],
) -> Result<()> {
    for change in changes
        .iter()
        .filter(|change| change.collection == Collection::Task)
    {
        let old = selected_agent(&change.node_did, change.before.as_ref())?;
        let new = selected_agent(&change.node_did, Some(&change.after))?;
        if old == new {
            continue;
        }
        for (agent, add) in [(old, false), (new, true)] {
            let Some(agent) = agent else { continue };
            let (agent_doc, _) = read_record(txn, Collection::Agent, &change.node_did, &agent)
                .await?
                .context("Task target Agent is unavailable")?;
            if add {
                txn.add_entity_set_relationship(
                    "Task",
                    &change.doc_id,
                    "selected_by",
                    "agents",
                    &agent_doc,
                    "executor",
                )
                .await?;
            } else {
                txn.delete_entity_set_relationship(
                    "Task",
                    &change.doc_id,
                    "selected_by",
                    "agents",
                    &agent_doc,
                    "executor",
                )
                .await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
