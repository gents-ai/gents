use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use super::params::{Agents, DatastoreSpec, DelegateSpec};
use super::tally::{tests, Tally};
use super::{capture_rows, grader, CheckVerdict, StageEvidence};
use super::{find, is_id, lookup};
use crate::checks::captured_fields_match::test;

pub(super) fn check_agents(
    agent_spec: Agents,
    stage: &StageEvidence,
    tally: &mut Tally,
) -> Result<(), CheckVerdict> {
    let rows = |name: &str| capture_rows(stage, name);
    let agents = rows(&agent_spec.agents).unwrap_or_default();
    let contexts = rows(&agent_spec.contexts).unwrap_or_default();
    let tools = rows(&agent_spec.tools).unwrap_or_default();
    let mut roles = BTreeMap::new();
    let mut selected = Vec::new();
    for mut spec in agent_spec.expect {
        let agent = if let Some(selector) = spec.source_match.take() {
            let (field, matcher) = match test(selector) {
                Ok(test) => test,
                Err(detail) => return Err(grader("bad_params", detail)),
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
                .and_then(|row| row.get("agent_did"))
                .and_then(Value::as_str);
            if roles.insert(spec.agent_did.clone(), id).is_some() {
                return Err(grader("bad_params", "duplicate named agent role"));
            }
            unique
        } else {
            find(agents, "agent_did", &spec.agent_did)
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
        let agent = agent.filter(|_| roles.get(&spec.agent_did).is_none_or(Option::is_some));
        let category = spec.category.as_deref();
        let (agent_tests, context_tests, tools_tests) =
            match (tests(spec.agent), tests(spec.context), tests(spec.tools)) {
                (Ok(b), Ok(c), Ok(t)) => (b, c, t),
                (Err(detail), _, _) | (_, Err(detail), _) | (_, _, Err(detail)) => {
                    return Err(grader("bad_params", detail))
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
        let id = &spec.agent_did;
        tally.record(category, agent.is_some(), || format!("agent {id} absent"));
        tally.expectations(category, &format!("agent {id}"), agent, &agent_tests);
        tally.expectations(category, &format!("{id} context"), context, &context_tests);
        tally.tools_expectations(category, &format!("{id} tools"), tools_row, &tools_tests);
        if let Some(expected) = spec.datastore {
            check_datastore_grant(expected, context, tools_row, stage, category, id, tally);
        }
        if let Some(expected) = spec.delegates {
            check_delegate_grant(expected, tools_row, &roles, stage, category, id, tally);
        }
    }
    Ok(())
}

fn check_datastore_grant(
    expected: DatastoreSpec,
    context: Option<&Value>,
    tools_row: Option<&Value>,
    stage: &StageEvidence,
    category: Option<&str>,
    id: &str,
    tally: &mut Tally,
) {
    let rows = |name: &str| capture_rows(stage, name);
    use gents_documents::{DatastoreToolSurfaceDocument, SurfaceToolDecl};
    let selected = tools_row.map(|t| lookup(t, "datastore.datastore_tool_surface_ids"));
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
                    rows(&expected.surfaces).and_then(|rows| find(rows, "surface_id", reference))
                })
                .and_then(|row| {
                    let mut document = row.clone();
                    document.as_object_mut()?.remove("_docID");
                    serde_json::from_value::<DatastoreToolSurfaceDocument>(document).ok()
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
                            if let Some(fields) = expected.required_fields.get(&d.collection) {
                                for name in fields {
                                    if !d.fields.iter().any(|f| {
                                        &f.name == name && (f.required || f.fill.is_some())
                                    }) {
                                        valid = false;
                                        contract_issues.push(format!("{} must write {name}: declare a required caller field or a runtime fill", d.tool_name));
                                    }
                                }
                            }
                            if let Some(fields) = expected.caller_fields.get(&d.collection) {
                                let complete = fields.iter().all(|name| {
                                    d.fields
                                        .iter()
                                        .any(|f| &f.name == name && f.required && f.fill.is_none())
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
                            if let Some(fields) = expected.query_fields.get(&d.collection) {
                                let complete = fields.iter().all(|field| d.fields.contains(field));
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
                                    && d.filter_fields
                                        .iter()
                                        .any(|f| &f.name == key && f.fill.is_none())
                                    && d.filter_fields.iter().all(|f| {
                                        f.fill.is_none() && (!f.required || &f.name == key)
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

fn check_delegate_grant(
    expected: DelegateSpec,
    tools_row: Option<&Value>,
    roles: &BTreeMap<String, Option<&str>>,
    stage: &StageEvidence,
    category: Option<&str>,
    id: &str,
    tally: &mut Tally,
) {
    let rows = |name: &str| capture_rows(stage, name);
    let selected = tools_row.map(|t| lookup(t, "agents.target_ids"));
    let enabled = tools_row.map(|t| lookup(t, "agents.enabled")) == Some(json!(true));
    let mut actual = BTreeSet::new();
    let mut valid = enabled
        && expected
            .agents
            .iter()
            .all(|name| roles.get(name).is_none_or(Option::is_some));
    if let Some(Value::Array(ids)) = selected {
        for reference in ids {
            let target = reference.as_str().and_then(|reference| {
                rows(&expected.targets).and_then(|rows| find(rows, "target_id", reference))
            });
            valid &= target.is_some_and(|t| {
                t.get("node_did").and_then(Value::as_str).is_some()
                    && t.get("target_node_did") == t.get("node_did")
            });
            if let Some(agent) = target
                .and_then(|t| t.get("agent_did"))
                .and_then(Value::as_str)
            {
                if let Some(name) = expected.agents.iter().find(|name| match roles.get(*name) {
                    Some(Some(id)) => agent == *id,
                    Some(None) => false,
                    None => is_id(agent, name),
                }) {
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
        format!(
            "{id} selected delegates {actual:?}; expected {:?}; references valid: {valid}",
            expected.agents
        )
    });
}
