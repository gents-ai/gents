//! The round loop for a plugin the host serves: a plugin that cannot reach a
//! model or the network itself answers a call with a batch of requests, the
//! host serves them, and calls the plugin again with the answers until it
//! returns a result.
//!
//! The services are [`super::model_calls`] (`model_calls` / `model_results`)
//! and [`super::http_calls`] (`http_calls` / `http_results`). The input of a
//! driven call is always a JSON object carrying `true` under each offered
//! service's key; the caller's `"state"` and result keys are stripped (only
//! the host sets them) and a null input becomes `{}`. One round asks one
//! service. Bounds shared by every service: [`MAX_ROUNDS`] rounds, and the
//! call's wall clock and fuel across all rounds. When the wall clock runs out
//! while requests are served, the unanswered ones get error results and the
//! plugin gets one final round (the last [`FINAL_ROUND_RESERVE_DIVISOR`]th of
//! the clock) to finish with what it has; only a plugin that asks again is a
//! timeout.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use serde_json::{Map, Value};

use super::{http_calls, model_calls, PluginBudget, PluginOutcome, PluginVerdict};

/// Rounds of requests one call may be answered.
pub const MAX_ROUNDS: u32 = 64;
/// The plugin's final round after request time runs out gets this fraction of
/// the call's wall clock (one eighth).
pub const FINAL_ROUND_RESERVE_DIVISOR: u32 = 8;

/// One plugin run, as the executor supplies it.
pub(super) type Round = Arc<dyn Fn(Value, PluginBudget) -> Result<PluginOutcome> + Send + Sync>;

/// The services one call is offered. A call with none runs undriven.
#[derive(Default)]
pub(super) struct HostCalls {
    pub(super) model: Option<model_calls::Session>,
    pub(super) http: Option<http_calls::Session>,
}

impl HostCalls {
    pub(super) fn is_empty(&self) -> bool {
        self.model.is_none() && self.http.is_none()
    }

    pub(super) fn prepare_input(&self, input: Value) -> Result<Value> {
        let mut base = match input {
            Value::Null => Map::new(),
            Value::Object(object) => object,
            _ => {
                anyhow::bail!("a plugin the host serves requests for takes a JSON object as input")
            }
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
        Ok(Value::Object(base))
    }
}

enum Asked {
    Model(model_calls::Batch),
    Http(http_calls::Batch),
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

/// What the plugin's output asks for: `None` for a final result. Only keys
/// of an offered service are requests; any other output is a result.
fn asked(calls: &HostCalls, output: &Value) -> Result<Option<Asked>, String> {
    let model = match calls.model {
        Some(_) => model_calls::parse_batch(output)?,
        None => None,
    };
    let http = match &calls.http {
        Some(_) => http_calls::parse_batch(output)?,
        None => None,
    };
    match (model, http) {
        (None, None) => Ok(None),
        (Some(model), None) => Ok(Some(Asked::Model(model))),
        (None, Some(http)) => Ok(Some(Asked::Http(http))),
        (Some(_), Some(_)) => {
            Err("one round asks for model_calls or http_calls, not both".to_owned())
        }
    }
}

/// Runs the plugin, serving its requests until it returns a result.
pub(super) async fn drive(
    mut calls: HostCalls,
    input: Value,
    budget: PluginBudget,
    round: Round,
) -> Result<PluginOutcome> {
    let started = Instant::now();
    let deadline = started + budget.wall_clock;
    // Requests stop here; the plugin's final round runs on the rest.
    let serve_deadline = deadline - budget.wall_clock / FINAL_ROUND_RESERVE_DIVISOR;
    let Value::Object(base) = calls.prepare_input(input)? else {
        anyhow::bail!("a driven plugin requires host services");
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
            .context("the plugin stopped unexpectedly")??;
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
                })
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
        let state = match asked {
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
        last_round = Instant::now() >= serve_deadline;
        if let Some(state) = state {
            object.insert("state".to_owned(), state);
        }
        next = Value::Object(object);
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}
