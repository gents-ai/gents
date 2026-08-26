//! Runtime-owned desired-state write boundary.
//!
//! Loading files, interpolation, and operator-facing diff reports remain CLI
//! concerns. This module owns the reusable control-plane operation: apply a
//! validated set of normalized documents in dependency order inside an
//! existing transaction. There is deliberately no delete or prune operation.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use gents_protocol::graphql::{extract_mutation_doc_id, graphql_input_literal};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::graphql::escape_graphql_string;
use crate::{Collection, DESIRED_STATE_APPLY_ORDER};

use super::{
    common::{sanitize_create_input, sanitize_update_input},
    mint_recreate_identity, write_event_trigger_document, write_schedule_document,
    write_task_document, ConfigApplyTxn,
};

fn normalize_digest_value(value: Value) -> Option<Value> {
    match value {
        Value::Null => None,
        Value::Array(values) => {
            let values = values
                .into_iter()
                .filter_map(|value| match value {
                    Value::String(raw) => serde_json::from_str::<Value>(&raw)
                        .ok()
                        .filter(|parsed| parsed.is_object() || parsed.is_array())
                        .and_then(normalize_digest_value)
                        .or(Some(Value::String(raw))),
                    value => normalize_digest_value(value),
                })
                .collect::<Vec<_>>();
            (!values.is_empty()).then_some(Value::Array(values))
        }
        Value::Object(values) => {
            let values = values
                .into_iter()
                .filter(|(key, _)| key != "updated_at")
                .filter_map(|(key, value)| normalize_digest_value(value).map(|value| (key, value)))
                .collect::<Map<_, _>>();
            Some(Value::Object(values))
        }
        value => Some(value),
    }
}

/// Canonical semantic commitment shared by package planning, retry checks,
/// runtime visibility, and start readiness. DefraDB null/absent and its
/// string-encoded object-list representation normalize to one value.
pub(crate) fn desired_state_document_digest(value: &Value) -> Result<String> {
    let normalized = normalize_digest_value(value.clone()).unwrap_or(Value::Object(Map::new()));
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&normalized)?)
    ))
}

pub(crate) async fn read_desired_state_document_in_txn(
    txn: &ConfigApplyTxn<'_>,
    collection: Collection,
    unique_value: &str,
) -> Result<Option<Value>> {
    let target = desired_state_read_target(collection)?;
    if let Some(target) = target {
        return Ok(super::patch::read_doc_in_txn(txn, target, unique_value)
            .await?
            .map(|(_, document)| Value::Object(document)));
    }

    let collection_name = collection.graphql_type();
    let unique_field = collection.unique_field();
    let response = txn
        .execute(&format!(
            r#"{{
                {collection_name}(
                    filter: {{ {unique_field}: {{ _eq: "{}" }} }}, limit: 2
                ) {{
                    _docID surface_id agent_did display_name enabled entries created_at updated_at
                }}
            }}"#,
            escape_graphql_string(unique_value),
        ))
        .await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get(collection_name))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if rows.len() > 1 {
        anyhow::bail!(
            "multiple live {collection_name} documents share {unique_field}={unique_value}"
        );
    }
    Ok(rows.into_iter().next().map(|mut row| {
        row.as_object_mut()
            .expect("GraphQL document row is an object")
            .remove("_docID");
        row
    }))
}

fn desired_state_read_target(
    collection: Collection,
) -> Result<Option<super::patch::SelfConfigTarget>> {
    let target = match collection {
        Collection::AgentBehavior => Some(super::patch::SelfConfigTarget::AgentBehavior),
        Collection::ToolSelection => Some(super::patch::SelfConfigTarget::ToolSelection),
        Collection::Task => Some(super::patch::SelfConfigTarget::Task),
        Collection::Schedule => Some(super::patch::SelfConfigTarget::Schedule),
        Collection::EventTrigger => Some(super::patch::SelfConfigTarget::EventTrigger),
        Collection::DatastoreToolSurface => None,
        _ => {
            return Err(anyhow::anyhow!(
                "{} is not a package-owned desired-state resource",
                collection.graphql_type()
            ))
        }
    };
    Ok(target)
}

/// Existing revision-owned documents are immutable. Missing rows are
/// resumable; a present row with different semantic content is drift and is
/// never overwritten by package retry.
pub(crate) async fn verify_existing_desired_state_plan(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
) -> Result<()> {
    for document in plan.documents() {
        let id = unique_value(document)?;
        let Some(live) = read_desired_state_document_in_txn(txn, document.collection, id).await?
        else {
            continue;
        };
        let expected = desired_state_document_digest(&document.add)?;
        let observed = desired_state_document_digest(&live)?;
        if observed != expected {
            anyhow::bail!(
                "immutable package resource {} {id:?} drifted: expected {expected}, observed {observed}",
                document.collection.graphql_type()
            );
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub struct DesiredStateApplyDocument {
    pub collection: Collection,
    /// Normalized create shape. It must contain the collection's unique key.
    pub add: Value,
    /// Normalized update shape. Omitted fields preserve existing values and
    /// explicit null clears nullable values.
    pub update: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DesiredStateApplyPlan {
    documents: Vec<DesiredStateApplyDocument>,
}

impl DesiredStateApplyPlan {
    pub fn new(mut documents: Vec<DesiredStateApplyDocument>) -> Result<Self> {
        let mut identities = BTreeSet::new();
        for document in &documents {
            if document.collection == Collection::PeerPairingDesired {
                anyhow::bail!(
                    "PeerPairingDesired uses manifest ownership semantics and is not supported by the non-pruning runtime apply API"
                );
            }
            let collection = document.collection.graphql_type();
            let unique_field = document.collection.unique_field();
            let unique_value = unique_value(document)?;
            if !identities.insert((document.collection, unique_value.to_owned())) {
                anyhow::bail!(
                    "desired-state apply plan contains duplicate {collection} {unique_field}={unique_value}"
                );
            }
            if !document.add.is_object() || !document.update.is_object() {
                anyhow::bail!("desired-state {collection} add/update documents must be objects");
            }
        }
        documents.sort_by(|left, right| {
            let left_order = apply_order_index(left.collection);
            let right_order = apply_order_index(right.collection);
            (left_order, unique_value(left).unwrap_or_default())
                .cmp(&(right_order, unique_value(right).unwrap_or_default()))
        });
        Ok(Self { documents })
    }

    pub fn documents(&self) -> &[DesiredStateApplyDocument] {
        &self.documents
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct DesiredStateApplyCounts {
    counts: BTreeMap<String, usize>,
}

impl DesiredStateApplyCounts {
    pub fn get(&self, collection: Collection) -> usize {
        self.counts
            .get(collection.graphql_type())
            .copied()
            .unwrap_or_default()
    }

    fn increment(&mut self, collection: Collection) {
        *self
            .counts
            .entry(collection.graphql_type().to_owned())
            .or_default() += 1;
    }
}

fn apply_order_index(collection: Collection) -> usize {
    DESIRED_STATE_APPLY_ORDER
        .iter()
        .position(|candidate| *candidate == collection)
        .unwrap_or(usize::MAX)
}

fn unique_value(document: &DesiredStateApplyDocument) -> Result<&str> {
    let unique_field = document.collection.unique_field();
    document
        .add
        .get(unique_field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .with_context(|| {
            format!(
                "desired-state {} document is missing {}",
                document.collection.graphql_type(),
                unique_field
            )
        })
}

/// Apply all documents in the canonical dependency order inside `txn`.
///
/// The caller owns schema readiness and transaction commit/discard. Keeping
/// those boundaries outside this function lets package installation combine
/// schema checks, config writes, and graph publication without a second
/// execution engine.
pub async fn apply_desired_state_plan(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
) -> Result<DesiredStateApplyCounts> {
    let mut counts = DesiredStateApplyCounts::default();
    for document in plan.documents() {
        apply_document(txn, document).await.with_context(|| {
            format!(
                "apply desired-state {} {}",
                document.collection.graphql_type(),
                unique_value(document).unwrap_or("<missing>")
            )
        })?;
        counts.increment(document.collection);
    }
    Ok(counts)
}

async fn apply_document(
    txn: &ConfigApplyTxn<'_>,
    document: &DesiredStateApplyDocument,
) -> Result<String> {
    let unique_value = unique_value(document)?;
    match document.collection {
        Collection::Task => {
            write_task_document(txn, unique_value, &document.add, &document.update).await
        }
        Collection::Schedule => {
            write_schedule_document(txn, unique_value, &document.add, &document.update).await
        }
        Collection::EventTrigger => {
            write_event_trigger_document(txn, unique_value, &document.add, &document.update).await
        }
        Collection::PeerPairingDesired => unreachable!("rejected by plan validation"),
        collection => apply_generic_document(txn, collection, unique_value, document).await,
    }
}

async fn apply_generic_document(
    txn: &ConfigApplyTxn<'_>,
    collection: Collection,
    unique_value: &str,
    document: &DesiredStateApplyDocument,
) -> Result<String> {
    let collection_name = collection.graphql_type();
    let unique_field = collection.unique_field();
    // The existing desired-state path uses an upsert for generic config
    // collections. Minting recreate identity fields on the add branch keeps
    // tombstone recovery idempotent while the update branch preserves the
    // existing document identity.
    let add = graphql_input_literal(&sanitize_create_input(&mint_recreate_identity(
        &document.add,
    )))?;
    let update = graphql_input_literal(&sanitize_update_input(&document.update))?;
    let mutation = format!(
        "mutation {{ upsert_{collection_name}(filter: {{ {unique_field}: {{ _eq: \"{}\" }} }}, add: {add}, update: {update}) {{ _docID }} }}",
        escape_graphql_string(unique_value),
    );
    let response = txn.execute(&mutation).await?;
    extract_mutation_doc_id(&response, collection_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc(collection: Collection, id: &str) -> DesiredStateApplyDocument {
        let unique = collection.unique_field();
        DesiredStateApplyDocument {
            collection,
            add: json!({ (unique): id }),
            update: json!({}),
        }
    }

    #[test]
    fn plan_is_dependency_ordered_and_rejects_duplicates_and_prune_owned_docs() {
        let plan = DesiredStateApplyPlan::new(vec![
            doc(Collection::EventTrigger, "trigger"),
            doc(Collection::ToolSelection, "tools"),
            doc(Collection::Task, "task"),
        ])
        .unwrap();
        assert_eq!(plan.documents[0].collection, Collection::ToolSelection);
        assert_eq!(plan.documents[1].collection, Collection::Task);
        assert_eq!(plan.documents[2].collection, Collection::EventTrigger);

        assert!(DesiredStateApplyPlan::new(vec![
            doc(Collection::Task, "same"),
            doc(Collection::Task, "same"),
        ])
        .is_err());
        assert!(
            DesiredStateApplyPlan::new(vec![doc(Collection::PeerPairingDesired, "peer")]).is_err()
        );
    }

    #[test]
    fn every_specialized_desired_state_writer_has_a_verification_reader() {
        assert_eq!(
            desired_state_read_target(Collection::Task).unwrap(),
            Some(super::super::patch::SelfConfigTarget::Task)
        );
        assert_eq!(
            desired_state_read_target(Collection::Schedule).unwrap(),
            Some(super::super::patch::SelfConfigTarget::Schedule)
        );
        assert_eq!(
            desired_state_read_target(Collection::EventTrigger).unwrap(),
            Some(super::super::patch::SelfConfigTarget::EventTrigger)
        );
    }
}
