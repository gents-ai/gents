//! Canonical configuration replacement inside the existing apply transaction.
//! File loading belongs to the common pack loader. This owner validates the
//! complete retained reference set; callers own commit/discard.
use super::{mint_recreate_identity, ConfigApplyTxn};
use crate::graphql::escape_graphql_string;
use crate::{Collection, DESIRED_STATE_APPLY_ORDER};
use anyhow::{Context, Result};
use gents_protocol::graphql::{extract_mutation_doc_id, graphql_rows_from_response};
use serde::{
    de::{DeserializeOwned, Visitor},
    Serialize,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

// Derived serde root metadata is the configuration-field source of truth.
// Stop before visiting values: flattened/map/unsupported roots fail explicitly.
#[derive(Default)]
struct RootFields(Option<&'static [&'static str]>);
impl<'de> serde::Deserializer<'de> for &mut RootFields {
    type Error = serde::de::value::Error;
    fn deserialize_any<V: Visitor<'de>>(self, _: V) -> std::result::Result<V::Value, Self::Error> {
        Err(serde::de::Error::custom(
            "configuration must derive struct deserialization",
        ))
    }
    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        fields: &'static [&'static str],
        _: V,
    ) -> std::result::Result<V::Value, Self::Error> {
        self.0 = Some(fields);
        Err(serde::de::Error::custom(
            "configuration field inspection complete",
        ))
    }
    serde::forward_to_deserialize_any! { bool i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 char str string bytes byte_buf option unit unit_struct newtype_struct seq tuple tuple_struct map enum identifier ignored_any }
}

fn project<T: DeserializeOwned + Serialize>(
    value: Option<&Value>,
) -> Result<(&'static [&'static str], Option<Value>)> {
    let mut fields = RootFields::default();
    let _ = T::deserialize(&mut fields);
    let fields = fields
        .0
        .filter(|fields| !fields.is_empty())
        .context("canonical configuration root has no struct fields")?;
    let normalized = value
        .map(|value| -> Result<Value> {
            let config: T = serde_json::from_value(value.clone())
                .context("decode canonical desired configuration")?;
            let mut config = serde_json::to_value(config)?;
            let object = config
                .as_object_mut()
                .context("canonical configuration did not serialize as an object")?;
            for field in fields {
                object.entry((*field).to_owned()).or_insert_with(|| {
                    if *field == "enabled" {
                        Value::Bool(true)
                    } else {
                        Value::Null
                    }
                });
            }
            // `updated_at` is written by the mutation owner, including the
            // recreate-identity nonce on create. It is observation metadata,
            // not desired configuration, so retaining it would make an
            // immediate read-after-create differ from the authored document.
            object.remove("updated_at");
            // Verify that clearing omitted values preserves the canonical type's defaults.
            let _: T = serde_json::from_value(config.clone())
                .context("canonical replacement defaults do not roundtrip")?;
            Ok(config)
        })
        .transpose()?;
    Ok((fields, normalized))
}

/// Canonical desired-field metadata and optional typed configuration normalization.
/// Runtime observations are excluded. This describes data; it grants no read or
/// write authority and callers must retain the existing scoped transaction owner.
pub fn config_projection(
    collection: Collection,
    value: Option<&Value>,
) -> Result<(&'static [&'static str], Option<Value>)> {
    use crate::document_config::*;
    match collection {
        Collection::AgentPrincipal => project::<AgentPrincipal>(value),
        Collection::AgentBehavior => project::<AgentBehavior>(value),
        Collection::AgentContext => project::<AgentContext>(value),
        Collection::Compaction => project::<CompactionConfig>(value),
        Collection::Tools => project::<Tools>(value),
        Collection::SubagentTarget => project::<SubagentTargetDocument>(value),
        Collection::Skill => project::<SkillDocument>(value),
        Collection::DatastoreToolSurface => project::<DatastoreToolSurfaceDocument>(value),
        Collection::ChainKeyBinding => project::<ChainKeyBindingDocument>(value),
        Collection::EthTool => project::<EthToolDocument>(value),
        Collection::InferenceBackend => project::<InferenceBackend>(value),
        Collection::InferenceProfile => project::<InferenceProfile>(value),
        Collection::InferenceSampling => project::<InferenceSampling>(value),
        Collection::InferenceExecution => project::<InferenceExecution>(value),
        Collection::InferenceRetryPolicy => project::<InferenceRetryPolicy>(value),
        Collection::ToolServiceRegistry => project::<ToolServiceRegistry>(value),
        Collection::ProjectionAcpBinding => project::<ProjectionAcpBinding>(value),
        Collection::Task => project::<Task>(value),
        Collection::Trigger => project::<Trigger>(value),
        Collection::Schedule => project::<Schedule>(value),
        Collection::EventSource => project::<EventSource>(value),
        Collection::Callback => project::<Callback>(value),
        Collection::CallbackBinding => project::<CallbackBinding>(value),
        Collection::CallbackModule => project::<CallbackModule>(value),
        Collection::RepositoryPlacement => project::<RepositoryPlacement>(value),
        Collection::GraphDefinition => project::<GraphDefinition>(value),
    }
}

/// Commit canonical desired values, never reinterpret strings as legacy JSON.
/// Callers compare normalized plan/read projections, excluding runtime observations.
pub(crate) fn desired_state_document_digest(value: &Value) -> Result<String> {
    let mut value = value.clone();
    let root = value
        .as_object_mut()
        .context("desired commitment must be a document object")?;
    root.remove("updated_at");
    // Only root compact config omissions normalize. Nested JSON and strings
    // retain their exact meaning (including null array elements and key names).
    root.retain(|_, value| !value.is_null() && !value.as_array().is_some_and(Vec::is_empty));
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&value)?)
    ))
}

fn reference_filter(collection: Collection, owner: &str, id: &str) -> Result<String> {
    anyhow::ensure!(
        !owner.trim().is_empty() && !id.trim().is_empty(),
        "configuration reference requires owner and ID"
    );
    if collection == Collection::AgentPrincipal {
        anyhow::ensure!(id == owner, "principal reference must match owner");
        Ok(format!(
            r#"{{ agent_did: {{ _eq: "{}" }} }}"#,
            escape_graphql_string(owner)
        ))
    } else {
        Ok(format!(
            r#"{{ agent_did: {{ _eq: "{}" }}, {}: {{ _eq: "{}" }} }}"#,
            escape_graphql_string(owner),
            collection.unique_field(),
            escape_graphql_string(id)
        ))
    }
}

pub async fn read_record(
    txn: &ConfigApplyTxn<'_>,
    collection: Collection,
    owner: &str,
    id: &str,
) -> Result<Option<(String, Value)>> {
    let filter = reference_filter(collection, owner, id)?;
    let (fields, _) = config_projection(collection, None)?;
    let name = collection.graphql_type();
    let response = txn
        .execute(&format!(
            "{{ {name}(filter: {filter}, limit: 2) {{ _docID {} }} }}",
            fields.join(" ")
        ))
        .await?;
    let mut rows = graphql_rows_from_response(&response, name);
    anyhow::ensure!(
        rows.len() <= 1,
        "multiple live {name} documents share owner {owner:?} and ID {id:?}"
    );
    rows.pop()
        .map(|mut row| {
            let doc_id = row
                .as_object_mut()
                .context("configuration row must be an object")?
                .remove("_docID")
                .and_then(|value| value.as_str().map(str::to_owned))
                .context("configuration row missing physical ID")?;
            let (_, value) = config_projection(collection, Some(&row))?;
            Ok((doc_id, value.context("canonical projection missing")?))
        })
        .transpose()
}

pub(crate) async fn read_desired_state_document_in_txn(
    txn: &ConfigApplyTxn<'_>,
    collection: Collection,
    agent_did: &str,
    unique_value: &str,
) -> Result<Option<Value>> {
    Ok(read_record(txn, collection, agent_did, unique_value)
        .await?
        .map(|(_, value)| value))
}

/// Immutable revision checks remain separate from ordinary config replacement.
pub(crate) async fn verify_existing_desired_state_plan(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
) -> Result<()> {
    for document in plan.documents() {
        let (owner, id) = document_identity(document.collection, &document.add)?;
        if let Some(live) =
            read_desired_state_document_in_txn(txn, document.collection, owner, id).await?
        {
            anyhow::ensure!(
                desired_state_document_digest(&document.add)?
                    == desired_state_document_digest(&live)?,
                "immutable package resource {} {owner:?}/{id:?} drifted",
                document.collection.graphql_type()
            );
        }
    }
    Ok(())
}

/// Read-only preview of the same retained configuration checked at publication.
/// Schema installation may follow this check; publication still validates in
/// its own transaction to fence changes made after the preview.
pub(crate) async fn validate_desired_state_plan(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
) -> Result<()> {
    let mut owners = BTreeSet::new();
    for document in plan.documents() {
        owners.insert(document_identity(document.collection, &document.add)?.0);
    }
    owners.extend(plan.removals().iter().map(|(_, owner, _)| owner.as_str()));
    for owner in owners {
        let retained = crate::ConfigReferences::load_in_txn(txn, owner).await?;
        let mut candidate: BTreeMap<_, _> = retained
            .documents()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        for document in plan.documents() {
            let (document_owner, id) = document_identity(document.collection, &document.add)?;
            if document_owner != owner {
                continue;
            }
            let key = (document.collection, id.to_owned());
            let replacement = if candidate.contains_key(&key) {
                &document.update
            } else {
                &document.add
            };
            candidate.insert(key, replacement.clone());
        }
        for (collection, document_owner, id) in plan.removals() {
            if document_owner == owner {
                candidate.remove(&(*collection, id.clone()));
            }
        }
        crate::ConfigReferences::from_documents(
            owner,
            candidate
                .into_iter()
                .map(|((collection, _), value)| (collection, value)),
        )?
        .validate()?;
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub struct DesiredStateApplyDocument {
    pub collection: Collection,
    /// Complete canonical create configuration (compact authoring accepted).
    pub add: Value,
    /// Complete canonical replacement configuration; not a sparse patch.
    /// Its owner and logical ID must match add. Runtime observations are forbidden.
    pub update: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DesiredStateApplyPlan {
    documents: Vec<DesiredStateApplyDocument>,
    removals: Vec<(Collection, String, String)>,
}
impl DesiredStateApplyPlan {
    /// Ordinary and graph packs share the canonical collection mapping and
    /// full-replacement normalization. Topology is compiled separately.
    pub fn from_pack_config(config: &crate::document_config::PackConfig) -> Result<Self> {
        let bundle = serde_json::to_value(config)?;
        let mut documents = Vec::new();
        for collection in Collection::ALL {
            let values: Vec<Value> = match collection.dir_name() {
                Some(field) => bundle
                    .get(field)
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
                None => vec![bundle
                    .get("agent_principal")
                    .context("pack requires agent_principal")?
                    .clone()],
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
        for document in &mut documents {
            document.add = config_projection(document.collection, Some(&document.add))?
                .1
                .context("missing create configuration")?;
            document.update = config_projection(document.collection, Some(&document.update))?
                .1
                .context("missing replacement configuration")?;
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
        })
    }
    /// Remove exact owner/logical identities in the same atomic publication.
    /// Retained inbound references must still resolve after all removals.
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
}

fn document_identity(collection: Collection, value: &Value) -> Result<(&str, &str)> {
    let owner = value
        .get("agent_did")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .context("configuration requires agent_did")?;
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

    fn increment(&mut self, collection: Collection) {
        *self
            .counts
            .entry(collection.graphql_type().to_owned())
            .or_default() += 1;
    }
}

/// All writes remain in the caller's transaction. Reference checks run after
/// staged writes so valid cyclic configurations do not depend on write order.
pub async fn apply_desired_state_plan(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
) -> Result<DesiredStateApplyCounts> {
    let mut counts = DesiredStateApplyCounts::default();
    for document in plan.documents() {
        let (owner, id) = document_identity(document.collection, &document.add)?;
        let existing = read_record(txn, document.collection, owner, id).await?;
        let name = document.collection.graphql_type();
        let (mutation, input) = if let Some((doc_id, _)) = existing {
            let mut update = document.update.clone();
            if document.collection == Collection::ChainKeyBinding {
                crate::document_config::preserve_chain_key_binding_update_fields(&mut update)?;
            }
            (
                format!(
                    r#"mutation($input: {name}MutationInputArg!) {{ update_{name}(docID: "{}", input: $input) {{ _docID }} }}"#,
                    escape_graphql_string(&doc_id)
                ),
                update,
            )
        } else {
            (
                format!(
                    "mutation($input: {name}MutationInputArg!) {{ create_{name}(input: $input) {{ _docID }} }}"
                ),
                mint_recreate_identity(&document.add),
            )
        };
        let response = txn
            .execute_with_variables(&mutation, &serde_json::json!({"input": input}))
            .await?;
        extract_mutation_doc_id(&response, name)?;
        counts.increment(document.collection);
    }
    for (collection, owner, id) in plan.removals() {
        if let Some((doc_id, _)) = read_record(txn, *collection, owner, id).await? {
            let name = collection.graphql_type();
            txn.execute(&format!(
                r#"mutation {{ delete_{name}(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                escape_graphql_string(&doc_id)
            ))
            .await?;
        }
    }
    let mut owners: BTreeSet<_> = plan
        .documents()
        .iter()
        .map(|document| {
            document_identity(document.collection, &document.add).map(|(owner, _)| owner)
        })
        .collect::<Result<_>>()?;
    owners.extend(plan.removals().iter().map(|(_, owner, _)| owner.as_str()));
    for owner in owners {
        crate::ConfigReferences::load_in_txn(txn, owner)
            .await?
            .validate()?;
    }
    Ok(counts)
}

#[cfg(test)]
mod tests;
