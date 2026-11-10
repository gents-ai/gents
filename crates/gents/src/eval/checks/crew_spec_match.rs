mod agents;
mod documents;
mod lookup;
mod params;
mod rows;
mod tally;

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::eval::checks::{
    bounded, graded, graded_reason_codes, grader, Check, CheckDescription, CheckVerdict,
};
use crate::eval::runner::executor::{CaptureResult, StageEvidence};
use agents::check_agents;
use documents::{check_receipt, check_templates};
use lookup::{doc_fields, find, is_id, lookup};
use params::Params;
use rows::{check_presence, check_rows_and_links};
use tally::Tally;

pub struct CrewSpecMatch;

impl Check for CrewSpecMatch {
    fn name(&self) -> &'static str {
        "crew_spec_match"
    }

    fn version(&self) -> &'static str {
        "10"
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
                            "exact": {"type": "boolean"},
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
                                    "agent_did": {"type": "string"}, "category": category,
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
                                "required": ["agent_did"], "additionalProperties": false
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
        let mut tally = Tally::default();

        if let Err(verdict) = check_presence(&params.present, stage, &mut tally) {
            return verdict;
        }
        if let Err(verdict) = check_rows_and_links(params.rows, params.links, stage, &mut tally) {
            return verdict;
        }
        if let Some(agent_spec) = params.agents {
            if let Err(verdict) = check_agents(agent_spec, stage, &mut tally) {
                return verdict;
            }
        }
        check_templates(&params.templates, stage, &mut tally);
        if let Some(receipt) = &params.receipt {
            check_receipt(receipt, stage, &mut tally);
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

fn find_link<'a>(rows: &'a [Value], key: &str, id: &str, exact: bool) -> Option<&'a Value> {
    if exact {
        rows.iter()
            .find(|row| lookup(row, key).as_str() == Some(id))
    } else {
        find(rows, key, id)
    }
}
fn capture_rows<'a>(stage: &'a StageEvidence, name: &str) -> Option<&'a [Value]> {
    match stage.captures.get(name) {
        Some(CaptureResult::Documents { rows }) => Some(rows.as_slice()),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
