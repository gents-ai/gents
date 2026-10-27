//! The host's JavaScript step engine: Afterburner's embedded QuickJS
//! (`Mode::Wasm`, Wasmtime plus the QuickJS plugin baked into the crate, no
//! external toolchain), sealed (no filesystem, network or environment). One
//! JSON in and one JSON out per call, under the fuel, memory, wall-clock and
//! output bounds of a plugin budget.

use std::sync::OnceLock;
use std::time::Instant;

use afterburner::{Afterburner, Mode};
use afterburner_core::{AfterburnerError, FuelGauge, Manifold};
use anyhow::{Context, Result};
use serde_json::Value;

use super::{PluginBudget, PluginOutcome, PluginVerdict};

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

/// Runs `script` (`module.exports = (input) => output`) once on `input`,
/// bounded by `budget`: the script's value on success, else the engine's
/// verdict and wording. `plugin` names the caller in errors.
pub(crate) fn eval(
    script: &str,
    plugin: &str,
    input: &Value,
    budget: &PluginBudget,
) -> Result<PluginOutcome> {
    let engine = engine()?;
    let started = Instant::now();
    let outcome = |verdict, output, diagnostics: String| PluginOutcome {
        verdict,
        output,
        diagnostics,
        fuel_used: 0,
        wall_ms: elapsed_ms(started),
    };
    let id = match engine.register(script) {
        Ok(id) => id,
        Err(AfterburnerError::CompileFailed(message)) => {
            return Ok(outcome(
                PluginVerdict::Failed,
                Value::Null,
                format!("the script does not compile: {message}"),
            ))
        }
        Err(error) => {
            return Err(error).with_context(|| format!("registering the script of {plugin}"))
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
    Ok(match engine.run_with(&id, input, &limits) {
        Ok(output) => outcome(PluginVerdict::Success, output, String::new()),
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

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::PluginVerdict;
    use serde_json::json;
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
    fn a_script_that_does_not_compile_fails_with_the_message() {
        let outcome = eval(
            "module.exports = (i) => {",
            "workflow",
            &json!({}),
            &budget(),
        )
        .unwrap();
        assert_eq!(outcome.verdict, PluginVerdict::Failed);
        assert!(
            outcome.diagnostics.contains("does not compile"),
            "{}",
            outcome.diagnostics
        );
    }

    #[test]
    fn a_script_that_never_returns_hits_the_wall_clock() {
        let outcome = eval(
            "module.exports = (i) => { for (;;) {} };",
            "workflow",
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
