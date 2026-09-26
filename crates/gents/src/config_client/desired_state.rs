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

pub(crate) fn canonical_struct_fields<T: DeserializeOwned>() -> Result<&'static [&'static str]> {
    let mut fields = RootFields::default();
    let _ = T::deserialize(&mut fields);
    fields
        .0
        .filter(|fields| !fields.is_empty())
        .context("canonical configuration root has no struct fields")
}

fn project<T: DeserializeOwned + Serialize>(
    value: Option<&Value>,
) -> Result<(&'static [&'static str], Option<Value>)> {
    let fields = canonical_struct_fields::<T>()?;
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
        Collection::EvalDefinition => project::<EvalDefinition>(value),
    }
}

/// Commit canonical desired values, never reinterpret strings as legacy JSON.
/// Callers compare normalized plan/read projections, excluding runtime observations.
pub fn desired_state_document_digest(value: &Value) -> Result<String> {
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

/// Expect every document this plan writes that already exists to still match
/// the plan's authored create form. Absent documents carry no expectation.
#[cfg(test)]
pub(crate) async fn expect_existing_documents_unchanged(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
) -> Result<()> {
    let mut expected = Vec::new();
    for document in plan.documents() {
        let (owner, id) = document_identity(document.collection, &document.add)?;
        if read_desired_state_document_in_txn(txn, document.collection, owner, id)
            .await?
            .is_some()
        {
            expected.push(DesiredStateExpectation {
                collection: document.collection,
                owner: owner.to_owned(),
                id: id.to_owned(),
                digest: Some(desired_state_document_digest(&document.add)?),
            });
        }
    }
    let guarded = DesiredStateApplyPlan::new(Vec::new())?.with_expected(expected)?;
    ensure_expectations_hold(txn, &guarded).await
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
    let mut count_fields = CountFieldSchemas::new();
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
            if document.collection == Collection::DatastoreToolSurface {
                validate_output_obligation_count_fields(txn, replacement, &mut count_fields)
                    .await?;
            }
            candidate.insert(key, replacement.clone());
        }
        for (collection, document_owner, id) in plan.removals() {
            if document_owner == owner {
                candidate.remove(&(*collection, id.clone()));
            }
        }
        let references = crate::ConfigReferences::from_documents(
            owner,
            candidate
                .into_iter()
                .map(|((collection, _), value)| (collection, value)),
        )?;
        references.validate()?;
        validate_advertised_profiles(txn, plan, owner, &references).await?;
    }
    Ok(())
}

/// Profiles this plan writes, or whose backend it writes, are admitted against
/// the backend's advertised catalog read in the same transaction, through the
/// owner runtime resolution uses. Preview and publication both run it, so
/// neither accepts what the runtime would reject, such as a context window
/// above the advertised maximum.
async fn validate_advertised_profiles(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
    owner: &str,
    references: &crate::ConfigReferences,
) -> Result<()> {
    let mut profiles = BTreeSet::new();
    for document in plan.documents() {
        let (document_owner, id) = document_identity(document.collection, &document.add)?;
        if document_owner != owner {
            continue;
        }
        match document.collection {
            Collection::InferenceProfile => {
                profiles.insert(id.to_owned());
            }
            Collection::InferenceBackend => {
                profiles.extend(references.profiles_on_backend(id));
            }
            _ => {}
        }
    }
    for profile_id in profiles {
        let Some((profile, backend)) = references.profile_with_backend(&profile_id)? else {
            continue;
        };
        let observation = crate::backend_registry::lookup_backend_observation_in_txn(
            txn,
            owner,
            &backend.backend_id,
        )
        .await?;
        crate::config::advertised_model_for_profile(&backend, &profile, observation.as_ref())?;
    }
    Ok(())
}

/// Introspected field types per target collection; `None` for a collection the
/// schema does not have yet.
type CountFieldSchemas = BTreeMap<String, Option<BTreeMap<String, String>>>;

/// The runtime reads an obligation's expected count from the durable arguments
/// of each completed write, not from the stored document, so whether a count it
/// can parse could ever reach `expected_count_field` follows from that field's
/// GraphQL type as `defra_query::schema` reports it;
/// [`crate::defra_write::can_hold_canonical_count`] owns that question.
/// Refusing at publication precedes the runtime failure, which differs by
/// refused class: a `Boolean` or list field still resolves a write-tool
/// argument schema, so the tool registers, a write completes, and the
/// obligation fails only at completion, after the work ran; a relation-typed or
/// absent field resolves none, so `BoundedWriteTool` is not well formed,
/// `ToolSurface::build_tools` refuses to register it, and no write completes at
/// all.
/// The target collection's schema is observable here, inside the publishing
/// transaction; the structural owner
/// (`WriteToolDecl::output_obligation_is_well_formed`) has no schema access.
///
/// `candidate` is the surface exactly as it will be stored: a plan carries a
/// create and a replacement payload that need only agree on identity, and the
/// row's presence in this transaction decides which one is written.
async fn validate_output_obligation_count_fields(
    txn: &ConfigApplyTxn<'_>,
    candidate: &Value,
    introspected: &mut CountFieldSchemas,
) -> Result<()> {
    let surface_id = candidate
        .get("surface_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let entries = crate::document_config::deserialize_optional_surface_tools(
        candidate.get("entries").cloned().unwrap_or(Value::Null),
    )?;
    for entry in entries.unwrap_or_default() {
        let crate::document_config::SurfaceToolDecl::Create(decl) = entry else {
            continue;
        };
        let Some(field) = decl
            .output_obligation
            .as_ref()
            .and_then(|obligation| obligation.expected_count_field.as_deref())
        else {
            continue;
        };
        // A malformed collection name is the structural owner's diagnostic,
        // not an introspection failure.
        let Ok(query) = crate::defra_query::schema::introspection_query(&decl.collection) else {
            continue;
        };
        if !introspected.contains_key(&decl.collection) {
            let response = txn.execute(&query).await?;
            let fields = crate::defra_query::schema::parse_collection_schema(response.get("data"))
                .map(|schema| {
                    schema
                        .fields
                        .into_iter()
                        .map(|field| (field.name, field.type_name))
                        .collect::<BTreeMap<_, _>>()
                });
            introspected.insert(decl.collection.clone(), fields);
        }
        // Introspection cannot see a collection that does not exist yet,
        // and publishing a surface ahead of its schema is legitimate.
        // Nothing revalidates the obligation when that schema arrives, so a
        // surface published in that order is never checked here. A package
        // that installs the target collection's own schema takes this path
        // in its preflight, because `ensure_package_schemas` runs after it;
        // only the publishing transaction sees the installed schema.
        let Some(fields) = introspected[&decl.collection].as_ref() else {
            continue;
        };
        match fields.get(field).map(String::as_str) {
            Some(reported) if crate::defra_write::can_hold_canonical_count(reported) => {}
            Some(reported) => anyhow::bail!(
                "DatastoreToolSurface {surface_id} tool {:?} output_obligation.expected_count_field {field:?} names a {reported} field of {}, which cannot carry the count; the runtime parses an integer or its canonical decimal spelling out of the call argument",
                decl.tool_name,
                decl.collection,
            ),
            None => anyhow::bail!(
                "DatastoreToolSurface {surface_id} tool {:?} output_obligation.expected_count_field {field:?} does not exist on {}",
                decl.tool_name,
                decl.collection,
            ),
        }
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

/// A digest precondition checked inside the publishing transaction. `digest:
/// None` requires the document to be absent. Digests come from
/// [`desired_state_document_digest`] over the normalized live projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesiredStateExpectation {
    pub collection: Collection,
    pub owner: String,
    pub id: String,
    /// Compute this from a live read of the document's normalized projection
    /// (`read_desired_state_document_in_txn` then
    /// [`desired_state_document_digest`]), never from an authored form. The
    /// two forms diverge wherever the update path strips fields: a
    /// `ChainKeyBinding` replacement always drops the authored `created_at`,
    /// and drops a blank `revoked_at`, keeping the live values instead, so an
    /// authored digest would never match there.
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

/// Refinement of `ApplyReconcile.publishIf`: a stale expectation leaves desired
/// and live state unchanged. Recover it with [`stale_expectation`].
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

    /// Guard this publication with digest preconditions. Expectations may name
    /// documents the plan does not write: promotion freezes a whole closure
    /// and writes only its targets. A second call replaces the previous
    /// expectation set, the same convention [`Self::with_removals`] follows.
    /// An expectation may also name a document this plan removes
    /// (delete-if-unchanged); it is checked exactly like any other, and is
    /// outside the Lean `publishIf` model, which has no removals.
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

async fn ensure_expectations_hold(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
) -> Result<()> {
    let mut drifted = Vec::new();
    for expectation in plan.expected() {
        let found = read_desired_state_document_in_txn(
            txn,
            expectation.collection,
            &expectation.owner,
            &expectation.id,
        )
        .await?
        .map(|live| desired_state_document_digest(&live))
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

/// All writes remain in the caller's transaction. Reference checks run after
/// staged writes so valid cyclic configurations do not depend on write order.
/// Digest expectations are checked first, in the same transaction; a mismatch
/// returns [`StaleExpectation`] before any write.
pub async fn apply_desired_state_plan(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
) -> Result<DesiredStateApplyCounts> {
    ensure_expectations_hold(txn, plan).await?;
    let mut count_fields = CountFieldSchemas::new();
    let mut counts = DesiredStateApplyCounts::default();
    for document in plan.documents() {
        let (owner, id) = document_identity(document.collection, &document.add)?;
        let existing = read_record(txn, document.collection, owner, id).await?;
        // Only configuration changes are diagnostic events. Keeping this at
        // the common desired-state owner covers packs, self-config, and
        // direct writers without making runtime resolution log on every
        // reconcile.
        let changed = existing
            .as_ref()
            .is_none_or(|(_, current)| current != &document.update);
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
        if document.collection == Collection::DatastoreToolSurface {
            validate_output_obligation_count_fields(txn, &input, &mut count_fields).await?;
        }
        let response = txn
            .execute_with_variables(&mutation, &serde_json::json!({"input": input}))
            .await?;
        extract_mutation_doc_id(&response, name)?;
        counts.increment(document.collection);
        if changed && document.collection == Collection::InferenceBackend {
            // DesiredStateApplyPlan already validates this canonical document.
            // A diagnostic must never turn an accepted configuration write into
            // a failed one if that invariant changes in the future.
            if let Ok(backend) =
                serde_json::from_value::<crate::InferenceBackend>(document.update.clone())
            {
                crate::OpenAiWireApi::warn_if_ignored(
                    backend.provider_kind,
                    backend.openai_wire_api,
                    &backend.backend_id,
                );
            }
        }
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
        let references = crate::ConfigReferences::load_in_txn(txn, owner).await?;
        references.validate()?;
        validate_advertised_profiles(txn, plan, owner, &references).await?;
    }
    Ok(counts)
}

#[cfg(test)]
mod tests;
