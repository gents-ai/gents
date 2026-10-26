use super::{mint_recreate_identity, ConfigApplyTxn, ConfigClientError, TransactionFailure};
use crate::defra_query::SchemaField;
use crate::graphql::{escape_graphql_string, graphql_string_list_literal};
use crate::{Collection, DESIRED_STATE_APPLY_ORDER};
type Result<T, E = DesiredStateError> = std::result::Result<T, E>;
use gents_protocol::graphql::{extract_mutation_doc_id, graphql_rows_from_response};
use serde::{
    de::{DeserializeOwned, Visitor},
    Serialize,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, thiserror::Error)]
pub enum DesiredStateError {
    #[error("canonical configuration root has no struct fields")]
    CanonicalRootFields,
    #[error("decode canonical desired configuration (field path)")]
    DecodeCanonical(#[source] serde_path_to_error::Error<serde_json::Error>),
    #[error("canonical configuration did not serialize as an object")]
    CanonicalNotObject,
    #[error("canonical replacement defaults do not roundtrip")]
    CanonicalRoundtrip(#[source] serde_json::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("desired commitment must be a document object")]
    CommitmentNotObject,
    #[error("configuration reference requires owner and ID")]
    ReferenceRequired,
    #[error("node reference must match owner")]
    NodeReferenceMismatch,
    #[error("desired configuration documents[{index}] {collection} {operation}")]
    DocumentProjection {
        index: usize,
        collection: &'static str,
        operation: &'static str,
        #[source]
        source: Box<DesiredStateError>,
    },
    #[error("missing desired configuration")]
    MissingDesiredConfiguration,
    #[error("configuration requires node_did")]
    NodeDidRequired,
    #[error("configuration requires logical ID")]
    LogicalIdRequired,
    #[error("configuration replacement cannot change owner or logical ID")]
    ReplacementIdentityChanged,
    #[error("duplicate desired configuration {collection} {owner:?}/{id:?}")]
    DuplicateDocument {
        collection: &'static str,
        owner: String,
        id: String,
    },
    #[error("duplicate or replaced removal {collection} {owner:?}/{id:?}")]
    DuplicateRemoval {
        collection: &'static str,
        owner: String,
        id: String,
    },
    #[error("duplicate expectation {collection} {owner:?}/{id:?}")]
    DuplicateExpectation {
        collection: &'static str,
        owner: String,
        id: String,
    },
    #[error("pack requires node")]
    PackNodeRequired,
    #[error(transparent)]
    Stale(#[from] StaleExpectation),
    #[error("Node replacement would clear default_agent_id {current_default:?}; include the current default or another enabled agent in the replacement")]
    NodeDefaultAgentCleared { current_default: String },
    #[error(transparent)]
    Template(#[from] crate::template::TemplateError),
    #[error(transparent)]
    Access(#[from] ConfigClientError),
    #[error(transparent)]
    Graphql(#[from] gents_protocol::graphql::GraphqlError),
    #[error(transparent)]
    DocumentConfig(Box<crate::document_config::DocumentConfigError>),
    #[error(transparent)]
    BackendRegistry(#[from] crate::backend_registry::BackendRegistryError),
    #[error(transparent)]
    EventSourceCursor(Box<super::event_source_cursor::EventSourceCursorError>),
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),
    #[error(transparent)]
    Introspection(#[from] crate::defra_query::DefraQueryFailure),
    #[error(transparent)]
    OutcomeSource(#[from] OutcomeSourceSchemaError),
    #[error("multiple live {name} documents share owner {owner:?} and ID {id:?}")]
    MultipleLiveDocuments {
        name: String,
        owner: String,
        id: String,
    },
    #[error("configuration row missing logical ID")]
    RowLogicalIdMissing,
    #[error("grouped configuration row is empty")]
    RowGroupEmpty,
    #[error("configuration row must be an object")]
    RowNotObject,
    #[error("configuration row missing physical ID")]
    RowPhysicalIdMissing,
    #[error("canonical projection missing")]
    CanonicalProjectionMissing,
    #[error("decoding EventSource for live-field validation")]
    EventSourceDecode(#[source] serde_json::Error),
    #[error(
        "DatastoreToolSurface {} tool {:?} output_obligation.expected_count_field {:?} names a {} field of {}, which cannot carry the count; the runtime parses an integer or its canonical decimal spelling out of the call argument",
        .0.surface_id,
        .0.tool_name,
        .0.field,
        .0.declared_type,
        .0.collection
    )]
    ObligationCountNotCarryable(Box<UncountableObligationField>),
    #[error("DatastoreToolSurface {surface_id} tool {tool_name:?} output_obligation.expected_count_field {field:?} does not exist on {collection}")]
    ObligationCountFieldMissing {
        surface_id: String,
        tool_name: String,
        field: String,
        collection: String,
    },
    #[error("EventSource {event_source_id} correlation_field {field:?} must be String, found {declared_type}")]
    CorrelationFieldNotString {
        event_source_id: String,
        field: String,
        declared_type: String,
    },
    #[error(
        "EventSource {event_source_id} correlation_field {field:?} does not exist on {collection}"
    )]
    CorrelationFieldMissing {
        event_source_id: String,
        field: String,
        collection: String,
    },
    #[error("EventSource {event_source_id} expected_count_field {field:?} names a {declared_type} field of {collection}, which cannot carry the count; the runtime parses an integer or its canonical decimal spelling out of the source document")]
    CountFieldCannotHold {
        event_source_id: String,
        field: String,
        declared_type: String,
        collection: String,
    },
    #[error("EventSource {event_source_id} expected_count_field {field:?} does not exist on {collection}")]
    CountFieldMissing {
        event_source_id: String,
        field: String,
        collection: String,
    },
    #[error("trigger {trigger_id} {field} references doc.{name}, but {source_collection} has no field {name:?}; its fields are {fields}")]
    TriggerFieldUnknown {
        trigger_id: String,
        field: String,
        name: String,
        source_collection: String,
        fields: String,
    },
}

#[derive(Debug)]
pub struct UncountableObligationField {
    surface_id: String,
    tool_name: String,
    field: String,
    declared_type: String,
    collection: String,
}

impl TransactionFailure for DesiredStateError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Access(error) => Some(error),
            Self::DocumentConfig(error) => error.config_client_error(),
            Self::EventSourceCursor(error) => error.config_client_error(),
            Self::BackendRegistry(error) => error.config_client_error(),
            _ => None,
        }
    }
}

impl From<crate::document_config::DocumentConfigError> for DesiredStateError {
    fn from(error: crate::document_config::DocumentConfigError) -> Self {
        Self::DocumentConfig(Box::new(error))
    }
}

impl From<super::event_source_cursor::EventSourceCursorError> for DesiredStateError {
    fn from(error: super::event_source_cursor::EventSourceCursorError) -> Self {
        Self::EventSourceCursor(Box::new(error))
    }
}

#[derive(Default)]
struct RootFields(Option<&'static [&'static str]>);
impl<'de> serde::Deserializer<'de> for &mut RootFields {
    type Error = serde::de::value::Error;
    fn deserialize_any<V>(self, _: V) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        Err(serde::de::Error::custom(
            "configuration must derive struct deserialization",
        ))
    }
    fn deserialize_struct<V>(
        self,
        _: &'static str,
        fields: &'static [&'static str],
        _: V,
    ) -> std::result::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.0 = Some(fields);
        Err(serde::de::Error::custom(
            "configuration field inspection complete",
        ))
    }
    serde::forward_to_deserialize_any! { bool i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 char str string bytes byte_buf option unit unit_struct newtype_struct seq tuple tuple_struct map enum identifier ignored_any }
}

pub(crate) fn canonical_struct_fields<T>() -> Result<&'static [&'static str], DesiredStateError>
where
    T: DeserializeOwned,
{
    let mut fields = RootFields::default();
    let _ = T::deserialize(&mut fields);
    fields
        .0
        .filter(|fields| !fields.is_empty())
        .ok_or(DesiredStateError::CanonicalRootFields)
}

fn project<T>(
    value: Option<&Value>,
) -> Result<(&'static [&'static str], Option<Value>), DesiredStateError>
where
    T: DeserializeOwned + Serialize,
{
    let fields = canonical_struct_fields::<T>()?;
    let normalized = value
        .map(|value| -> Result<Value, DesiredStateError> {
            let config: T = serde_path_to_error::deserialize(value.clone())
                .map_err(DesiredStateError::DecodeCanonical)?;
            let mut config = serde_json::to_value(config)?;
            let object = config
                .as_object_mut()
                .ok_or(DesiredStateError::CanonicalNotObject)?;
            for field in fields {
                object.entry((*field).to_owned()).or_insert_with(|| {
                    if *field == "enabled" {
                        Value::Bool(true)
                    } else {
                        Value::Null
                    }
                });
            }
            object.remove("updated_at");
            let _: T = serde_json::from_value(config.clone())
                .map_err(DesiredStateError::CanonicalRoundtrip)?;
            Ok(config)
        })
        .transpose()?;
    Ok((fields, normalized))
}

pub fn config_projection(
    collection: Collection,
    value: Option<&Value>,
) -> Result<(&'static [&'static str], Option<Value>), DesiredStateError> {
    use crate::document_config::*;
    match collection {
        Collection::Node => project::<Node>(value),
        Collection::Agent => project::<Agent>(value),
        Collection::AgentContext => project::<AgentContext>(value),
        Collection::Compaction => project::<CompactionConfig>(value),
        Collection::Tools => project::<Tools>(value),
        Collection::AgentTarget => project::<AgentTargetDocument>(value),
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

pub fn desired_state_document_digest(value: &Value) -> Result<String, DesiredStateError> {
    let mut value = value.clone();
    let root = value
        .as_object_mut()
        .ok_or(DesiredStateError::CommitmentNotObject)?;
    root.remove("updated_at");
    root.retain(|_, value| !value.is_null() && !value.as_array().is_some_and(Vec::is_empty));
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&value)?)
    ))
}

fn reference_filter(
    collection: Collection,
    owner: &str,
    id: &str,
) -> Result<String, DesiredStateError> {
    if owner.trim().is_empty() || id.trim().is_empty() {
        return Err(DesiredStateError::ReferenceRequired);
    }
    if collection == Collection::Node {
        if id != owner {
            return Err(DesiredStateError::NodeReferenceMismatch);
        }
        Ok(format!(
            r#"{{ node_did: {{ _eq: "{}" }} }}"#,
            escape_graphql_string(owner)
        ))
    } else {
        Ok(format!(
            r#"{{ node_did: {{ _eq: "{}" }}, {}: {{ _eq: "{}" }} }}"#,
            escape_graphql_string(owner),
            collection.unique_field(),
            escape_graphql_string(id)
        ))
    }
}

const READ_RECORDS_PAGE_SIZE: usize = 256;

type DesiredStateRecordKey = (Collection, String, String);
type DesiredStateRecord = (String, Value);

#[cfg(test)]
tokio::task_local! {
    static DESIRED_STATE_READ_PAGE_QUERIES: std::sync::Arc<std::sync::atomic::AtomicUsize>;
}

#[cfg(test)]
fn record_desired_state_read_page_query() {
    let _ = DESIRED_STATE_READ_PAGE_QUERIES
        .try_with(|queries| queries.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
}

#[cfg(test)]
pub(crate) async fn count_desired_state_read_page_queries<F>(future: F) -> (F::Output, usize)
where
    F: std::future::Future,
{
    let queries = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let output = DESIRED_STATE_READ_PAGE_QUERIES
        .scope(queries.clone(), future)
        .await;
    (output, queries.load(std::sync::atomic::Ordering::Relaxed))
}

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

pub async fn read_records(
    txn: &ConfigApplyTxn<'_>,
    collection: Collection,
    owner: &str,
    ids: &[&str],
) -> Result<BTreeMap<String, (String, Value)>> {
    read_records_paged(txn, collection, owner, ids, READ_RECORDS_PAGE_SIZE).await
}

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
        if owner.trim().is_empty() || id.trim().is_empty() {
            return Err(DesiredStateError::ReferenceRequired);
        }
        if collection == Collection::Node && *id != owner {
            return Err(DesiredStateError::NodeReferenceMismatch);
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
        let filter = if collection == Collection::Node {
            format!(
                r#"{{ node_did: {{ _eq: "{}" }} }}"#,
                escape_graphql_string(owner)
            )
        } else {
            format!(
                r#"{{ node_did: {{ _eq: "{}" }}, {unique_field}: {{ _in: {} }} }}"#,
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
        #[cfg(test)]
        record_desired_state_read_page_query();
        let mut by_id: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for row in graphql_rows_from_response(&response, name) {
            let id = row
                .get(unique_field)
                .and_then(Value::as_str)
                .ok_or(DesiredStateError::RowLogicalIdMissing)?
                .to_owned();
            by_id.entry(id).or_default().push(row);
        }
        for (id, mut rows) in by_id {
            if rows.len() > 1 {
                return Err(DesiredStateError::MultipleLiveDocuments {
                    name: name.to_owned(),
                    owner: owner.to_owned(),
                    id,
                });
            }
            let mut row = rows.pop().ok_or(DesiredStateError::RowGroupEmpty)?;
            let doc_id = row
                .as_object_mut()
                .ok_or(DesiredStateError::RowNotObject)?
                .remove("_docID")
                .and_then(|value| value.as_str().map(str::to_owned))
                .ok_or(DesiredStateError::RowPhysicalIdMissing)?;
            let (_, value) = config_projection(collection, Some(&row))?;
            out.insert(
                id,
                (
                    doc_id,
                    value.ok_or(DesiredStateError::CanonicalProjectionMissing)?,
                ),
            );
        }
    }
    Ok(out)
}

async fn read_identity_records(
    txn: &ConfigApplyTxn<'_>,
    identities: &[(Collection, String, String)],
) -> Result<BTreeMap<DesiredStateRecordKey, DesiredStateRecord>> {
    let mut requested: BTreeMap<(Collection, String), BTreeSet<String>> = BTreeMap::new();
    for (collection, owner, id) in identities {
        requested
            .entry((*collection, owner.clone()))
            .or_default()
            .insert(id.clone());
    }

    let mut records = BTreeMap::new();
    for ((collection, owner), ids) in requested {
        let ids = ids.into_iter().collect::<Vec<_>>();
        let id_refs = ids.iter().map(String::as_str).collect::<Vec<_>>();
        for (id, record) in read_records(txn, collection, &owner, &id_refs).await? {
            records.insert((collection, owner.clone(), id), record);
        }
    }
    Ok(records)
}

pub(crate) async fn read_desired_state_document_in_txn(
    txn: &ConfigApplyTxn<'_>,
    collection: Collection,
    node_did: &str,
    unique_value: &str,
) -> Result<Option<Value>> {
    Ok(read_record(txn, collection, node_did, unique_value)
        .await?
        .map(|(_, value)| value))
}

#[cfg(test)]
pub(crate) async fn expect_existing_documents_unchanged(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
) -> Result<()> {
    let identities = plan
        .documents()
        .iter()
        .map(|document| {
            let (owner, id) = document_identity(document.collection, &document.add)?;
            Ok((document.collection, owner.to_owned(), id.to_owned()))
        })
        .collect::<Result<Vec<_>>>()?;
    let records = read_identity_records(txn, &identities).await?;
    let mut expected = Vec::new();
    for document in plan.documents() {
        let (owner, id) = document_identity(document.collection, &document.add)?;
        if records.contains_key(&(document.collection, owner.to_owned(), id.to_owned())) {
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
                if document.collection == Collection::Node {
                    validate_node_replacement(current, &document.update)?;
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

pub fn validate_event_trigger_document_fields(
    trigger: &crate::document_config::Trigger,
    task: &crate::document_config::Task,
    source: &crate::document_config::EventSource,
    declared: &BTreeMap<String, SchemaField>,
) -> Result<(), DesiredStateError> {
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
            return Err(DesiredStateError::TriggerFieldUnknown {
                trigger_id: trigger.trigger_id.to_string(),
                field: field.to_string(),
                name: name.to_string(),
                source_collection: source.source_collection.to_string(),
                fields: declared
                    .keys()
                    .filter(|name| own_field(name))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
            });
        }
    }
    Ok(())
}

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
pub struct OutcomeSourceSchemaError {
    pub collection: String,
    pub missing_handoff: bool,
    message: String,
}

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

type IntrospectedFields = BTreeMap<String, Option<BTreeMap<String, SchemaField>>>;

const INTROSPECTION_PAGE: usize = 64;

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
        let Some(fields) = declared_fields(txn, &collection, introspected).await? else {
            continue;
        };
        match fields.get(field) {
            Some(declared)
                if crate::defra_write::can_hold_canonical_count(declared.named_type()) => {}
            Some(declared) => {
                return Err(DesiredStateError::ObligationCountNotCarryable(Box::new(
                    UncountableObligationField {
                        surface_id: surface_id.to_owned(),
                        tool_name,
                        field: field.to_owned(),
                        declared_type: declared.type_name.to_string(),
                        collection,
                    },
                )))
            }
            None => {
                return Err(DesiredStateError::ObligationCountFieldMissing {
                    surface_id: surface_id.to_owned(),
                    tool_name,
                    field: field.to_owned(),
                    collection,
                })
            }
        }
    }
    Ok(())
}

async fn validate_event_source_live_fields(
    txn: &ConfigApplyTxn<'_>,
    candidate: &Value,
    introspected: &mut IntrospectedFields,
) -> Result<()> {
    let source: crate::document_config::EventSource =
        serde_json::from_value(candidate.clone()).map_err(DesiredStateError::EventSourceDecode)?;
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
            Some(declared) => {
                return Err(DesiredStateError::CorrelationFieldNotString {
                    event_source_id: source.event_source_id.clone(),
                    field: field.to_owned(),
                    declared_type: declared.type_name.to_string(),
                })
            }
            None => {
                return Err(DesiredStateError::CorrelationFieldMissing {
                    event_source_id: source.event_source_id.clone(),
                    field: field.to_owned(),
                    collection: source.source_collection.clone(),
                })
            }
        }
    }
    if let Some(field) = count_field {
        match fields.get(field) {
            Some(declared)
                if crate::defra_write::can_hold_canonical_count(declared.named_type()) => {}
            Some(declared) => {
                return Err(DesiredStateError::CountFieldCannotHold {
                    event_source_id: source.event_source_id.clone(),
                    field: field.to_owned(),
                    declared_type: declared.type_name.to_string(),
                    collection: source.source_collection.clone(),
                })
            }
            None => {
                return Err(DesiredStateError::CountFieldMissing {
                    event_source_id: source.event_source_id.clone(),
                    field: field.to_owned(),
                    collection: source.source_collection.clone(),
                })
            }
        }
    }
    Ok(())
}

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

pub fn stale_expectation<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a StaleExpectation> {
    std::iter::successors(Some(error), |cause| cause.source()).find_map(|cause| {
        cause
            .downcast_ref::<StaleExpectation>()
            .or_else(|| match desired_state_error_in(cause) {
                Some(DesiredStateError::Stale(stale)) => Some(stale),
                _ => None,
            })
            .or_else(|| {
                match cause.downcast_ref::<crate::graph_package::GraphPackageInstallError>() {
                    Some(crate::graph_package::GraphPackageInstallError::Stale(stale)) => {
                        Some(stale)
                    }
                    _ => None,
                }
            })
    })
}

pub fn outcome_source_refusal<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a OutcomeSourceSchemaError> {
    std::iter::successors(Some(error), |cause| cause.source()).find_map(|cause| {
        cause
            .downcast_ref::<OutcomeSourceSchemaError>()
            .or_else(|| match desired_state_error_in(cause) {
                Some(DesiredStateError::OutcomeSource(refusal)) => Some(refusal),
                _ => None,
            })
    })
}

/// Owners that wrap `DesiredStateError` transparently forward `source()` past
/// it, so a chain walk must open those wrappers to see the refusal.
fn desired_state_error_in<'a>(
    cause: &'a (dyn std::error::Error + 'static),
) -> Option<&'a DesiredStateError> {
    use crate::graph_package::GraphPackageInstallError;
    use crate::pack::{InstallationError, ProvenanceError};
    fn provenance(error: &ProvenanceError) -> Option<&DesiredStateError> {
        match error {
            ProvenanceError::DesiredState(error) => Some(error),
            _ => None,
        }
    }
    cause
        .downcast_ref::<DesiredStateError>()
        .or_else(|| cause.downcast_ref::<ProvenanceError>().and_then(provenance))
        .or_else(|| {
            match cause
                .downcast_ref::<InstallationError>()
                .or_else(|| cause.downcast_ref::<Box<InstallationError>>().map(|boxed| &**boxed))
            {
                Some(InstallationError::DesiredState(error)) => Some(error),
                Some(InstallationError::Provenance(error)) => provenance(error),
                _ => None,
            }
        })
        .or_else(|| match cause.downcast_ref::<GraphPackageInstallError>() {
            Some(GraphPackageInstallError::DesiredState(error)) => Some(error),
            _ => None,
        })
}

#[derive(Clone, Debug, PartialEq)]
pub struct DesiredStateApplyPlan {
    documents: Vec<DesiredStateApplyDocument>,
    removals: Vec<(Collection, String, String)>,
    expected: Vec<DesiredStateExpectation>,
}
impl DesiredStateApplyPlan {
    pub fn from_pack_config(
        config: &crate::document_config::PackConfig,
    ) -> Result<Self, DesiredStateError> {
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
                    .get("node")
                    .ok_or(DesiredStateError::PackNodeRequired)?
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

    pub fn new(mut documents: Vec<DesiredStateApplyDocument>) -> Result<Self, DesiredStateError> {
        let mut identities = BTreeSet::new();
        for (index, document) in documents.iter_mut().enumerate() {
            for (operation, value) in [
                ("create", &mut document.add),
                ("replacement", &mut document.update),
            ] {
                *value = config_projection(document.collection, Some(value))
                    .map_err(|source| DesiredStateError::DocumentProjection {
                        index,
                        collection: document.collection.graphql_type(),
                        operation,
                        source: Box::new(source),
                    })?
                    .1
                    .ok_or(DesiredStateError::MissingDesiredConfiguration)?;
            }
            let (owner, id) = document_identity(document.collection, &document.add)?;
            if document_identity(document.collection, &document.update)? != (owner, id) {
                return Err(DesiredStateError::ReplacementIdentityChanged);
            }
            if !identities.insert((document.collection, owner.to_owned(), id.to_owned())) {
                return Err(DesiredStateError::DuplicateDocument {
                    collection: document.collection.graphql_type(),
                    owner: owner.to_owned(),
                    id: id.to_owned(),
                });
            }
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
    pub fn with_removals(
        mut self,
        removals: Vec<(Collection, String, String)>,
    ) -> Result<Self, DesiredStateError> {
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
            if !identities.insert((*collection, owner.clone(), id.clone())) {
                return Err(DesiredStateError::DuplicateRemoval {
                    collection: collection.graphql_type(),
                    owner: owner.clone(),
                    id: id.clone(),
                });
            }
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

    pub fn with_expected(
        mut self,
        expected: Vec<DesiredStateExpectation>,
    ) -> Result<Self, DesiredStateError> {
        let mut identities = BTreeSet::new();
        for expectation in &expected {
            reference_filter(expectation.collection, &expectation.owner, &expectation.id)?;
            if !identities.insert((
                expectation.collection,
                expectation.owner.clone(),
                expectation.id.clone(),
            )) {
                return Err(DesiredStateError::DuplicateExpectation {
                    collection: expectation.collection.graphql_type(),
                    owner: expectation.owner.clone(),
                    id: expectation.id.clone(),
                });
            }
        }
        self.expected = expected;
        Ok(self)
    }

    pub fn expected(&self) -> &[DesiredStateExpectation] {
        &self.expected
    }
}

fn document_identity(
    collection: Collection,
    value: &Value,
) -> Result<(&str, &str), DesiredStateError> {
    let owner = value
        .get("node_did")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .ok_or(DesiredStateError::NodeDidRequired)?;
    let id = value
        .get(collection.unique_field())
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .ok_or(DesiredStateError::LogicalIdRequired)?;
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
    ensure_expectations_match(plan, &records)?;
    Ok(())
}

fn ensure_expectations_match(
    plan: &DesiredStateApplyPlan,
    records: &BTreeMap<DesiredStateRecordKey, DesiredStateRecord>,
) -> Result<(), DesiredStateError> {
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
        Err(DesiredStateError::Stale(StaleExpectation { drifted }))
    }
}

fn validate_node_replacement(current: &Value, candidate: &Value) -> Result<(), DesiredStateError> {
    if let Some(default) = current.get("default_agent_id").and_then(Value::as_str) {
        if candidate
            .get("default_agent_id")
            .and_then(Value::as_str)
            .is_none()
        {
            return Err(DesiredStateError::NodeDefaultAgentCleared {
                current_default: default.to_owned(),
            });
        }
    }
    Ok(())
}

/// All writes remain in the caller's transaction. Reference checks run after
/// staged writes so valid cyclic configurations do not depend on write order.
pub async fn apply_desired_state_plan(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
) -> Result<DesiredStateApplyCounts> {
    ensure_expectations_hold(txn, plan).await?;
    let mut introspected = IntrospectedFields::new();
    prefetch_declared_fields(txn, plan, &mut introspected).await?;
    let document_identities = plan
        .documents()
        .iter()
        .map(|document| {
            let (owner, id) = document_identity(document.collection, &document.add)?;
            Ok((document.collection, owner.to_owned(), id.to_owned()))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut existing_documents = read_identity_records(txn, &document_identities).await?;
    let mut counts = DesiredStateApplyCounts::default();
    for document in plan.documents() {
        let (owner, id) = document_identity(document.collection, &document.add)?;
        let existing =
            existing_documents.remove(&(document.collection, owner.to_owned(), id.to_owned()));
        let changed = existing
            .as_ref()
            .is_none_or(|(_, current)| current != &document.update);
        let name = document.collection.graphql_type();
        let (mutation, input) = if let Some((doc_id, current)) = existing {
            if document.collection == Collection::Node {
                validate_node_replacement(&current, &document.update)?;
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
        if changed {
            let response = txn
                .execute_with_variables(&mutation, &serde_json::json!({"input": input}))
                .await?;
            extract_mutation_doc_id(&response, name)?;
        }
        counts.increment(document.collection);
        if changed && document.collection == Collection::InferenceBackend {
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
    let removal_identities = plan
        .removals()
        .iter()
        .map(|(collection, owner, id)| (*collection, owner.clone(), id.clone()))
        .collect::<Vec<_>>();
    let mut existing_removals = read_identity_records(txn, &removal_identities).await?;
    for (collection, owner, id) in plan.removals() {
        if let Some((doc_id, _)) =
            existing_removals.remove(&(*collection, owner.clone(), id.clone()))
        {
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
        .collect::<Result<_, DesiredStateError>>()?;
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
