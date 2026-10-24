use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config_client::{
    desired_state_document_digest, ConfigApplyTxn, DesiredStateApplyDocument,
    DesiredStateApplyPlan, DesiredStateExpectation,
};
use crate::Collection;

#[derive(Debug, thiserror::Error)]
pub enum TargetError {
    #[error("target {collection} {owner:?}/{id:?} is not in the baseline closure")]
    NotInClosure {
        collection: &'static str,
        owner: String,
        id: String,
    },
    #[error("target document is not an object")]
    NotAnObject,
}

type Result<T, E = TargetError> = std::result::Result<T, E>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetField {
    AgentContextSystemPrompt,
    TaskPromptTemplate,
}

impl TargetField {
    pub fn collection(&self) -> Collection {
        match self {
            Self::AgentContextSystemPrompt => Collection::AgentContext,
            Self::TaskPromptTemplate => Collection::Task,
        }
    }

    pub fn field_name(&self) -> &'static str {
        self.pack_slot().2
    }

    pub fn job_target(&self, id: &str) -> JobTarget {
        match self {
            Self::AgentContextSystemPrompt => JobTarget::Context,
            Self::TaskPromptTemplate => JobTarget::Task(id.to_owned()),
        }
    }

    pub fn pack_slot(&self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::AgentContextSystemPrompt => ("contexts", "context_id", "system_prompt"),
            Self::TaskPromptTemplate => ("tasks", "task_id", "prompt_template"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub field: TargetField,
    pub owner: String,
    pub id: String,
}

impl Target {
    pub fn job_target(&self) -> JobTarget {
        self.field.job_target(&self.id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobTarget {
    Context,
    Task(String),
}

impl JobTarget {
    pub fn field(&self) -> TargetField {
        match self {
            Self::Context => TargetField::AgentContextSystemPrompt,
            Self::Task(_) => TargetField::TaskPromptTemplate,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenDocument {
    pub collection: Collection,
    pub owner: String,
    pub id: String,
    pub digest: String,
}

pub type Closure = Vec<(Collection, Value)>;

pub(crate) fn is_closure_collection(collection: Collection) -> bool {
    collection != Collection::EvalDefinition
}

pub async fn capture_closure(txn: &ConfigApplyTxn<'_>, owner: &str) -> anyhow::Result<Closure> {
    let references = crate::ConfigReferences::load_in_txn(txn, owner).await?;
    let mut closure: Closure = references
        .documents()
        .filter(|((collection, _), _)| is_closure_collection(*collection))
        .map(|((collection, _), value)| (*collection, value.clone()))
        .collect();
    closure.sort_by_key(|(collection, value)| {
        (
            *collection,
            document_id(*collection, value).unwrap_or_default(),
        )
    });
    Ok(closure)
}

fn document_id(collection: Collection, value: &Value) -> Option<String> {
    value
        .get(collection.unique_field())?
        .as_str()
        .map(str::to_owned)
}

fn document_owner(value: &Value) -> Option<String> {
    value.get("node_did")?.as_str().map(str::to_owned)
}

pub fn closure_digests(closure: &Closure) -> anyhow::Result<Vec<FrozenDocument>> {
    closure
        .iter()
        .map(|(collection, value)| {
            Ok(FrozenDocument {
                collection: *collection,
                owner: document_owner(value).context("closure document has no node_did")?,
                id: document_id(*collection, value)
                    .context("closure document has no logical ID")?,
                digest: desired_state_document_digest(value)?,
            })
        })
        .collect()
}

fn target_index(closure: &Closure, target: &Target) -> Result<usize> {
    closure
        .iter()
        .position(|(collection, value)| {
            *collection == target.field.collection()
                && document_id(*collection, value).as_deref() == Some(target.id.as_str())
                && document_owner(value).as_deref() == Some(target.owner.as_str())
        })
        .ok_or_else(|| TargetError::NotInClosure {
            collection: target.field.collection().graphql_type(),
            owner: target.owner.clone(),
            id: target.id.clone(),
        })
}

pub fn current_text(closure: &Closure, target: &Target) -> Result<String> {
    let (_, value) = &closure[target_index(closure, target)?];
    Ok(value
        .get(target.field.field_name())
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}

pub fn apply_text(closure: &Closure, target: &Target, text: &str) -> Result<Closure> {
    let index = target_index(closure, target)?;
    let mut patched = closure.clone();
    patched[index]
        .1
        .as_object_mut()
        .ok_or(TargetError::NotAnObject)?
        .insert(
            target.field.field_name().to_owned(),
            Value::String(text.to_owned()),
        );
    Ok(patched)
}

pub fn target_digest(closure: &Closure, target: &Target) -> anyhow::Result<String> {
    Ok(desired_state_document_digest(
        &closure[target_index(closure, target)?].1,
    )?)
}

pub fn expectations(frozen: &[FrozenDocument]) -> Vec<DesiredStateExpectation> {
    frozen
        .iter()
        .map(|document| DesiredStateExpectation {
            collection: document.collection,
            owner: document.owner.clone(),
            id: document.id.clone(),
            digest: Some(document.digest.clone()),
        })
        .collect()
}

pub fn target_plan(
    closure: &Closure,
    target: &Target,
    frozen: &[FrozenDocument],
) -> anyhow::Result<DesiredStateApplyPlan> {
    let (collection, value) = &closure[target_index(closure, target)?];
    Ok(DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: *collection,
        add: value.clone(),
        update: value.clone(),
    }])?
    .with_expected(expectations(frozen))?)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaselineMismatch {
    pub collection: Collection,
    pub id: String,
    pub field: Option<String>,
}

impl std::fmt::Display for BaselineMismatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = self.collection.graphql_type();
        match &self.field {
            Some(field) => write!(
                formatter,
                "the baseline pack's {name} {:?} differs from the live one in {field}; supply a pack exported from this configuration",
                self.id
            ),
            None => write!(
                formatter,
                "the baseline pack declares {name} {:?}, which the live configuration does not have; supply a pack exported from this configuration",
                self.id
            ),
        }
    }
}

impl std::error::Error for BaselineMismatch {}

fn remapped_by_trial(collection: Collection, field: Option<&str>, pack_document: &Value) -> bool {
    match (collection, field) {
        (Collection::Node, _) => true,
        (_, Some("tags")) => true,
        (Collection::Agent, Some(field)) => {
            field == "inference_profile_id"
                && pack_document
                    .get(field)
                    .and_then(Value::as_str)
                    .is_some_and(|id| id.starts_with(crate::pack::INFERENCE_SLOT_REFERENCE_PREFIX))
        }
        _ => false,
    }
}

fn comparable(
    collection: Collection,
    document: &Value,
    pack_document: &Value,
) -> anyhow::Result<Value> {
    let (_, projected) = crate::config_client::config_projection(collection, Some(document))?;
    let mut projected = projected.context("canonical projection missing")?;
    if let Some(object) = projected.as_object_mut() {
        object.retain(|field, _| !remapped_by_trial(collection, Some(field), pack_document));
    }
    Ok(projected)
}

pub fn baseline_equivalence(pack: &DesiredStateApplyPlan, live: &Closure) -> anyhow::Result<()> {
    for document in pack.documents() {
        let collection = document.collection;
        if !is_closure_collection(collection) || remapped_by_trial(collection, None, &document.add)
        {
            continue;
        }
        let id =
            document_id(collection, &document.add).context("pack document has no logical ID")?;
        let pack_value = comparable(collection, &document.add, &document.add)?;
        let live_value = live
            .iter()
            .find(|(candidate, value)| {
                *candidate == collection && document_id(*candidate, value).as_deref() == Some(&id)
            })
            .map(|(_, value)| comparable(collection, value, &document.add))
            .transpose()?;
        let Some(live_value) = live_value else {
            return Err(BaselineMismatch {
                collection,
                id,
                field: None,
            }
            .into());
        };
        if desired_state_document_digest(&pack_value)?
            == desired_state_document_digest(&live_value)?
        {
            continue;
        }
        let field = first_differing_field(&pack_value, &live_value)
            .unwrap_or_else(|| "its canonical form".to_owned());
        return Err(BaselineMismatch {
            collection,
            id,
            field: Some(field),
        }
        .into());
    }
    Ok(())
}

fn first_differing_field(left: &Value, right: &Value) -> Option<String> {
    fn unset(value: Option<&Value>) -> Option<&Value> {
        value.filter(|value| !value.is_null() && !value.as_array().is_some_and(Vec::is_empty))
    }
    let (left, right) = (left.as_object()?, right.as_object()?);
    let fields: std::collections::BTreeSet<&String> = left.keys().chain(right.keys()).collect();
    fields
        .into_iter()
        .find(|field| unset(left.get(*field)) != unset(right.get(*field)))
        .cloned()
}
