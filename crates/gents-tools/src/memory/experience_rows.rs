use serde_json::Value;

use super::experience::{score_admitted, AttemptRow, OutcomeRow, ProblemRow};
use super::{created_at_millis, MAX_ALIAS_ROWS};

pub(super) struct CallerContext {
    pub(super) request_id: Option<String>,
    pub(super) session_id: Option<String>,
    pub(super) executed_model_name: Option<String>,
    pub(super) provenance: String,
}

#[derive(Clone)]
pub(super) struct ProblemRecord {
    pub(super) problem_key: String,
    pub(super) name: String,
    pub(super) carried_from: Option<String>,
    pub(super) created_at: String,
}

impl ProblemRecord {
    pub(super) fn row(&self) -> ProblemRow {
        ProblemRow {
            problem_key: self.problem_key.clone(),
            name: self.name.clone(),
            carried_from: self.carried_from.clone(),
            created_at: created_at_millis(&self.created_at),
        }
    }
}

pub(super) fn parse_aliases(value: &Value) -> anyhow::Result<Vec<(String, String)>> {
    let rows = value
        .pointer("/data/AgentMemoryAlias")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if rows.len() > MAX_ALIAS_ROWS {
        anyhow::bail!(
            "the agent has more than {MAX_ALIAS_ROWS} problem aliases, so key resolution is not knowable; compact the alias rows before reading experience"
        );
    }
    Ok(rows
        .iter()
        .filter_map(|row| {
            Some((
                row.get("from_problem_key")?.as_str()?.to_owned(),
                row.get("to_problem_key")?.as_str()?.to_owned(),
            ))
        })
        .collect())
}

pub(super) fn parse_problems(value: &Value) -> Vec<ProblemRecord> {
    value
        .pointer("/data/AgentMemoryProblem")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some(ProblemRecord {
                        problem_key: row.get("problem_key")?.as_str()?.to_owned(),
                        name: row.get("name")?.as_str()?.to_owned(),
                        carried_from: row
                            .get("carried_from")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        created_at: row
                            .get("created_at")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn parse_attempts(value: &Value) -> Vec<AttemptRow> {
    value
        .pointer("/data/AgentMemoryAttempt")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some(AttemptRow {
                        attempt_key: row.get("attempt_key")?.as_str()?.to_owned(),
                        problem_key: row
                            .get("problem_key")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        predicted: parse_string_list(row.get("predicted")),
                        accepted: row.get("accepted").and_then(Value::as_bool).unwrap_or(true),
                        executed_model_name: row
                            .pointer("/provenance/executed_model_name")
                            .and_then(Value::as_str)
                            .filter(|model| !model.is_empty())
                            .map(str::to_owned),
                        carried_from: row
                            .get("carried_from")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        created_at: row
                            .get("created_at")
                            .and_then(Value::as_str)
                            .map(created_at_millis)
                            .unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn parse_outcomes(value: &Value) -> Vec<OutcomeRow> {
    value
        .pointer("/data/AgentMemoryOutcome")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some(OutcomeRow {
                        outcome_key: row.get("outcome_key")?.as_str()?.to_owned(),
                        attempt_key: row.get("attempt_key")?.as_str()?.to_owned(),
                        fixed: parse_string_list(row.get("fixed")),
                        missed: parse_string_list(row.get("missed")),
                        score: score_admitted(
                            row.get("passed").and_then(Value::as_i64),
                            row.get("total").and_then(Value::as_i64),
                        )
                        .score,
                        executed_model_name: row
                            .pointer("/provenance/executed_model_name")
                            .and_then(Value::as_str)
                            .filter(|model| !model.is_empty())
                            .map(str::to_owned),
                        carried_from: row
                            .get("carried_from")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        created_at: row
                            .get("created_at")
                            .and_then(Value::as_str)
                            .map(created_at_millis)
                            .unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn string_list(items: &[String]) -> String {
    format!(
        "[{}]",
        items
            .iter()
            .map(|item| format!("\"{}\"", gents_store::graphql::escape_graphql_string(item)))
            .collect::<Vec<_>>()
            .join(",")
    )
}

pub(crate) const PROBLEM_COLLECTION: &str = gents_protocol::schemas::AGENT_MEMORY_PROBLEM_NAME;
pub(super) const ATTEMPT_COLLECTION: &str = gents_protocol::schemas::AGENT_MEMORY_ATTEMPT_NAME;
pub(super) const OUTCOME_COLLECTION: &str = gents_protocol::schemas::AGENT_MEMORY_OUTCOME_NAME;
pub(super) const ALIAS_COLLECTION: &str = gents_protocol::schemas::AGENT_MEMORY_ALIAS_NAME;
