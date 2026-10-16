//! Running one of a pack's plugins: a complete Afterburner `.afb`
//! ([`crate::pack::PackPlugin`]), handed to `afterburner::afb_run::run_afb_bytes`.
//! The call ABI is deliberately narrow: canonical JSON arguments on stdin, one
//! JSON value on stdout, stderr is diagnostics for a human.
//!
//! Called as a model tool ([`tool::PluginTool`]), the output is split by
//! `gents_protocol::message::ToolResultContent::from_tool_output`; text parts
//! are held to the loop's tool-result text bound, image parts are passed whole
//! bounded only by `limits.max_output_mib` and replayed as a note per image
//! (`gents_loop::loop_stream::replayed_tool_result_text`). Called as a graph
//! stage ([`crate::callback::plugin`]), the value is the stage's output
//! documents and is never split.
//!
//! Invariants enforced here rather than trusted from the caller:
//!
//! 1. One run per instantiation: `run_afb_bytes` parses, compiles, and
//!    instantiates fresh on every call; nothing is reused across calls.
//! 2. The manifold is a ceiling, narrowed, never widened: [`narrow_manifold`]
//!    intersects what the plugin declared with what this runner grants.
//! 3. A plugin never listens and never holds a socket: `listen` is forced off
//!    and every run strips `net`; a granted `net` axis is served by the host
//!    ([`http_calls`]).
//! 4. A plugin whose dispatch path cannot enforce every bound is refused at
//!    admission ([`PluginRunner::compile`]) rather than run with a bound
//!    silently dropped: `run_afb_bytes` does not enforce `stdin`, `fuel`,
//!    `memory_bytes`, or manifold grants for every language it dispatches.
//! 5. Every bound is real, and a default is one the artifact can start under
//!    ([`PluginBudget::for_artifact`]). Stdout and stderr are each capped
//!    after the run, truncation named in `diagnostics`, never silent.
//! 6. Output is validated, `input_schema` is not (it describes arguments).
//! 7. Failures are typed, never silent: routine bounds are verdicts, genuine
//!    traps are hard errors.

use std::path::PathBuf;
use std::time::Instant;

use afterburner::afb_run::{run_afb_bytes, AfbRunOutcome, AfbRunRequest};
use afterburner::wasi::embedder_vm::NanMode;
use afterburner_core::manifold::{EnvAccess, FsAccess, ListenAccess, NetAccess};
use anyhow::{Context, Result};

use crate::pack::{BindAccess, PackPlugin};

/// Default bytes of a plugin's JSON result kept before the call is judged
/// [`PluginVerdict::BadOutput`]; [`PluginBudget::for_plugin`] raises this to
/// a plugin's declared `limits.max_output_mib`.
const DEFAULT_MAX_OUTPUT_BYTES: usize = 1024 * 1024;

/// Host ceiling on what a plugin's `limits` may declare, enforced by
/// [`crate::pack::PackPlugin::validate`] at manifest load.
pub const MAX_DECLARED_MEMORY_MIB: u32 = 4096;
pub const MAX_DECLARED_WALL_CLOCK_SECS: u32 = 900;
/// `run_afb_bytes`'s WASI-command dispatch captures guest stdout into a fixed
/// 4 MiB pipe inside the guest, so declaring more than that would promise an
/// output size this host can never deliver. A guest that traps having filled
/// the pipe is reclassified as [`PluginVerdict::BadOutput`] rather than
/// surfaced as an opaque trap (see `STDOUT_CAPTURE_TRAP_MARGIN_BYTES`).
pub const MAX_DECLARED_OUTPUT_MIB: u32 = 4;

/// The real hard ceiling on captured guest stdout regardless of what a plugin
/// declared.
const STDOUT_CAPTURE_PIPE_BYTES: usize = MAX_DECLARED_OUTPUT_MIB as usize * 1024 * 1024;

/// How close a trapped guest's captured stdout must sit to
/// [`STDOUT_CAPTURE_PIPE_BYTES`] to be judged an output-capacity trap rather
/// than a genuine guest crash: the preview1 `fd_write` shim writes in 4 KiB
/// chunks, so an overflowing guest has filled the pipe to within one chunk of
/// capacity; an unrelated trap has no reason to sit near that boundary. The
/// margin is generous relative to that granularity so it survives a change in
/// the shim's chunk size.
const STDOUT_CAPTURE_TRAP_MARGIN_BYTES: usize = 64 * 1024;

/// Bytes of a plugin's stderr kept for a human to read. Diagnostics, not a
/// log archive.
const MAX_STDERR_BYTES: usize = 64 * 1024;

/// What a plugin may spend on one call. Maps 1:1 onto
/// [`AfbRunRequest`]'s `fuel`, `memory_bytes`, and `timeout` fields.
#[derive(Debug, Clone, Copy)]
pub struct PluginBudget {
    /// Wasmtime fuel: a deterministic instruction ceiling. `None` means no
    /// ceiling; the wall clock is the backstop against a guest that never
    /// returns.
    pub fuel: Option<u64>,
    /// Linear-memory ceiling in bytes, enforced by wasmtime on every
    /// `memory.grow`.
    pub memory_bytes: u64,
    /// Wall-clock deadline for the whole call, enforced by `run_afb_bytes`
    /// as a real preemption. Granularity is one epoch tick, 10 ms today.
    pub wall_clock: std::time::Duration,
    /// Bytes of a plugin's JSON result kept before the call is judged
    /// [`PluginVerdict::BadOutput`].
    pub max_output_bytes: usize,
}

impl PluginBudget {
    /// [`Self::default`], raised where it could not host `afb` at all: a
    /// bound below what a guest spends before its own code runs fails during
    /// startup and bounds nothing. The floor comes from
    /// [`afterburner::afb_run::startup_floor`], where the dispatch shape is
    /// known, rather than a copy of that knowledge here. The wall clock is
    /// raised with it for the same reason; fuel stays the caller's choice,
    /// raised by the interpreter's own startup cost, saturating.
    pub fn for_artifact(afb: &afterburner_afb::Afb) -> Self {
        let default = Self::default();
        match afterburner::afb_run::startup_floor(afb) {
            Some(floor) => Self {
                memory_bytes: default.memory_bytes.max(floor.memory_bytes),
                fuel: default
                    .fuel
                    .map(|ceiling| floor.fuel.saturating_add(ceiling)),
                wall_clock: default.wall_clock.max(INTERPRETER_BOOT_ALLOWANCE),
                ..default
            },
            None => default,
        }
    }

    /// Declared resource requests stay within host ceilings even when a caller
    /// did not load the manifest through the pack validator.
    pub fn for_plugin(afb: &afterburner_afb::Afb, plugin: &PackPlugin) -> Self {
        let budget = Self::for_artifact(afb);
        match &plugin.limits {
            Some(limits) => budget.with_declared_limits(limits),
            None => budget,
        }
    }

    fn with_declared_limits(mut self, limits: &crate::pack::PluginLimits) -> Self {
        self.memory_bytes = self
            .memory_bytes
            .max(u64::from(limits.memory_mib.unwrap_or(0)) * 1024 * 1024)
            .min(u64::from(MAX_DECLARED_MEMORY_MIB) * 1024 * 1024);
        self.wall_clock = self
            .wall_clock
            .max(std::time::Duration::from_secs(u64::from(
                limits.wall_clock_secs.unwrap_or(0),
            )))
            .min(std::time::Duration::from_secs(u64::from(
                MAX_DECLARED_WALL_CLOCK_SECS,
            )));
        self.max_output_bytes = self
            .max_output_bytes
            .max(
                limits
                    .max_output_mib
                    .unwrap_or(0)
                    .min(MAX_DECLARED_OUTPUT_MIB) as usize
                    * 1024
                    * 1024,
            )
            .min(MAX_DECLARED_OUTPUT_MIB as usize * 1024 * 1024);
        self
    }
}

/// What a plugin that has to boot an interpreter before its own code runs
/// is allowed for the whole call: booting CPython is a second or two warm
/// and more cold, all inside the wall clock, so [`PluginBudget::default`]'s
/// five seconds would time out before the plugin's first line.
const INTERPRETER_BOOT_ALLOWANCE: std::time::Duration = std::time::Duration::from_secs(60);

impl Default for PluginBudget {
    fn default() -> Self {
        Self {
            fuel: None,
            memory_bytes: 64 * 1024 * 1024,
            wall_clock: std::time::Duration::from_secs(5),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }
}

/// Why a call did not return the plugin's own JSON value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginVerdict {
    /// The plugin ran and produced exactly one JSON value.
    Success,
    /// A caller's own pre-call check (e.g. `input_schema` validation)
    /// declined to make the call. Never produced by [`PluginRunner`] itself;
    /// part of the shared vocabulary so a caller reports a refusal in the
    /// same [`PluginOutcome`] shape.
    Refused,
    /// `budget.fuel` ran out before the plugin returned.
    OutOfFuel,
    /// `budget.memory_bytes` was exceeded.
    OutOfMemory,
    /// `budget.wall_clock` elapsed before the plugin returned.
    Timeout,
    /// The plugin ran but stdout was not exactly one JSON value (not
    /// parseable, more than one value, or truncated past the output bound).
    BadOutput,
    /// The plugin exited with a non-zero code. Its stdout is not taken as
    /// a result even when it parses.
    Failed,
}

/// The guest itself trapped; distinct from the host failing to run it.
#[derive(Debug, thiserror::Error)]
#[error("plugin {plugin:?} trapped: {message}")]
pub struct GuestTrap {
    plugin: String,
    message: String,
}

/// What came back from one plugin call.
#[derive(Debug, Clone)]
pub struct PluginOutcome {
    pub verdict: PluginVerdict,
    /// The plugin's own JSON value on [`PluginVerdict::Success`];
    /// [`serde_json::Value::Null`] for every other verdict.
    pub output: serde_json::Value,
    /// Bounded stderr text, for a human. Carries a truncation note when
    /// stdout or stderr ran past a bound.
    pub diagnostics: String,
    /// Fuel actually consumed, as `run_afb_bytes` reports it. `0` on
    /// `Timeout`, where the store is torn down with the interrupted guest
    /// and the figure is unknowable rather than estimated.
    pub fuel_used: u64,
    pub wall_ms: u64,
}

/// A plugin, ready to be called more than once.
pub struct PluginRunner {
    /// The plugin's raw `.afb` bytes; `run_afb_bytes` parses, compiles, and
    /// instantiates fresh on every call, so there is nothing to cache.
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
    /// carries nothing beyond stdin/stdout/stderr, so the honest ceiling
    /// today is "nothing". `narrow_manifold` takes the ceiling as an
    /// ordinary argument.
    const CEILING: Manifold = Manifold::sealed();

    /// Admits one plugin's compiled `.afb`. Refuses admission outright,
    /// before any call, when the real `.afb` would dispatch through a path
    /// `run_afb_bytes` does not fully bound — the definitive check,
    /// independent of `crate::pack::SUPPORTED_PLUGIN_LANGUAGES`' earlier
    /// manifest-authoring screen, because a plugin can arrive as a raw
    /// `.afb` without passing that validator.
    pub fn compile(afb_bytes: &[u8], plugin: &PackPlugin) -> Result<Self> {
        Self::compile_within(afb_bytes, plugin, &Self::CEILING)
    }

    /// The same, under a ceiling the caller supplies: the seam an admitting
    /// caller uses. A ceiling can only ever take capability away. It is
    /// decided once at admission, never per call.
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
        let manifold = narrow_manifold(&declared, ceiling);
        if let Some(reason) = unbounded_dispatch_reason(&afb, &manifold) {
            anyhow::bail!(
                "plugin {:?} cannot be admitted: this artifact would run without every declared \
                 bound enforced ({reason})",
                plugin.name
            );
        }
        if let Some(binding) = &plugin.bind_dir {
            let supported = afterburner::afb_run::bounds_for(&afb);
            let reads = binding.access == BindAccess::Read || !binding.write_fields.is_empty();
            let writes = binding.access == BindAccess::ReadWrite;
            for (needed, enforced, kind) in [
                (reads, supported.manifold_fs_ro, "read-only"),
                (writes, supported.manifold_fs_rw, "read-write"),
            ] {
                anyhow::ensure!(
                    !needed || enforced,
                    "plugin {:?} declares bind_dir, but its dispatch path cannot enforce a \
                     {kind} filesystem grant, so a caller-bound directory would not actually be \
                     contained",
                    plugin.name
                );
            }
        }

        Ok(Self {
            afb_bytes: afb_bytes.to_vec(),
            plugin: plugin.clone(),
            manifold,
        })
    }

    /// Runs it once with the given arguments, under the manifold this
    /// runner was admitted with.
    pub fn call(
        &self,
        arguments: &serde_json::Value,
        budget: &PluginBudget,
    ) -> Result<PluginOutcome> {
        self.call_with_manifold(arguments, budget, self.manifold.clone())
    }

    /// Runs it once, binding `bound` into `plugin.bind_dir`'s declared
    /// input field, with the access this call asks for
    /// ([`crate::pack::PluginDirBinding::call_access`]), and only for this one
    /// call. Refuses outright when the plugin declares no `bind_dir` (the
    /// field it would overwrite does not exist) or when `bound` was allowed
    /// less access than the call asks for.
    ///
    /// `arguments[bind_dir.input_field]` is overwritten with `bound`'s own
    /// canonical target regardless of what the caller passed, so a plugin
    /// can never point the binding at a path other than the one its caller
    /// named. `arguments` may be `Value::Null` (an omitted `--input`, for
    /// instance): the binding itself can be the only field a call needs, so
    /// a missing operator input is treated as an empty object rather than
    /// refused; any other non-object value is still refused. The manifold
    /// this call actually runs under is the admitted one with `fs` replaced
    /// by exactly `bound`'s directory at the call's access - never wider, and never recorded as a
    /// standing grant (the install record's `granted` is untouched; see
    /// [`BoundDir`]'s own doc).
    pub fn call_bound(
        &self,
        arguments: &serde_json::Value,
        budget: &PluginBudget,
        bound: &BoundDir,
    ) -> Result<PluginOutcome> {
        let (arguments, manifold) = self.prepare_bound(arguments, bound)?;
        self.call_with_manifold(&arguments, budget, manifold)
    }

    pub(super) fn prepare_bound(
        &self,
        arguments: &serde_json::Value,
        bound: &BoundDir,
    ) -> Result<(serde_json::Value, Manifold)> {
        let bind_dir = self.plugin.bind_dir.as_ref().with_context(|| {
            format!(
                "plugin {:?} does not declare bind_dir; it cannot be bound to a directory",
                self.plugin.name
            )
        })?;
        let access = bind_dir.call_access(arguments);
        anyhow::ensure!(
            bound.access() >= access,
            "plugin {:?} needs {} access for this call, but {} is only allowed {}",
            self.plugin.name,
            access.as_str(),
            bound.path().display(),
            bound.access().as_str()
        );
        bound.recheck()?;
        let canonical = bound.target().to_str().with_context(|| {
            format!("bound path {} is not valid UTF-8", bound.target().display())
        })?;
        let mut arguments = if arguments.is_null() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            arguments.clone()
        };
        let object = arguments.as_object_mut().with_context(|| {
            format!(
                "plugin {:?} arguments must be a JSON object to bind {:?}",
                self.plugin.name, bind_dir.input_field
            )
        })?;
        object.insert(
            bind_dir.input_field.clone(),
            serde_json::Value::String(canonical.to_owned()),
        );
        if let Some(field) = &bind_dir.original_field {
            let original = bound.original().to_str().with_context(|| {
                format!(
                    "bound path {} is not valid UTF-8",
                    bound.original().display()
                )
            })?;
            object.insert(
                field.clone(),
                serde_json::Value::String(original.to_owned()),
            );
        }
        let roots = vec![bound.path().to_path_buf()];
        let manifold = Manifold {
            fs: match access {
                BindAccess::Read => FsAccess::ReadOnly(roots),
                BindAccess::ReadWrite => FsAccess::ReadWrite(roots),
            },
            ..self.manifold.clone()
        };
        Ok((arguments, manifold))
    }

    /// [`Self::call`] and [`Self::call_bound`]'s shared implementation: the
    /// only difference between an ordinary call and a bound one is which
    /// manifold the guest actually runs under.
    fn call_with_manifold(
        &self,
        arguments: &serde_json::Value,
        budget: &PluginBudget,
        manifold: Manifold,
    ) -> Result<PluginOutcome> {
        debug_assert!(
            matches!(manifold.listen, ListenAccess::None),
            "a granted manifold must never carry a listen capability"
        );
        let manifold = Manifold {
            net: NetAccess::None,
            ..manifold
        };

        start_wasm_trap_handler_with_signals_blocked()?;
        let stdin =
            serde_json::to_vec(arguments).context("encoding plugin arguments as canonical JSON")?;
        let request = plugin_run_request(stdin, manifold, budget);

        let started = Instant::now();
        let output = run_afb_bytes(&self.afb_bytes, request)
            .with_context(|| format!("plugin {:?} failed to start", self.plugin.name))?;
        let wall_ms = elapsed_ms(started);

        match output.outcome {
            AfbRunOutcome::OutOfFuel => Ok(PluginOutcome {
                verdict: PluginVerdict::OutOfFuel,
                output: serde_json::Value::Null,
                // A ceiling the caller actually set is a budget it
                // exhausted; the unlimited default has no budget to name,
                // only the sentinel this call carried in its place.
                diagnostics: match budget.fuel {
                    Some(_) => "the plugin exhausted its fuel budget".to_owned(),
                    None => "the plugin ran out of fuel".to_owned(),
                },
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
                    "the plugin did not answer within its {}ms wall-clock budget and was stopped",
                    budget.wall_clock.as_millis()
                ),
                fuel_used: output.fuel_used,
                wall_ms,
            }),
            AfbRunOutcome::Trapped(message) => {
                if output.stdout.len() + STDOUT_CAPTURE_TRAP_MARGIN_BYTES
                    >= STDOUT_CAPTURE_PIPE_BYTES
                {
                    Ok(PluginOutcome {
                        verdict: PluginVerdict::BadOutput,
                        output: serde_json::Value::Null,
                        diagnostics: format!(
                            "plugin output filled the host's {} MiB stdout capture buffer \
                             before finishing; its result did not fit (guest trap: {message})",
                            STDOUT_CAPTURE_PIPE_BYTES / (1024 * 1024)
                        ),
                        fuel_used: output.fuel_used,
                        wall_ms,
                    })
                } else {
                    Err(GuestTrap {
                        plugin: self.plugin.name.clone(),
                        message,
                    }
                    .into())
                }
            }
            AfbRunOutcome::Exited(code) => Ok(outcome_from_exit(
                code,
                output,
                wall_ms,
                budget.max_output_bytes,
            )),
        }
    }

    /// What the model is shown for this plugin.
    pub fn definition(&self) -> &PackPlugin {
        &self.plugin
    }
}

fn plugin_run_request(stdin: Vec<u8>, manifold: Manifold, budget: &PluginBudget) -> AfbRunRequest {
    AfbRunRequest {
        stdin,
        manifold,
        // Afterburner's own `None` means its family's default fuel budget,
        // not "unlimited"; the sentinel is the largest ceiling
        // `Store::set_fuel` accepts, as Afterburner's own runtime uses.
        fuel: Some(budget.fuel.unwrap_or(u64::MAX)),
        memory_bytes: Some(budget.memory_bytes),
        timeout: Some(budget.wall_clock),
        nan_mode: PLUGIN_NAN_MODE,
        ..Default::default()
    }
}

/// WASM command/component plugins use native NaN arithmetic for OCR throughput.
/// Python dispatch ignores this mode and retains its interpreter engine's
/// canonicalization. Native payload bits may differ across CPUs and guests can
/// expose them as JSON integers or strings; plugin output is a recorded external
/// observation, not a promise of byte-identical recomputation on another host.
/// The macOS startup must select this same engine mode before installing handlers.
const PLUGIN_NAN_MODE: NanMode = NanMode::Native;

/// Creates Afterburner's first Wasmtime engine on a thread that blocks every
/// signal, before any plugin runs.
///
/// Afterburner's engines keep Wasmtime's macOS default of Mach-port trap
/// handling; its handler thread aborts the process when a signal interrupts
/// its `mach_msg` receive. The runtime installs signal handlers (tokio's
/// `SIGCHLD` for child processes), and the kernel may deliver one to any
/// thread that does not block it, so the handler thread is started from a
/// creator whose own mask blocks every signal. Opting the engine out of
/// Mach ports instead is not available: Afterburner exposes no Wasmtime
/// configuration, and Wasmtime panics on an engine whose trap mode differs
/// from the first one's.
///
/// Fails closed: if the masked thread cannot be started, cannot mask its
/// signals, or cannot create the engine, every plugin call is refused with
/// that error rather than letting a call create the engine unprotected.
fn start_wasm_trap_handler_with_signals_blocked(
) -> Result<Option<&'static afterburner::wasi::embedder_vm::EmbedderVm>> {
    #[cfg(target_os = "macos")]
    {
        static STARTED: std::sync::OnceLock<
            std::result::Result<&'static afterburner::wasi::embedder_vm::EmbedderVm, String>,
        > = std::sync::OnceLock::new();
        return STARTED
            .get_or_init(|| {
                std::thread::Builder::new()
                    .name("gents-wasm-trap-init".into())
                    .spawn(|| -> std::result::Result<&'static afterburner::wasi::embedder_vm::EmbedderVm, String> {
                        // SAFETY: changes only this short-lived thread's own mask.
                        let masked = unsafe {
                            let mut all: libc::sigset_t = std::mem::zeroed();
                            libc::sigfillset(&mut all);
                            libc::pthread_sigmask(libc::SIG_BLOCK, &all, std::ptr::null_mut())
                        };
                        if masked != 0 {
                            return Err(format!(
                                "blocking signals: {}",
                                std::io::Error::from_raw_os_error(masked)
                            ));
                        }
                        afterburner::wasi::embedder_vm::shared_epoch_vm_with(PLUGIN_NAN_MODE)
                            .map_err(|error| format!("creating the engine: {error}"))
                    })
                    .map_err(|error| format!("spawning the thread: {error}"))?
                    .join()
                    .map_err(|_| "the thread panicked".to_owned())?
            })
            .clone()
            .map_err(|error| {
                anyhow::anyhow!(
                    "could not start the Wasmtime trap handler with signals blocked: {error}"
                )
            }).map(Some);
    }
    #[cfg(not(target_os = "macos"))]
    Ok(None)
}

/// What one call to this artifact would ask for that its dispatch path
/// cannot actually enforce.
///
/// `None` means every bound this runner applies is real; `Some(reason)`
/// names the axes that would be silently dropped — the message
/// [`PluginRunner::compile_within`] refuses with. The classification is
/// Afterburner's own ([`afterburner::afb_run::bounds_for`]), never
/// re-derived from the manifest here; an earlier re-derived copy drifted
/// and reported a fully bounded language as unsafe.
fn unbounded_dispatch_reason(afb: &afterburner_afb::Afb, granted: &Manifold) -> Option<String> {
    let supported = afterburner::afb_run::bounds_for(afb);
    let mut missing: Vec<String> = Vec::new();

    if !supported.stdin {
        missing.push("the arguments on stdin".to_owned());
    }
    if !supported.fuel {
        missing.push("the fuel ceiling".to_owned());
    }
    if !supported.memory_bytes {
        missing.push("the memory ceiling".to_owned());
    }
    if !supported.timeout {
        missing.push("the wall-clock budget".to_owned());
    }

    // An empty grant list is not a grant.
    match &granted.fs {
        FsAccess::ReadOnly(paths) if !paths.is_empty() && !supported.manifold_fs_ro => {
            missing.push("the read-only filesystem grant".to_owned())
        }
        FsAccess::ReadWrite(paths) if !paths.is_empty() && !supported.manifold_fs_rw => {
            missing.push("the read-write filesystem grant".to_owned())
        }
        _ => {}
    }
    if !matches!(granted.env, EnvAccess::None) && !supported.manifold_env {
        missing.push("the environment grant".to_owned());
    }
    if let Err(why) = http_calls::net_grant(&granted.net) {
        missing.push(format!("the network grant ({why})"));
    }

    if missing.is_empty() {
        return None;
    }
    Some(format!("{} would not be enforced", missing.join(", ")))
}

/// Validates and bounds a completed run's stdout/stderr. `max_output_bytes`
/// is the calling [`PluginBudget`]'s own bound, so a plugin that declared
/// `limits.max_output_mib` is judged against what it asked for.
fn outcome_from_exit(
    code: i32,
    output: afterburner::afb_run::AfbRunOutput,
    wall_ms: u64,
    max_output_bytes: usize,
) -> PluginOutcome {
    let mut notes = Vec::new();
    let stderr = bound(&output.stderr, MAX_STDERR_BYTES, "stderr", &mut notes);
    let diagnostics_base = String::from_utf8_lossy(stderr).into_owned();
    if code != 0 {
        notes.push(format!("plugin exited with code {code}"));
        return PluginOutcome {
            verdict: PluginVerdict::Failed,
            output: serde_json::Value::Null,
            diagnostics: append_notes(diagnostics_base, &notes),
            fuel_used: output.fuel_used,
            wall_ms,
        };
    }

    if output.stdout.len() > max_output_bytes {
        notes.push(format!(
            "plugin output exceeded {max_output_bytes} bytes ({} bytes produced); declare \
             limits.max_output_mib to raise it, up to the host ceiling",
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

/// Caps `bytes` at `max`, recording a truncation note when it does.
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
/// regardless of either side.
///
/// Written out field by field, mirroring `pack_archive::union_manifold`'s
/// shape, so a capability axis added to `Manifold` upstream fails this to
/// compile instead of silently defaulting one way or the other.
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

    /// An empty root list means "every root"; the narrower side wins when
    /// one is empty, and two finite lists narrow to their common roots.
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
        // A plugin is called, never a server: forced here regardless of
        // what either side asked.
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

pub mod allowed;
pub mod approval;
pub mod authority;
mod bound;
pub use bound::BoundDir;

/// The attempts a plugin call gets in all when its caller configured
/// `max_attempts`: absent, or zero, is one.
pub fn attempts_allowed(max_attempts: Option<u32>) -> u32 {
    max_attempts.unwrap_or(1).max(1)
}

/// How long a failed plugin call waits before its next attempt: one second,
/// doubling per attempt, at most a minute.
pub fn retry_backoff(attempts: u32) -> std::time::Duration {
    std::time::Duration::from_secs(1u64 << attempts.saturating_sub(1).min(6))
        .min(std::time::Duration::from_secs(60))
}
pub mod executor;
pub mod http_calls;
pub mod install;
pub mod model_calls;
mod rounds;
pub mod store;
pub mod tool;
pub(crate) mod tool_calls;

/// A plugin's authority: files, network, environment and the rest.
pub use afterburner_core::manifold::Manifold;

#[cfg(test)]
pub(crate) mod tests;
