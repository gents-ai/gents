use std::path::PathBuf;
use std::time::Instant;

use afterburner::afb_run::{run_afb_bytes, AfbRunOutcome, AfbRunRequest};
use afterburner::wasi::embedder_vm::NanMode;
use afterburner_core::manifold::{EnvAccess, FsAccess, ListenAccess, NetAccess};
use gents_protocol::job::{AssetMount, ASSET_MOUNTS_INPUT_KEY, PROJECT_MOUNT_CLASS};
use gents_protocol::plugin::PluginFilesystemScope;

use crate::pack::{BindAccess, PackPlugin};

type Result<T, E = PluginError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("plugin {name:?}'s artifact is not a readable .afb: {error}")]
    ArtifactUnreadable {
        name: String,
        error: afterburner_afb::AfbError,
    },
    #[error("plugin {name:?} declares a manifold that is not one")]
    InvalidManifold {
        name: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("plugin {name:?} cannot be admitted: this artifact would run without every declared bound enforced ({reason})")]
    Unbounded { name: String, reason: String },
    #[error("plugin {name:?} declares bind_dir, but its dispatch path cannot enforce a {kind} filesystem grant, so a caller-bound directory would not actually be contained")]
    BindDirUnenforced { name: String, kind: &'static str },
    #[error("plugin {name:?} declares asset_mounts, but its dispatch path cannot enforce a read-only filesystem grant, so mounted assets would not actually be contained")]
    AssetMountsUnenforced { name: String },
    #[error(
        "plugin {name:?} arguments must be a JSON object to receive {ASSET_MOUNTS_INPUT_KEY:?}"
    )]
    ReceiveNotObject { name: String },
    #[error("plugin {name:?} cannot mount assets into a read-write call: this call writes; run it without mounted workflows")]
    MountReadWrite { name: String },
    #[error("plugin {name:?} does not declare bind_dir; it cannot be bound to a directory")]
    NoBindDir { name: String },
    #[error("plugin {name:?} needs {} access for this call, but {} is only allowed {}", needed.as_str(), path.display(), allowed.as_str())]
    BindAccessDenied {
        name: String,
        needed: BindAccess,
        path: PathBuf,
        allowed: BindAccess,
    },
    #[error("plugin {name:?} arguments must be a JSON object to bind {field:?}")]
    BindNotObject { name: String, field: String },
    #[error("bound path {} is not valid UTF-8", path.display())]
    BoundPathNotUtf8 { path: PathBuf },
    #[error("encoding plugin arguments as canonical JSON")]
    EncodeArguments(#[source] serde_json::Error),
    #[error("plugin {name:?} failed to start")]
    Start {
        name: String,
        #[source]
        source: afterburner_core::AfterburnerError,
    },
    #[error(transparent)]
    Trap(#[from] GuestTrap),
    #[error(transparent)]
    Allowed(#[from] crate::plugin::allowed::PluginAllowedError),
    #[error(transparent)]
    Bound(#[from] bound::BoundDirError),
    #[error(transparent)]
    Encode(#[from] serde_json::Error),
}

const DEFAULT_MAX_OUTPUT_BYTES: usize = 1024 * 1024;

pub const MAX_DECLARED_MEMORY_MIB: u32 = 4096;
pub const MAX_DECLARED_WALL_CLOCK_SECS: u32 = 900;
pub const MAX_DECLARED_OUTPUT_MIB: u32 = 4;

const STDOUT_CAPTURE_PIPE_BYTES: usize = MAX_DECLARED_OUTPUT_MIB as usize * 1024 * 1024;

const STDOUT_CAPTURE_TRAP_MARGIN_BYTES: usize = 64 * 1024;

const MAX_STDERR_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct PluginBudget {
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
            PluginError::ArtifactUnreadable {
                name: plugin.name.clone(),
                error,
            }
        })?;
        let declared: Manifold = match &plugin.manifold {
            Some(value) => serde_json::from_value(value.clone()).map_err(|source| {
                PluginError::InvalidManifold {
                    name: plugin.name.clone(),
                    source,
                }
            })?,
            None => Manifold::sealed(),
        };
        let manifold = narrow_manifold(&declared, ceiling);
        if let Some(reason) = unbounded_dispatch_reason(&afb, &manifold) {
            return Err(PluginError::Unbounded {
                name: plugin.name.clone(),
                reason,
            });
        }
        if let Some(binding) = &plugin.bind_dir {
            let supported = afterburner::afb_run::bounds_for(&afb);
            let reads = binding.access == BindAccess::Read || !binding.write_fields.is_empty();
            let writes = binding.access == BindAccess::ReadWrite;
            for (needed, enforced, kind) in [
                (reads, supported.manifold_fs_ro, "read-only"),
                (writes, supported.manifold_fs_rw, "read-write"),
            ] {
                if needed && !enforced {
                    return Err(PluginError::BindDirUnenforced {
                        name: plugin.name.clone(),
                        kind,
                    });
                }
            }
        }

        if !(plugin.asset_mounts.is_empty()
            || afterburner::afb_run::bounds_for(&afb).manifold_fs_ro)
        {
            return Err(PluginError::AssetMountsUnenforced {
                name: plugin.name.clone(),
            });
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

    /// [`Self::call`] or [`Self::call_bound`] (when `bound` is given) with
    /// `mounts` preopened read-only beside whatever the call already grants.
    pub(super) fn call_mounted(
        &self,
        arguments: &serde_json::Value,
        budget: &PluginBudget,
        bound: Option<&BoundDir>,
        mounts: &[AssetMount],
    ) -> Result<PluginOutcome> {
        let (arguments, manifold, _) = self.prepare_mounted(arguments, bound, mounts)?;
        self.call_with_manifold(&arguments, budget, manifold)
    }

    /// The input and manifold of one call under `mounts`, and the receipt
    /// scope that names them (`None` when there are no mounts). A read bind
    /// and the mounts share one `ReadOnly` list; a read-write grant with any
    /// mount is refused ([`mounts::resolve_access`]).
    pub(super) fn prepare_mounted(
        &self,
        arguments: &serde_json::Value,
        bound: Option<&BoundDir>,
        mounts: &[AssetMount],
    ) -> Result<(serde_json::Value, Manifold, Option<PluginFilesystemScope>)> {
        let (mut arguments, mut manifold) = match bound {
            Some(bound) => self.prepare_bound(arguments, bound)?,
            None => (arguments.clone(), self.manifold.clone()),
        };
        if !self.plugin.asset_mounts.is_empty() {
            // Host-overwritten like `bind_dir`'s field: a guest value never survives.
            if arguments.is_null() {
                arguments = serde_json::Value::Object(serde_json::Map::new());
            }
            let object =
                arguments
                    .as_object_mut()
                    .ok_or_else(|| PluginError::ReceiveNotObject {
                        name: self.plugin.name.clone(),
                    })?;
            object.insert(
                ASSET_MOUNTS_INPUT_KEY.to_owned(),
                serde_json::to_value(mounts)?,
            );
        }
        if mounts.is_empty() {
            return Ok((arguments, manifold, None));
        }
        let (mut roots, rw_bind) = match std::mem::replace(&mut manifold.fs, FsAccess::None) {
            FsAccess::ReadOnly(roots) => (roots, false),
            FsAccess::ReadWrite(roots) => {
                let writes = !roots.is_empty();
                (roots, writes)
            }
            FsAccess::None => (Vec::new(), false),
        };
        let installed_roots = roots.len() as u64;
        let resolution = mounts::resolve_access(
            rw_bind,
            &mounts
                .iter()
                .map(|mount| (mount.clone(), std::path::Path::new(&mount.path).is_dir()))
                .collect::<Vec<_>>(),
        );
        if !resolution.admitted {
            return Err(PluginError::MountReadWrite {
                name: self.plugin.name.clone(),
            });
        }
        let mut mounted = 0;
        let mut project_tier = false;
        for (mount, access) in mounts.iter().zip(&resolution.access) {
            if access.is_none() {
                continue;
            }
            mounted += 1;
            project_tier |= mount.class == PROJECT_MOUNT_CLASS;
            let root = PathBuf::from(&mount.path);
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        manifold.fs = FsAccess::ReadOnly(roots);
        let bound_input = bound
            .and(self.plugin.bind_dir.as_ref())
            .map(|binding| binding.input_field.clone());
        let scope = PluginFilesystemScope::Mounted {
            installed_roots: if bound.is_some() { 0 } else { installed_roots },
            mounts: mounted,
            project_tier,
            bound_input,
        };
        Ok((arguments, manifold, Some(scope)))
    }

    pub(super) fn prepare_bound(
        &self,
        arguments: &serde_json::Value,
        bound: &BoundDir,
    ) -> Result<(serde_json::Value, Manifold)> {
        let bind_dir = self
            .plugin
            .bind_dir
            .as_ref()
            .ok_or_else(|| PluginError::NoBindDir {
                name: self.plugin.name.clone(),
            })?;
        let access = bind_dir.call_access(arguments);
        if bound.access() < access {
            return Err(PluginError::BindAccessDenied {
                name: self.plugin.name.clone(),
                needed: access,
                path: bound.path().to_path_buf(),
                allowed: bound.access(),
            });
        }
        bound.recheck()?;
        let canonical = bound
            .target()
            .to_str()
            .ok_or_else(|| PluginError::BoundPathNotUtf8 {
                path: bound.target().to_path_buf(),
            })?;
        let mut arguments = if arguments.is_null() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            arguments.clone()
        };
        let object = arguments
            .as_object_mut()
            .ok_or_else(|| PluginError::BindNotObject {
                name: self.plugin.name.clone(),
                field: bind_dir.input_field.clone(),
            })?;
        object.insert(
            bind_dir.input_field.clone(),
            serde_json::Value::String(canonical.to_owned()),
        );
        if let Some(field) = &bind_dir.original_field {
            let original =
                bound
                    .original()
                    .to_str()
                    .ok_or_else(|| PluginError::BoundPathNotUtf8 {
                        path: bound.original().to_path_buf(),
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
            } else if allowed::is_protected(root) {
                return Err(crate::plugin::allowed::PluginAllowedError::Protected.into());
            }
        }

        let stdin = serde_json::to_vec(arguments).map_err(PluginError::EncodeArguments)?;
        let request = plugin_run_request(stdin, manifold, budget);

        let started = Instant::now();
        let output =
            run_afb_bytes(&self.afb_bytes, request).map_err(|source| PluginError::Start {
                name: self.plugin.name.clone(),
                source,
            })?;
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
                    Err(PluginError::Trap(GuestTrap {
                        plugin: self.plugin.name.clone(),
                        message,
                    }))
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
pub use catalog::catalog;

pub fn attempts_allowed(max_attempts: Option<u32>) -> u32 {
    max_attempts.unwrap_or(1).max(1)
}

pub fn retry_backoff(attempts: u32) -> std::time::Duration {
    std::time::Duration::from_secs(1u64 << attempts.saturating_sub(1).min(6))
        .min(std::time::Duration::from_secs(60))
}
pub mod catalog;
pub mod data;
pub mod executor;
pub mod http_calls;
pub mod install;
pub mod job_calls;
pub mod model_calls;
pub mod mounts;
mod rounds;
mod service;
pub mod store;
pub mod tool;
pub mod tool_calls;

pub use afterburner_core::manifold::Manifold;

#[cfg(test)]
pub(crate) mod tests;
