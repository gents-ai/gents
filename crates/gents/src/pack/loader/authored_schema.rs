//! JSON Schemas of what [`super::decode_pack_config`] accepts, which is wider
//! than the documents it returns: `bind_owner` fills every omitted owner and
//! `hydrate_eval_cases` reads `./` case sidecars.

use schemars::{schema_for, JsonSchema, Schema};
use serde_json::{json, Map, Value};

use crate::documents::{
    Agent, AgentContext, AgentTargetDocument, Callback, CallbackBinding, CallbackModule,
    CompactionConfig, DatastoreToolSurfaceDocument, EvalDefinition, EventSource, InferenceBackend,
    InferenceExecution, InferenceProfile, InferenceRetryPolicy, InferenceSampling, Node,
    PackConfig, ProjectionAcpBinding, RepositoryPlacement, Schedule, SkillDocument, Task,
    ToolServiceRegistry, Tools, Trigger,
};
use crate::Collection;

use super::{EVAL_CASES_FIELD, OWNER_FIELD};

/// `pack_config.json` as authored.
pub fn authored_pack_config_schema() -> Schema {
    let mut schema = schema_for!(PackConfig);
    let documents: Vec<String> = schema
        .get("properties")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|properties| properties.values())
        .filter_map(definition_name)
        .collect();
    if let Some(definitions) = schema.get_mut("$defs").and_then(Value::as_object_mut) {
        for name in documents {
            if let Some(document) = definitions.get_mut(&name).and_then(Value::as_object_mut) {
                omit_owner(document);
            }
        }
        if let Some(definition) = definitions
            .get_mut(EvalDefinition::schema_name().as_ref())
            .and_then(Value::as_object_mut)
        {
            allow_case_sidecars(definition);
        }
    }
    schema
}

/// One collection's document as authored inside `pack_config.json`.
pub fn authored_document_schema(collection: Collection) -> Schema {
    let mut schema = match collection {
        Collection::Node => schema_for!(Node),
        Collection::Agent => schema_for!(Agent),
        Collection::AgentContext => schema_for!(AgentContext),
        Collection::Compaction => schema_for!(CompactionConfig),
        Collection::Skill => schema_for!(SkillDocument),
        Collection::DatastoreToolSurface => schema_for!(DatastoreToolSurfaceDocument),
        Collection::Tools => schema_for!(Tools),
        Collection::AgentTarget => schema_for!(AgentTargetDocument),
        Collection::InferenceBackend => schema_for!(InferenceBackend),
        Collection::InferenceProfile => schema_for!(InferenceProfile),
        Collection::InferenceSampling => schema_for!(InferenceSampling),
        Collection::InferenceExecution => schema_for!(InferenceExecution),
        Collection::InferenceRetryPolicy => schema_for!(InferenceRetryPolicy),
        Collection::ToolServiceRegistry => schema_for!(ToolServiceRegistry),
        Collection::ProjectionAcpBinding => schema_for!(ProjectionAcpBinding),
        Collection::Task => schema_for!(Task),
        Collection::Schedule => schema_for!(Schedule),
        Collection::EventSource => schema_for!(EventSource),
        Collection::Trigger => schema_for!(Trigger),
        Collection::Callback => schema_for!(Callback),
        Collection::CallbackBinding => schema_for!(CallbackBinding),
        Collection::CallbackModule => schema_for!(CallbackModule),
        Collection::RepositoryPlacement => schema_for!(RepositoryPlacement),
        Collection::EvalDefinition => schema_for!(EvalDefinition),
    };
    let document = schema.ensure_object();
    omit_owner(document);
    if collection == Collection::EvalDefinition {
        allow_case_sidecars(document);
    }
    schema
}

/// The `$defs` entry a pack root property names, directly or through its items.
fn definition_name(property: &Value) -> Option<String> {
    let reference = property.get("$ref").or_else(|| {
        property
            .get("items")
            .and_then(|items| items.get("$ref"))
            .or_else(|| {
                property
                    .get("anyOf")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .find_map(|branch| branch.get("items").and_then(|items| items.get("$ref")))
            })
    })?;
    reference
        .as_str()?
        .strip_prefix("#/$defs/")
        .map(str::to_owned)
}

fn omit_owner(document: &mut Map<String, Value>) {
    if let Some(required) = document.get_mut("required").and_then(Value::as_array_mut) {
        required.retain(|field| field != OWNER_FIELD);
        if required.is_empty() {
            document.remove("required");
        }
    }
}

fn allow_case_sidecars(definition: &mut Map<String, Value>) {
    let Some(cases) = definition
        .get_mut("properties")
        .and_then(|properties| properties.get_mut(EVAL_CASES_FIELD))
    else {
        return;
    };
    if let Some(items) = cases.get_mut("items") {
        let case = items.take();
        *items = json!({
            "anyOf": [
                case,
                { "type": "string", "pattern": "^\\./", "description": "A sidecar file holding one case." }
            ]
        });
    }
}
