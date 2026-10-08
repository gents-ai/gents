//! Canonical configuration replacement inside the existing apply transaction.
//! File loading belongs to the common pack loader. This owner validates the
//! complete retained reference set; callers own commit/discard.
use super::{mint_recreate_identity, ConfigApplyTxn};
use crate::defra_query::SchemaField;
use crate::graphql::{escape_graphql_string, graphql_string_list_literal};
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
            let config: T = serde_path_to_error::deserialize(value.clone())
                .context("decode canonical desired configuration (field path)")?;
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

/// One query loads at most this many logical IDs, so a page's query size
/// never scales with the plan being read.
const READ_RECORDS_PAGE_SIZE: usize = 256;

pub async fn read_record(
    txn: &ConfigApplyTxn<'_>,
    collection: Collection,
    owner: &str,
    id: &str,
) -> Result<Option<(String, Value)>> {
    Ok(read_records(txn, collection, owner, &[id])
        .await?
        .remove(id))
}

/// Batched form of [`read_record`]: one query per page of `ids` (bounded by
/// [`READ_RECORDS_PAGE_SIZE`]) rather than one query per document. A missing
/// ID is simply absent from the result map. Two live documents sharing an
/// owner and ID still fail loudly, naming them, exactly as [`read_record`]
/// does for a single ID.
pub async fn read_records(
    txn: &ConfigApplyTxn<'_>,
    collection: Collection,
    owner: &str,
    ids: &[&str],
) -> Result<BTreeMap<String, (String, Value)>> {
    read_records_paged(txn, collection, owner, ids, READ_RECORDS_PAGE_SIZE).await
}

/// Test-only entry point exercising the pager at a page size smaller than
/// [`READ_RECORDS_PAGE_SIZE`], so a multi-page read can be tested without a
/// multi-hundred-document fixture.
#[cfg(test)]
pub(crate) async fn read_records_with_page_size(
    txn: &ConfigApplyTxn<'_>,
    collection: Collection,
    owner: &str,
    ids: &[&str],
    page_size: usize,
) -> Result<BTreeMap<String, (String, Value)>> {
    read_records_paged(txn, collection, owner, ids, page_size).await
}

async fn read_records_paged(
    txn: &ConfigApplyTxn<'_>,
    collection: Collection,
    owner: &str,
    ids: &[&str],
    page_size: usize,
) -> Result<BTreeMap<String, (String, Value)>> {
    let mut unique_ids = BTreeSet::new();
    for id in ids {
        anyhow::ensure!(
            !owner.trim().is_empty() && !id.trim().is_empty(),
            "configuration reference requires owner and ID"
        );
        if collection == Collection::AgentPrincipal {
            anyhow::ensure!(*id == owner, "principal reference must match owner");
        }
        unique_ids.insert(*id);
    }
    let mut out = BTreeMap::new();
    if unique_ids.is_empty() {
        return Ok(out);
    }
    let (fields, _) = config_projection(collection, None)?;
    let name = collection.graphql_type();
    let unique_field = collection.unique_field();
    let ordered: Vec<&str> = unique_ids.into_iter().collect();
    for page in ordered.chunks(page_size.max(1)) {
        let filter = if collection == Collection::AgentPrincipal {
            // The unique field IS `agent_did` here; an `_in` clause on it
            // would duplicate the `_eq` key in the same filter object.
            format!(
                r#"{{ agent_did: {{ _eq: "{}" }} }}"#,
                escape_graphql_string(owner)
            )
        } else {
            format!(
                r#"{{ agent_did: {{ _eq: "{}" }}, {unique_field}: {{ _in: {} }} }}"#,
                escape_graphql_string(owner),
                graphql_string_list_literal(page.iter().copied())
            )
        };
        let response = txn
            .execute(&format!(
                "{{ {name}(filter: {filter}) {{ _docID {} }} }}",
                fields.join(" ")
            ))
            .await?;
        let mut by_id: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for row in graphql_rows_from_response(&response, name) {
            let id = row
                .get(unique_field)
                .and_then(Value::as_str)
                .context("configuration row missing logical ID")?
                .to_owned();
            by_id.entry(id).or_default().push(row);
        }
        for (id, mut rows) in by_id {
            anyhow::ensure!(
                rows.len() <= 1,
                "multiple live {name} documents share owner {owner:?} and ID {id:?}"
            );
            let mut row = rows.pop().context("grouped configuration row is empty")?;
            let doc_id = row
                .as_object_mut()
                .context("configuration row must be an object")?
                .remove("_docID")
                .and_then(|value| value.as_str().map(str::to_owned))
                .context("configuration row missing physical ID")?;
            let (_, value) = config_projection(collection, Some(&row))?;
            out.insert(id, (doc_id, value.context("canonical projection missing")?));
        }
    }
    Ok(out)
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
    let mut introspected = IntrospectedFields::new();
    prefetch_declared_fields(txn, plan, &mut introspected).await?;
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
            let replacement = if let Some(current) = candidate.get(&key) {
                if document.collection == Collection::AgentPrincipal {
                    validate_principal_replacement(current, &document.update)?;
                }
                &document.update
            } else {
                &document.add
            };
            match document.collection {
                Collection::DatastoreToolSurface => {
                    validate_output_obligation_count_fields(txn, replacement, &mut introspected)
                        .await?;
                }
                Collection::EventSource => {
                    validate_event_source_live_fields(txn, replacement, &mut introspected).await?;
                }
                _ => {}
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
        validate_outcome_source_fields(txn, plan, owner, &references, &mut introspected).await?;
        validate_advertised_profiles(txn, plan, owner, &references).await?;
        validate_trigger_document_fields(txn, plan, owner, &references, &mut introspected).await?;
    }
    Ok(())
}

/// A fire renders `{{ doc.X }}` against the delivered source document with
/// strict undefined values, so a field the source collection does not declare
/// fails every fire (#1970). The source collection's schema is observable here;
/// one the schema does not have yet cannot refute the template, matching the
/// other live-field checks. Only triggers whose trigger, task or event source
/// this plan writes are checked, so an unrelated write never fails on retained
/// configuration.
async fn validate_trigger_document_fields(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
    owner: &str,
    references: &crate::ConfigReferences,
    introspected: &mut IntrospectedFields,
) -> Result<()> {
    let mut written = BTreeSet::new();
    for document in plan.documents() {
        let (document_owner, id) = document_identity(document.collection, &document.add)?;
        if document_owner == owner {
            written.insert((document.collection, id.to_owned()));
        }
    }
    if written.is_empty() {
        return Ok(());
    }
    let lookup = |collection: Collection, id: &str| {
        references
            .documents()
            .find(|((kind, key), _)| *kind == collection && key == id)
            .map(|(_, value)| value.clone())
    };
    let triggers = references
        .documents()
        .filter(|((collection, _), _)| *collection == Collection::Trigger)
        .map(|(_, value)| serde_json::from_value::<crate::document_config::Trigger>(value.clone()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for trigger in triggers {
        let crate::document_config::TriggerSource::Event { event_source_id } = &trigger.source
        else {
            continue;
        };
        if !written.contains(&(Collection::Trigger, trigger.trigger_id.clone()))
            && !written.contains(&(Collection::Task, trigger.task_id.clone()))
            && !written.contains(&(Collection::EventSource, event_source_id.clone()))
        {
            continue;
        }
        let (Some(task), Some(source)) = (
            lookup(Collection::Task, &trigger.task_id),
            lookup(Collection::EventSource, event_source_id),
        ) else {
            continue;
        };
        let task: crate::document_config::Task = serde_json::from_value(task)?;
        let source: crate::document_config::EventSource = serde_json::from_value(source)?;
        let Some(fields) = declared_fields(txn, &source.source_collection, introspected).await?
        else {
            continue;
        };
        validate_event_trigger_document_fields(&trigger, &task, &source, fields)?;
    }
    Ok(())
}

/// The templates a fire of `trigger` renders against the delivered document:
/// the task's prompt and goal objective, and the trigger's session template.
fn event_trigger_templates<'a>(
    trigger: &'a crate::document_config::Trigger,
    task: &'a crate::document_config::Task,
) -> [(&'static str, Option<&'a str>); 3] {
    [
        ("prompt_template", Some(task.prompt_template.as_str())),
        (
            "goal_objective_template",
            task.goal_objective_template.as_deref(),
        ),
        (
            "session_id_template",
            trigger.session_id_template.as_deref(),
        ),
    ]
}

/// A fire renders `{{ doc.X }}` against the delivered source document with
/// strict undefined values, so a field the source collection does not declare
/// fails every fire (#1970). `_`-prefixed names are engine metadata, aggregate
/// pseudo-fields resolve through the query engine, and a template that guards
/// an absent value renders it instead of failing. A per-document
/// `emit_outcome` delivery of `CallbackResult` or `WorkspaceReceipt` resolves
/// `handoff_id`, `reply_session_id` and `attempt` even undeclared: the
/// trigger engine injects that native-route provenance into the fire's
/// document (#2341).
pub fn validate_event_trigger_document_fields(
    trigger: &crate::document_config::Trigger,
    task: &crate::document_config::Task,
    source: &crate::document_config::EventSource,
    declared: &BTreeMap<String, SchemaField>,
) -> Result<()> {
    for (field, template) in event_trigger_templates(trigger, task) {
        let Some(template) =
            template.filter(|template| !crate::template::template_guards_undefined(template))
        else {
            continue;
        };
        for reference in crate::template::parse_template_for_validation(template)? {
            let Some(name) = reference
                .path
                .get(1)
                .filter(|_| reference.root() == Some("doc"))
            else {
                continue;
            };
            let own_field = |name: &String| {
                !name.starts_with('_')
                    && !crate::defra_query::schema::is_aggregate_pseudo_field(name)
            };
            let resolved_native_route_field = task.emit_outcome
                && source.group.is_none()
                && matches!(
                    source.source_collection.as_str(),
                    "CallbackResult" | "WorkspaceReceipt"
                )
                && matches!(name.as_str(), "handoff_id" | "reply_session_id" | "attempt");
            if name.starts_with('_')
                || declared.contains_key(name) && own_field(name)
                || resolved_native_route_field
            {
                continue;
            }
            anyhow::bail!(
                "trigger {} {field} references doc.{name}, but {} has no field {name:?}; its fields are {}",
                trigger.trigger_id,
                source.source_collection,
                declared
                    .keys()
                    .filter(|name| own_field(name))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    Ok(())
}

/// The `{{ doc.X }}` field names [`validate_event_trigger_document_fields`]
/// judges for this trigger and task, so a caller that must decide whether the
/// source collection's schema is worth introspecting derives that from the
/// rule instead of re-deriving a template walk.
pub fn event_trigger_document_field_names(
    trigger: &crate::document_config::Trigger,
    task: &crate::document_config::Task,
) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for (_, template) in event_trigger_templates(trigger, task) {
        let Some(template) =
            template.filter(|template| !crate::template::template_guards_undefined(template))
        else {
            continue;
        };
        for reference in crate::template::parse_template_for_validation(template)? {
            if let Some(name) = reference
                .path
                .get(1)
                .filter(|_| reference.root() == Some("doc"))
            {
                names.insert(name.clone());
            }
        }
    }
    Ok(names)
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct OutcomeSourceSchemaError {
    pub collection: String,
    pub missing_handoff: bool,
    message: String,
}

/// A `FireOutcome` is itself a handoff document: its `source_handoff_id` is
/// copied from the delivered document's `handoff_id`, and fire admission
/// refuses an opted-in delivery whose document has none. A Trigger that
/// delivers a collection without a `String` `handoff_id` field to an
/// `emit_outcome` Task could therefore never admit a fire, so publication
/// refuses it. Only deliveries whose Trigger, Task or EventSource this plan
/// writes are checked, and a collection the schema does not have yet cannot
/// refute the document; admission still refuses each document without a
/// `handoff_id` value.
async fn validate_outcome_source_fields(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
    owner: &str,
    references: &crate::ConfigReferences,
    introspected: &mut IntrospectedFields,
) -> Result<()> {
    let mut written = BTreeSet::new();
    for document in plan.documents() {
        let (document_owner, id) = document_identity(document.collection, &document.add)?;
        if document_owner == owner {
            written.insert((document.collection, id.to_owned()));
        }
    }
    for (trigger_id, task_id, event_source_id, collection, per_document) in
        references.outcome_event_deliveries()
    {
        if !written.contains(&(Collection::Trigger, trigger_id.clone()))
            && !written.contains(&(Collection::Task, task_id.clone()))
            && !written.contains(&(Collection::EventSource, event_source_id.clone()))
        {
            continue;
        }
        let Some(fields) = declared_fields(txn, &collection, introspected).await? else {
            continue;
        };
        let declared = fields.get("handoff_id");
        if declared.is_none()
            && per_document
            && matches!(collection.as_str(), "CallbackResult" | "WorkspaceReceipt")
        {
            continue;
        }
        let message = match declared {
            Some(declared) if declared.named_type() == "String" => continue,
            Some(declared) => format!(
                "Trigger {trigger_id} delivers {collection} to Task {task_id}, which sets emit_outcome, but {collection}.handoff_id is {}, not String; a FireOutcome copies the delivered document's handoff_id. Inspect the collection with schema before changing its field type; preserve existing data",
                declared.type_name
            ),
            None => format!(
                "Trigger {trigger_id} delivers {collection} to Task {task_id}, which sets emit_outcome, but {collection} has no handoff_id field; a FireOutcome copies the delivered document's handoff_id, so every fire would be refused. Use the schema tool to add handoff_id: String (collection update), then update the write tool to populate it. Existing rows need meaningful values before delivery. Preserve Tasks, Triggers and sources; deleting them cannot repair the schema"
            ),
        };
        return Err(OutcomeSourceSchemaError {
            collection,
            missing_handoff: declared.is_none(),
            message,
        }
        .into());
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

/// Introspected fields per target collection; `None` for a collection the
/// schema does not have yet.
type IntrospectedFields = BTreeMap<String, Option<BTreeMap<String, SchemaField>>>;

/// Collections one introspection query asks about at most, so a plan's size
/// never grows a single query without bound.
const INTROSPECTION_PAGE: usize = 64;

/// The correlation and expected-count fields `source` asks the schema about;
/// both `None` means its validation needs no schema.
fn event_source_schema_fields(
    source: &crate::document_config::EventSource,
) -> (Option<&str>, Option<&str>) {
    let correlation = source
        .correlation_field
        .as_deref()
        .map(str::trim)
        .filter(|field| !field.is_empty());
    let count_field = source
        .group
        .as_ref()
        .and_then(|group| group.expected_count.as_ref())
        .and_then(|count| match count {
            crate::document_config::EventGroupCount::SourceField { source_field } => {
                Some(source_field.as_str())
            }
            crate::document_config::EventGroupCount::Fixed(_) => None,
        })
        .map(str::trim)
        .filter(|field| !field.is_empty());
    (correlation, count_field)
}

/// Every create tool of the surface `candidate` whose output obligation names
/// an expected-count field: `(tool_name, collection, field)`.
fn obligation_count_fields(candidate: &Value) -> Result<Vec<(String, String, String)>> {
    let entries = crate::document_config::deserialize_optional_surface_tools(
        candidate.get("entries").cloned().unwrap_or(Value::Null),
    )?;
    Ok(entries
        .unwrap_or_default()
        .into_iter()
        .filter_map(|entry| {
            let crate::document_config::SurfaceToolDecl::Create(decl) = entry else {
                return None;
            };
            let field = decl
                .output_obligation
                .as_ref()
                .and_then(|obligation| obligation.expected_count_field.clone())?;
            Some((decl.tool_name.clone(), decl.collection.clone(), field))
        })
        .collect())
}

/// The collections the schema checks of a `collection` document would
/// introspect for `candidate`. A payload that does not decode names none: the
/// check that reads it reports that failure itself.
fn schema_targets(collection: Collection, candidate: &Value) -> Vec<String> {
    match collection {
        Collection::EventSource => {
            serde_json::from_value::<crate::document_config::EventSource>(candidate.clone())
                .ok()
                .filter(|source| event_source_schema_fields(source) != (None, None))
                .map(|source| source.source_collection)
                .into_iter()
                .collect()
        }
        Collection::DatastoreToolSurface => obligation_count_fields(candidate)
            .map(|fields| {
                fields
                    .into_iter()
                    .map(|(_, collection, _)| collection)
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Introspects, a page of collections per query, every collection the schema
/// checks of `plan` may read, so [`declared_fields`] answers each from
/// `introspected`. Both payloads of a document are covered: which one is
/// written depends on the row's presence, decided later in the transaction.
async fn prefetch_declared_fields(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
    introspected: &mut IntrospectedFields,
) -> Result<()> {
    let wanted: Vec<String> = plan
        .documents()
        .iter()
        .flat_map(|document| {
            [&document.add, &document.update]
                .into_iter()
                .flat_map(|payload| schema_targets(document.collection, payload))
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        // A name that is not an identifier is the structural owner's
        // diagnostic; `declared_fields` reports no collection for it.
        .filter(|collection| {
            !introspected.contains_key(collection)
                && crate::defra_query::schema::introspection_query(collection).is_ok()
        })
        .collect();
    for page in wanted.chunks(INTROSPECTION_PAGE) {
        let names: Vec<&str> = page.iter().map(String::as_str).collect();
        let query = crate::defra_query::schema::introspection_query_many(&names)?;
        let response = txn.execute(&query).await?;
        let schemas =
            crate::defra_query::schema::parse_collection_schemas(response.get("data"), names.len());
        for (name, schema) in names.into_iter().zip(schemas) {
            introspected.insert(
                name.to_owned(),
                schema.map(|schema| {
                    schema
                        .fields
                        .into_iter()
                        .map(|field| (field.name.clone(), field))
                        .collect()
                }),
            );
        }
    }
    Ok(())
}

/// The declared fields of one collection, or `None` when introspection cannot
/// see the collection. A malformed collection name is the structural owner's
/// diagnostic, not an introspection failure, so it reports no collection
/// rather than an error.
async fn declared_fields<'a>(
    txn: &ConfigApplyTxn<'_>,
    collection: &str,
    introspected: &'a mut IntrospectedFields,
) -> Result<Option<&'a BTreeMap<String, SchemaField>>> {
    if !introspected.contains_key(collection) {
        if let Ok(query) = crate::defra_query::schema::introspection_query(collection) {
            let response = txn.execute(&query).await?;
            let fields = crate::defra_query::schema::parse_collection_schema(response.get("data"))
                .map(|schema| {
                    schema
                        .fields
                        .into_iter()
                        .map(|field| (field.name.clone(), field))
                        .collect::<BTreeMap<_, _>>()
                });
            introspected.insert(collection.to_owned(), fields);
        }
    }
    Ok(introspected.get(collection).and_then(Option::as_ref))
}

/// Whether introspection sees `collection` in this transaction.
pub(crate) async fn collection_is_installed(
    txn: &ConfigApplyTxn<'_>,
    collection: &str,
) -> Result<bool> {
    Ok(
        declared_fields(txn, collection, &mut IntrospectedFields::new())
            .await?
            .is_some(),
    )
}

/// The runtime reads an obligation's expected count from the durable arguments
/// of each completed write, not from the stored document, so whether a count it
/// can parse could ever reach `expected_count_field` follows from that field's
/// GraphQL type as `defra_query::schema` reports it;
/// [`crate::defra_write::can_hold_canonical_count`] owns that question.
/// Refusing at publication precedes the runtime failure, which differs by
/// refused class: a `Boolean` or scalar-list field still resolves a write-tool
/// argument schema, so the tool registers, a write completes, and the
/// obligation fails only at completion, after the work ran; a relation-typed or
/// absent field resolves none, so `BoundedWriteTool` is not well formed,
/// `ToolSurface::build_tools` refuses to register it, and no write completes at
/// all. A relation list is relation-typed, not a scalar list.
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
    introspected: &mut IntrospectedFields,
) -> Result<()> {
    let surface_id = candidate
        .get("surface_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    for (tool_name, collection, field) in obligation_count_fields(candidate)? {
        let field = field.as_str();
        // Introspection cannot see a collection that does not exist yet,
        // and publishing a surface ahead of its schema is legitimate.
        // Nothing revalidates the obligation when that schema arrives, so a
        // surface published in that order is never checked here. A package
        // that installs the target collection's own schema takes this path
        // in its preflight, because `ensure_package_schemas` runs after it;
        // only the publishing transaction sees the installed schema.
        let Some(fields) = declared_fields(txn, &collection, introspected).await? else {
            continue;
        };
        match fields.get(field) {
            Some(declared)
                if crate::defra_write::can_hold_canonical_count(declared.named_type()) => {}
            Some(declared) => anyhow::bail!(
                "DatastoreToolSurface {surface_id} tool {:?} output_obligation.expected_count_field {field:?} names a {} field of {}, which cannot carry the count; the runtime parses an integer or its canonical decimal spelling out of the call argument",
                tool_name,
                declared.type_name,
                collection,
            ),
            None => anyhow::bail!(
                "DatastoreToolSurface {surface_id} tool {:?} output_obligation.expected_count_field {field:?} does not exist on {}",
                tool_name,
                collection,
            ),
        }
    }
    Ok(())
}

/// The runtime reads a group's expected count out of the stored source
/// documents and the correlation out of each delivered document with
/// `Value::as_str`, so a correlation field whose named type is not `String`
/// can never correlate and a count field that cannot carry a canonical count
/// can never complete a group. The correlation rule stays at `String` even
/// where the reader would also read another string scalar: widening it is a
/// policy change, not a report of what the runtime does. The structural owner
/// (`EventSource::validate`) has no schema access; the target collection's
/// schema is observable here, inside the publishing transaction. A collection
/// the schema does not have yet cannot refute the document, and publishing
/// ahead of a later schema install is legitimate.
///
/// `candidate` is the source exactly as it will be stored: a plan carries a
/// create and a replacement payload that need only agree on identity, and the
/// row's presence in this transaction decides which one is written.
async fn validate_event_source_live_fields(
    txn: &ConfigApplyTxn<'_>,
    candidate: &Value,
    introspected: &mut IntrospectedFields,
) -> Result<()> {
    let source: crate::document_config::EventSource = serde_json::from_value(candidate.clone())
        .context("decoding EventSource for live-field validation")?;
    let (correlation, count_field) = event_source_schema_fields(&source);
    if correlation.is_none() && count_field.is_none() {
        return Ok(());
    }
    let Some(fields) = declared_fields(txn, &source.source_collection, introspected).await? else {
        return Ok(());
    };
    if let Some(field) = correlation {
        match fields.get(field) {
            Some(declared) if declared.named_type() == "String" => {}
            Some(declared) => anyhow::bail!(
                "EventSource {} correlation_field {:?} must be String, found {}",
                source.event_source_id,
                field,
                declared.type_name
            ),
            None => anyhow::bail!(
                "EventSource {} correlation_field {:?} does not exist on {}",
                source.event_source_id,
                field,
                source.source_collection
            ),
        }
    }
    if let Some(field) = count_field {
        match fields.get(field) {
            Some(declared)
                if crate::defra_write::can_hold_canonical_count(declared.named_type()) => {}
            Some(declared) => anyhow::bail!(
                "EventSource {} expected_count_field {:?} names a {} field of {}, which cannot \
                 carry the count; the runtime parses an integer or its canonical decimal spelling \
                 out of the source document",
                source.event_source_id,
                field,
                declared.type_name,
                source.source_collection
            ),
            None => anyhow::bail!(
                "EventSource {} expected_count_field {:?} does not exist on {}",
                source.event_source_id,
                field,
                source.source_collection
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

fn validate_principal_replacement(current: &Value, candidate: &Value) -> Result<()> {
    if let Some(default) = current.get("default_behavior_id").and_then(Value::as_str) {
        anyhow::ensure!(
            candidate
                .get("default_behavior_id")
                .and_then(Value::as_str)
                .is_some(),
            "AgentPrincipal replacement would clear default_behavior_id {default:?}; \
             include the current default or another enabled behavior in the replacement"
        );
    }
    Ok(())
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
    let mut introspected = IntrospectedFields::new();
    prefetch_declared_fields(txn, plan, &mut introspected).await?;
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
        let (mutation, input) = if let Some((doc_id, current)) = existing {
            if document.collection == Collection::AgentPrincipal {
                validate_principal_replacement(&current, &document.update)?;
            }
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
        match document.collection {
            Collection::DatastoreToolSurface => {
                validate_output_obligation_count_fields(txn, &input, &mut introspected).await?;
            }
            Collection::EventSource => {
                validate_event_source_live_fields(txn, &input, &mut introspected).await?;
            }
            _ => {}
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
    for document in plan.documents() {
        let (owner, id) = document_identity(document.collection, &document.add)?;
        match document.collection {
            Collection::EventSource => {
                super::event_source_cursor::seed_referencing_consumers(txn, owner, id).await?;
            }
            Collection::Trigger => {
                let trigger: crate::document_config::Trigger =
                    serde_json::from_value(document.update.clone())?;
                if matches!(
                    trigger.source,
                    crate::document_config::TriggerSource::Event { .. }
                ) {
                    let consumer = gents_protocol::event_delivery::EventConsumer::Trigger {
                        trigger_id: id.to_owned(),
                    };
                    super::event_source_cursor::load_or_seed(txn, owner, &consumer).await?;
                }
            }
            Collection::CallbackBinding => {
                let consumer = gents_protocol::event_delivery::EventConsumer::CallbackBinding {
                    binding_id: id.to_owned(),
                };
                super::event_source_cursor::load_or_seed(txn, owner, &consumer).await?;
            }
            _ => {}
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
    let mut introspected = IntrospectedFields::new();
    for owner in owners {
        let references = crate::ConfigReferences::load_in_txn(txn, owner).await?;
        references.validate()?;
        validate_outcome_source_fields(txn, plan, owner, &references, &mut introspected).await?;
        validate_advertised_profiles(txn, plan, owner, &references).await?;
        validate_trigger_document_fields(txn, plan, owner, &references, &mut introspected).await?;
    }
    Ok(counts)
}

#[cfg(test)]
mod tests;
