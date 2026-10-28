use std::path::PathBuf;
use std::time::Instant;

use afterburner::afb_run::{run_afb_bytes, AfbRunOutcome, AfbRunRequest};
use afterburner::wasi::embedder_vm::NanMode;
use afterburner_core::manifold::{EnvAccess, FsAccess, ListenAccess, NetAccess};
use anyhow::{Context, Result};

use crate::pack::{BindAccess, PackPlugin};

const DEFAULT_MAX_OUTPUT_BYTES: usize = 1024 * 1024;

pub const MAX_DECLARED_MEMORY_MIB: u32 = 4096;
pub const MAX_DECLARED_WALL_CLOCK_SECS: u32 = 900;
pub const MAX_DECLARED_OUTPUT_MIB: u32 = 4;

const STDOUT_CAPTURE_PIPE_BYTES: usize = MAX_DECLARED_OUTPUT_MIB as usize * 1024 * 1024;

const STDOUT_CAPTURE_TRAP_MARGIN_BYTES: usize = 64 * 1024;

const MAX_STDERR_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct PluginBudget {
    /// Per-VM execution limit. An embedded JS invocation has its own meter;
    /// Afterburner does not expose its consumed fuel for aggregate accounting.
    /// The wall-clock limit bounds the whole plugin call across host imports.
    pub fuel: Option<u64>,
    pub memory_bytes: u64,
    pub wall_clock: std::time::Duration,
    pub max_output_bytes: usize,
}

impl PluginBudget {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginVerdict {
    Success,
    Refused,
    OutOfFuel,
    OutOfMemory,
    Timeout,
    BadOutput,
    Failed,
}

#[derive(Debug, thiserror::Error)]
#[error("plugin {plugin:?} trapped: {message}")]
pub struct GuestTrap {
    plugin: String,
    message: String,
}

#[derive(Debug, Clone)]
pub struct PluginOutcome {
    pub verdict: PluginVerdict,
    pub output: serde_json::Value,
    pub diagnostics: String,
    pub fuel_used: u64,
    pub wall_ms: u64,
}

pub struct PluginRunner {
    afb_bytes: Vec<u8>,
    plugin: PackPlugin,
    manifold: Manifold,
}

impl std::fmt::Debug for PluginRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginRunner")
            .field("plugin", &self.plugin.name)
            .finish()
    }
}

impl PluginRunner {
    const CEILING: Manifold = Manifold::sealed();

    pub fn compile(afb_bytes: &[u8], plugin: &PackPlugin) -> Result<Self> {
        Self::compile_within(afb_bytes, plugin, &Self::CEILING)
    }

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

    pub fn call(
        &self,
        arguments: &serde_json::Value,
        budget: &PluginBudget,
    ) -> Result<PluginOutcome> {
        self.call_with_manifold(arguments, budget, self.manifold.clone())
    }

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

        let roots = match &manifold.fs {
            FsAccess::None => &[][..],
            FsAccess::ReadOnly(roots) | FsAccess::ReadWrite(roots) => roots.as_slice(),
        };
        for root in roots {
            if root.is_dir() {
                allowed::ensure_mount_safe(root)?;
            } else {
                anyhow::ensure!(
                    !allowed::is_protected(root),
                    "Runtime permission files cannot be shared with a plugin."
                );
            }
        }

        let stdin =
            serde_json::to_vec(arguments).context("encoding plugin arguments as canonical JSON")?;
        let request = plugin_run_request(stdin, manifold, budget);

        let started = Instant::now();
        let session = (!self.plugin.host_imports.is_empty()).then(|| {
            std::sync::Arc::new(std::sync::Mutex::new(host_imports::ImportSession {
                script: host_imports::script_of(arguments),
                plugin: self.plugin.name.clone(),
                budget: *budget,
                started,
                stored: Vec::new(),
                failure: None,
            }))
        });
        let output = match &session {
            None => run_afb_bytes(&self.afb_bytes, request).map_err(anyhow::Error::from),
            Some(session) => {
                let afb = afterburner_afb::Afb::from_bytes(&self.afb_bytes).map_err(|error| {
                    anyhow::anyhow!(
                        "plugin {:?}'s artifact is not a readable .afb: {error}",
                        self.plugin.name
                    )
                })?;
                host_imports::run_with_imports(&afb, request, session.clone())
            }
        }
        .with_context(|| format!("plugin {:?} failed to start", self.plugin.name))?;
        let wall_ms = elapsed_ms(started);

        match output.outcome {
            AfbRunOutcome::OutOfFuel => Ok(PluginOutcome {
                verdict: PluginVerdict::OutOfFuel,
                output: serde_json::Value::Null,
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
            AfbRunOutcome::Exited(code) => Ok(host_imports::outcome_with_import_failure(
                outcome_from_exit(code, output, wall_ms, budget.max_output_bytes),
                code,
                session.as_deref(),
            )),
        }
    }

    pub fn definition(&self) -> &PackPlugin {
        &self.plugin
    }
}

fn plugin_run_request(stdin: Vec<u8>, manifold: Manifold, budget: &PluginBudget) -> AfbRunRequest {
    AfbRunRequest {
        stdin,
        manifold,
        fuel: Some(budget.fuel.unwrap_or(u64::MAX)),
        memory_bytes: Some(budget.memory_bytes),
        timeout: Some(budget.wall_clock),
        nan_mode: PLUGIN_NAN_MODE,
        ..Default::default()
    }
}

/// Native NaN arithmetic is an external observation: payload bits can differ across hosts.
const PLUGIN_NAN_MODE: NanMode = NanMode::Native;

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

fn narrow_manifold(declared: &Manifold, ceiling: &Manifold) -> Manifold {
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

pub fn attempts_allowed(max_attempts: Option<u32>) -> u32 {
    max_attempts.unwrap_or(1).max(1)
}

pub fn retry_backoff(attempts: u32) -> std::time::Duration {
    std::time::Duration::from_secs(1u64 << attempts.saturating_sub(1).min(6))
        .min(std::time::Duration::from_secs(60))
}
pub mod data;
pub mod executor;
pub mod host_imports;
pub mod http_calls;
pub mod install;
pub mod job_calls;
pub mod model_calls;
mod rounds;
pub(crate) mod script_engine;
mod service;
pub mod store;
pub mod tool;
pub mod tool_calls;

pub use afterburner_core::manifold::Manifold;

#[cfg(test)]
pub(crate) mod tests;
