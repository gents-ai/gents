//! JSON Schemas of what [`super::decode_pack_config`] accepts, which is wider
//! than the documents it returns: `interpolate_values` substitutes `${...}` in
//! every string first, `bind_owner` fills every omitted owner and
//! `hydrate_eval_cases` reads `./` case sidecars.

use schemars::{schema_for, JsonSchema, Schema};
use serde_json::{json, Map, Value};

use crate::documents::widen_schema;
use crate::documents::{
    Agent, AgentContext, AgentTargetDocument, Callback, CallbackBinding, CallbackModule,
    CompactionConfig, DatastoreToolSurfaceDocument, EvalDefinition, EventSource, InferenceBackend,
    InferenceExecution, InferenceProfile, InferenceRetryPolicy, InferenceSampling, Node,
    PackConfig, ProjectionAcpBinding, RepositoryPlacement, Schedule, SkillDocument, Task,
    ToolServiceRegistry, Tools, Trigger,
};
use crate::pack::interpolate::SUBSTITUTION_PATTERN;
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
    allow_substitutions(schema.ensure_object());
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
    allow_substitutions(document);
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

const SCHEMA_MAPS: [&str; 4] = [
    "properties",
    "patternProperties",
    "$defs",
    "dependentSchemas",
];
const SCHEMA_LISTS: [&str; 4] = ["anyOf", "oneOf", "allOf", "prefixItems"];
const SCHEMAS: [&str; 10] = [
    "items",
    "additionalProperties",
    "not",
    "contains",
    "propertyNames",
    "if",
    "then",
    "else",
    "unevaluatedItems",
    "unevaluatedProperties",
];

/// A string the loader substitutes into may be written as any substitution,
/// so every constrained string also accepts one. `oneOf` becomes `anyOf`
/// because an interpolated tag no longer tells the branches apart.
fn allow_substitutions(schema: &mut Map<String, Value>) {
    for key in SCHEMA_MAPS {
        if let Some(children) = schema.get_mut(key).and_then(Value::as_object_mut) {
            children
                .values_mut()
                .filter_map(Value::as_object_mut)
                .for_each(allow_substitutions);
        }
    }
    for key in SCHEMA_LISTS {
        if let Some(children) = schema.get_mut(key).and_then(Value::as_array_mut) {
            children
                .iter_mut()
                .filter_map(Value::as_object_mut)
                .for_each(allow_substitutions);
        }
    }
    for key in SCHEMAS {
        if let Some(child) = schema.get_mut(key).and_then(Value::as_object_mut) {
            allow_substitutions(child);
        }
    }
    if let Some(branches) = schema.remove("oneOf") {
        if schema.contains_key("anyOf") {
            if let Some(all) = schema
                .entry("allOf")
                .or_insert_with(|| json!([]))
                .as_array_mut()
            {
                all.push(json!({ "anyOf": branches }));
            }
        } else {
            schema.insert("anyOf".into(), branches);
        }
    }
    if constrains_strings(schema) {
        widen_schema(
            schema,
            json!({ "type": "string", "pattern": SUBSTITUTION_PATTERN }),
        );
    }
}

fn constrains_strings(schema: &Map<String, Value>) -> bool {
    let string_values = schema
        .get("enum")
        .and_then(Value::as_array)
        .is_some_and(|values| values.iter().any(Value::is_string))
        || schema.get("const").is_some_and(Value::is_string);
    let typed_string = match schema.get("type") {
        Some(Value::String(kind)) => kind == "string",
        Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind == "string"),
        _ => false,
    };
    string_values
        || (typed_string
            && ["pattern", "format", "minLength", "maxLength"]
                .iter()
                .any(|key| schema.contains_key(*key)))
}
