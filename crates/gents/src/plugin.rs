//! Running one of a pack's plugins.
//!
//! A plugin is a complete Afterburner `.afb` ([`crate::pack::PackPlugin`]).
//! Only Afterburner knows how to dispatch every language it supports (a
//! Python plugin compiles to an emscripten-pyodide bundle, not a WASI
//! command; a compiled Ruby plugin runs as a WASI command; a JS/TS/Rust/
//! Go/C/C++ plugin always runs as a WASI command), so this file never runs
//! a plugin itself - it hands the `.afb` bytes to
//! `afterburner::afb_run::run_afb_bytes`, the library entry point built for
//! exactly this: an embedder that needs to run a package without the `burn`
//! CLI's registry-client weight.
//!
//! The call ABI is deliberately narrow: canonical JSON arguments arrive on
//! stdin, a single JSON value is written to stdout, and anything on stderr
//! is diagnostics for a human. Nothing else carries the call: no second
//! channel, no shared memory, no side import for passing data.
//!
//! Seven invariants hold for every call, each enforced here rather than
//! trusted from the caller:
//!
//! 1. **One run per instantiation.** `run_afb_bytes` parses, compiles, and
//!    instantiates fresh on every call (its own module doc: "nothing is
//!    written to the calling process's real stdout or stderr" and no store
//!    is reused across calls); [`PluginRunner`] holds nothing but the raw
//!    `.afb` bytes and the granted manifold between calls, so a plugin
//!    keeps nothing between them either.
//! 2. **The manifold is a ceiling, narrowed, never widened.**
//!    [`narrow_manifold`] intersects what the plugin declared
//!    (`PackPlugin::manifold`) with what this runner is willing to grant,
//!    on every axis, and forces `listen` to `None` regardless of either
//!    side. See [`PluginRunner::compile`] for why the ceiling is
//!    `Manifold::sealed()` today.
//! 3. **A plugin never listens.** WASI preview 1 command modules (what
//!    every bounded dispatch path in `run_afb_bytes` runs) have no socket
//!    import wired at all, so this holds structurally; [`narrow_manifold`]
//!    enforces it a second time regardless, as defense in depth against a
//!    manifold that somehow reached this file unvalidated.
//! 4. **A plugin that cannot be run with every bound enforced is refused
//!    before it is ever called, not run with the bound silently dropped.**
//!    `run_afb_bytes` does not enforce `stdin`, `fuel`, `memory_bytes`, or
//!    the manifold's grants for every language it can dispatch (its own
//!    module doc's "Known gaps": Python, compiled or source, and Ruby
//!    source, all drop at least one of them). [`PluginRunner::compile_within`]
//!    parses the real `.afb` and refuses admission with
//!    [`unbounded_dispatch_reason`] naming exactly what would not be
//!    enforced, rather than letting [`PluginRunner::call`] silently run it
//!    under a weaker guarantee than the caller asked for. The effective
//!    authority claim in rule 2 is only honest for a plugin admitted this
//!    way.
//! 5. **Every bound that is enforced is real.** `budget.fuel`,
//!    `budget.memory_bytes`, and `budget.wall_clock` go straight to
//!    `AfbRunRequest`; `run_afb_bytes` itself races the wall-clock deadline
//!    against the run (see `AfbRunRequest::timeout`'s own doc) rather than
//!    this file hand-rolling a second timeout thread. Stdout and stderr are
//!    each capped after the run, truncation is named in `diagnostics`,
//!    never silently applied.
//! 6. **Output is validated, `input_schema` is not.** Stdout that is not
//!    exactly one JSON value is [`PluginVerdict::BadOutput`] naming why.
//!    `input_schema` describes the *arguments*, not the result, so this
//!    file never checks arguments against it; a caller that wants that
//!    checks before calling and reports [`PluginVerdict::Refused`] itself.
//! 7. **Failures are typed, never silent.** A routine bound (fuel, memory,
//!    wall clock, malformed output) is a [`PluginOutcome`] with its own
//!    verdict; a genuine trap (division by zero, unreachable, a missing
//!    precompiled artifact) is a hard `Err` from [`PluginRunner::compile`]
//!    or [`PluginRunner::call`], never swallowed into a generic verdict.

use std::path::PathBuf;
use std::time::Instant;

use afterburner::afb_run::{run_afb_bytes, AfbRunOutcome, AfbRunRequest};
use afterburner_core::manifold::{EnvAccess, FsAccess, ListenAccess, Manifold, NetAccess};
use anyhow::{Context, Result};

use crate::pack::PackPlugin;

/// Bytes of a plugin's JSON result kept before the call is judged
/// [`PluginVerdict::BadOutput`] for running past this bound. A result is a
/// small JSON value describing one call's outcome, not a file; anything
/// larger is treated as a bad result rather than silently accepted
/// truncated (rules 5 and 6 in this module's own doc).
const MAX_STDOUT_BYTES: usize = 1024 * 1024;

/// Bytes of a plugin's stderr kept for a human to read. Diagnostics, not a
/// log archive.
const MAX_STDERR_BYTES: usize = 64 * 1024;

/// What a plugin may spend on one call. Maps 1:1 onto
/// [`AfbRunRequest`]'s `fuel`, `memory_bytes`, and `timeout` fields.
#[derive(Debug, Clone, Copy)]
pub struct PluginBudget {
    /// Wasmtime fuel: a deterministic instruction budget, the real
    /// backstop against a guest that never returns.
    pub fuel: u64,
    /// Linear-memory ceiling in bytes, enforced by wasmtime on every
    /// `memory.grow`.
    pub memory_bytes: u64,
    /// Wall-clock deadline for the whole call, enforced by
    /// `run_afb_bytes` itself: a guest that ignores it keeps running on a
    /// detached thread until its own fuel runs out (see
    /// `AfbRunRequest::timeout`'s own doc), exactly like this module's
    /// previous hand-rolled timeout did for the identical reason.
    pub wall_clock: std::time::Duration,
}

impl PluginBudget {
    /// [`Self::default`], raised where it could not host `afb` at all.
    ///
    /// A bound below what a guest spends before its own code runs is not a
    /// bound: the run fails during startup, the plugin never executes a
    /// line, and the ceiling never bounds the thing it was meant to bound.
    /// A Pyodide-backed plugin carries CPython, which needs more memory to
    /// instantiate than [`Self::default`] allows and spends orders of
    /// magnitude more fuel booting than it allows for a whole call, so
    /// admitting one and then handing it the default budget would admit a
    /// language that can never answer. The floor comes from
    /// [`afterburner::afb_run::startup_floor`], which is where the dispatch
    /// shape is known, rather than from a copy of that knowledge here.
    ///
    /// The wall clock is raised with it, for the same reason and no other:
    /// booting an interpreter is work the caller waits through before the
    /// plugin's own code starts, and a budget that cannot cover the boot
    /// reports `Timeout` on every call.
    pub fn for_artifact(afb: &afterburner_afb::Afb) -> Self {
        let default = Self::default();
        match afterburner::afb_run::startup_floor(afb) {
            Some(floor) => Self {
                memory_bytes: default.memory_bytes.max(floor.memory_bytes),
                // The startup cost plus the default's own allowance, so a
                // plugin still gets its own budget to work in after the
                // runtime has finished booting.
                fuel: floor.fuel.saturating_add(default.fuel),
                wall_clock: default.wall_clock.max(INTERPRETER_BOOT_ALLOWANCE),
                ..default
            },
            None => default,
        }
    }
}

/// What a plugin that has to boot an interpreter before its own code runs
/// is allowed for the whole call, when the caller does not say otherwise.
///
/// Booting CPython is a second or two warm and more cold, all of it inside
/// the wall clock the caller asked for, so [`PluginBudget::default`]'s five
/// seconds would time out before the plugin's first line.
const INTERPRETER_BOOT_ALLOWANCE: std::time::Duration = std::time::Duration::from_secs(60);

impl Default for PluginBudget {
    /// `fuel` mirrors `afterburner_wasi::embedder_vm`'s own default
    /// instruction budget (100 million, "generous enough for unit
    /// tests"); `memory_bytes` (64 MiB) is comfortable for a small
    /// compiled transform without giving a runaway allocation the run of
    /// the host; `wall_clock` (5 s) is long enough for a cold-ish
    /// compute-bound call and short enough that a caller waiting on a
    /// plugin result is not left hanging.
    fn default() -> Self {
        Self {
            fuel: 100_000_000,
            memory_bytes: 64 * 1024 * 1024,
            wall_clock: std::time::Duration::from_secs(5),
        }
    }
}

/// Why a call did not return the plugin's own JSON value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginVerdict {
    /// The plugin ran and produced exactly one JSON value.
    Success,
    /// A caller's own pre-call check (e.g. `input_schema` validation)
    /// declined to make the call. Never produced by [`PluginRunner`]
    /// itself; part of the shared vocabulary so a caller can report a
    /// refusal in the same [`PluginOutcome`] shape as a real call would
    /// have used (rule 6 in this module's own doc). A plugin that cannot
    /// be run with every bound enforced is refused earlier, at
    /// [`PluginRunner::compile`], never here (rule 4).
    Refused,
    /// `budget.fuel` ran out before the plugin returned.
    OutOfFuel,
    /// `budget.memory_bytes` was exceeded.
    OutOfMemory,
    /// `budget.wall_clock` elapsed before the plugin returned.
    Timeout,
    /// The plugin ran but stdout was not exactly one JSON value (not
    /// parseable, more than one value, or truncated past the output
    /// bound).
    BadOutput,
}

/// What came back from one plugin call.
#[derive(Debug, Clone)]
pub struct PluginOutcome {
    pub verdict: PluginVerdict,
    /// The plugin's own JSON value on [`PluginVerdict::Success`];
    /// [`serde_json::Value::Null`] for every other verdict.
    pub output: serde_json::Value,
    /// Bounded stderr text, for a human. Carries a truncation note when
    /// stdout or stderr ran past this module's own bound.
    pub diagnostics: String,
    /// Fuel actually consumed, as `run_afb_bytes` reports it: exact on
    /// every outcome for the bounded dispatch family this module ever
    /// admits (rule 4), including `OutOfMemory` and `OutOfFuel`; `0` on
    /// `Timeout` (the run that owns the number is still running in the
    /// background).
    pub fuel_used: u64,
    pub wall_ms: u64,
}

/// A plugin, ready to be called more than once.
pub struct PluginRunner {
    /// The plugin's raw `.afb` bytes. Kept whole (not pre-compiled)
    /// because `run_afb_bytes` parses, compiles, and instantiates fresh on
    /// every call - there is nothing cheaper to cache across calls without
    /// re-implementing part of that function (rule 1).
    afb_bytes: Vec<u8>,
    plugin: PackPlugin,
    /// What this runner will actually grant a call: the intersection of
    /// what the plugin declared and [`Self::CEILING`].
    manifold: Manifold,
}

impl std::fmt::Debug for PluginRunner {
    /// Names the plugin, never the compiled bytes.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginRunner")
            .field("plugin", &self.plugin.name)
            .finish()
    }
}

impl PluginRunner {
    /// This runner's own capability ceiling. `compile`'s fixed signature
    /// carries no admission-time ceiling from a caller, and the plugin ABI
    /// itself carries nothing beyond stdin/stdout/stderr (this module's
    /// own doc), so the honest ceiling today is "nothing". `narrow_manifold`
    /// takes the ceiling as an ordinary argument; a caller that gains a
    /// real admission ceiling to enforce plugs it in in place of this
    /// constant, and every declared manifold is narrowed against it
    /// exactly the way it already is here.
    const CEILING: Manifold = Manifold::sealed();

    /// Admits one plugin's compiled `.afb`.
    ///
    /// `plugin` is the pack's own declaration for it (name, description,
    /// schema, manifold); `afb_bytes` is the artifact that declaration
    /// names (`PackArchive::plugin_artifact`'s own doc: an artifact cannot
    /// be swapped underneath the name it was admitted under). Refuses
    /// admission outright, before any call is attempted, when the real
    /// `.afb` would dispatch through a path `run_afb_bytes` does not fully
    /// bound (rule 4) - this is the definitive check, independent of
    /// whatever `crate::pack::SUPPORTED_PLUGIN_LANGUAGES` already screened
    /// at manifest-authoring time, because a plugin can arrive as a raw
    /// `.afb` without ever passing through that validator.
    pub fn compile(afb_bytes: &[u8], plugin: &PackPlugin) -> Result<Self> {
        Self::compile_within(afb_bytes, plugin, &Self::CEILING)
    }

    /// The same, under a ceiling the caller supplies.
    ///
    /// This is the seam an admitting caller uses. Effective authority is
    /// the intersection of what the plugin declared and what the operator
    /// allows, so a ceiling can only ever take capability away: passing a
    /// wide one does not grant anything the plugin did not ask for, and
    /// passing none at all leaves [`Self::compile`]'s sealed default.
    ///
    /// It exists as its own entry point rather than as a field on the
    /// budget because authority is decided once, when a plugin is admitted,
    /// and a per-call knob would invite deciding it again at the call.
    pub fn compile_within(
        afb_bytes: &[u8],
        plugin: &PackPlugin,
        ceiling: &Manifold,
    ) -> Result<Self> {
        let afb = afterburner_afb::Afb::from_bytes(afb_bytes).map_err(|error| {
            anyhow::anyhow!(
                "plugin {:?}'s artifact is not a readable .afb: {error}",
                plugin.name
            )
        })?;
        let declared: Manifold = match &plugin.manifold {
            Some(value) => serde_json::from_value(value.clone()).with_context(|| {
                format!(
                    "plugin {:?} declares a manifold that is not one",
                    plugin.name
                )
            })?,
            // A plugin that declared no manifold asks for nothing, which
            // is the right default for a pure transform (rule 2).
            None => Manifold::sealed(),
        };
        // Narrowed first: what a call actually asks for is the *granted*
        // manifold, so gating on the declared one would refuse a plugin
        // over authority the ceiling had already taken away.
        let manifold = narrow_manifold(&declared, ceiling);
        if let Some(reason) = unbounded_dispatch_reason(&afb, &manifold) {
            anyhow::bail!(
                "plugin {:?} cannot be admitted: this artifact would run without every declared \
                 bound enforced ({reason})",
                plugin.name
            );
        }

        Ok(Self {
            afb_bytes: afb_bytes.to_vec(),
            plugin: plugin.clone(),
            manifold,
        })
    }

    /// Runs it once with the given arguments.
    pub fn call(
        &self,
        arguments: &serde_json::Value,
        budget: &PluginBudget,
    ) -> Result<PluginOutcome> {
        // Re-checked at every call, not just once at `compile` time: a
        // plugin is called, never a server (rule 3), so a granted manifold
        // that somehow carried a listen grant must never reach a run.
        // `narrow_manifold` already guarantees this; this is the second,
        // independent place that would have to break for it to matter.
        debug_assert!(
            matches!(self.manifold.listen, ListenAccess::None),
            "a granted manifold must never carry a listen capability"
        );

        let stdin =
            serde_json::to_vec(arguments).context("encoding plugin arguments as canonical JSON")?;
        let request = AfbRunRequest {
            stdin,
            manifold: self.manifold.clone(),
            fuel: Some(budget.fuel),
            memory_bytes: Some(budget.memory_bytes),
            timeout: Some(budget.wall_clock),
            ..Default::default()
        };

        let started = Instant::now();
        let output = run_afb_bytes(&self.afb_bytes, request)
            .with_context(|| format!("plugin {:?} failed to start", self.plugin.name))?;
        let wall_ms = elapsed_ms(started);

        match output.outcome {
            AfbRunOutcome::OutOfFuel => Ok(PluginOutcome {
                verdict: PluginVerdict::OutOfFuel,
                output: serde_json::Value::Null,
                diagnostics: "the plugin exhausted its fuel budget".to_owned(),
                fuel_used: output.fuel_used,
                wall_ms,
            }),
            AfbRunOutcome::OutOfMemory => Ok(PluginOutcome {
                verdict: PluginVerdict::OutOfMemory,
                output: serde_json::Value::Null,
                diagnostics: "the plugin exceeded its memory budget".to_owned(),
                fuel_used: output.fuel_used,
                wall_ms,
            }),
            AfbRunOutcome::Timeout => Ok(PluginOutcome {
                verdict: PluginVerdict::Timeout,
                output: serde_json::Value::Null,
                diagnostics: format!(
                    "the plugin did not answer within its {}ms wall-clock budget; it may still \
                     be running in the background until its fuel runs out",
                    budget.wall_clock.as_millis()
                ),
                fuel_used: output.fuel_used,
                wall_ms,
            }),
            AfbRunOutcome::Trapped(message) => Err(anyhow::anyhow!(
                "plugin {:?} trapped: {message}",
                self.plugin.name
            )),
            AfbRunOutcome::Exited(code) => Ok(outcome_from_exit(code, output, wall_ms)),
        }
    }

    /// What the model is shown for this plugin.
    pub fn definition(&self) -> &PackPlugin {
        &self.plugin
    }
}

/// What one call to this artifact would ask for that its dispatch path
/// cannot actually enforce.
///
/// `None` means every bound this runner applies is real; `Some(reason)`
/// names, in one sentence, the axes that would be silently dropped - the
/// message [`PluginRunner::compile_within`] refuses with.
///
/// The classification is Afterburner's own
/// ([`afterburner::afb_run::bounds_for`], the same table
/// `run_afb_bytes` dispatches on), never re-derived here. This file only
/// contributes the half it actually knows: which axes a plugin call asks
/// for. An earlier version did re-derive the classification from the
/// manifest's language and runtime target, and that copy drifted - it went
/// on refusing Python after Python's bounds had been wired, so a language
/// that ran fully bounded was reported as unsafe.
fn unbounded_dispatch_reason(afb: &afterburner_afb::Afb, granted: &Manifold) -> Option<String> {
    let supported = afterburner::afb_run::bounds_for(afb);
    let mut missing: Vec<&str> = Vec::new();

    // Every call carries its arguments on stdin and all three budget axes
    // (see [`PluginRunner::call`]), so these are always asked for.
    if !supported.stdin {
        missing.push("the arguments on stdin");
    }
    if !supported.fuel {
        missing.push("the fuel ceiling");
    }
    if !supported.memory_bytes {
        missing.push("the memory ceiling");
    }
    if !supported.timeout {
        missing.push("the wall-clock budget");
    }

    // The manifold axes are asked for only when this plugin was actually
    // granted them. An empty grant list is not a grant.
    match &granted.fs {
        FsAccess::ReadOnly(paths) if !paths.is_empty() && !supported.manifold_fs_ro => {
            missing.push("the read-only filesystem grant")
        }
        FsAccess::ReadWrite(paths) if !paths.is_empty() && !supported.manifold_fs_rw => {
            missing.push("the read-write filesystem grant")
        }
        _ => {}
    }
    if !matches!(granted.env, EnvAccess::None) && !supported.manifold_env {
        missing.push("the environment grant");
    }

    if missing.is_empty() {
        return None;
    }
    Some(format!("{} would not be enforced", missing.join(", ")))
}

/// Validates and bounds a completed run's stdout/stderr (rules 5 and 6).
fn outcome_from_exit(
    code: i32,
    output: afterburner::afb_run::AfbRunOutput,
    wall_ms: u64,
) -> PluginOutcome {
    let mut notes = Vec::new();
    let stderr = bound(&output.stderr, MAX_STDERR_BYTES, "stderr", &mut notes);
    let diagnostics_base = String::from_utf8_lossy(stderr).into_owned();
    if code != 0 {
        notes.push(format!("plugin exited with code {code}"));
    }

    if output.stdout.len() > MAX_STDOUT_BYTES {
        notes.push(format!(
            "stdout truncated to {MAX_STDOUT_BYTES} of {} bytes; a plugin result is a small JSON \
             value, not a file, so a result this large is refused rather than accepted partial",
            output.stdout.len()
        ));
        return PluginOutcome {
            verdict: PluginVerdict::BadOutput,
            output: serde_json::Value::Null,
            diagnostics: append_notes(diagnostics_base, &notes),
            fuel_used: output.fuel_used,
            wall_ms,
        };
    }

    match serde_json::from_slice::<serde_json::Value>(&output.stdout) {
        Ok(value) => PluginOutcome {
            verdict: PluginVerdict::Success,
            output: value,
            diagnostics: append_notes(diagnostics_base, &notes),
            fuel_used: output.fuel_used,
            wall_ms,
        },
        Err(error) => {
            notes.push(format!("stdout is not a single JSON value: {error}"));
            PluginOutcome {
                verdict: PluginVerdict::BadOutput,
                output: serde_json::Value::Null,
                diagnostics: append_notes(diagnostics_base, &notes),
                fuel_used: output.fuel_used,
                wall_ms,
            }
        }
    }
}

/// Caps `bytes` at `max`, recording a truncation note when it does
/// (rule 5: "say so in the diagnostics when you truncate").
fn bound<'a>(bytes: &'a [u8], max: usize, label: &str, notes: &mut Vec<String>) -> &'a [u8] {
    if bytes.len() > max {
        notes.push(format!(
            "{label} truncated to {max} of {} bytes",
            bytes.len()
        ));
        &bytes[..max]
    } else {
        bytes
    }
}

fn append_notes(base: String, notes: &[String]) -> String {
    if notes.is_empty() {
        base
    } else if base.is_empty() {
        notes.join("; ")
    } else {
        format!("{base} ({})", notes.join("; "))
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// The manifold actually granted: never wider than `declared`, never
/// wider than `ceiling`, on every axis, and `listen` forced to `None`
/// regardless of either side (rule 2 and rule 3).
///
/// Written out field by field, mirroring `pack_archive::union_manifold`'s
/// own shape (that function computes the wider of two grants for a
/// package's declared manifold; this one computes the narrower, for what
/// a single call is actually allowed), so a capability axis added to
/// `Manifold` upstream fails this to compile instead of silently
/// defaulting one way or the other.
fn narrow_manifold(declared: &Manifold, ceiling: &Manifold) -> Manifold {
    /// Two host allow-lists narrow to their common hosts; `None` means
    /// "any host" (the wider option), so it defers to the other side.
    fn narrower_hosts(a: &Option<Vec<String>>, b: &Option<Vec<String>>) -> Option<Vec<String>> {
        match (a, b) {
            (None, other) | (other, None) => other.clone(),
            (Some(a), Some(b)) => {
                let mut merged: Vec<String> =
                    a.iter().filter(|host| b.contains(host)).cloned().collect();
                merged.sort();
                merged.dedup();
                Some(merged)
            }
        }
    }

    /// An empty root list means "every root" (`Manifold::open`'s own
    /// convention); the narrower side wins when one is empty, and two
    /// finite lists narrow to their common roots.
    fn narrower_roots(a: &[PathBuf], b: &[PathBuf]) -> Vec<PathBuf> {
        match (a.is_empty(), b.is_empty()) {
            (true, true) => Vec::new(),
            (true, false) => b.to_vec(),
            (false, true) => a.to_vec(),
            (false, false) => {
                let mut merged: Vec<PathBuf> =
                    a.iter().filter(|root| b.contains(root)).cloned().collect();
                merged.sort();
                merged.dedup();
                merged
            }
        }
    }

    Manifold {
        fs: match (&declared.fs, &ceiling.fs) {
            (FsAccess::None, _) | (_, FsAccess::None) => FsAccess::None,
            (FsAccess::ReadOnly(a), FsAccess::ReadOnly(b)) => {
                FsAccess::ReadOnly(narrower_roots(a, b))
            }
            (FsAccess::ReadOnly(a), FsAccess::ReadWrite(b))
            | (FsAccess::ReadWrite(a), FsAccess::ReadOnly(b)) => {
                FsAccess::ReadOnly(narrower_roots(a, b))
            }
            (FsAccess::ReadWrite(a), FsAccess::ReadWrite(b)) => {
                FsAccess::ReadWrite(narrower_roots(a, b))
            }
        },
        net: match (&declared.net, &ceiling.net) {
            (NetAccess::None, _) | (_, NetAccess::None) => NetAccess::None,
            (NetAccess::OutboundHttp(a), NetAccess::OutboundHttp(b)) => {
                NetAccess::OutboundHttp(narrower_hosts(a, b))
            }
            (NetAccess::OutboundHttp(a), NetAccess::OutboundFull(b))
            | (NetAccess::OutboundFull(a), NetAccess::OutboundHttp(b)) => {
                NetAccess::OutboundHttp(narrower_hosts(a, b))
            }
            (NetAccess::OutboundFull(a), NetAccess::OutboundFull(b)) => {
                NetAccess::OutboundFull(narrower_hosts(a, b))
            }
        },
        env: match (&declared.env, &ceiling.env) {
            (EnvAccess::None, _) | (_, EnvAccess::None) => EnvAccess::None,
            (EnvAccess::Full, EnvAccess::Full) => EnvAccess::Full,
            (EnvAccess::Full, EnvAccess::AllowList(keys))
            | (EnvAccess::AllowList(keys), EnvAccess::Full) => EnvAccess::AllowList(keys.clone()),
            (EnvAccess::AllowList(a), EnvAccess::AllowList(b)) => {
                let mut merged: Vec<String> =
                    a.iter().filter(|key| b.contains(key)).cloned().collect();
                merged.sort();
                merged.dedup();
                EnvAccess::AllowList(merged)
            }
        },
        // A plugin is called, never a server (rule 3): forced here
        // regardless of what either side asked, not merely because
        // `PackPlugin::validate` already refused a declared `listen`
        // upstream - this function is a correct ceiling on its own.
        listen: ListenAccess::None,
        crypto: declared.crypto && ceiling.crypto,
        child_process: declared.child_process && ceiling.child_process,
        allow_exit: declared.allow_exit && ceiling.allow_exit,
        http_timeout_ms: match (declared.http_timeout_ms, ceiling.http_timeout_ms) {
            (None, other) | (other, None) => other,
            (Some(a), Some(b)) => Some(a.min(b)),
        },
    }
}

#[cfg(test)]
mod tests;
