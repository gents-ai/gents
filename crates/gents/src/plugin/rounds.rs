use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use serde_json::{Map, Value};

use super::{
    data, http_calls, job_calls, model_calls, tool_calls, PluginBudget, PluginOutcome,
    PluginVerdict,
};

pub const MAX_ROUNDS: u32 = 64;
pub const FINAL_ROUND_RESERVE_DIVISOR: u32 = 8;

pub(super) type Round = Arc<dyn Fn(Value, PluginBudget) -> Result<PluginOutcome> + Send + Sync>;

#[derive(Default)]
pub(super) struct HostCalls {
    pub(super) model: Option<model_calls::Session>,
    pub(super) http: Option<http_calls::Session>,
    pub(super) tools: Option<tool_calls::Session>,
    pub(super) data: Option<data::Session>,
    pub(super) jobs: Option<job_calls::Session>,
}

impl HostCalls {
    pub(super) fn is_empty(&self) -> bool {
        self.model.is_none()
            && self.http.is_none()
            && self.tools.is_none()
            && self.data.is_none()
            && self.jobs.is_none()
    }

    pub(super) fn prepare_input(&self, input: Value) -> Result<Value> {
        let mut base = match input {
            Value::Null => Map::new(),
            Value::Object(object) => object,
            _ => {
                anyhow::bail!("a plugin the host serves requests for takes a JSON object as input")
            }
        };
        // A job step's `state` is the host's stored guest state, which every
        // round answer carries unless the guest's own `state` replaces it.
        if !self.jobs.as_ref().is_some_and(job_calls::Session::is_step) {
            base.remove("state");
        }
        if self.model.is_some() {
            base.remove("model_results");
            base.insert("model_calls".to_owned(), Value::Bool(true));
        }
        if self.http.is_some() {
            base.remove("http_results");
            base.insert("http_calls".to_owned(), Value::Bool(true));
        }
        if self.tools.is_some() {
            base.remove("tool_results");
            base.insert("tool_calls".to_owned(), Value::Bool(true));
        }
        if self.data.is_some() {
            base.remove("data_results");
            base.insert("data_calls".to_owned(), Value::Bool(true));
        }
        if self.jobs.is_some() {
            base.remove("job_results");
            base.insert("job_calls".to_owned(), Value::Bool(true));
        }
        Ok(Value::Object(base))
    }
}

enum Asked {
    Model(model_calls::Batch),
    Http(http_calls::Batch),
    Tools(tool_calls::Batch),
    Data(data::Batch),
    Job(job_calls::Batch),
}

fn refused(
    started: Instant,
    now: Instant,
    fuel: u64,
    verdict: PluginVerdict,
    why: String,
) -> PluginOutcome {
    PluginOutcome {
        verdict,
        output: Value::Null,
        diagnostics: why,
        fuel_used: fuel,
        wall_ms: elapsed_ms(started, now),
    }
}

fn asked(calls: &HostCalls, output: &Value) -> Result<Option<Asked>, String> {
    let model = match calls.model {
        Some(_) => model_calls::parse_batch(output)?,
        None => None,
    };
    let http = match &calls.http {
        Some(_) => http_calls::parse_batch(output)?,
        None => None,
    };
    let tools = match calls.tools {
        Some(_) => tool_calls::parse_batch(output)?,
        None => None,
    };
    let data = match calls.data {
        Some(_) => data::parse_batch(output)?,
        None => None,
    };
    let jobs = match calls.jobs {
        Some(_) => job_calls::parse_batch(output)?,
        None => None,
    };
    let mut asks = Vec::new();
    asks.extend(model.map(Asked::Model));
    asks.extend(http.map(Asked::Http));
    asks.extend(tools.map(Asked::Tools));
    asks.extend(data.map(Asked::Data));
    asks.extend(jobs.map(Asked::Job));
    match asks.len() {
        0 | 1 => Ok(asks.pop()),
        _ => Err(
            "one round asks for exactly one of model_calls, http_calls, tool_calls, data_calls or job_calls"
                .to_owned(),
        ),
    }
}

pub(super) async fn drive(
    calls: HostCalls,
    input: Value,
    budget: PluginBudget,
    round: Round,
) -> Result<PluginOutcome> {
    drive_with_clock(calls, input, budget, round, Instant::now).await
}

/// Clock observations must be monotonic; the production caller supplies `Instant::now`.
pub(super) async fn drive_with_clock<C>(
    mut calls: HostCalls,
    input: Value,
    budget: PluginBudget,
    round: Round,
    now: C,
) -> Result<PluginOutcome>
where
    C: Fn() -> Instant + Send + Sync,
{
    let started = now();
    let mut deadline = started + budget.wall_clock;
    let reserve = budget.wall_clock / FINAL_ROUND_RESERVE_DIVISOR;
    let Value::Object(base) = calls.prepare_input(input)? else {
        anyhow::bail!("a driven plugin requires host services");
    };
    let mut next = Value::Object(base.clone());
    let mut fuel = 0u64;
    let mut served = 0u32;
    let mut last_round = false;
    loop {
        let remaining = deadline.saturating_duration_since(now());
        if remaining.is_zero() {
            return Ok(refused(
                started,
                now(),
                fuel,
                PluginVerdict::Timeout,
                "the call used its whole wall-clock budget across request rounds".to_owned(),
            ));
        }
        let fuel_left = budget.fuel.map(|total| total.saturating_sub(fuel));
        if fuel_left == Some(0) {
            return Ok(refused(
                started,
                now(),
                fuel,
                PluginVerdict::OutOfFuel,
                "the call used its whole fuel budget across request rounds".to_owned(),
            ));
        }
        let budget = PluginBudget {
            wall_clock: remaining,
            fuel: fuel_left,
            ..budget
        };
        let run = round.clone();
        let outcome = tokio::task::spawn_blocking(move || run(next, budget))
            .await
            .context("the plugin stopped unexpectedly")??;
        fuel = fuel.saturating_add(outcome.fuel_used);
        if outcome.verdict != PluginVerdict::Success {
            return Ok(PluginOutcome {
                fuel_used: fuel,
                wall_ms: elapsed_ms(started, now()),
                ..outcome
            });
        }
        let asked = match asked(&calls, &outcome.output) {
            Ok(None) => {
                return Ok(PluginOutcome {
                    fuel_used: fuel,
                    wall_ms: elapsed_ms(started, now()),
                    ..outcome
                });
            }
            Ok(Some(asked)) => asked,
            Err(why) => return Ok(refused(started, now(), fuel, PluginVerdict::BadOutput, why)),
        };
        if last_round {
            return Ok(refused(
                started,
                now(),
                fuel,
                PluginVerdict::Timeout,
                "the plugin asked for more requests after the call's request time ran out"
                    .to_owned(),
            ));
        }
        if served == MAX_ROUNDS {
            return Ok(refused(
                started,
                now(),
                fuel,
                PluginVerdict::Failed,
                format!("the plugin asked for more than {MAX_ROUNDS} rounds of requests"),
            ));
        }
        served += 1;
        let mut object = base.clone();
        let serve_deadline = deadline - reserve;
        let state = match asked {
            Asked::Tools(batch) => {
                let session = calls.tools.as_mut().expect("parsed only when offered");
                let serving = now();
                let results = session.serve(batch.requests).await;
                deadline += now().saturating_duration_since(serving);
                if let Some(reason) = session.fatal.take() {
                    return Ok(refused(started, now(), fuel, PluginVerdict::Failed, reason));
                }
                object.insert("tool_results".to_owned(), Value::Object(results));
                batch.state
            }
            Asked::Data(batch) => {
                let session = calls.data.as_ref().expect("parsed only when offered");
                let results = session.serve(batch.requests, budget.max_output_bytes).await;
                object.insert("data_results".to_owned(), Value::Object(results));
                batch.state
            }
            Asked::Job(batch) => {
                let session = calls.jobs.as_ref().expect("parsed only when offered");
                let results = session.serve(batch.requests, budget.max_output_bytes).await;
                object.insert("job_results".to_owned(), Value::Object(results));
                batch.state
            }
            Asked::Model(batch) => {
                tracing::debug!(
                    round = served,
                    requests = batch.requests.len(),
                    "serving plugin model calls"
                );
                let session = calls.model.as_mut().expect("parsed only when offered");
                let results = session.serve(batch.requests, serve_deadline).await;
                object.insert("model_results".to_owned(), Value::Object(results));
                batch.state
            }
            Asked::Http(batch) => {
                tracing::debug!(
                    round = served,
                    requests = batch.requests.len(),
                    "serving plugin http calls"
                );
                let session = calls.http.as_mut().expect("parsed only when offered");
                let results = session.serve(batch.requests, serve_deadline).await;
                object.insert("http_results".to_owned(), Value::Object(results));
                batch.state
            }
        };
        last_round = now() >= deadline - reserve;
        if let Some(state) = state {
            object.insert("state".to_owned(), state);
        }
        next = Value::Object(object);
    }
}

fn elapsed_ms(started: Instant, now: Instant) -> u64 {
    u64::try_from(now.saturating_duration_since(started).as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::background_tools::ProcessControlScope;
    use crate::plugin::data::StampContext;
    use serde_json::json;
    use std::sync::Mutex;
    use std::time::Duration;

    /// A job step's `state` is the host's stored guest state: the first call
    /// sees it, a round answer whose request named no state sees it again, and
    /// a request's own `job_calls.state` wins.
    #[tokio::test]
    async fn a_job_step_keeps_its_stored_state_across_rounds() {
        let dir = tempfile::tempdir().unwrap();
        let node = Arc::new(
            crate::defra_node::EmbeddedNode::builder()
                .data_path(dir.path())
                .build()
                .await
                .unwrap(),
        );
        let caller = job_calls::JobCaller::new(
            node.clone(),
            ProcessControlScope {
                request_id: "r".into(),
                session_id: "s".into(),
                node_did: "did:n".into(),
                requester_did: None,
            },
            StampContext::JobStep {
                node_did: "did:n".into(),
                session_id: "s".into(),
                request_doc_id: "r".into(),
                tool_call_doc_id: "t".into(),
                job_row_doc_id: "t".into(),
                chain_root_doc_id: "t".into(),
            },
        );
        let calls = HostCalls {
            jobs: job_calls::Session::for_call(true, "team/plugin", Some(caller)),
            ..HostCalls::default()
        };
        let ask = json!([{"id": "a", "child_output": {"slots": ["s"], "offset": 0, "limit": 1}}]);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let round: Round = {
            let seen = seen.clone();
            Arc::new(move |input: Value, _| {
                let mut seen = seen.lock().unwrap();
                seen.push(input["state"].clone());
                let output = match seen.len() {
                    1 => json!({"job_calls": {"requests": ask}}),
                    2 => json!({"job_calls": {"requests": ask, "state": {"n": 2}}}),
                    _ => json!({"result": "done"}),
                };
                Ok(PluginOutcome {
                    verdict: PluginVerdict::Success,
                    output,
                    diagnostics: String::new(),
                    fuel_used: 0,
                    wall_ms: 0,
                })
            })
        };
        let budget = PluginBudget {
            fuel: None,
            memory_bytes: 1 << 20,
            wall_clock: Duration::from_secs(5),
            max_output_bytes: 1 << 20,
        };
        let outcome = drive(calls, json!({"state": {"n": 1}}), budget, round)
            .await
            .unwrap();
        assert_eq!(
            outcome.verdict,
            PluginVerdict::Success,
            "{}",
            outcome.diagnostics
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [json!({"n": 1}), json!({"n": 1}), json!({"n": 2})]
        );
        node.shutdown().await;
    }
}
