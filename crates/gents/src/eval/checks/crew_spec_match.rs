use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::eval::checks::captured_fields_match::{test, Expectation, FieldPath, Test};
use crate::eval::checks::{
    bounded, excerpt, graded, graded_reason_codes, grader, Check, CheckDescription, CheckVerdict,
    EXCERPT_CHARS,
};
use crate::eval::runner::executor::{CaptureResult, StageEvidence};

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
    agents: String,
    contexts: String,
    tools: String,
    expect: Vec<AgentSpec>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentSpec {
    agent_id: String,
    #[serde(default)]
    source_match: Option<Expectation>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    agent: Vec<Expectation>,
    #[serde(default)]
    context: Vec<Expectation>,
    #[serde(default)]
    tools: Vec<Expectation>,
    #[serde(default)]
    datastore: Option<DatastoreSpec>,
    #[serde(default)]
    delegates: Option<DelegateSpec>,
}

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
    agents: BTreeSet<String>,
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
        "8"
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
            summary: "Scores the fraction of a declared configuration's requirements that the captured home holds: captures present, keyed rows and their fields, each agent's Context and Tools, template fields against each source collection, and ids named in a receipt. raw.categories breaks the score down; raw.activity counts tool and inference calls.".into(),
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
                            "agents": {"type": "string"}, "contexts": {"type": "string"},
                            "tools": {"type": "string"},
                            "expect": {"type": "array", "items": {
                                "type": "object",
                                "properties": {
                                    "agent_id": {"type": "string"}, "category": category,
                                    "source_match": expectation,
                                    "agent": expectations, "context": expectations,
                                    "tools": expectations,
                                    "datastore": {"type":"object", "properties": {
                                        "surfaces":{"type":"string"}, "create":strings, "query":strings, "caller_fields":{"type":"object", "additionalProperties":strings}, "required_fields":{"type":"object", "additionalProperties":strings}, "query_fields":{"type":"object", "additionalProperties":strings}, "lookup_by":{"type":"string"}, "called_create":strings, "called_query":strings
                                    }, "required":["surfaces","create","query"], "additionalProperties":false},
                                    "delegates": {"type":"object", "properties": {
                                        "targets":{"type":"string"}, "agents":strings
                                    }, "required":["targets","agents"], "additionalProperties":false}
                                },
                                "required": ["agent_id"], "additionalProperties": false
                            }}
                        },
                        "required": ["agents", "contexts", "tools", "expect"],
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
        if let Some(agent_spec) = params.agents {
            let agents = rows(&agent_spec.agents).unwrap_or_default();
            let contexts = rows(&agent_spec.contexts).unwrap_or_default();
            let tools = rows(&agent_spec.tools).unwrap_or_default();
            let mut roles = BTreeMap::new();
            let mut selected = Vec::new();
            for mut spec in agent_spec.expect {
                let agent = if let Some(selector) = spec.source_match.take() {
                    let (field, matcher) = match test(selector) {
                        Ok(test) => test,
                        Err(detail) => return grader("bad_params", detail),
                    };
                    let mut matches = agents.iter().filter(|row| {
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
                        .and_then(|row| row.get("agent_id"))
                        .and_then(Value::as_str);
                    if roles.insert(spec.agent_id.clone(), id).is_some() {
                        return grader("bad_params", "duplicate named agent role");
                    }
                    unique
                } else {
                    find(agents, "agent_id", &spec.agent_id)
                };
                selected.push((spec, agent));
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
            for (spec, agent) in selected {
                let agent = agent.filter(|_| roles.get(&spec.agent_id).is_none_or(Option::is_some));
                let category = spec.category.as_deref();
                let (agent_tests, context_tests, tools_tests) =
                    match (tests(spec.agent), tests(spec.context), tests(spec.tools)) {
                        (Ok(b), Ok(c), Ok(t)) => (b, c, t),
                        (Err(detail), _, _) | (_, Err(detail), _) | (_, _, Err(detail)) => {
                            return grader("bad_params", detail)
                        }
                    };
                let context = agent
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
                let id = &spec.agent_id;
                tally.record(category, agent.is_some(), || format!("agent {id} absent"));
                tally.expectations(category, &format!("agent {id}"), agent, &agent_tests);
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
                    let selected = tools_row.map(|t| lookup(t, "agents.target_ids"));
                    let enabled =
                        tools_row.map(|t| lookup(t, "agents.enabled")) == Some(json!(true));
                    let mut actual = BTreeSet::new();
                    let mut valid = enabled
                        && expected
                            .agents
                            .iter()
                            .all(|name| roles.get(name).is_none_or(Option::is_some));
                    if let Some(Value::Array(ids)) = selected {
                        for reference in ids {
                            let target = reference.as_str().and_then(|reference| {
                                rows(&expected.targets)
                                    .and_then(|rows| find(rows, "target_id", reference))
                            });
                            valid &= target.is_some_and(|t| {
                                t.get("node_did").and_then(Value::as_str).is_some()
                                    && t.get("target_node_did") == t.get("node_did")
                            });
                            if let Some(agent) = target
                                .and_then(|t| t.get("agent_id"))
                                .and_then(Value::as_str)
                            {
                                if let Some(name) =
                                    expected.agents.iter().find(|name| match roles.get(*name) {
                                        Some(Some(id)) => agent == *id,
                                        Some(None) => false,
                                        None => is_id(agent, name),
                                    })
                                {
                                    actual.insert(name.clone());
                                } else {
                                    valid = false;
                                    actual.insert(agent.to_string());
                                }
                            } else {
                                valid = false;
                            }
                        }
                    } else {
                        valid = false;
                    }
                    tally.record(category, valid && actual == expected.agents, || {
                        format!("{id} selected delegates {actual:?}; expected {:?}; references valid: {valid}", expected.agents)
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
mod tests;
