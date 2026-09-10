//! Running one of a pack's tools.
//!
//! A tool is a compiled WASM command guest ([`crate::pack::PackTool`]).
//! The ABI is deliberately narrow: canonical JSON arguments arrive on
//! stdin, a single JSON value is written to stdout, and anything on
//! stderr is diagnostics for a human. Nothing else carries the call: no
//! second channel, no shared memory, no side import for passing data.
//!
//! Six invariants hold for every call, each enforced here rather than
//! trusted from the caller:
//!
//! 1. **One turn per instantiation.** [`EmbedderVm::run_command`] builds a
//!    fresh `wasmtime::Store` per call (its own module doc: "no linker
//!    re-walk, no import re-typecheck" describes the compile-once,
//!    instantiate-per-call split); this file never reuses one across two
//!    [`PackToolRunner::call`]s, so a tool keeps nothing between them.
//! 2. **The manifold is a ceiling, narrowed, never widened.**
//!    [`narrow_manifold`] intersects what the tool declared
//!    (`PackTool::manifold`) with what this runner is willing to grant,
//!    on every axis, and forces `listen` to `None` regardless of either
//!    side. See [`PackToolRunner::compile`] for why the ceiling is
//!    `Manifold::sealed()` today.
//! 3. **A tool never listens.** WASI preview 1 command modules (what
//!    [`EmbedderVm::run_command`] runs) have no socket import wired at
//!    all, so this holds structurally; [`narrow_manifold`] enforces it a
//!    second time regardless, as defense in depth against a manifold
//!    that somehow reached this file unvalidated.
//! 4. **Every bound is real.** `budget.fuel` and `budget.memory_bytes` go
//!    straight to [`EmbedderVm::run_command`]; `budget.wall_clock` is
//!    enforced by racing the run against a channel receive, since
//!    `run_command` is fuel-bounded but not preemptible. Stdout and
//!    stderr are each capped after the run, truncation is named in
//!    `diagnostics`, never silently applied.
//! 5. **Output is validated, `input_schema` is not.** Stdout that is not
//!    exactly one JSON value is [`ToolVerdict::BadOutput`] naming why.
//!    `input_schema` describes the *arguments*, not the result, so this
//!    file never checks arguments against it; a caller that wants that
//!    checks before calling and reports [`ToolVerdict::Refused`] itself.
//! 6. **Failures are typed, never silent.** A routine bound (fuel,
//!    memory, wall clock, malformed output) is a [`ToolOutcome`] with its
//!    own verdict; anything else the guest does (an out-of-bounds access,
//!    an unreachable trap, a missing export) is a hard `Err` from
//!    [`PackToolRunner::call`], never swallowed into a generic verdict.
//!
//! ## Honest gaps
//!
//! [`ToolVerdict::OutOfMemory`] exists and is wired from
//! `AfterburnerError::MemoryLimit`, but that variant is not reachable
//! through [`EmbedderVm::run_command`] today: a `memory.grow` past
//! `WasiCommandOpts::max_memory_bytes` is denied by returning `-1` to the
//! guest (an ordinary allocation failure the guest's own allocator
//! observes), never a trap or a typed error
//! (`afterburner_wasi::embedder_vm`'s own test,
//! `run_command_max_memory_bytes_denies_growth_past_the_cap`, is the
//! proof). `gc_sandbox::sandbox::TurnFailure::MemoryExceeded` documents
//! the identical gap for the same reason. The memory ceiling is still
//! real (a guest genuinely cannot grow past it), it just has no WAT-only
//! test that reaches a distinct `OutOfMemory` verdict rather than the
//! guest's own reaction to a denied grow.

use std::path::PathBuf;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::time::{Duration, Instant};

use afterburner_core::manifold::{EnvAccess, FsAccess, ListenAccess, Manifold, NetAccess};
use afterburner_core::AfterburnerError;
use afterburner_wasi::embedder_vm::{
    EmbedderModule, EmbedderRunOutput, EmbedderVm, WasiCommandOpts,
};
use anyhow::{Context, Result};

use crate::pack::PackTool;
use crate::pack_archive::PackAfb;

/// Bytes of a tool's JSON result kept before the call is judged
/// [`ToolVerdict::BadOutput`] for running past this bound. A result is a
/// small JSON value describing one call's outcome, not a file; anything
/// larger is treated as a bad result rather than silently accepted
/// truncated (rule 4 and rule 5 in this module's own doc).
const MAX_STDOUT_BYTES: usize = 1024 * 1024;

/// Bytes of a tool's stderr kept for a human to read. Diagnostics, not a
/// log archive.
const MAX_STDERR_BYTES: usize = 64 * 1024;

/// What a tool may spend on one call.
#[derive(Debug, Clone, Copy)]
pub struct ToolBudget {
    /// Wasmtime fuel: a deterministic instruction budget, the real
    /// backstop against a guest that never returns.
    pub fuel: u64,
    /// Linear-memory ceiling in bytes, enforced by wasmtime on every
    /// `memory.grow` (see this module's own "Honest gaps" note for how a
    /// denied grow surfaces to the guest).
    pub memory_bytes: u64,
    /// Wall-clock deadline for the whole call. `run_command` cannot be
    /// preempted mid-instruction, so this is a reporting deadline: a
    /// guest that ignores it keeps running on a detached thread until its
    /// own fuel runs out, exactly like
    /// `gc_sandbox::sandbox::PinnedFiber::run_command`'s own documented
    /// choice for the identical reason.
    pub wall_clock: Duration,
}

impl Default for ToolBudget {
    /// `fuel` mirrors `afterburner_wasi::embedder_vm`'s own default
    /// instruction budget (100 million, "generous enough for unit
    /// tests"); `memory_bytes` (64 MiB) is comfortable for a small
    /// compiled transform without giving a runaway allocation the run of
    /// the host; `wall_clock` (5 s) is long enough for a cold-ish
    /// compute-bound call and short enough that a caller waiting on a
    /// tool result is not left hanging.
    fn default() -> Self {
        Self {
            fuel: 100_000_000,
            memory_bytes: 64 * 1024 * 1024,
            wall_clock: Duration::from_secs(5),
        }
    }
}

/// Why a call did not return the tool's own JSON value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolVerdict {
    /// The tool ran and produced exactly one JSON value.
    Success,
    /// A caller's own pre-call check (e.g. `input_schema` validation)
    /// declined to make the call. Never produced by [`PackToolRunner`]
    /// itself; part of the shared vocabulary so a caller can report a
    /// refusal in the same [`ToolOutcome`] shape as a real call would
    /// have used (rule 5 in this module's own doc).
    Refused,
    /// `budget.fuel` ran out before the tool returned.
    OutOfFuel,
    /// `budget.memory_bytes` was exceeded. See this module's own "Honest
    /// gaps" note: not reachable through `EmbedderVm::run_command` today.
    OutOfMemory,
    /// `budget.wall_clock` elapsed before the tool returned.
    Timeout,
    /// The tool ran but stdout was not exactly one JSON value (not
    /// parseable, more than one value, or truncated past the output
    /// bound).
    BadOutput,
}

/// What came back from one tool call.
#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub verdict: ToolVerdict,
    /// The tool's own JSON value on [`ToolVerdict::Success`];
    /// [`serde_json::Value::Null`] for every other verdict.
    pub output: serde_json::Value,
    /// Bounded stderr text, for a human. Carries a truncation note when
    /// stdout or stderr ran past this module's own bound.
    pub diagnostics: String,
    /// Fuel actually consumed, when the underlying VM reports it. On the
    /// [`ToolVerdict::OutOfFuel`] path this is `budget.fuel` (exhaustion
    /// means the whole budget was spent, by definition); on the
    /// [`ToolVerdict::Timeout`] and [`ToolVerdict::OutOfMemory`] paths
    /// the VM's public API exposes no partial count, so this is `0` -
    /// not a measurement, an honest "not available here".
    pub fuel_used: u64,
    pub wall_ms: u64,
}

/// A compiled tool, ready to be called more than once.
pub struct PackToolRunner {
    vm: Arc<EmbedderVm>,
    module: Arc<EmbedderModule>,
    tool: PackTool,
    /// What this runner will actually grant a call: the intersection of
    /// what the tool declared and [`Self::CEILING`]. Not yet threaded
    /// into `WasiCommandOpts` (nothing beyond stdin/stdout/stderr crosses
    /// this tool ABI today, this module's own doc); kept so
    /// `narrow_manifold`'s narrowing is a real, checked step of
    /// `compile`, not merely a function that exists and is never called.
    manifold: Manifold,
}

impl std::fmt::Debug for PackToolRunner {
    /// Names the tool, never the compiled bytes.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackToolRunner")
            .field("tool", &self.tool.name)
            .finish()
    }
}

impl PackToolRunner {
    /// This runner's own capability ceiling. `compile`'s fixed signature
    /// (`pack`, `tool_name`) carries no admission-time ceiling from a
    /// caller, and the tool ABI itself carries nothing beyond
    /// stdin/stdout/stderr (this module's own doc), so the honest ceiling
    /// today is "nothing". `narrow_manifold` takes the ceiling as an
    /// ordinary argument; a caller that gains a real admission ceiling to
    /// enforce plugs it in in place of this constant, and every declared
    /// manifold is narrowed against it exactly the way it already is here.
    const CEILING: Manifold = Manifold::sealed();

    /// Compiles one tool out of a pack.
    ///
    /// `pack.tool_module` looks the module up by this exact tool's
    /// declared asset path inside a manifest `PackAfb` already validated
    /// on construction (`PackAfb::from_afb`'s own doc: a module cannot be
    /// swapped underneath the name it was admitted under). Fetching the
    /// module through that accessor, rather than reading `tool.module` by
    /// hand, is what makes the module compiled below provably the one
    /// the pack declares for `tool_name`.
    pub fn compile(pack: &PackAfb, tool_name: &str) -> Result<Self> {
        Self::compile_within(pack, tool_name, &Self::CEILING)
    }

    /// The same, under a ceiling the caller supplies.
    ///
    /// This is the seam an admitting caller uses. Effective authority is
    /// the intersection of what the tool declared and what the operator
    /// allows, so a ceiling can only ever take capability away: passing a
    /// wide one does not grant anything the tool did not ask for, and
    /// passing none at all leaves [`Self::compile`]'s sealed default.
    ///
    /// It exists as its own entry point rather than as a field on the
    /// budget because authority is decided once, when a tool is admitted,
    /// and a per-call knob would invite deciding it again at the call.
    pub fn compile_within(pack: &PackAfb, tool_name: &str, ceiling: &Manifold) -> Result<Self> {
        let tool = pack
            .tools()
            .iter()
            .find(|candidate| candidate.name == tool_name)
            .with_context(|| format!("this pack ships no tool called {tool_name:?}"))?
            .clone();
        let wasm = pack.tool_module(tool_name)?;

        let declared: Manifold = match &tool.manifold {
            Some(value) => serde_json::from_value(value.clone()).with_context(|| {
                format!("tool {tool_name:?} declares a manifold that is not one")
            })?,
            // A tool that declared no manifold asks for nothing, which is
            // the right default for a pure transform (rule 2).
            None => Manifold::sealed(),
        };
        let manifold = narrow_manifold(&declared, ceiling);

        let vm = EmbedderVm::new().context("starting the tool sandbox engine")?;
        let module = vm
            .compile(wasm, true, |_| Ok(()))
            .map_err(|error| anyhow::anyhow!("compiling tool {tool_name:?}: {error}"))?;

        Ok(Self {
            vm: Arc::new(vm),
            module: Arc::new(module),
            tool,
            manifold,
        })
    }

    /// Runs it once with the given arguments.
    pub fn call(&self, arguments: &serde_json::Value, budget: &ToolBudget) -> Result<ToolOutcome> {
        // Re-checked at every call, not just once at `compile` time: a
        // tool is called, never a server (rule 3), so a granted manifold
        // that somehow carried a listen grant must never reach a run.
        // `narrow_manifold` already guarantees this; this is the second,
        // independent place that would have to break for it to matter.
        debug_assert!(
            matches!(self.manifold.listen, ListenAccess::None),
            "a granted manifold must never carry a listen capability"
        );

        let stdin =
            serde_json::to_vec(arguments).context("encoding tool arguments as canonical JSON")?;
        let opts = WasiCommandOpts::new()
            .stdin(stdin)
            .max_memory_bytes(budget.memory_bytes as usize);

        let vm = self.vm.clone();
        let module = self.module.clone();
        let fuel = budget.fuel;
        let (tx, rx) = std::sync::mpsc::channel();
        // A detached thread, not a joined one: `run_command` cannot be
        // preempted mid-instruction (no epoch ticker;
        // `afterburner_wasi::embedder_vm`'s own module doc), so a guest
        // that ignores `budget.wall_clock` keeps running here until its
        // own fuel runs out. Joining would turn "the call is over" into
        // "wait for the runaway guest anyway", defeating the timeout.
        std::thread::spawn(move || {
            let _ = tx.send(vm.run_command(&module, opts, Some(fuel)));
        });

        let started = Instant::now();
        match rx.recv_timeout(budget.wall_clock) {
            Ok(run) => outcome_from_run(run, budget.fuel, elapsed_ms(started), &self.tool.name),
            Err(RecvTimeoutError::Timeout) => Ok(ToolOutcome {
                verdict: ToolVerdict::Timeout,
                output: serde_json::Value::Null,
                diagnostics: format!(
                    "the tool did not answer within its {}ms wall-clock budget; it may still be \
                     running in the background until its fuel runs out",
                    budget.wall_clock.as_millis()
                ),
                fuel_used: 0,
                wall_ms: elapsed_ms(started),
            }),
            Err(RecvTimeoutError::Disconnected) => Err(anyhow::anyhow!(
                "the tool sandbox thread for {:?} ended without reporting a result",
                self.tool.name
            )),
        }
    }

    /// What the model is shown for this tool.
    pub fn definition(&self) -> &PackTool {
        &self.tool
    }
}

/// Turns one VM run (or the typed error it failed with) into a
/// [`ToolOutcome`], or a hard `Err` for anything outside the routine
/// bounds this module models as its own verdict (rule 6).
fn outcome_from_run(
    run: std::result::Result<EmbedderRunOutput, AfterburnerError>,
    budget_fuel: u64,
    wall_ms: u64,
    tool_name: &str,
) -> Result<ToolOutcome> {
    match run {
        Ok(output) => Ok(outcome_from_output(output, wall_ms)),
        Err(AfterburnerError::FuelExhausted) => Ok(ToolOutcome {
            verdict: ToolVerdict::OutOfFuel,
            output: serde_json::Value::Null,
            diagnostics: "the tool exhausted its fuel budget".to_owned(),
            fuel_used: budget_fuel,
            wall_ms,
        }),
        Err(AfterburnerError::MemoryLimit) => Ok(ToolOutcome {
            verdict: ToolVerdict::OutOfMemory,
            output: serde_json::Value::Null,
            diagnostics: "the tool exceeded its memory budget".to_owned(),
            fuel_used: 0,
            wall_ms,
        }),
        Err(AfterburnerError::Timeout) => Ok(ToolOutcome {
            verdict: ToolVerdict::Timeout,
            output: serde_json::Value::Null,
            diagnostics: "the tool exceeded its wall-clock budget".to_owned(),
            fuel_used: 0,
            wall_ms,
        }),
        Err(other) => Err(anyhow::anyhow!("tool {tool_name:?} trapped: {other}")),
    }
}

/// Validates and bounds a completed run's stdout/stderr (rules 4 and 5).
fn outcome_from_output(output: EmbedderRunOutput, wall_ms: u64) -> ToolOutcome {
    let mut notes = Vec::new();
    let stderr = bound(&output.stderr, MAX_STDERR_BYTES, "stderr", &mut notes);
    let diagnostics_base = String::from_utf8_lossy(stderr).into_owned();
    if output.result != 0 {
        notes.push(format!("tool exited with code {}", output.result));
    }

    if output.stdout.len() > MAX_STDOUT_BYTES {
        notes.push(format!(
            "stdout truncated to {MAX_STDOUT_BYTES} of {} bytes; a tool result is a small JSON \
             value, not a file, so a result this large is refused rather than accepted partial",
            output.stdout.len()
        ));
        return ToolOutcome {
            verdict: ToolVerdict::BadOutput,
            output: serde_json::Value::Null,
            diagnostics: append_notes(diagnostics_base, &notes),
            fuel_used: output.fuel_consumed,
            wall_ms,
        };
    }

    match serde_json::from_slice::<serde_json::Value>(&output.stdout) {
        Ok(value) => ToolOutcome {
            verdict: ToolVerdict::Success,
            output: value,
            diagnostics: append_notes(diagnostics_base, &notes),
            fuel_used: output.fuel_consumed,
            wall_ms,
        },
        Err(error) => {
            notes.push(format!("stdout is not a single JSON value: {error}"));
            ToolOutcome {
                verdict: ToolVerdict::BadOutput,
                output: serde_json::Value::Null,
                diagnostics: append_notes(diagnostics_base, &notes),
                fuel_used: output.fuel_consumed,
                wall_ms,
            }
        }
    }
}

/// Caps `bytes` at `max`, recording a truncation note when it does
/// (rule 4: "say so in the diagnostics when you truncate").
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
        // A tool is called, never a server (rule 3): forced here
        // regardless of what either side asked, not merely because
        // `PackTool::validate` already refused a declared `listen`
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
mod tests {
    use super::*;
    use crate::pack_archive::{pack_dir, PublishAs};

    fn wat(src: &str) -> Vec<u8> {
        wat::parse_str(src).expect("WAT parses")
    }

    /// Builds a one-tool pack `.afb` around a compiled Wasm module, so
    /// `PackToolRunner` sees exactly what a real installed pack ships (a
    /// `.wasm` asset, not WAT text on disk) even though the module itself
    /// is authored as WAT here so the tests need no external toolchain.
    fn build_tool_pack(
        pack_name: &str,
        wat_source: &str,
        manifold: Option<serde_json::Value>,
    ) -> PackAfb {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join(pack_name);
        std::fs::create_dir_all(root.join("tools")).expect("mkdir");
        std::fs::write(root.join("README.md"), b"# test pack").expect("write");
        std::fs::write(root.join("tools/tool.wasm"), wat(wat_source)).expect("write");

        let mut tool = serde_json::json!({
            "name": "tool",
            "description": "a test tool",
            "module": "tools/tool.wasm",
            "input_schema": {"type": "object"},
        });
        if let Some(manifold) = manifold {
            tool["manifold"] = manifold;
        }
        let manifest = serde_json::json!({
            "manifest_version": 1,
            "name": pack_name,
            "version": "0.1.0",
            "description": "a test pack",
            "authors": ["test"],
            "tags": [],
            "kind": "tools",
            "assets": ["README.md", "tools/tool.wasm"],
            "tools": [tool],
        });
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).expect("encode"),
        )
        .expect("write");

        let (bytes, _) = pack_dir(&root, &PublishAs::default()).expect("packing");
        PackAfb::from_bytes(&bytes).expect("reading the pack back")
    }

    /// Reads fd 0 in one shot and writes exactly what it read to fd 1:
    /// the identity tool, and the vehicle for every "does the ABI carry
    /// arguments through" test below.
    const ECHO_WAT: &str = r#"
      (module
        (import "wasi_snapshot_preview1" "fd_read"
          (func $fd_read (param i32 i32 i32 i32) (result i32)))
        (import "wasi_snapshot_preview1" "fd_write"
          (func $fd_write (param i32 i32 i32 i32) (result i32)))
        (memory (export "memory") 1)
        (func (export "_start")
          ;; iovec at 256: buf=0, buf_len=256
          i32.const 256  i32.const 0    i32.store
          i32.const 260  i32.const 256  i32.store
          ;; fd_read(fd=0, iovs_ptr=256, iovs_len=1, nread_ptr=264)
          i32.const 0
          i32.const 256
          i32.const 1
          i32.const 264
          call $fd_read
          drop
          ;; reuse the iovec, buf_len = bytes actually read
          i32.const 260  i32.const 264 i32.load  i32.store
          ;; fd_write(fd=1, iovs_ptr=256, iovs_len=1, nwritten_ptr=268)
          i32.const 1
          i32.const 256
          i32.const 1
          i32.const 268
          call $fd_write
          drop))
    "#;

    #[test]
    fn echo_tool_returns_exactly_its_input() {
        let pack = build_tool_pack("echo_pack", ECHO_WAT, None);
        let runner = PackToolRunner::compile(&pack, "tool").expect("compiles");
        assert_eq!(runner.definition().name, "tool");

        let arguments = serde_json::json!({"hello": "world", "n": 42});
        let outcome = runner
            .call(&arguments, &ToolBudget::default())
            .expect("call succeeds");
        assert_eq!(outcome.verdict, ToolVerdict::Success);
        assert_eq!(outcome.output, arguments);
    }

    /// Rule 1: every call gets a fresh instance, so two calls on the same
    /// compiled runner never see each other's arguments.
    #[test]
    fn two_calls_on_one_runner_never_see_each_others_arguments() {
        let pack = build_tool_pack("echo_twice_pack", ECHO_WAT, None);
        let runner = PackToolRunner::compile(&pack, "tool").expect("compiles");

        let first = runner
            .call(&serde_json::json!({"call": 1}), &ToolBudget::default())
            .expect("first call");
        let second = runner
            .call(&serde_json::json!({"call": 2}), &ToolBudget::default())
            .expect("second call");

        assert_eq!(first.output, serde_json::json!({"call": 1}));
        assert_eq!(second.output, serde_json::json!({"call": 2}));
    }

    /// Rule 5: stdout that is not a single JSON value is `BadOutput`,
    /// naming why, never a silent empty result.
    #[test]
    fn non_json_stdout_is_bad_output() {
        let wat_source = r#"
          (module
            (import "wasi_snapshot_preview1" "fd_write"
              (func $fd_write (param i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "not json")
            (func (export "_start")
              i32.const 16  i32.const 0  i32.store
              i32.const 20  i32.const 8  i32.store
              i32.const 1
              i32.const 16
              i32.const 1
              i32.const 24
              call $fd_write
              drop))
        "#;
        let pack = build_tool_pack("bad_output_pack", wat_source, None);
        let runner = PackToolRunner::compile(&pack, "tool").expect("compiles");

        let outcome = runner
            .call(&serde_json::json!({}), &ToolBudget::default())
            .expect("call succeeds at the VM level");
        assert_eq!(outcome.verdict, ToolVerdict::BadOutput);
        assert_eq!(outcome.output, serde_json::Value::Null);
        assert!(
            outcome.diagnostics.contains("not a single JSON value"),
            "diagnostics: {}",
            outcome.diagnostics
        );
    }

    /// Rule 4: a tool that loops forever is bounded by fuel, and the
    /// exhaustion is its own verdict, not a generic failure.
    #[test]
    fn an_infinite_loop_exhausts_its_fuel_budget() {
        let wat_source = r#"
          (module
            (func (export "_start")
              (loop $forever
                br $forever)))
        "#;
        let pack = build_tool_pack("fuel_pack", wat_source, None);
        let runner = PackToolRunner::compile(&pack, "tool").expect("compiles");

        let budget = ToolBudget {
            fuel: 10_000,
            ..ToolBudget::default()
        };
        let outcome = runner
            .call(&serde_json::json!({}), &budget)
            .expect("call reports fuel exhaustion, not a hard error");
        assert_eq!(outcome.verdict, ToolVerdict::OutOfFuel);
        assert_eq!(
            outcome.fuel_used, 10_000,
            "exhaustion means the whole budget was spent"
        );
    }

    /// Rule 4: `budget.wall_clock` is a real, independent bound, not
    /// merely a description of the fuel bound. Fuel is set far larger
    /// than what could exhaust within the wall-clock deadline (so the
    /// deadline reliably wins the race) but still finite (so the
    /// detached background thread this call leaves running eventually
    /// exhausts its own fuel and exits, rather than spinning forever).
    #[test]
    fn a_slow_tool_is_stopped_by_its_wall_clock_budget() {
        let wat_source = r#"
          (module
            (func (export "_start")
              (loop $forever
                br $forever)))
        "#;
        let pack = build_tool_pack("timeout_pack", wat_source, None);
        let runner = PackToolRunner::compile(&pack, "tool").expect("compiles");

        let budget = ToolBudget {
            fuel: 5_000_000_000,
            memory_bytes: ToolBudget::default().memory_bytes,
            wall_clock: Duration::from_millis(30),
        };
        let outcome = runner
            .call(&serde_json::json!({}), &budget)
            .expect("call reports a timeout, not a hard error");
        assert_eq!(outcome.verdict, ToolVerdict::Timeout);
    }

    /// Rule 2: a tool that declared no manifold gets no capabilities. Fd
    /// 3 only exists when a preopen was configured; this runner never
    /// configures one, so the guest observes a `fd_write` failure on it
    /// regardless of what it might have wanted.
    #[test]
    fn a_tool_with_no_declared_manifold_gets_no_filesystem() {
        let wat_source = r#"
          (module
            (import "wasi_snapshot_preview1" "fd_write"
              (func $fd_write (param i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "{\"ok\":true}")
            (data (i32.const 16) "{\"ok\":false}")
            (func (export "_start")
              (local $errno i32)
              i32.const 64  i32.const 32  i32.store
              i32.const 68  i32.const 4   i32.store
              i32.const 3
              i32.const 64
              i32.const 1
              i32.const 72
              call $fd_write
              local.set $errno
              (if (i32.eq (local.get $errno) (i32.const 0))
                (then
                  i32.const 80  i32.const 0   i32.store
                  i32.const 84  i32.const 11  i32.store)
                (else
                  i32.const 80  i32.const 16  i32.store
                  i32.const 84  i32.const 12  i32.store))
              i32.const 1
              i32.const 80
              i32.const 1
              i32.const 88
              call $fd_write
              drop))
        "#;
        let pack = build_tool_pack("no_manifold_pack", wat_source, None);
        let runner = PackToolRunner::compile(&pack, "tool").expect("compiles");
        assert_eq!(
            runner.manifold,
            Manifold::sealed(),
            "no declared manifold narrows to nothing"
        );

        let outcome = runner
            .call(&serde_json::json!({}), &ToolBudget::default())
            .expect("call succeeds");
        assert_eq!(outcome.verdict, ToolVerdict::Success);
        assert_eq!(
            outcome.output,
            serde_json::json!({"ok": false}),
            "fd 3 must not exist: no filesystem capability was granted"
        );
    }

    /// This runner's ceiling narrows even a wide-open declared manifold
    /// to nothing, because nothing beyond stdin/stdout/stderr crosses
    /// this tool ABI today (`PackToolRunner::compile`'s own doc).
    #[test]
    fn a_wide_declared_manifold_is_still_narrowed_to_this_runners_ceiling() {
        let wat_source = r#"(module (func (export "_start")))"#;
        let manifold = serde_json::json!({
            "fs": {"ReadWrite": ["/data"]},
            "net": {"OutboundFull": null},
            "env": "Full",
            "crypto": true,
            "child_process": false,
            "allow_exit": false,
            "http_timeout_ms": null,
            "listen": "None"
        });
        let pack = build_tool_pack("wide_manifold_pack", wat_source, Some(manifold));
        let runner = PackToolRunner::compile(&pack, "tool").expect("compiles");
        assert_eq!(runner.manifold, Manifold::sealed());
    }

    #[test]
    fn compiling_an_unknown_tool_name_is_refused() {
        let pack = build_tool_pack("unknown_tool_pack", ECHO_WAT, None);
        let error = PackToolRunner::compile(&pack, "nonexistent").expect_err("must be refused");
        assert!(format!("{error:#}").contains("nonexistent"), "{error:#}");
    }

    // ---- narrow_manifold: the intersection rule from (2) --------------

    #[test]
    fn an_admitting_caller_can_widen_the_ceiling_but_never_past_the_tool() {
        use afterburner_core::manifold::FsAccess;

        // A tool that asked to read a workspace gets nothing under the
        // sealed default, because nothing has admitted it yet.
        let sealed = narrow_manifold(
            &Manifold {
                fs: FsAccess::ReadOnly(vec!["/workspace".into()]),
                ..Manifold::sealed()
            },
            &PackToolRunner::CEILING,
        );
        assert!(matches!(sealed.fs, FsAccess::None));

        // An operator that allows the workspace admits exactly what the
        // tool asked for, and no more.
        let admitted = narrow_manifold(
            &Manifold {
                fs: FsAccess::ReadOnly(vec!["/workspace".into()]),
                ..Manifold::sealed()
            },
            &Manifold {
                fs: FsAccess::ReadWrite(vec!["/workspace".into(), "/tmp".into()]),
                ..Manifold::sealed()
            },
        );
        match admitted.fs {
            FsAccess::ReadOnly(roots) => {
                assert_eq!(roots, vec![std::path::PathBuf::from("/workspace")])
            }
            other => panic!("a read-only request must stay read-only, got {other:?}"),
        }
    }

    #[test]
    fn narrow_manifold_grants_the_intersection_not_the_union() {
        let declared = Manifold {
            fs: FsAccess::ReadWrite(vec![PathBuf::from("/a"), PathBuf::from("/b")]),
            net: NetAccess::OutboundFull(None),
            env: EnvAccess::Full,
            crypto: true,
            child_process: true,
            allow_exit: true,
            http_timeout_ms: Some(10_000),
            listen: ListenAccess::None,
        };
        let ceiling = Manifold {
            fs: FsAccess::ReadOnly(vec![PathBuf::from("/a")]),
            net: NetAccess::OutboundHttp(Some(vec!["example.com".to_owned()])),
            env: EnvAccess::AllowList(vec!["HOME".to_owned()]),
            crypto: false,
            child_process: true,
            allow_exit: false,
            http_timeout_ms: Some(5_000),
            listen: ListenAccess::None,
        };

        let granted = narrow_manifold(&declared, &ceiling);

        assert_eq!(
            granted.fs,
            FsAccess::ReadOnly(vec![PathBuf::from("/a")]),
            "the narrower access level, over the roots common to both"
        );
        assert_eq!(
            granted.net,
            NetAccess::OutboundHttp(Some(vec!["example.com".to_owned()])),
            "the narrower net kind, over the hosts common to both"
        );
        assert_eq!(granted.env, EnvAccess::AllowList(vec!["HOME".to_owned()]));
        assert!(!granted.crypto, "crypto requires both sides to grant it");
        assert!(granted.child_process, "both sides grant child_process");
        assert!(!granted.allow_exit, "allow_exit requires both sides");
        assert_eq!(granted.http_timeout_ms, Some(5_000), "the tighter cap wins");
    }

    #[test]
    fn narrow_manifold_never_widens_past_what_the_tool_declared() {
        let declared = Manifold {
            fs: FsAccess::ReadOnly(vec![PathBuf::from("/a")]),
            ..Manifold::sealed()
        };
        let granted = narrow_manifold(&declared, &Manifold::open());
        assert_eq!(
            granted.fs,
            FsAccess::ReadOnly(vec![PathBuf::from("/a")]),
            "a wider ceiling must never widen the tool's own declared grant"
        );
        assert!(!granted.crypto, "the tool never declared crypto");
    }

    #[test]
    fn narrow_manifold_with_no_declared_manifold_grants_nothing() {
        assert_eq!(
            narrow_manifold(&Manifold::sealed(), &Manifold::open()),
            Manifold::sealed()
        );
    }

    #[test]
    fn narrow_manifold_always_forces_listen_to_none() {
        let mut declared = Manifold::open();
        declared.listen = ListenAccess::Any;
        let mut ceiling = Manifold::open();
        ceiling.listen = ListenAccess::Any;
        assert_eq!(
            narrow_manifold(&declared, &ceiling).listen,
            ListenAccess::None,
            "a tool is called, never a server, whatever either side asked for"
        );
    }
}
