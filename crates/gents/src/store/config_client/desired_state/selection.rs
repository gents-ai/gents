use std::collections::BTreeSet;

use anyhow::{Context, Result};
use serde_json::Value;

use super::read::read_record;
use super::ConfigApplyTxn;
use crate::Collection;

pub(super) struct Change {
    pub(super) collection: Collection,
    pub(super) node_did: String,
    pub(super) doc_id: String,
    pub(super) before: Option<Value>,
    pub(super) after: Value,
}

fn reference_ids(value: Option<&Value>, field: &str) -> BTreeSet<String> {
    value
        .and_then(|value| value[field].as_str())
        .map(str::to_owned)
        .into_iter()
        .collect()
}

fn skill_ids(value: Option<&Value>) -> BTreeSet<String> {
    value
        .and_then(|value| value["skill_ids"].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn tool_reference_ids(value: Option<&Value>, source: Collection) -> BTreeSet<String> {
    let Some(value) = value else {
        return BTreeSet::new();
    };
    let (path, member) = match source {
        Collection::AgentTarget => ("/agents/target_ids", None),
        Collection::ToolServiceRegistry => ("/remote/services", Some("mcp_service_id")),
        Collection::DatastoreToolSurface => ("/datastore/datastore_tool_surface_ids", None),
        _ => return BTreeSet::new(),
    };
    value
        .pointer(path)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| match member {
            Some(field) => item.get(field).and_then(Value::as_str),
            None => item.as_str(),
        })
        .map(str::to_owned)
        .collect()
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
pub(super) async fn apply_selection_changes(
    txn: &ConfigApplyTxn<'_>,
    changes: &[Change],
    bound: fn(Collection) -> bool,
) -> Result<()> {
    for change in changes {
        let references: &[(Collection, &str)] = match change.collection {
            Collection::Agent => &[
                (Collection::AgentContext, "context_id"),
                (Collection::InferenceProfile, "inference_profile_id"),
            ],
            Collection::AgentContext => &[
                (Collection::Skill, "skill_ids"),
                (Collection::Compaction, "compaction_id"),
                (Collection::Tools, "tools_id"),
            ],
            Collection::Compaction => &[(Collection::InferenceProfile, "inference_profile_id")],
            Collection::InferenceProfile => &[
                (Collection::InferenceBackend, "backend_id"),
                (Collection::InferenceSampling, "sampling_id"),
                (Collection::InferenceExecution, "execution_id"),
            ],
            Collection::InferenceExecution => {
                &[(Collection::InferenceRetryPolicy, "retry_policy_id")]
            }
            Collection::Tools => &[
                (Collection::AgentTarget, "agents.target_ids"),
                (Collection::ToolServiceRegistry, "remote.services"),
                (
                    Collection::DatastoreToolSurface,
                    "datastore.datastore_tool_surface_ids",
                ),
            ],
            _ => continue,
        };
        let (target_resource, target_relation) = if change.collection == Collection::Agent {
            ("agents", "executor")
        } else {
            ("configuration", "selected_by")
        };
        for &(source, field) in references {
            if !bound(source)
                || (source == Collection::InferenceBackend
                    && source.graphql_type() != "InferenceBackendMetadata")
                || (change.collection != Collection::Agent && !bound(change.collection))
                || (matches!(source, Collection::AgentContext | Collection::Tools)
                    && !context_hierarchy_enabled(bound))
            {
                continue;
            }
            let ids = |value| {
                if change.collection == Collection::Tools {
                    tool_reference_ids(value, source)
                } else if field == "skill_ids" {
                    skill_ids(value)
                } else {
                    reference_ids(value, field)
                }
            };
            let before = ids(change.before.as_ref());
            let after = ids(Some(&change.after));
            for (ids, add) in [
                (before.difference(&after), false),
                (after.difference(&before), true),
            ] {
                for id in ids {
                    let (source_doc, _) = read_record(txn, source, &change.node_did, id)
                        .await?
                        .context("selected configuration is unavailable")?;
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
mod tests;
#[cfg(test)]
mod compaction_tests;
