use std::sync::Arc;
use std::time::Instant;

use serde_json::{Map, Value};

use super::{
    data, http_calls, job_calls, model_calls, tool_calls, PluginBudget, PluginError, PluginOutcome,
    PluginVerdict,
};

#[derive(Debug, thiserror::Error)]
pub enum RoundsError {
    #[error("a plugin the host serves requests for takes a JSON object as input")]
    InputNotObject,
    #[error("a driven plugin requires host services")]
    NotDriven,
    #[error("the plugin stopped unexpectedly")]
    Stopped(#[source] tokio::task::JoinError),
    #[error(transparent)]
    Plugin(#[from] PluginError),
}

type Result<T, E = RoundsError> = std::result::Result<T, E>;

pub const MAX_ROUNDS: u32 = 64;
pub const FINAL_ROUND_RESERVE_DIVISOR: u32 = 8;

pub(super) type Round =
    Arc<dyn Fn(Value, PluginBudget) -> Result<PluginOutcome, PluginError> + Send + Sync>;

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
            _ => return Err(RoundsError::InputNotObject),
        };
        base.remove("state");
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

fn refused(started: Instant, fuel: u64, verdict: PluginVerdict, why: String) -> PluginOutcome {
    PluginOutcome {
        verdict,
        output: Value::Null,
        diagnostics: why,
        fuel_used: fuel,
        wall_ms: elapsed_ms(started),
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
    mut calls: HostCalls,
    input: Value,
    budget: PluginBudget,
    round: Round,
) -> Result<PluginOutcome> {
    let started = Instant::now();
    let mut deadline = started + budget.wall_clock;
    let reserve = budget.wall_clock / FINAL_ROUND_RESERVE_DIVISOR;
    let Value::Object(base) = calls.prepare_input(input)? else {
        return Err(RoundsError::NotDriven);
    };
    let mut next = Value::Object(base.clone());
    let mut fuel = 0u64;
    let mut served = 0u32;
    let mut last_round = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(refused(
                started,
                fuel,
                PluginVerdict::Timeout,
                "the call used its whole wall-clock budget across request rounds".to_owned(),
            ));
        }
        let fuel_left = budget.fuel.map(|total| total.saturating_sub(fuel));
        if fuel_left == Some(0) {
            return Ok(refused(
                started,
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
            .map_err(RoundsError::Stopped)??;
        fuel = fuel.saturating_add(outcome.fuel_used);
        if outcome.verdict != PluginVerdict::Success {
            return Ok(PluginOutcome {
                fuel_used: fuel,
                wall_ms: elapsed_ms(started),
                ..outcome
            });
        }
        let asked = match asked(&calls, &outcome.output) {
            Ok(None) => {
                return Ok(PluginOutcome {
                    fuel_used: fuel,
                    wall_ms: elapsed_ms(started),
                    ..outcome
                });
            }
            Ok(Some(asked)) => asked,
            Err(why) => return Ok(refused(started, fuel, PluginVerdict::BadOutput, why)),
        };
        if last_round {
            return Ok(refused(
                started,
                fuel,
                PluginVerdict::Timeout,
                "the plugin asked for more requests after the call's request time ran out"
                    .to_owned(),
            ));
        }
        if served == MAX_ROUNDS {
            return Ok(refused(
                started,
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
                let serving = Instant::now();
                let results = session.serve(batch.requests).await;
                deadline += serving.elapsed();
                if let Some(reason) = session.fatal.take() {
                    return Ok(refused(started, fuel, PluginVerdict::Failed, reason));
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
        last_round = Instant::now() >= deadline - reserve;
        if let Some(state) = state {
            object.insert("state".to_owned(), state);
        }
        next = Value::Object(object);
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}
