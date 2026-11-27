//! JSON Schemas of what [`super::decode_pack_config`] accepts, which is wider
//! than the documents it returns: `interpolate_values` substitutes `${...}` in
//! every string first, `bind_owner` fills every omitted owner and
//! `hydrate_eval_cases` reads `./` case sidecars.

use schemars::{schema_for, JsonSchema, Schema};
use serde_json::{json, Map, Value};

use crate::pack::interpolate::SUBSTITUTION_PATTERN;
use gents_documents::{noted, widen_schema, PageDocument, PluginSourceDocument};
use gents_documents::{
    Agent, AgentContext, AgentTargetDocument, Callback, CallbackBinding, CallbackModule,
    CompactionConfig, DatastoreToolSurfaceDocument, DefaultAgentSelection, EvalDefinition,
    EventSource, InferenceBackend, InferenceExecution, InferenceProfile, InferenceRetryPolicy,
    InferenceSampling, Node, PackConfig, ProjectionAcpBinding, RepositoryPlacement, Schedule,
    SkillDocument, Task, ToolServiceRegistry, Tools, Trigger,
};
use gents_store::collection::Collection;

use super::{EVAL_CASES_FIELD, OWNER_FIELD};

const SUBSTITUTION_NOTE: &str =
    "A ${VAR} or ${VAR:-default} substitution, resolved from the environment when the pack loads.";
const CASE_SIDECAR_NOTE: &str =
    "A ./ path to a pack file that holds one case, read when the pack loads.";
/// The scalar a field holds, when a substitution written there resolves into
/// that scalar instead of a string. Set by [`allow_substitutions`], which is
/// also what widens the field to accept the substitution; the loader reads it
/// to type the substituted text by its field rather than by its spelling.
pub(super) const SUBSTITUTION_KIND: &str = "x-gents-substitution-kind";

/// The fields a substitution resolves into as a JSON scalar rather than text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SubstitutionKind {
    Number,
    Integer,
    Boolean,
}

impl SubstitutionKind {
    pub(super) fn annotation(self) -> &'static str {
        match self {
            Self::Number => "number",
            Self::Integer => "integer",
            Self::Boolean => "boolean",
        }
    }

    pub(super) fn from_annotation(text: &str) -> Option<Self> {
        match text {
            "number" => Some(Self::Number),
            "integer" => Some(Self::Integer),
            "boolean" => Some(Self::Boolean),
            _ => None,
        }
    }

    /// The scalar `text` spells in this field, or `None` when it is not one.
    pub(super) fn parse(self, text: &str) -> Option<Value> {
        match self {
            Self::Number => text
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map(Value::Number),
            Self::Integer => text.parse::<i64>().ok().map(Value::from),
            Self::Boolean => match text {
                "true" => Some(Value::Bool(true)),
                "false" => Some(Value::Bool(false)),
                _ => None,
            },
        }
    }
}

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
            .get_mut(Agent::schema_name().as_ref())
            .and_then(Value::as_object_mut)
        {
            author_agent_slot(definition);
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
        Collection::DefaultAgentSelection => schema_for!(DefaultAgentSelection),
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
        Collection::Page => schema_for!(PageDocument),
        Collection::PluginSource => schema_for!(PluginSourceDocument),
    };
    let document = schema.ensure_object();
    omit_owner(document);
    if collection == Collection::Agent {
        author_agent_slot(document);
    }
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

fn author_agent_slot(document: &mut Map<String, Value>) {
    if let Some(properties) = document
        .get_mut("properties")
        .and_then(Value::as_object_mut)
    {
        properties.remove("agent_did");
        properties.remove("creation_nonce");
        properties.insert(
            "agent_slot".into(),
            json!({"type": "string", "minLength": 1}),
        );
    }
    if let Some(required) = document.get_mut("required").and_then(Value::as_array_mut) {
        required.retain(|field| field != "agent_did" && field != "creation_nonce");
        required.push(json!("agent_slot"));
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
                noted(json!({ "type": "string", "pattern": "^\\./" }), CASE_SIDECAR_NOTE)
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
/// so every constrained string also accepts one, as one more member when it
/// is already a choice of strings. `oneOf` becomes `anyOf` because an
/// interpolated tag no longer tells the branches apart.
fn allow_substitutions(schema: &mut Map<String, Value>) {
    let substitution = noted(
        json!({ "type": "string", "pattern": SUBSTITUTION_PATTERN }),
        SUBSTITUTION_NOTE,
    );
    if let Some(members) = schema.get_mut("anyOf").and_then(Value::as_array_mut) {
        let only_strings = members.iter().all(|member| {
            member.as_object().is_some_and(|member| {
                constrains_strings(member) || member.get("type") == Some(&json!("null"))
            })
        });
        if only_strings {
            members.push(substitution);
            return;
        }
    }
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
        widen_schema(schema, substitution);
    } else if let Some(kind) = scalar_substitution_kind(schema) {
        // An integer, number or boolean field reads a substitution as that
        // scalar; `widen_schema` keeps only annotations, so the kind is set
        // after it.
        widen_schema(schema, substitution);
        schema.insert(SUBSTITUTION_KIND.into(), kind.annotation().into());
    }
}

/// The scalar a field typed only as one of `integer`, `number` or `boolean`
/// (plus `null`) holds: a substitution written there resolves into it.
fn scalar_substitution_kind(schema: &Map<String, Value>) -> Option<SubstitutionKind> {
    if ["enum", "const", "if", "not"]
        .iter()
        .any(|key| schema.contains_key(*key))
    {
        return None;
    }
    let kinds: Vec<&str> = match schema.get("type") {
        Some(Value::String(kind)) => vec![kind.as_str()],
        Some(Value::Array(kinds)) => kinds.iter().filter_map(Value::as_str).collect(),
        _ => return None,
    };
    if ["anyOf", "oneOf", "allOf"]
        .iter()
        .any(|key| schema.contains_key(*key))
    {
        return None;
    }
    let mut non_null = kinds.into_iter().filter(|kind| *kind != "null");
    let kind = non_null.next()?;
    if non_null.next().is_some() {
        return None;
    }
    Some(match kind {
        "integer" => SubstitutionKind::Integer,
        "number" => SubstitutionKind::Number,
        "boolean" => SubstitutionKind::Boolean,
        _ => return None,
    })
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
