//! The host functions a plugin artifact may import under the wasm module
//! `gents` when its manifest declares them (`PackPlugin.host_imports`).
//!
//! `run_script(in_ptr, in_len) -> i32` evaluates the call's own `script` on
//! the JSON the guest hands it, through the host's embedded JS engine
//! (`script_engine`), and returns the answer's byte length, or a negative
//! code: `-1` the call carries no script, `-2` the engine failed (its worded
//! diagnostics are the stored bytes), `-3` the bytes were not UTF-8 JSON,
//! `-4` the answer passed the plugin's output cap. `read_result(out_ptr,
//! out_cap) -> i32` copies the stored bytes (answer or message) into the
//! guest and returns the count copied.
//!
//! The script text never crosses the boundary: the closure captures it from
//! the arguments of the call being dispatched, so a guest can evaluate
//! nothing but that script.
//!
//! Afterburner's `afb_run::run_afb_bytes` takes no linker hook, so a
//! declaring plugin runs through a copy of its `run_wasm` shape with a
//! `setup` that defines the two functions. The two private helpers it
//! needs are copied with their source lines named.
use std::sync::{Arc, Mutex};
use std::time::Instant;

use afterburner::afb_run::{AfbRunOutcome, AfbRunOutput, AfbRunRequest};
use afterburner::wasi::embedder_vm::{
    shared_epoch_vm_with, BoundedCommandOutput, CommandOutcome, EmbedderState, WasiCommandOpts,
};
use afterburner_core::manifold::{EnvAccess, FsAccess, Manifold};
use anyhow::{Context, Result};
use serde_json::Value;

use super::{script_engine, PluginBudget, PluginOutcome, PluginVerdict};

const PRECOMPILED_WASM_MEMBER: &str = "precompiled/wasm32-wasip1/main.wasm";

pub(super) const NO_SCRIPT: i32 = -1;
pub(super) const ENGINE_FAILED: i32 = -2;
pub(super) const BAD_INPUT: i32 = -3;
pub(super) const TOO_LARGE: i32 = -4;

/// One call's import state: what the host may evaluate, under which bounds,
/// and what the guest may read back.
pub(super) struct ImportSession {
    pub script: Option<String>,
    pub plugin: String,
    pub budget: PluginBudget,
    pub started: Instant,
    pub stored: Vec<u8>,
    /// The engine's verdict and wording when `run_script` answered
    /// `ENGINE_FAILED`, so a guest that then exits non-zero reports the
    /// engine's failure, not the exit.
    pub failure: Option<(PluginVerdict, String)>,
}

/// The call's `script`: a non-empty string. Whitespace is source.
pub fn script_of(arguments: &Value) -> Option<String> {
    arguments
        .get("script")
        .and_then(Value::as_str)
        .filter(|script| !script.is_empty())
        .map(str::to_owned)
}

/// A guest that exited non-zero after the import reported an engine failure
/// reports that failure: the engine's verdict and wording stand in for the
/// generic exit diagnostics.
pub(super) fn outcome_with_import_failure(
    outcome: PluginOutcome,
    code: i32,
    session: Option<&Mutex<ImportSession>>,
) -> PluginOutcome {
    if code == 0 {
        return outcome;
    }
    let Some(session) = session else {
        return outcome;
    };
    let recorded = session.lock().ok().and_then(|s| s.failure.clone());
    match recorded {
        Some((verdict, diagnostics)) => PluginOutcome {
            verdict,
            diagnostics,
            output: Value::Null,
            ..outcome
        },
        None => outcome,
    }
}

fn memory_of(caller: &mut wasmtime::Caller<'_, EmbedderState>) -> Option<wasmtime::Memory> {
    caller
        .get_export("memory")
        .and_then(wasmtime::Extern::into_memory)
}

/// Runs `afb` as a WASI command with the `gents` imports defined: the shape
/// of `afterburner::afb_run::run_wasm` (afb_run.rs:368-400) plus a `setup`.
pub(super) fn run_with_imports(
    afb: &afterburner_afb::Afb,
    request: AfbRunRequest,
    session: Arc<Mutex<ImportSession>>,
) -> Result<AfbRunOutput> {
    let AfbRunRequest {
        stdin,
        args,
        manifold,
        fuel,
        memory_bytes,
        timeout,
        nan_mode,
    } = request;
    let wasm_bytes = afb
        .precompiled
        .get(PRECOMPILED_WASM_MEMBER)
        .with_context(|| {
            format!(
                "{} has no {PRECOMPILED_WASM_MEMBER}; run `gents pack build` to produce it",
                afb.qualified_name()
            )
        })?;
    let vm = shared_epoch_vm_with(nan_mode).context("the plugin VM did not start")?;
    let run_session = session.clone();
    let read_session = session;
    let module = vm
        .compile(wasm_bytes, true, move |linker| {
            linker.func_wrap(
                "gents",
                "run_script",
                move |mut caller: wasmtime::Caller<'_, EmbedderState>, ptr: i32, len: i32| -> i32 {
                    run_script(&mut caller, &run_session, ptr, len)
                },
            )?;
            linker.func_wrap(
                "gents",
                "read_result",
                move |mut caller: wasmtime::Caller<'_, EmbedderState>, ptr: i32, cap: i32| -> i32 {
                    read_result(&mut caller, &read_session, ptr, cap)
                },
            )?;
            Ok(())
        })
        .context("compiling the plugin with its host imports")?;
    let mut argv = vec![afb.qualified_name()];
    argv.extend(args);
    let mut opts = WasiCommandOpts::new().args(argv).stdin(stdin);
    if let Some(bytes) = memory_bytes {
        opts = opts.max_memory_bytes(usize::try_from(bytes).unwrap_or(usize::MAX));
    }
    opts = manifold_to_wasi_opts(&manifold, opts);
    let raw = vm
        .run_command_bounded(&module, opts, fuel, None, timeout)
        .context("running the plugin with its host imports")?;
    Ok(finish(raw))
}

fn run_script(
    caller: &mut wasmtime::Caller<'_, EmbedderState>,
    session: &Mutex<ImportSession>,
    ptr: i32,
    len: i32,
) -> i32 {
    let Ok(mut s) = session.lock() else {
        return BAD_INPUT;
    };
    // Every call starts clean: a later `read_result` reads this call's answer or
    // message, never an earlier call's, and only this call's engine failure
    // can stand in for a non-zero exit.
    s.stored.clear();
    s.failure = None;
    let Some(memory) = memory_of(caller) else {
        s.stored = b"the guest exports no memory".to_vec();
        return BAD_INPUT;
    };
    let (Ok(start), Ok(len)) = (usize::try_from(ptr), usize::try_from(len)) else {
        s.stored = b"the input pointer or length is negative".to_vec();
        return BAD_INPUT;
    };
    let Some(bytes) = memory.data(&*caller).get(start..start.saturating_add(len)) else {
        s.stored = b"the input lies outside the guest's memory".to_vec();
        return BAD_INPUT;
    };
    let input: Value = match serde_json::from_slice(bytes) {
        Ok(value) => value,
        Err(error) => {
            s.stored = format!("the step input is not JSON: {error}").into_bytes();
            return BAD_INPUT;
        }
    };
    let Some(script) = s.script.clone() else {
        s.stored = b"no script on this call".to_vec();
        return NO_SCRIPT;
    };
    let budget = PluginBudget {
        wall_clock: s.budget.wall_clock.saturating_sub(s.started.elapsed()),
        ..s.budget
    };
    match script_engine::eval(&script, &s.plugin, &input, &budget) {
        Ok(outcome) if outcome.verdict == PluginVerdict::Success => {
            match serde_json::to_vec(&outcome.output) {
                Ok(answer) if answer.len() > s.budget.max_output_bytes => {
                    s.stored = format!(
                        "the script's answer is {} bytes, over the plugin's {} byte output cap",
                        answer.len(),
                        s.budget.max_output_bytes
                    )
                    .into_bytes();
                    TOO_LARGE
                }
                Ok(answer) => {
                    let len = i32::try_from(answer.len()).unwrap_or(i32::MAX);
                    s.stored = answer;
                    len
                }
                Err(error) => {
                    s.stored =
                        format!("the script's answer did not serialize: {error}").into_bytes();
                    BAD_INPUT
                }
            }
        }
        Ok(outcome) => {
            s.stored = outcome.diagnostics.clone().into_bytes();
            s.failure = Some((outcome.verdict, outcome.diagnostics));
            ENGINE_FAILED
        }
        Err(error) => {
            let message = format!("the script failed: {error:#}");
            s.stored = message.clone().into_bytes();
            s.failure = Some((PluginVerdict::Failed, message));
            ENGINE_FAILED
        }
    }
}

fn read_result(
    caller: &mut wasmtime::Caller<'_, EmbedderState>,
    session: &Mutex<ImportSession>,
    ptr: i32,
    cap: i32,
) -> i32 {
    let Some(memory) = memory_of(caller) else {
        return 0;
    };
    let (Ok(start), Ok(cap)) = (usize::try_from(ptr), usize::try_from(cap)) else {
        return 0;
    };
    let Ok(s) = session.lock() else {
        return 0;
    };
    let n = s.stored.len().min(cap);
    let Some(target) = memory.data_mut(&mut *caller).get_mut(start..start + n) else {
        return 0;
    };
    target.copy_from_slice(&s.stored[..n]);
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// Copy of `afterburner::afb_run::manifold_to_wasi_opts` (afb_run.rs:301-335):
/// private there; keep in step with it.
fn manifold_to_wasi_opts(manifold: &Manifold, mut opts: WasiCommandOpts) -> WasiCommandOpts {
    match &manifold.fs {
        FsAccess::None => {}
        FsAccess::ReadOnly(paths) => {
            for p in paths {
                let guest = p.to_string_lossy().into_owned();
                opts = opts.preopen_ro(p, guest);
            }
        }
        FsAccess::ReadWrite(paths) => {
            for p in paths {
                let guest = p.to_string_lossy().into_owned();
                opts = opts.preopen_rw(p, guest);
            }
        }
    }
    match &manifold.env {
        EnvAccess::None => {}
        EnvAccess::AllowList(keys) => {
            for key in keys {
                if let Ok(val) = std::env::var(key) {
                    opts = opts.env_var(key.clone(), val);
                }
            }
        }
        EnvAccess::Full => {
            for (key, val) in std::env::vars() {
                opts = opts.env_var(key, val);
            }
        }
    }
    opts
}

/// Copy of `afterburner::afb_run::finish` (afb_run.rs:341-360): private
/// there; keep in step with it.
fn finish(raw: BoundedCommandOutput) -> AfbRunOutput {
    let outcome = if raw.memory_limit_hit {
        AfbRunOutcome::OutOfMemory
    } else {
        match raw.outcome {
            CommandOutcome::Exited(code) => AfbRunOutcome::Exited(code),
            CommandOutcome::OutOfFuel => AfbRunOutcome::OutOfFuel,
            CommandOutcome::Timeout => AfbRunOutcome::Timeout,
            CommandOutcome::Trapped(msg) => AfbRunOutcome::Trapped(msg),
        }
    };
    AfbRunOutput {
        outcome,
        stdout: raw.stdout,
        stderr: raw.stderr,
        fuel_used: raw.fuel_consumed,
    }
}
