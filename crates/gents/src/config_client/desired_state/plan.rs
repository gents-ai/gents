use super::projection::{config_projection, desired_state_document_digest, reference_filter};
use super::read::{read_identity_records, DesiredStateRecord, DesiredStateRecordKey};
use super::ConfigApplyTxn;
use crate::{Collection, DESIRED_STATE_APPLY_ORDER};
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq)]
pub struct DesiredStateApplyDocument {
    pub collection: Collection,
    pub add: Value,
    pub update: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesiredStateExpectation {
    pub collection: Collection,
    pub owner: String,
    pub id: String,
    pub digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriftedDocument {
    pub collection: Collection,
    pub owner: String,
    pub id: String,
    pub expected: Option<String>,
    pub found: Option<String>,
}

#[derive(Debug)]
pub struct StaleExpectation {
    pub drifted: Vec<DriftedDocument>,
}

impl std::fmt::Display for StaleExpectation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "desired configuration changed since it was read:"
        )?;
        for document in &self.drifted {
            write!(
                formatter,
                " {} {:?}/{:?}",
                document.collection.graphql_type(),
                document.owner,
                document.id
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for StaleExpectation {}

pub fn stale_expectation(error: &anyhow::Error) -> Option<&StaleExpectation> {
    error.downcast_ref::<StaleExpectation>()
}

#[derive(Clone, Debug, PartialEq)]
pub struct DesiredStateApplyPlan {
    documents: Vec<DesiredStateApplyDocument>,
    removals: Vec<(Collection, String, String)>,
    expected: Vec<DesiredStateExpectation>,
}
impl DesiredStateApplyPlan {
    pub fn from_pack_config(config: &crate::documents::PackConfig) -> Result<Self> {
        let bundle = serde_json::to_value(config)?;
        let mut documents = Vec::new();
        for collection in Collection::ALL {
            let values: Vec<Value> = match collection.dir_name() {
                Some(field) => bundle
                    .get(field)
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
                None => vec![bundle.get("node").context("pack requires node")?.clone()],
            };
            documents.extend(values.into_iter().map(|value| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            }));
        }
        Self::new(documents)
    }

    pub fn new(mut documents: Vec<DesiredStateApplyDocument>) -> Result<Self> {
        let mut identities = BTreeSet::new();
        for (index, document) in documents.iter_mut().enumerate() {
            for (operation, value) in [
                ("create", &mut document.add),
                ("replacement", &mut document.update),
            ] {
                *value = config_projection(document.collection, Some(value))
                    .with_context(|| {
                        format!(
                            "desired configuration documents[{index}] {} {operation}",
                            document.collection.graphql_type()
                        )
                    })?
                    .1
                    .context("missing desired configuration")?;
            }
            let (owner, id) = document_identity(document.collection, &document.add)?;
            anyhow::ensure!(
                document_identity(document.collection, &document.update)? == (owner, id),
                "configuration replacement cannot change owner or logical ID"
            );
            anyhow::ensure!(
                identities.insert((document.collection, owner.to_owned(), id.to_owned())),
                "duplicate desired configuration {} {owner:?}/{id:?}",
                document.collection.graphql_type()
            );
        }
        documents.sort_by_key(|document| {
            let (owner, id) = document_identity(document.collection, &document.add)
                .expect("validated plan identity");
            (
                DESIRED_STATE_APPLY_ORDER
                    .iter()
                    .position(|c| *c == document.collection),
                owner.to_owned(),
                id.to_owned(),
            )
        });
        Ok(Self {
            documents,
            removals: Vec::new(),
            expected: Vec::new(),
        })
    }
    pub fn with_removals(mut self, removals: Vec<(Collection, String, String)>) -> Result<Self> {
        let mut identities: BTreeSet<_> = self
            .documents
            .iter()
            .map(|document| {
                let (owner, id) = document_identity(document.collection, &document.add)
                    .expect("validated plan identity");
                (document.collection, owner.to_owned(), id.to_owned())
            })
            .collect();
        for (collection, owner, id) in &removals {
            reference_filter(*collection, owner, id)?;
            anyhow::ensure!(
                identities.insert((*collection, owner.clone(), id.clone())),
                "duplicate or replaced removal {} {owner:?}/{id:?}",
                collection.graphql_type()
            );
        }
        self.removals = removals;
        Ok(self)
    }

    pub fn removals(&self) -> &[(Collection, String, String)] {
        &self.removals
    }

    pub fn documents(&self) -> &[DesiredStateApplyDocument] {
        &self.documents
    }

    pub fn with_expected(mut self, expected: Vec<DesiredStateExpectation>) -> Result<Self> {
        let mut identities = BTreeSet::new();
        for expectation in &expected {
            reference_filter(expectation.collection, &expectation.owner, &expectation.id)?;
            anyhow::ensure!(
                identities.insert((
                    expectation.collection,
                    expectation.owner.clone(),
                    expectation.id.clone()
                )),
                "duplicate expectation {} {:?}/{:?}",
                expectation.collection.graphql_type(),
                expectation.owner,
                expectation.id
            );
        }
        self.expected = expected;
        Ok(self)
    }

    pub fn expected(&self) -> &[DesiredStateExpectation] {
        &self.expected
    }
}

pub(super) fn document_identity(collection: Collection, value: &Value) -> Result<(&str, &str)> {
    let owner = value
        .get("node_did")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .context("configuration requires node_did")?;
    let id = value
        .get(collection.unique_field())
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .context("configuration requires logical ID")?;
    Ok((owner, id))
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

    pub(super) fn increment(&mut self, collection: Collection) {
        *self
            .counts
            .entry(collection.graphql_type().to_owned())
            .or_default() += 1;
    }
}

pub(super) async fn ensure_expectations_hold(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
) -> Result<()> {
    let identities = plan
        .expected()
        .iter()
        .map(|expectation| {
            (
                expectation.collection,
                expectation.owner.clone(),
                expectation.id.clone(),
            )
        })
        .collect::<Vec<_>>();
    let records = read_identity_records(txn, &identities).await?;
    ensure_expectations_match(plan, &records)
}

fn ensure_expectations_match(
    plan: &DesiredStateApplyPlan,
    records: &BTreeMap<DesiredStateRecordKey, DesiredStateRecord>,
) -> Result<()> {
    let mut drifted = Vec::new();
    for expectation in plan.expected() {
        let found = records
            .get(&(
                expectation.collection,
                expectation.owner.clone(),
                expectation.id.clone(),
            ))
            .map(|(_, live)| live)
            .map(desired_state_document_digest)
            .transpose()?;
        if found != expectation.digest {
            drifted.push(DriftedDocument {
                collection: expectation.collection,
                owner: expectation.owner.clone(),
                id: expectation.id.clone(),
                expected: expectation.digest.clone(),
                found,
            });
        }
    }
    if drifted.is_empty() {
        Ok(())
    } else {
        Err(anyhow::Error::new(StaleExpectation { drifted }))
    }
}

pub(super) fn validate_node_replacement(current: &Value, candidate: &Value) -> Result<()> {
    if let Some(default) = current.get("default_agent_id").and_then(Value::as_str) {
        anyhow::ensure!(
            candidate
                .get("default_agent_id")
                .and_then(Value::as_str)
                .is_some(),
            "Node replacement would clear default_agent_id {default:?}; \
             include the current default or another enabled agent in the replacement"
        );
    }
    Ok(())
}
