//! Runs a job step written as JavaScript source through Afterburner's embedded
//! JS engine (`Mode::Wasm`: Wasmtime plus the QuickJS plugin baked into the
//! crate, no external toolchain). A plugin that declares
//! `script_runtime: "js"` ships a stub artifact; when a call carries `script`,
//! the host runs that source as the step instead, one JSON in and one JSON
//! out per round, under the same fuel, memory, wall-clock and output bounds as
//! an artifact. The engine is sealed: no filesystem, network or environment.
//!
//! The source is the step function, `module.exports = (input) => output`,
//! over the job contract (`JobStepInput` in, `JobStepOutput` out). A step-0
//! `effects` output that spawns without a `pin` gets one from the host: the
//! script's digest as `script_id` and `source_digest`, the call's
//! `agent_budget` (default 128) and `args.handle` (default "workflow"), so the
//! existing origin rule classifies the run `dynamic` and the job policy
//! applies unchanged.
use std::sync::OnceLock;
use std::time::Instant;

use afterburner::{Afterburner, Mode};
use afterburner_core::{AfterburnerError, FuelGauge, Manifold};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{PluginBudget, PluginOutcome, PluginVerdict};

/// The agent budget a script run pins when its call names none.
pub const DEFAULT_SCRIPT_AGENT_BUDGET: u64 = 128;
/// The handle a script run pins when its call names none.
pub const DEFAULT_SCRIPT_HANDLE: &str = "workflow";

static ENGINE: OnceLock<std::result::Result<Afterburner, String>> = OnceLock::new();

/// The one sealed JS engine of this process.
fn engine() -> Result<&'static Afterburner> {
    ENGINE
        .get_or_init(|| {
            Afterburner::builder()
                .mode(Mode::Wasm)
                .manifold(Manifold::sealed())
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(|error| anyhow::anyhow!("the JS step engine did not start: {error}"))
}

/// One call's JavaScript step function.
#[derive(Debug, Clone)]
pub struct SourceRunner {
    source: String,
    plugin: String,
}

impl SourceRunner {
    pub fn new(source: String, plugin: &str) -> Self {
        Self {
            source,
            plugin: plugin.to_owned(),
        }
    }

    /// `sha256(source)` as hex: the run's `script_id`.
    pub fn script_id(&self) -> String {
        format!("{:x}", Sha256::digest(self.source.as_bytes()))
    }

    /// Runs one step: `input` in, the script's value out, bounded by `budget`.
    pub fn call(&self, input: &Value, budget: &PluginBudget) -> Result<PluginOutcome> {
        let engine = engine()?;
        let started = Instant::now();
        let id = match engine.register(&self.source) {
            Ok(id) => id,
            Err(AfterburnerError::CompileFailed(message)) => {
                return Ok(PluginOutcome {
                    verdict: PluginVerdict::Failed,
                    output: Value::Null,
                    diagnostics: format!("the script does not compile: {message}"),
                    fuel_used: 0,
                    wall_ms: elapsed_ms(started),
                })
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("registering the script of {}", self.plugin))
            }
        };
        let limits = FuelGauge {
            fuel: budget.fuel,
            memory_bytes: Some(usize::try_from(budget.memory_bytes).unwrap_or(usize::MAX)),
            timeout_ms: Some(u64::try_from(budget.wall_clock.as_millis()).unwrap_or(u64::MAX)),
            output_bytes: Some(budget.max_output_bytes),
            manifold: Manifold::sealed(),
            ..FuelGauge::default()
        };
        let outcome = |verdict, output, diagnostics: String| PluginOutcome {
            verdict,
            output,
            diagnostics,
            fuel_used: 0,
            wall_ms: elapsed_ms(started),
        };
        Ok(match engine.run_with(&id, input, &limits) {
            Ok(output) => outcome(
                PluginVerdict::Success,
                self.with_pin(output, input),
                String::new(),
            ),
            Err(AfterburnerError::FuelExhausted) => outcome(
                PluginVerdict::OutOfFuel,
                Value::Null,
                "the script exhausted its fuel budget".into(),
            ),
            Err(AfterburnerError::MemoryLimit) => outcome(
                PluginVerdict::OutOfMemory,
                Value::Null,
                "the script exceeded its memory budget".into(),
            ),
            Err(AfterburnerError::Timeout) => outcome(
                PluginVerdict::Timeout,
                Value::Null,
                format!(
                    "the script did not answer within its {}ms wall-clock budget and was stopped",
                    budget.wall_clock.as_millis()
                ),
            ),
            Err(AfterburnerError::OutputTooLarge { limit }) => outcome(
                PluginVerdict::BadOutput,
                Value::Null,
                format!("the script's output passed the {limit} byte cap"),
            ),
            Err(error) => outcome(
                PluginVerdict::Failed,
                Value::Null,
                format!("the script failed: {error}"),
            ),
        })
    }

    /// A step-0 `effects` output that spawns but carries no `pin` gets the host's.
    fn with_pin(&self, mut output: Value, input: &Value) -> Value {
        let spawns = output
            .pointer("/effects/spawn")
            .and_then(Value::as_array)
            .is_some_and(|spawns| !spawns.is_empty());
        let pinned = output
            .pointer("/effects/pin")
            .is_some_and(|pin| !pin.is_null());
        let step0 = input.get("pin").is_none_or(Value::is_null);
        if !(spawns && !pinned && step0) {
            return output;
        }
        let digest = self.script_id();
        let agent_budget = input
            .get("agent_budget")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_SCRIPT_AGENT_BUDGET);
        let handle = input
            .pointer("/args/handle")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_SCRIPT_HANDLE)
            .to_owned();
        if let Some(effects) = output.get_mut("effects").and_then(Value::as_object_mut) {
            effects.insert(
                "pin".into(),
                json!({
                    "script_id": digest,
                    "source_digest": format!("sha256:{digest}"),
                    "agent_budget": agent_budget,
                    "handle": handle,
                }),
            );
        }
        output
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn budget() -> PluginBudget {
        PluginBudget {
            fuel: None,
            memory_bytes: 64 * 1024 * 1024,
            wall_clock: Duration::from_secs(5),
            max_output_bytes: 1024 * 1024,
        }
    }

    #[test]
    fn a_step_function_runs_and_a_step0_spawn_gets_the_hosts_pin() {
        let runner = SourceRunner::new(
            "module.exports = (i) => ({effects: {spawn: [{slot: \"s\", attempt: 0, target: \"t\", prompt: i.args.prompt, title: \"t\"}]}});"
                .into(),
            "workflow",
        );
        let input =
            json!({"pin": null, "agent_budget": 3, "args": {"prompt": "go", "handle": "h"}});
        let outcome = runner.call(&input, &budget()).unwrap();
        assert_eq!(
            outcome.verdict,
            PluginVerdict::Success,
            "{}",
            outcome.diagnostics
        );
        assert_eq!(outcome.output["effects"]["spawn"][0]["prompt"], "go");
        let pin = &outcome.output["effects"]["pin"];
        assert_eq!(pin["script_id"], runner.script_id());
        assert_eq!(
            pin["source_digest"],
            format!("sha256:{}", runner.script_id())
        );
        assert_eq!(pin["agent_budget"], 3);
        assert_eq!(pin["handle"], "h");
    }

    #[test]
    fn a_later_step_and_a_pinned_output_keep_their_own_pin() {
        let runner = SourceRunner::new(
            "module.exports = (i) => i.pin ? {result: {done: true}} : {effects: {pin: {script_id: \"mine\", source_digest: \"sha256:mine\", agent_budget: 1, handle: \"m\"}, spawn: [{slot: \"s\", attempt: 0, target: \"t\", prompt: \"p\", title: \"t\"}]}};"
                .into(),
            "workflow",
        );
        let step0 = runner.call(&json!({"pin": null}), &budget()).unwrap();
        assert_eq!(step0.output["effects"]["pin"]["script_id"], "mine");
        let later = runner
            .call(&json!({"pin": {"script_id": "mine"}}), &budget())
            .unwrap();
        assert_eq!(later.output, json!({"result": {"done": true}}));
    }

    #[test]
    fn a_script_that_does_not_compile_fails_with_the_message() {
        let runner = SourceRunner::new("module.exports = (i) => {".into(), "workflow");
        let outcome = runner.call(&json!({}), &budget()).unwrap();
        assert_eq!(outcome.verdict, PluginVerdict::Failed);
        assert!(
            outcome.diagnostics.contains("does not compile"),
            "{}",
            outcome.diagnostics
        );
    }

    #[test]
    fn a_script_that_never_returns_hits_the_wall_clock() {
        let runner = SourceRunner::new(
            "module.exports = (i) => { for (;;) {} };".into(),
            "workflow",
        );
        let outcome = runner
            .call(
                &json!({}),
                &PluginBudget {
                    wall_clock: Duration::from_millis(300),
                    ..budget()
                },
            )
            .unwrap();
        assert!(
            matches!(
                outcome.verdict,
                PluginVerdict::Timeout | PluginVerdict::OutOfFuel
            ),
            "{:?} {}",
            outcome.verdict,
            outcome.diagnostics
        );
    }
}
