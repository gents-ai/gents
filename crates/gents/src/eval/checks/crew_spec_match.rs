//! `crew_spec_match`: how much of a declared configuration a subject built,
//! graded per requirement and reported per category.
//!
//! One check scores a whole specification because a stage names each check
//! once: the categories (completeness, templates, least privilege, receipt,
//! ...) are reported in `raw.categories` rather than as separate verdicts.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::eval::checks::captured_fields_match::{test, Expectation, FieldPath, Test};
use crate::eval::checks::{
    bounded, excerpt, graded, graded_reason_codes, grader, Check, CheckDescription, CheckVerdict,
    EXCERPT_CHARS,
};
use crate::eval::runner::executor::{CaptureResult, StageEvidence};

/// Params (every list optional, at least one requirement overall):
///
/// - `present`: `[{capture, min?, max?, category?}]`, each capture must exist
///   and meet any row-count bounds. A
///   documents capture that selects fields exists only when its collection
///   has them, so this checks a schema as well as rows.
/// - `rows`: `[{capture, key, id, category?, expect: [expectation]}]`: a row
///   whose `key` equals `id` exists (one requirement) and holds each
///   expectation (one requirement each).
/// - `links`: `[{capture, key, id, field, target_capture, target_key, expect, category?}]`
///   follows a stored ID in `field` and tests the selected target row. A
///   `source_match` field expectation can replace `key`/`id`; it must select
///   exactly one source row. Optional
///   `via` hops follow more references before testing. `reported_fields` requires
///   non-null target values in the final assistant message (ignoring case).
/// - `agents`: `{behaviors, contexts, tools, expect: [{behavior_id, category?,
///   behavior: [...], context: [...], tools: [...]}]}`: the behavior exists,
///   and each expectation holds on it, on the Context it selects and on the
///   Tools that Context selects. Optional `datastore` follows selected surfaces,
///   comparing exact create/query collection sets. `caller_fields` requires model-
///   supplied required create fields; `required_fields` accepts required caller
///   fields or runtime fills; `query_fields` requires returned columns;
///   `lookup_by` requires queries usable with
///   that key alone. `called_create`/`called_query` require successful calls to
///   the selected tools. Optional `delegates` checks the exact local behavior set
///   reached through selected subagent targets. Both reject unresolved references.
///   With `source_match`, `behavior_id` names a role rather than a stored ID:
///   the selector must identify one behavior, distinct from other named roles.
///   Delegate expectations resolve those role names to the selected stored IDs.
/// - `templates`: `[{capture, key, id, fields: [field], allowed: [name],
///   category?}]`: every `{{ doc.NAME }}` in each present template field of
///   that row names an allowed field (one requirement per field present).
/// - `receipt`: `{capture, fields: [field], ids: [id], category?}`: each id
///   appears in the text of some row's fields.
/// - `continuation`: `{max, category?}`: each of the `max` prods the stage
///   did not need is one requirement held.
///
/// An expectation is `captured_fields_match`'s: `{field, equals | contains |
/// matches}`, `field` a dotted path; a JSON string along the path is read as
/// the JSON it holds, and a missing path reads as `null`. A row's `id` (and a
/// behavior's) matches its stored ID exactly or as the `<scope>:<id>` suffix a
/// principal-scoped behavior ID carries. `raw.activity` counts the stage's tool calls, failed
/// tool calls and inference calls.
pub struct CrewSpecMatch;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    #[serde(default)]
    present: Vec<Present>,
    #[serde(default)]
    rows: Vec<RowSpec>,
    #[serde(default)]
    links: Vec<LinkSpec>,
    #[serde(default)]
    agents: Option<Agents>,
    #[serde(default)]
    templates: Vec<TemplateSpec>,
    #[serde(default)]
    receipt: Option<Receipt>,
    #[serde(default)]
    continuation: Option<Continuation>,
}

/// Grades how many continuation prods the stage needed: `max - prods` of
/// `max` requirements hold, so finishing unprompted scores full marks and a
/// prod costs a requirement rather than failing the trial.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Continuation {
    max: u32,
    #[serde(default)]
    category: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Present {
    capture: String,
    #[serde(default)]
    min: Option<usize>,
    #[serde(default)]
    max: Option<usize>,
    #[serde(default)]
    category: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RowSpec {
    capture: String,
    key: String,
    id: String,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    expect: Vec<Expectation>,
}

/// Follow the reference a source row actually stores; matching an unrelated
/// target row does not establish that the configured path reaches it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkSpec {
    capture: String,
    #[serde(default)]
    key: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    source_match: Option<Expectation>,
    field: String,
    target_capture: String,
    target_key: String,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    expect: Vec<Expectation>,
    #[serde(default)]
    via: Vec<LinkHop>,
    #[serde(default)]
    reported_fields: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkHop {
    field: String,
    target_capture: String,
    target_key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Agents {
    behaviors: String,
    contexts: String,
    tools: String,
    expect: Vec<AgentSpec>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentSpec {
    behavior_id: String,
    #[serde(default)]
    source_match: Option<Expectation>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    behavior: Vec<Expectation>,
    #[serde(default)]
    context: Vec<Expectation>,
    #[serde(default)]
    tools: Vec<Expectation>,
    #[serde(default)]
    datastore: Option<DatastoreSpec>,
    #[serde(default)]
    delegates: Option<DelegateSpec>,
}

/// Compare capabilities through the selected documents, allowing the model to
/// choose internal IDs and tool names. Extra grants fail the exact-set check.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DatastoreSpec {
    surfaces: String,
    create: BTreeSet<String>,
    query: BTreeSet<String>,
    #[serde(default)]
    caller_fields: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    required_fields: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    query_fields: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    lookup_by: Option<String>,
    #[serde(default)]
    called_create: BTreeSet<String>,
    #[serde(default)]
    called_query: BTreeSet<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DelegateSpec {
    targets: String,
    behaviors: BTreeSet<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TemplateSpec {
    capture: String,
    key: String,
    id: String,
    fields: Vec<String>,
    allowed: Vec<String>,
    #[serde(default)]
    category: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    capture: String,
    fields: Vec<String>,
    ids: Vec<String>,
    #[serde(default)]
    category: Option<String>,
}

/// A dotted path into `row`, reading a JSON string on the way as its JSON.
/// A path the row lacks reads as `null`, so `equals: null` and `matches`
/// over `null` can require a field to be absent or unset.
fn lookup(row: &Value, field: &str) -> Value {
    let mut current = row.clone();
    for key in field.split('.') {
        if let Value::String(text) = &current {
            if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                current = parsed;
            }
        }
        match current.get(key) {
            Some(next) => current = next.clone(),
            None => return Value::Null,
        }
    }
    current
}

/// Whether a stored ID is the declared one. Behavior IDs a principal creates
/// are stored scoped to it (`<DID>:<id>`), so a declared `id` also matches a
/// stored ID ending in `:<id>`; an exact match wins.
fn is_id(stored: &str, id: &str) -> bool {
    stored == id
        || stored
            .strip_suffix(id)
            .is_some_and(|scope| scope.ends_with(':'))
}

fn find<'a>(rows: &'a [Value], key: &str, id: &str) -> Option<&'a Value> {
    rows.iter()
        .find(|row| lookup(row, key).as_str() == Some(id))
        .or_else(|| {
            rows.iter().find(|row| {
                lookup(row, key)
                    .as_str()
                    .is_some_and(|stored| is_id(stored, id))
            })
        })
}

/// Every `NAME` a template reads as `doc.NAME`.
fn doc_fields(template: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("doc.") {
        let preceded = rest[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
        let tail = &rest[start + 4..];
        let name: String = tail
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if !preceded && !name.is_empty() && !names.contains(&name) {
            names.push(name.clone());
        }
        rest = &tail[name.len()..];
    }
    names
}

#[derive(Default)]
struct Tally {
    categories: BTreeMap<String, (usize, usize)>,
    unmet: Vec<String>,
}

impl Tally {
    fn record(&mut self, category: Option<&str>, met: bool, what: impl FnOnce() -> String) {
        let entry = self
            .categories
            .entry(category.unwrap_or("completeness").to_owned())
            .or_default();
        entry.1 += 1;
        if met {
            entry.0 += 1;
        } else {
            self.unmet.push(format!(
                "[{}] {}",
                category.unwrap_or("completeness"),
                what()
            ));
        }
    }

    fn expectations(
        &mut self,
        category: Option<&str>,
        subject: &str,
        row: Option<&Value>,
        tests: &[(FieldPath, Test)],
    ) {
        for (field, test) in tests {
            let actual = row.and_then(|row| field.resolve(|path| Some(lookup(row, path))));
            let met = actual.as_ref().is_some_and(|actual| test.holds(actual));
            self.record(category, met, || {
                format!(
                    "{subject}: {field} expected {}, got {}",
                    excerpt(&test.describe(), EXCERPT_CHARS),
                    actual.map_or("no row".to_string(), |actual| excerpt(
                        &actual.to_string(),
                        EXCERPT_CHARS
                    ))
                )
            });
        }
    }
}

fn tests(expectations: Vec<Expectation>) -> Result<Vec<(FieldPath, Test)>, String> {
    expectations.into_iter().map(test).collect()
}

impl Check for CrewSpecMatch {
    fn name(&self) -> &'static str {
        "crew_spec_match"
    }

    fn version(&self) -> &'static str {
        "7"
    }

    fn describe(&self) -> CheckDescription {
        let expectation = json!({
            "type": "object",
            "properties": {
                "field": {"type": "string"},
                "fallback_field": {"type": "string", "description": "path used only when field is absent or null"},
                "equals": {},
                "contains": {"type": "string"},
                "matches": {"type": "string"}
            },
            "required": ["field"],
            "additionalProperties": false
        });
        let expectations = json!({"type": "array", "items": expectation});
        let strings = json!({"type": "array", "items": {"type": "string"}});
        let category = json!({"type": "string"});
        CheckDescription {
            name: self.name().into(),
            version: self.version().into(),
            summary: "Scores the fraction of a declared configuration's requirements that the captured home holds: captures present, keyed rows and their fields, each behavior's Context and Tools, template fields against each source collection, and ids named in a receipt. raw.categories breaks the score down; raw.activity counts tool and inference calls.".into(),
            params_schema: json!({
                "type": "object",
                "properties": {
                    "present": {"type": "array", "items": {
                        "type": "object",
                        "properties": {"capture": {"type": "string"}, "category": category, "min":{"type":"integer","minimum":0}, "max":{"type":"integer","minimum":0}},
                        "required": ["capture"], "additionalProperties": false
                    }},
                    "rows": {"type": "array", "items": {
                        "type": "object",
                        "properties": {
                            "capture": {"type": "string"}, "key": {"type": "string"},
                            "id": {"type": "string"}, "category": category, "expect": expectations
                        },
                        "required": ["capture", "key", "id"], "additionalProperties": false
                    }},
                    "links": {"type": "array", "items": {
                        "type": "object",
                        "properties": {
                            "capture": {"type": "string"}, "key": {"type": "string"},
                            "id": {"type": "string"}, "field": {"type": "string"},
                            "source_match": expectation,
                            "target_capture": {"type": "string"}, "target_key": {"type": "string"},
                            "category": category, "expect": expectations,
                            "reported_fields": strings,
                            "via": {"type":"array", "items": {
                                "type":"object", "properties": {
                                    "field":{"type":"string"}, "target_capture":{"type":"string"}, "target_key":{"type":"string"}
                                }, "required":["field","target_capture","target_key"], "additionalProperties":false
                            }}
                        },
                        "required": ["capture", "field", "target_capture", "target_key"],
                        "oneOf": [
                            {"required":["key","id"],"not":{"required":["source_match"]}},
                            {"required":["source_match"],"not":{"anyOf":[{"required":["key"]},{"required":["id"]}]}}
                        ],
                        "additionalProperties": false
                    }},
                    "agents": {
                        "type": "object",
                        "properties": {
                            "behaviors": {"type": "string"}, "contexts": {"type": "string"},
                            "tools": {"type": "string"},
                            "expect": {"type": "array", "items": {
                                "type": "object",
                                "properties": {
                                    "behavior_id": {"type": "string"}, "category": category,
                                    "source_match": expectation,
                                    "behavior": expectations, "context": expectations,
                                    "tools": expectations,
                                    "datastore": {"type":"object", "properties": {
                                        "surfaces":{"type":"string"}, "create":strings, "query":strings, "caller_fields":{"type":"object", "additionalProperties":strings}, "required_fields":{"type":"object", "additionalProperties":strings}, "query_fields":{"type":"object", "additionalProperties":strings}, "lookup_by":{"type":"string"}, "called_create":strings, "called_query":strings
                                    }, "required":["surfaces","create","query"], "additionalProperties":false},
                                    "delegates": {"type":"object", "properties": {
                                        "targets":{"type":"string"}, "behaviors":strings
                                    }, "required":["targets","behaviors"], "additionalProperties":false}
                                },
                                "required": ["behavior_id"], "additionalProperties": false
                            }}
                        },
                        "required": ["behaviors", "contexts", "tools", "expect"],
                        "additionalProperties": false
                    },
                    "templates": {"type": "array", "items": {
                        "type": "object",
                        "properties": {
                            "capture": {"type": "string"}, "key": {"type": "string"},
                            "id": {"type": "string"}, "fields": strings, "allowed": strings,
                            "category": category
                        },
                        "required": ["capture", "key", "id", "fields", "allowed"],
                        "additionalProperties": false
                    }},
                    "continuation": {
                        "type": "object",
                        "properties": {"max": {"type": "integer", "minimum": 1}, "category": category},
                        "required": ["max"], "additionalProperties": false
                    },
                    "receipt": {
                        "type": "object",
                        "properties": {
                            "capture": {"type": "string"}, "fields": strings, "ids": strings,
                            "category": category
                        },
                        "required": ["capture", "fields", "ids"], "additionalProperties": false
                    }
                },
                "additionalProperties": false
            }),
            reads: vec!["capture:documents".into(), "stage:tool_calls".into()],
            reason_codes: graded_reason_codes(&[]),
        }
    }

    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict {
        let params: Params = match serde_json::from_value(params.clone()) {
            Ok(params) => params,
            Err(error) => return grader("bad_params", error.to_string()),
        };
        let rows = |name: &str| match stage.captures.get(name) {
            Some(CaptureResult::Documents { rows }) => Some(rows.as_slice()),
            _ => None,
        };
        let mut tally = Tally::default();

        for present in &params.present {
            if present
                .min
                .zip(present.max)
                .is_some_and(|(min, max)| min > max)
            {
                return grader("bad_params", "capture minimum exceeds maximum");
            }
            let count = rows(&present.capture).map(<[Value]>::len);
            tally.record(
                present.category.as_deref(),
                count.is_some_and(|n| {
                    present.min.is_none_or(|min| n >= min) && present.max.is_none_or(|max| n <= max)
                }),
                || {
                    format!(
                        "capture {} has {:?} rows; expected min {:?}, max {:?}",
                        present.capture, count, present.min, present.max
                    )
                },
            );
        }
        for spec in params.rows {
            let category = spec.category.as_deref();
            let tests = match tests(spec.expect) {
                Ok(tests) => tests,
                Err(detail) => return grader("bad_params", detail),
            };
            let row = rows(&spec.capture).and_then(|rows| find(rows, &spec.key, &spec.id));
            let subject = format!("{} {}", spec.capture, spec.id);
            tally.record(category, row.is_some(), || format!("{subject} absent"));
            tally.expectations(category, &subject, row, &tests);
        }
        for spec in params.links {
            let tests = match tests(spec.expect) {
                Ok(tests) => tests,
                Err(detail) => return grader("bad_params", detail),
            };
            let source = if let Some(selector) = spec.source_match {
                if !spec.key.is_empty() || !spec.id.is_empty() {
                    return grader("bad_params", "links use either key/id or source_match");
                }
                let (field, matcher) = match test(selector) {
                    Ok(test) => test,
                    Err(detail) => return grader("bad_params", detail),
                };
                rows(&spec.capture).and_then(|rows| {
                    let mut matches = rows.iter().filter(|row| {
                        field
                            .resolve(|path| Some(lookup(row, path)))
                            .is_some_and(|actual| matcher.holds(&actual))
                    });
                    let first = matches.next();
                    if matches.next().is_some() {
                        None
                    } else {
                        first
                    }
                })
            } else {
                if spec.key.is_empty() || spec.id.is_empty() {
                    return grader("bad_params", "links require key/id or source_match");
                }
                rows(&spec.capture).and_then(|rows| find(rows, &spec.key, &spec.id))
            };
            let reference = source.map(|row| lookup(row, &spec.field));
            let mut target = reference.as_ref().and_then(Value::as_str).and_then(|id| {
                rows(&spec.target_capture).and_then(|rows| find(rows, &spec.target_key, id))
            });
            for hop in &spec.via {
                let reference = target.map(|row| lookup(row, &hop.field));
                target = reference.as_ref().and_then(Value::as_str).and_then(|id| {
                    rows(&hop.target_capture).and_then(|rows| find(rows, &hop.target_key, id))
                });
            }
            let (destination, destination_key) = spec
                .via
                .last()
                .map(|hop| (&hop.target_capture, &hop.target_key))
                .unwrap_or((&spec.target_capture, &spec.target_key));
            let source_name = if spec.id.is_empty() {
                source
                    .map(|row| lookup(row, "display_name"))
                    .unwrap_or(Value::Null)
                    .to_string()
            } else {
                spec.id.clone()
            };
            let target_name = target
                .map(|row| lookup(row, destination_key))
                .unwrap_or(Value::Null);
            let subject = format!(
                "{} {} -> {} {}",
                spec.capture, source_name, destination, target_name
            );
            let category = spec.category.as_deref();
            tally.record(category, target.is_some(), || {
                format!("{subject}: unresolved reference")
            });
            tally.expectations(category, &subject, target, &tests);
            let final_message = stage
                .messages
                .iter()
                .rev()
                .find(|m| m.role == "assistant" && !m.content.trim().is_empty())
                .map(|m| m.content.to_lowercase())
                .unwrap_or_default();
            for field in &spec.reported_fields {
                let value = target.map(|row| lookup(row, field)).unwrap_or(Value::Null);
                let text = match &value {
                    Value::String(s) => s.to_lowercase(),
                    other => other.to_string(),
                };
                tally.record(
                    category,
                    !value.is_null() && !text.is_empty() && final_message.contains(&text),
                    || format!("final message does not report {subject}.{field}"),
                );
            }
        }
        if let Some(agents) = params.agents {
            let behaviors = rows(&agents.behaviors).unwrap_or_default();
            let contexts = rows(&agents.contexts).unwrap_or_default();
            let tools = rows(&agents.tools).unwrap_or_default();
            let mut roles = BTreeMap::new();
            let mut selected = Vec::new();
            for mut spec in agents.expect {
                let behavior = if let Some(selector) = spec.source_match.take() {
                    let (field, matcher) = match test(selector) {
                        Ok(test) => test,
                        Err(detail) => return grader("bad_params", detail),
                    };
                    let mut matches = behaviors.iter().filter(|row| {
                        field
                            .resolve(|path| Some(lookup(row, path)))
                            .is_some_and(|actual| matcher.holds(&actual))
                    });
                    let first = matches.next();
                    let unique = if matches.next().is_none() {
                        first
                    } else {
                        None
                    };
                    let id = unique
                        .and_then(|row| row.get("behavior_id"))
                        .and_then(Value::as_str);
                    if roles.insert(spec.behavior_id.clone(), id).is_some() {
                        return grader("bad_params", "duplicate named behavior role");
                    }
                    unique
                } else {
                    find(behaviors, "behavior_id", &spec.behavior_id)
                };
                selected.push((spec, behavior));
            }
            let mut counts = BTreeMap::new();
            for id in roles.values().flatten() {
                *counts.entry(*id).or_insert(0) += 1;
            }
            for id in roles.values_mut() {
                if id.is_some_and(|id| counts[&id] != 1) {
                    *id = None;
                }
            }
            for (spec, behavior) in selected {
                let behavior =
                    behavior.filter(|_| roles.get(&spec.behavior_id).is_none_or(Option::is_some));
                let category = spec.category.as_deref();
                let (behavior_tests, context_tests, tools_tests) =
                    match (tests(spec.behavior), tests(spec.context), tests(spec.tools)) {
                        (Ok(b), Ok(c), Ok(t)) => (b, c, t),
                        (Err(detail), _, _) | (_, Err(detail), _) | (_, _, Err(detail)) => {
                            return grader("bad_params", detail)
                        }
                    };
                let context = behavior
                    .and_then(|row| row.get("context_id").and_then(Value::as_str))
                    .and_then(|id| find(contexts, "context_id", id));
                let no_tools = json!({});
                let tools_row = context
                    .and_then(|row| row.get("tools_id").and_then(Value::as_str))
                    .and_then(|id| find(tools, "tools_id", id))
                    .or_else(|| {
                        context
                            .filter(|row| row.get("tools_id").is_some_and(Value::is_null))
                            .map(|_| &no_tools)
                    });
                let id = &spec.behavior_id;
                tally.record(category, behavior.is_some(), || {
                    format!("behavior {id} absent")
                });
                tally.expectations(
                    category,
                    &format!("behavior {id}"),
                    behavior,
                    &behavior_tests,
                );
                tally.expectations(category, &format!("{id} context"), context, &context_tests);
                tally.expectations(category, &format!("{id} tools"), tools_row, &tools_tests);
                if let Some(expected) = spec.datastore {
                    use crate::document_config::{DatastoreToolSurfaceDocument, SurfaceToolDecl};
                    let selected =
                        tools_row.map(|t| lookup(t, "datastore.datastore_tool_surface_ids"));
                    let mut creates = BTreeSet::new();
                    let mut queries = BTreeSet::new();
                    let mut called_create = BTreeSet::new();
                    let mut called_query = BTreeSet::new();
                    let mut contract_issues = Vec::new();
                    let mut valid = context.is_some() && tools_row.is_some();
                    if let Some(Value::Array(ids)) = selected {
                        for reference in ids {
                            let surface = reference
                                .as_str()
                                .and_then(|reference| {
                                    rows(&expected.surfaces)
                                        .and_then(|rows| find(rows, "surface_id", reference))
                                })
                                .and_then(|row| {
                                    let mut document = row.clone();
                                    document.as_object_mut()?.remove("_docID");
                                    serde_json::from_value::<DatastoreToolSurfaceDocument>(document)
                                        .ok()
                                });
                            if let Some(surface) = surface.filter(|s| s.enabled) {
                                for entry in surface.entries.unwrap_or_default() {
                                    valid &= entry.is_well_formed();
                                    let called = stage.tool_calls.iter().any(|call| {
                                        call.tool_name == entry.tool_name()
                                            && call.status.as_deref() == Some("completed")
                                            && call.tool_failure_class.is_none()
                                    });
                                    match entry {
                                        SurfaceToolDecl::Create(d) => {
                                            if let Some(fields) =
                                                expected.required_fields.get(&d.collection)
                                            {
                                                for name in fields {
                                                    if !d.fields.iter().any(|f| {
                                                        &f.name == name
                                                            && (f.required || f.fill.is_some())
                                                    }) {
                                                        valid = false;
                                                        contract_issues.push(format!("{} must write {name}: declare a required caller field or a runtime fill", d.tool_name));
                                                    }
                                                }
                                            }
                                            if let Some(fields) =
                                                expected.caller_fields.get(&d.collection)
                                            {
                                                let complete = fields.iter().all(|name| {
                                                    d.fields.iter().any(|f| {
                                                        &f.name == name
                                                            && f.required
                                                            && f.fill.is_none()
                                                    })
                                                });
                                                valid &= complete;
                                                if !complete {
                                                    contract_issues.push(format!("{} create fields {fields:?} must be required and caller-supplied", d.tool_name));
                                                }
                                            }
                                            if called {
                                                called_create.insert(d.collection.clone());
                                            }
                                            creates.insert(d.collection);
                                        }
                                        SurfaceToolDecl::Query(d) => {
                                            if let Some(fields) =
                                                expected.query_fields.get(&d.collection)
                                            {
                                                let complete = fields
                                                    .iter()
                                                    .all(|field| d.fields.contains(field));
                                                valid &= complete;
                                                if !complete {
                                                    contract_issues.push(format!(
                                                        "{} must return query fields {fields:?}",
                                                        d.tool_name
                                                    ));
                                                }
                                            }
                                            if let Some(key) = &expected.lookup_by {
                                                let by_key_alone = d.fields.contains(key)
                                                    && d.filter_fields.iter().any(|f| {
                                                        &f.name == key && f.fill.is_none()
                                                    })
                                                    && d.filter_fields.iter().all(|f| {
                                                        f.fill.is_none()
                                                            && (!f.required || &f.name == key)
                                                    });
                                                valid &= by_key_alone;
                                                if !by_key_alone {
                                                    contract_issues.push(format!("{} query must accept caller-supplied {key} alone: return {key}, remove fills only from this query's filter_fields, and make its other filters optional. Update this query entry; preserve write-tool fills and unrelated entries", d.tool_name));
                                                }
                                            }
                                            if called {
                                                called_query.insert(d.collection.clone());
                                            }
                                            queries.insert(d.collection);
                                        }
                                    }
                                }
                            } else {
                                valid = false;
                            }
                        }
                    } else if selected.is_some_and(|v| !v.is_null()) {
                        valid = false;
                    }
                    tally.record(category, valid && creates == expected.create && queries == expected.query && expected.called_create.is_subset(&called_create) && expected.called_query.is_subset(&called_query), || {
                        format!("{id} selected datastore grants: create {creates:?}, query {queries:?}; expected create {:?}, query {:?}; references valid: {valid}; called create {called_create:?}, query {called_query:?}; field contracts: {contract_issues:?}", expected.create, expected.query)
                    });
                }
                if let Some(expected) = spec.delegates {
                    let selected = tools_row.map(|t| lookup(t, "subagents.target_ids"));
                    let enabled =
                        tools_row.map(|t| lookup(t, "subagents.enabled")) == Some(json!(true));
                    let mut actual = BTreeSet::new();
                    let mut valid = enabled
                        && expected
                            .behaviors
                            .iter()
                            .all(|name| roles.get(name).is_none_or(Option::is_some));
                    if let Some(Value::Array(ids)) = selected {
                        for reference in ids {
                            let target = reference.as_str().and_then(|reference| {
                                rows(&expected.targets)
                                    .and_then(|rows| find(rows, "target_id", reference))
                            });
                            valid &= target.is_some_and(|t| {
                                t.get("agent_did").and_then(Value::as_str).is_some()
                                    && t.get("target_agent_did") == t.get("agent_did")
                            });
                            if let Some(behavior) = target
                                .and_then(|t| t.get("behavior_id"))
                                .and_then(Value::as_str)
                            {
                                if let Some(name) =
                                    expected
                                        .behaviors
                                        .iter()
                                        .find(|name| match roles.get(*name) {
                                            Some(Some(id)) => behavior == *id,
                                            Some(None) => false,
                                            None => is_id(behavior, name),
                                        })
                                {
                                    actual.insert(name.clone());
                                } else {
                                    valid = false;
                                    actual.insert(behavior.to_string());
                                }
                            } else {
                                valid = false;
                            }
                        }
                    } else {
                        valid = false;
                    }
                    tally.record(category, valid && actual == expected.behaviors, || {
                        format!("{id} selected delegates {actual:?}; expected {:?}; references valid: {valid}", expected.behaviors)
                    });
                }
            }
        }
        for spec in &params.templates {
            let category = spec.category.as_deref().or(Some("templates"));
            let row = rows(&spec.capture).and_then(|rows| find(rows, &spec.key, &spec.id));
            let Some(row) = row else {
                tally.record(category, false, || {
                    format!("{} {} absent, no template to check", spec.capture, spec.id)
                });
                continue;
            };
            for field in &spec.fields {
                let Some(template) = row.get(field).and_then(Value::as_str) else {
                    continue;
                };
                let unknown: Vec<String> = doc_fields(template)
                    .into_iter()
                    .filter(|name| !spec.allowed.contains(name))
                    .collect();
                tally.record(category, unknown.is_empty(), || {
                    format!(
                        "{} {}.{field} reads doc.{} outside its source collection",
                        spec.capture,
                        spec.id,
                        unknown.join(", doc.")
                    )
                });
            }
        }
        if let Some(receipt) = &params.receipt {
            let category = receipt.category.as_deref().or(Some("receipt"));
            let text: String = rows(&receipt.capture)
                .unwrap_or_default()
                .iter()
                .flat_map(|row| {
                    receipt.fields.iter().filter_map(|field| {
                        row.get(field).map(|value| match value {
                            Value::String(text) => text.clone(),
                            other => other.to_string(),
                        })
                    })
                })
                .collect::<Vec<_>>()
                .join("\n");
            for id in &receipt.ids {
                tally.record(category, text.contains(id.as_str()), || {
                    format!("receipt does not name {id}")
                });
            }
        }

        if let Some(continuation) = &params.continuation {
            let category = continuation.category.as_deref().or(Some("continuation"));
            for index in 0..continuation.max {
                tally.record(category, index >= stage.prods, || {
                    format!("continuation prod {} was needed", index + 1)
                });
            }
        }
        let (satisfied, total) = tally
            .categories
            .values()
            .fold((0, 0), |(s, t), (cs, ct)| (s + cs, t + ct));
        if total == 0 {
            return grader("bad_params", "params require nothing");
        }
        let failed_calls = stage
            .tool_calls
            .iter()
            .filter(|call| {
                call.status.as_deref() == Some("failed") || call.tool_failure_class.is_some()
            })
            .count();
        let mut by_tool: BTreeMap<&str, usize> = BTreeMap::new();
        for call in &stage.tool_calls {
            *by_tool.entry(call.tool_name.as_str()).or_default() += 1;
        }
        let feedback = (!tally.unmet.is_empty()).then(|| {
            let mut text = format!("{} of {total} requirements unmet:\n", tally.unmet.len());
            for line in tally.unmet.iter().take(20) {
                text.push_str(&format!("- {line}\n"));
            }
            bounded(text)
        });
        let mut verdict = graded(satisfied, total, feedback);
        verdict.raw["categories"] = tally
            .categories
            .iter()
            .map(|(name, (s, t))| (name.clone(), json!({"satisfied": s, "total": t})))
            .collect::<serde_json::Map<_, _>>()
            .into();
        verdict.raw["unmet"] = json!(tally.unmet);
        verdict.raw["unmet_count"] = json!(tally.unmet.len());
        verdict.raw["activity"] = json!({
            "tool_calls": stage.tool_calls.len(),
            "failed_tool_calls": failed_calls,
            "by_tool": by_tool,
            "inference_calls": stage.inference_calls.len(),
            "messages": stage.messages.len(),
            "prods": stage.prods,
        });
        verdict
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::runner::scripted::ScriptedExecutor;
    use crate::eval::OutcomeKind;

    fn stage(captures: &[(&str, Vec<Value>)]) -> StageEvidence {
        let mut stage = ScriptedExecutor::passed_evidence("did:x", "s1", "unused", Vec::new())
            .stages
            .remove(0);
        stage.captures.clear();
        for (name, rows) in captures {
            stage.captures.insert(
                (*name).to_string(),
                CaptureResult::Documents { rows: rows.clone() },
            );
        }
        stage
    }

    fn home() -> StageEvidence {
        stage(&[
            (
                "behaviors",
                vec![
                    json!({"behavior_id": "worker-a", "context_id": "c1", "inference_profile_id": "glm-a"}),
                ],
            ),
            (
                "contexts",
                vec![
                    json!({"context_id": "c1", "tools_id": "t1", "system_prompt": "You are a GLM worker"}),
                ],
            ),
            (
                "tools",
                vec![
                    json!({"tools_id": "t1", "host": "{\"files\":{\"mode\":\"ReadWrite\"}}", "subagents": {"enabled": false}}),
                ],
            ),
            (
                "tasks",
                vec![
                    json!({"task_id": "work-a", "emit_outcome": true, "prompt_template": "Shard {{ doc.shard_id }} {{doc.owner}}"}),
                ],
            ),
            ("c_assignment", vec![]),
            (
                "mailbox",
                vec![json!({"title": "Setup receipt", "payload": "worker-a work-a"})],
            ),
        ])
    }

    fn params() -> Value {
        json!({
            "present": [{"capture": "c_assignment", "category": "collections"}, {"capture": "c_result", "category": "collections"}],
            "rows": [{"capture": "tasks", "key": "task_id", "id": "work-a", "category": "emit_outcome",
                      "expect": [{"field": "emit_outcome", "equals": true}]}],
            "agents": {"behaviors": "behaviors", "contexts": "contexts", "tools": "tools", "expect": [
                {"behavior_id": "worker-a", "category": "least_privilege",
                 "behavior": [{"field": "inference_profile_id", "equals": "glm-a"}],
                 "context": [{"field": "system_prompt", "contains": "GLM worker"}],
                 "tools": [{"field": "host.files.mode", "equals": "ReadWrite"}, {"field": "subagents.enabled", "equals": false}]}
            ]},
            "templates": [{"capture": "tasks", "key": "task_id", "id": "work-a", "fields": ["prompt_template"], "allowed": ["shard_id"]}],
            "receipt": {"capture": "mailbox", "fields": ["title", "payload"], "ids": ["worker-a", "work-a", "work-b"]}
        })
    }

    #[test]
    fn multiple_hops_require_the_connected_limit_and_the_reported_model() {
        let params = json!({"links":[
            {"capture":"behaviors","key":"behavior_id","id":"engineer","field":"inference_profile_id","target_capture":"profiles","target_key":"profile_id","reported_fields":["model_name"]},
            {"capture":"behaviors","key":"behavior_id","id":"engineer","field":"inference_profile_id","target_capture":"profiles","target_key":"profile_id","via":[{"field":"execution_id","target_capture":"executions","target_key":"execution_id"}],"expect":[{"field":"max_turns","equals":30}]}
        ]});
        assert!(
            jsonschema::validator_for(&CrewSpecMatch.describe().params_schema)
                .unwrap()
                .is_valid(&params)
        );
        let make = |execution: &str, report: &str| {
            let mut e = stage(&[
                (
                    "behaviors",
                    vec![json!({"behavior_id":"engineer","inference_profile_id":"chosen"})],
                ),
                (
                    "profiles",
                    vec![
                        json!({"profile_id":"chosen","model_name":"actual-model","execution_id":execution}),
                    ],
                ),
                (
                    "executions",
                    vec![
                        json!({"execution_id":"good","max_turns":30}),
                        json!({"execution_id":"wrong","max_turns":2}),
                    ],
                ),
            ]);
            e.messages = vec![crate::eval::runner::embedded::MessageEvidence {
                role: "assistant".into(),
                content: report.into(),
                created_at: None,
            }];
            e
        };
        assert_eq!(
            CrewSpecMatch
                .evaluate(&params, &make("good", "We use ACTUAL-MODEL."))
                .score_bp,
            Some(10000)
        );
        for (exec, report) in [
            ("wrong", "actual-model"),
            ("missing", "actual-model"),
            ("good", "another-model"),
        ] {
            let v = CrewSpecMatch.evaluate(&params, &make(exec, report));
            assert!(v.score_bp.unwrap() < 10000, "{}", v.raw);
        }
    }

    #[test]
    fn reverse_reference_hops_do_not_accept_a_disconnected_task() {
        let params = json!({"links":[{"capture":"sources","key":"source_collection","id":"Note","field":"event_source_id","target_capture":"triggers","target_key":"source.event_source_id","via":[{"field":"task_id","target_capture":"tasks","target_key":"task_id"}],"expect":[{"field":"behavior_id","equals":"reviewer"}]}]});
        for (bound, score) in [("wanted", 10000), ("decoy", 5000)] {
            let e = stage(&[
                (
                    "sources",
                    vec![json!({"source_collection":"Note","event_source_id":"watch"})],
                ),
                (
                    "triggers",
                    vec![json!({"source": "{\"event_source_id\":\"watch\"}","task_id":bound})],
                ),
                (
                    "tasks",
                    vec![
                        json!({"task_id":"wanted","behavior_id":"reviewer"}),
                        json!({"task_id":"decoy","behavior_id":"writer"}),
                    ],
                ),
            ]);
            assert_eq!(CrewSpecMatch.evaluate(&params, &e).score_bp, Some(score));
        }
    }

    #[test]
    fn completion_record_requires_a_write_path_for_its_metadata() {
        let params = json!({"agents":{"behaviors":"behaviors","contexts":"contexts","tools":"tools","expect":[{"behavior_id":"worker-a","datastore":{"surfaces":"surfaces","create":["Result"],"query":[],"required_fields":{"Result":["handoff_id"]}}}]}});
        assert!(
            jsonschema::validator_for(&CrewSpecMatch.describe().params_schema)
                .unwrap()
                .is_valid(&params)
        );
        for (field, met) in [
            (None, false),
            (Some(json!({"name":"handoff_id"})), false),
            (Some(json!({"name":"handoff_id","required":true})), true),
            (
                Some(json!({"name":"handoff_id","fill":"correlation"})),
                true,
            ),
        ] {
            let mut evidence = home();
            evidence.captures.insert("tools".into(), CaptureResult::Documents { rows: vec![json!({"tools_id":"t1","datastore":{"datastore_tool_surface_ids":["results"]}})] });
            let mut fields = vec![json!({"name":"text","required":true})];
            fields.extend(field);
            evidence.captures.insert("surfaces".into(), CaptureResult::Documents { rows: vec![json!({"surface_id":"results","agent_did":"did:x","enabled":true,"entries":[{"tool_name":"write_result","collection":"Result","description":"Record the result","fields":fields}]})] });
            let verdict = CrewSpecMatch.evaluate(&params, &evidence);
            assert_eq!(verdict.score_bp == Some(10000), met, "{}", verdict.raw);
            if !met {
                assert!(verdict
                    .feedback
                    .unwrap()
                    .contains("write_result must write handoff_id"));
            }
        }
    }

    #[test]
    fn multi_hop_feedback_names_the_field_owner() {
        let params = json!({"links":[{"capture":"sources","key":"source_collection","id":"Note","field":"event_source_id","target_capture":"triggers","target_key":"source.event_source_id","via":[{"field":"task_id","target_capture":"tasks","target_key":"task_id"}],"expect":[{"field":"emit_outcome","equals":true}]}]});
        let evidence = stage(&[
            (
                "sources",
                vec![json!({"source_collection":"Note","event_source_id":"notes"})],
            ),
            (
                "triggers",
                vec![json!({"source":{"event_source_id":"notes"},"task_id":"ack"})],
            ),
            ("tasks", vec![json!({"task_id":"ack","emit_outcome":false})]),
        ]);
        let feedback = CrewSpecMatch.evaluate(&params, &evidence).feedback.unwrap();
        assert!(
            feedback.contains("tasks \"ack\": emit_outcome"),
            "{feedback}"
        );
    }

    #[test]
    fn selected_profile_label_uses_its_id_only_when_the_name_is_null() {
        let params = json!({"links":[{"capture":"behaviors","key":"behavior_id","id":"engineer","field":"inference_profile_id","target_capture":"profiles","target_key":"profile_id","expect":[{"field":"display_name","fallback_field":"profile_id","equals":"Research"}]}]});
        for (profile, met) in [
            (json!({"profile_id":"Research"}), true),
            (json!({"profile_id":"Research","display_name":null}), true),
            (
                json!({"profile_id":"Research","display_name":"Wrong"}),
                false,
            ),
            (json!({"profile_id":"Research","display_name":""}), false),
        ] {
            let evidence = stage(&[
                (
                    "behaviors",
                    vec![json!({"behavior_id":"engineer","inference_profile_id":"Research"})],
                ),
                (
                    "profiles",
                    vec![
                        profile.clone(),
                        json!({"profile_id":"unused","display_name":"Research"}),
                    ],
                ),
            ]);
            let verdict = CrewSpecMatch.evaluate(&params, &evidence);
            assert_eq!(
                verdict.score_bp == Some(10000),
                met,
                "{profile}: {}",
                verdict.raw
            );
        }
    }

    #[test]
    fn selected_datastore_contract_rejects_runtime_keys_decoys_and_missing_calls() {
        let params = json!({"agents":{"behaviors":"behaviors","contexts":"contexts","tools":"tools","expect":[{"behavior_id":"worker-a","datastore":{"surfaces":"surfaces","create":["Result"],"query":["Result"],"caller_fields":{"Result":["correlation","result"]},"query_fields":{"Result":["correlation","result"]},"lookup_by":"correlation","called_create":["Result"],"called_query":["Result"]}}]}});
        let mut e = home();
        let tool =
            json!({"tools_id":"t1","datastore":{"datastore_tool_surface_ids":["arbitrary-id"]}});
        let surface = json!({"_docID":"physical-surface","surface_id":"arbitrary-id","agent_did":"did:x","entries":[{"kind":"create","tool_name":"save_result","collection":"Result","description":"Save the caller's result","fields":[{"name":"correlation","required":true},{"name":"result","required":true}]},{"kind":"query","tool_name":"find_result","collection":"Result","description":"Find results by the caller's key","fields":["correlation","result"],"filter_fields":[{"name":"correlation"}]}]});
        e.captures.insert(
            "tools".into(),
            CaptureResult::Documents {
                rows: vec![tool.clone()],
            },
        );
        e.captures.insert(
            "surfaces".into(),
            CaptureResult::Documents {
                rows: vec![surface.clone()],
            },
        );
        e.tool_calls = ["save_result", "find_result"]
            .into_iter()
            .map(|name| crate::eval::runner::embedded::ToolCallEvidence {
                tool_name: name.into(),
                status: Some("completed".into()),
                lifecycle_state: Some("completed".into()),
                tool_failure_class: None,
                started_at: None,
                completed_at: None,
                args: Value::Null,
                result: Value::Null,
            })
            .collect();
        assert!(
            jsonschema::validator_for(&CrewSpecMatch.describe().params_schema)
                .unwrap()
                .is_valid(&params)
        );
        let good = CrewSpecMatch.evaluate(&params, &e);
        assert_eq!(good.score_bp, Some(10000), "{}", good.raw);
        for mutation in 0..6 {
            let mut bad = e.clone();
            match mutation {
                0 => {
                    let mut s = surface.clone();
                    s["entries"][0]["fields"][0] =
                        json!({"name":"correlation","fill":"correlation"});
                    bad.captures.insert(
                        "surfaces".into(),
                        CaptureResult::Documents { rows: vec![s] },
                    );
                }
                1 => {
                    let mut t = tool.clone();
                    t["datastore"]["datastore_tool_surface_ids"] = json!(["unselected"]);
                    bad.captures
                        .insert("tools".into(), CaptureResult::Documents { rows: vec![t] });
                }
                2 => {
                    bad.tool_calls.pop();
                }
                3 => {
                    let mut s = surface.clone();
                    s["entries"][1]["filter_fields"] =
                        json!([{"name":"correlation"},{"name":"result","required":true}]);
                    bad.captures.insert(
                        "surfaces".into(),
                        CaptureResult::Documents { rows: vec![s] },
                    );
                }
                4 => {
                    bad.tool_calls[0].status = Some("failed".into());
                }
                _ => {
                    let mut s = surface.clone();
                    s["entries"][1]["fields"] = json!(["correlation"]);
                    bad.captures.insert(
                        "surfaces".into(),
                        CaptureResult::Documents { rows: vec![s] },
                    );
                }
            }
            let v = CrewSpecMatch.evaluate(&params, &bad);
            if mutation == 3 {
                assert!(v
                    .feedback
                    .as_deref()
                    .unwrap()
                    .contains("find_result query must accept caller-supplied correlation alone"));
            }
            assert!(
                v.score_bp.unwrap() < 10000,
                "mutation {mutation}: {}",
                v.raw
            );
        }
    }

    #[test]
    fn named_roles_follow_actual_ids_and_reject_ambiguous_or_wrong_grants() {
        let params = json!({"agents":{"behaviors":"behaviors","contexts":"contexts","tools":"tools","expect":[
            {"behavior_id":"coordinator","source_match":{"field":"display_name","matches":"(?i)\\bcoordinator\\b"},"delegates":{"targets":"targets","behaviors":["reviewer"]}},
            {"behavior_id":"reviewer","source_match":{"field":"display_name","matches":"(?i)\\breviewer\\b"},"tools":[{"field":"host.bash.mode","equals":"Off"}]}
        ]}});
        assert!(
            jsonschema::validator_for(&CrewSpecMatch.describe().params_schema)
                .unwrap()
                .is_valid(&params)
        );
        let good = stage(&[
            (
                "behaviors",
                vec![
                    json!({"behavior_id":"did:x:desk-coordinator","display_name":"Research desk Coordinator","context_id":"c"}),
                    json!({"behavior_id":"did:x:random-42","display_name":"Research desk Reviewer","context_id":"r"}),
                ],
            ),
            (
                "contexts",
                vec![
                    json!({"context_id":"c","tools_id":"ct"}),
                    json!({"context_id":"r","tools_id":"rt"}),
                ],
            ),
            (
                "tools",
                vec![
                    json!({"tools_id":"ct","subagents":{"enabled":true,"target_ids":["review-route"]}}),
                    json!({"tools_id":"rt","host":{"bash":{"mode":"Off"}}}),
                ],
            ),
            (
                "targets",
                vec![
                    json!({"target_id":"review-route","agent_did":"did:x","target_agent_did":"did:x","behavior_id":"did:x:random-42"}),
                ],
            ),
        ]);
        assert_eq!(CrewSpecMatch.evaluate(&params, &good).score_bp, Some(10000));
        for mutation in 0..7 {
            let mut bad = good.clone();
            let capture = match mutation {
                0..=2 => "targets",
                3..=5 => "behaviors",
                _ => "tools",
            };
            let CaptureResult::Documents { rows } = bad.captures.get_mut(capture).unwrap() else {
                unreachable!()
            };
            match mutation {
                0 => rows[0]["behavior_id"] = json!("reviewer"),
                1 => rows[0]["target_agent_did"] = json!("did:foreign"),
                2 => rows[0]["behavior_id"] = json!("did:x:desk-coordinator"),
                3 => rows.push(
                    json!({"behavior_id":"another","display_name":"Reviewer","context_id":"r"}),
                ),
                4 => {
                    rows.pop();
                }
                5 => {
                    rows.pop();
                    rows[0]["display_name"] = json!("Coordinator Reviewer");
                }
                _ => rows[1]["host"]["bash"]["mode"] = json!("On"),
            }
            let result = CrewSpecMatch.evaluate(&params, &bad);
            assert!(
                result.score_bp.unwrap() < 10000,
                "mutation {mutation}: {}",
                result.raw
            );
        }
    }

    #[test]
    fn delegates_follow_selected_targets_and_reject_foreign_or_extra_grants() {
        let params = json!({"agents":{"behaviors":"behaviors","contexts":"contexts","tools":"tools","expect":[{"behavior_id":"worker-a","delegates":{"targets":"targets","behaviors":["reviewer"]}}]}});
        for (selected, owner, score) in [
            ("right", "did:x", 10000),
            ("wrong", "did:x", 5000),
            ("right", "did:other", 5000),
        ] {
            let mut e = home();
            e.captures.insert("tools".into(),CaptureResult::Documents{rows:vec![json!({"tools_id":"t1","subagents":{"enabled":true,"target_ids":[selected]}})]});
            e.captures.insert("targets".into(),CaptureResult::Documents{rows:vec![json!({"target_id":"right","agent_did":"did:x","target_agent_did":owner,"behavior_id":"scope:reviewer"}),json!({"target_id":"wrong","agent_did":"did:x","target_agent_did":"did:x","behavior_id":"scope:writer"})]});
            assert_eq!(CrewSpecMatch.evaluate(&params, &e).score_bp, Some(score));
        }
    }

    #[test]
    fn links_check_the_selected_source_and_fail_missing_or_wrong_references() {
        let params = json!({"links": [{
            "capture": "triggers", "key": "trigger_id", "id": "dispatch",
            "field": "source.event_source_id", "target_capture": "sources", "target_key": "event_source_id",
            "expect": [{"field": "source_collection", "equals": "Assignment"}]
        }]});
        let schema = CrewSpecMatch.describe().params_schema;
        assert!(jsonschema::validator_for(&schema)
            .unwrap()
            .is_valid(&params));
        for (source, expected) in [("wanted", 10000), ("decoy", 5000), ("missing", 0)] {
            let evidence = stage(&[
                (
                    "triggers",
                    vec![
                        json!({"trigger_id":"dispatch", "source": json!({"kind":"event", "event_source_id":source}).to_string()}),
                    ],
                ),
                (
                    "sources",
                    vec![
                        json!({"event_source_id":"wanted", "source_collection":"Assignment"}),
                        json!({"event_source_id":"decoy", "source_collection":"Result"}),
                    ],
                ),
            ]);
            let verdict = CrewSpecMatch.evaluate(&params, &evidence);
            assert_eq!(verdict.score_bp, Some(expected), "{}", verdict.raw);
        }
    }

    #[test]
    fn source_match_follows_one_role_and_rejects_ambiguity_and_decoys() {
        let params = json!({"links":[{"capture":"behaviors","source_match":{"field":"display_name","matches":"(?i)^(code )?reviewer$"},"field":"inference_profile_id","target_capture":"profiles","target_key":"profile_id","expect":[{"field":"max_turns","equals":30}]}]});
        assert!(
            jsonschema::validator_for(&CrewSpecMatch.describe().params_schema)
                .unwrap()
                .is_valid(&params)
        );
        for (name, selected, duplicate, expected) in [
            ("Code Reviewer", "right", false, 10000),
            ("Reviewer", "wrong", false, 5000),
            ("Writer", "right", false, 0),
            ("Reviewer", "right", true, 0),
        ] {
            let mut behaviors = vec![
                json!({"behavior_id":"unpredictable-id","display_name":name,"inference_profile_id":selected}),
            ];
            if duplicate {
                behaviors.push(behaviors[0].clone());
            }
            let evidence = stage(&[
                ("behaviors", behaviors),
                (
                    "profiles",
                    vec![
                        json!({"profile_id":"right","max_turns":30}),
                        json!({"profile_id":"wrong","max_turns":100}),
                    ],
                ),
            ]);
            assert_eq!(
                CrewSpecMatch.evaluate(&params, &evidence).score_bp,
                Some(expected)
            );
        }
    }

    #[test]
    fn null_tools_selection_has_no_grants_but_a_dangling_reference_fails() {
        let params = json!({"agents":{"behaviors":"behaviors","contexts":"contexts","tools":"tools","expect":[{"behavior_id":"planner","tools":[{"field":"host.bash.mode","matches":"^(Off|null)$"},{"field":"subagents.enabled","matches":"^(false|null)$"}]}]}});
        for (context, expected) in [
            (json!({"context_id":"c","tools_id":null}), 10000),
            (json!({"context_id":"c","tools_id":"missing"}), 3333),
            (json!({"context_id":"c"}), 3333),
        ] {
            let evidence = stage(&[
                (
                    "behaviors",
                    vec![json!({"behavior_id":"planner","context_id":"c"})],
                ),
                ("contexts", vec![context]),
                ("tools", vec![]),
            ]);
            assert_eq!(
                CrewSpecMatch.evaluate(&params, &evidence).score_bp,
                Some(expected)
            );
        }
    }

    #[test]
    fn the_score_is_the_satisfied_fraction_broken_down_by_category() {
        let verdict = CrewSpecMatch.evaluate(&params(), &home());
        assert_eq!(
            verdict.kind,
            OutcomeKind::ModelAcceptance,
            "{}",
            verdict.raw
        );
        let categories = &verdict.raw["categories"];
        assert_eq!(
            categories["collections"],
            json!({"satisfied": 1, "total": 2})
        );
        assert_eq!(
            categories["emit_outcome"],
            json!({"satisfied": 2, "total": 2})
        );
        assert_eq!(
            categories["least_privilege"],
            json!({"satisfied": 5, "total": 5})
        );
        assert_eq!(categories["templates"], json!({"satisfied": 0, "total": 1}));
        assert_eq!(categories["receipt"], json!({"satisfied": 2, "total": 3}));
        // 10 of 13.
        assert_eq!(verdict.score_bp, Some(7_692));
        let feedback = verdict.feedback.unwrap();
        assert!(feedback.contains("doc.owner"), "{feedback}");
        assert!(
            feedback.contains("capture c_result has None rows"),
            "{feedback}"
        );
        assert_eq!(verdict.raw["activity"]["tool_calls"], 0);
    }

    #[test]
    fn a_missing_behavior_fails_its_whole_chain() {
        let mut evidence = home();
        evidence.captures.insert(
            "behaviors".into(),
            CaptureResult::Documents { rows: vec![] },
        );
        let verdict = CrewSpecMatch.evaluate(&params(), &evidence);
        assert_eq!(
            verdict.raw["categories"]["least_privilege"],
            json!({"satisfied": 0, "total": 5})
        );
    }

    #[test]
    fn a_principal_scoped_behavior_id_matches_its_declared_id() {
        let mut evidence = home();
        evidence.captures.insert(
            "behaviors".into(),
            CaptureResult::Documents {
                rows: vec![json!({"behavior_id": "did:key:z6Mk:worker-a", "context_id": "c1", "inference_profile_id": "glm-a"})],
            },
        );
        let verdict = CrewSpecMatch.evaluate(&params(), &evidence);
        assert_eq!(
            verdict.raw["categories"]["least_privilege"],
            json!({"satisfied": 5, "total": 5})
        );
        assert!(is_id("did:key:z6Mk:worker-a", "worker-a"));
        assert!(!is_id("did:key:z6Mk:big-worker-a", "worker-a"));
        assert!(!is_id("worker-ab", "worker-a"));
    }

    #[test]
    fn each_prod_needed_costs_one_continuation_requirement() {
        let mut evidence = home();
        evidence.prods = 2;
        let verdict = CrewSpecMatch.evaluate(&json!({"continuation": {"max": 5}}), &evidence);
        assert_eq!(
            verdict.raw["categories"]["continuation"],
            json!({"satisfied": 3, "total": 5})
        );
        assert_eq!(verdict.raw["activity"]["prods"], 2);
    }

    #[test]
    fn an_absent_field_reads_as_null() {
        let row = json!({"subagents": {"target_ids": []}});
        assert_eq!(lookup(&row, "subagents.enabled"), Value::Null);
        assert_eq!(lookup(&row, "host.files.mode"), Value::Null);
    }

    #[test]
    fn doc_fields_reads_each_name_once_and_ignores_other_roots() {
        assert_eq!(
            doc_fields(
                "{{ doc.a }} {{doc.b_c}} {{ doc.a }} {{ session.session_id }} {{ mydoc.x }}"
            ),
            vec!["a".to_string(), "b_c".to_string()]
        );
    }

    #[test]
    fn params_that_require_nothing_are_the_grader() {
        let verdict = CrewSpecMatch.evaluate(&json!({}), &home());
        assert_eq!(verdict.kind, OutcomeKind::Grader);
    }
}
