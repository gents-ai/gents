use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use gents_protocol::plugin::{
    PluginEnvironmentGrant, PluginExecutionAuthority, PluginExecutionLimits,
    PluginExecutionReceipt, PluginExecutionVerdict, PluginFilesystemGrant, PluginFilesystemScope,
    PluginHttpGrant,
};
use sha2::{Digest, Sha256};

use super::data::{self, DataCaller};
use super::http_calls;
use super::job_calls::{self, JobCaller};
use super::model_calls::{self, ModelResolver};
use super::rounds::{self, HostCalls};
use super::store::{self, InstalledPlugin};
use super::{allowed, approval};
use super::{BoundDir, Manifold, PluginBudget, PluginOutcome, PluginRunner};

const ADMITTED_BYTES_BUDGET: u64 = 512 * 1024 * 1024;

struct Admitted {
    granted: Option<Manifold>,
    declaration: crate::pack::PackPlugin,
    runner: Arc<PluginRunner>,
    budget: PluginBudget,
    bytes: u64,
}

pub struct BindContext<'a> {
    pub workdir: Option<&'a Path>,
    pub session_id: Option<&'a str>,
    pub interactive: bool,
}

impl BindContext<'_> {
    pub fn headless(workdir: Option<&Path>) -> BindContext<'_> {
        BindContext {
            workdir,
            session_id: None,
            interactive: false,
        }
    }
}

fn not_allowed(
    resolved: &allowed::Resolved,
    access: crate::pack::BindAccess,
    granted: Option<crate::pack::BindAccess>,
    write_fields: &[&str],
    context: &BindContext<'_>,
) -> String {
    let flag = match access {
        crate::pack::BindAccess::Read => "",
        crate::pack::BindAccess::ReadWrite => " --access read_write",
    };
    let asked = if context.interactive {
        "the operator declined it; "
    } else {
        ""
    };
    let allow = format!(
        "allow it with `gents plugin dirs add {}{flag}`",
        resolved.folder().display()
    );
    let fields = write_fields
        .iter()
        .map(|field| format!("{field:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    match granted {
        Some(crate::pack::BindAccess::Read) if !write_fields.is_empty() => format!(
            "{} is allowed read-only and this call writes because it sets {fields} ({asked}{allow}, or call again without {fields} to only read)",
            resolved.target.display(),
        ),
        Some(crate::pack::BindAccess::Read) => format!(
            "{} is allowed read-only and this plugin writes on every call ({asked}{allow})",
            resolved.target.display(),
        ),
        _ => format!(
            "{} is outside the folders this call may {} ({asked}{allow})",
            resolved.target.display(),
            access.as_str().replace('_', " and "),
        ),
    }
}

#[derive(Debug, Clone)]
pub struct PluginCall {
    pub coordinate: String,
    pub digest: String,
    pub outcome: PluginOutcome,
    pub binding_note: Option<String>,
    pub receipt: PluginExecutionReceipt,
}

#[derive(Debug, thiserror::Error)]
#[error("{source:#}")]
struct PluginAttemptError {
    source: anyhow::Error,
    receipt: PluginExecutionReceipt,
}

fn json_digest(value: &serde_json::Value) -> String {
    let canonical = crate::workspace::canonical_json_string(value)
        .expect("JSON value has a canonical encoding");
    format!("sha256:{:x}", Sha256::digest(canonical.as_bytes()))
}

pub(super) fn initial_receipt(
    record: &InstalledPlugin,
    input: &serde_json::Value,
) -> PluginExecutionReceipt {
    PluginExecutionReceipt {
        coordinate: format!("{}/{}", record.namespace, record.name),
        artifact_digest: record.digest.clone(),
        input_digest: json_digest(input),
        output_digest: None,
        authority: None,
        limits: None,
        verdict: PluginExecutionVerdict::AdmissionRefused,
    }
}

fn authority(
    manifold: &Manifold,
    bound_field: Option<&str>,
    model: bool,
    http: Option<&http_calls::Session>,
    tools: bool,
) -> PluginExecutionAuthority {
    use afterburner_core::manifold::{EnvAccess, FsAccess, NetAccess};
    let scope = |roots: &[PathBuf]| match bound_field {
        Some(field) => PluginFilesystemScope::BoundInput {
            field: field.to_owned(),
        },
        None => PluginFilesystemScope::Installed {
            roots: roots.len() as u64,
        },
    };
    PluginExecutionAuthority {
        filesystem: match &manifold.fs {
            FsAccess::None => PluginFilesystemGrant::None,
            FsAccess::ReadOnly(roots) => PluginFilesystemGrant::ReadOnly(scope(roots)),
            FsAccess::ReadWrite(roots) => PluginFilesystemGrant::ReadWrite(scope(roots)),
        },
        environment: match &manifold.env {
            EnvAccess::None => PluginEnvironmentGrant::None,
            EnvAccess::AllowList(keys) => PluginEnvironmentGrant::AllowList(keys.clone()),
            EnvAccess::Full => PluginEnvironmentGrant::Full,
        },
        host_http: match &manifold.net {
            _ if http.is_none() => None,
            NetAccess::None => None,
            NetAccess::OutboundHttp(hosts) | NetAccess::OutboundFull(hosts) => {
                Some(PluginHttpGrant {
                    hosts: hosts.clone(),
                    timeout_ms: http.map(http_calls::Session::request_timeout_ms),
                })
            }
        },
        host_model: model,
        host_tools: tools,
    }
}

fn receipt_verdict(verdict: super::PluginVerdict) -> PluginExecutionVerdict {
    use super::PluginVerdict as V;
    match verdict {
        V::Success => PluginExecutionVerdict::Success,
        V::Refused => PluginExecutionVerdict::Refused,
        V::OutOfFuel => PluginExecutionVerdict::OutOfFuel,
        V::OutOfMemory => PluginExecutionVerdict::OutOfMemory,
        V::Timeout => PluginExecutionVerdict::Timeout,
        V::BadOutput => PluginExecutionVerdict::BadOutput,
        V::Failed => PluginExecutionVerdict::Failed,
    }
}

pub struct PluginExecutor {
    home: Option<PathBuf>,
    models: Option<Arc<dyn ModelResolver>>,
    admitted: kovan_map::HopscotchMap<String, Arc<Admitted>>,
    admitted_bytes: AtomicU64,
    services: super::service::Services,
}

impl std::fmt::Debug for PluginExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginExecutor")
            .field("home", &self.home)
            .finish()
    }
}

impl Default for PluginExecutor {
    fn default() -> Self {
        Self::new(None)
    }
}

impl PluginExecutor {
    pub(crate) fn call_sealed_artifact(
        artifact: &[u8],
        input: &serde_json::Value,
        budget: PluginBudget,
    ) -> Result<PluginOutcome> {
        Self::sealed_artifact_runner(artifact)?.call(input, &budget)
    }

    pub(crate) fn validate_sealed_artifact(artifact: &[u8]) -> Result<()> {
        Self::sealed_artifact_runner(artifact).map(|_| ())
    }

    fn sealed_artifact_runner(artifact: &[u8]) -> Result<PluginRunner> {
        static DECLARATION: std::sync::LazyLock<crate::pack::PackPlugin> =
            std::sync::LazyLock::new(|| {
                serde_json::from_value(serde_json::json!({
                    "name": "callback-planner", "description": "Callback ActionPlan",
                    "language": "rust", "artifact": "plugins/callback.afb",
                    "input_schema": {"type":"object"}
                }))
                .expect("the sealed callback declaration is a valid PackPlugin")
            });
        PluginRunner::compile_within(artifact, &DECLARATION, &Manifold::sealed())
    }

    pub fn new(home: Option<PathBuf>) -> Self {
        if let Some(home) = &home {
            allowed::protect(home);
        }
        Self {
            home,
            models: None,
            admitted: kovan_map::HopscotchMap::new(),
            admitted_bytes: AtomicU64::new(0),
            services: super::service::Services::default(),
        }
    }

    pub fn home(&self) -> Option<&std::path::Path> {
        self.home.as_deref()
    }

    pub fn with_models(mut self, models: Arc<dyn ModelResolver>) -> Self {
        self.models = Some(models);
        self
    }

    async fn model_session(
        &self,
        record: &InstalledPlugin,
    ) -> Result<(Option<model_calls::Session>, Option<String>)> {
        let (Some(slot), Some(binding)) = (&record.declaration.model_slot, &record.model_binding)
        else {
            return Ok((None, None));
        };
        let coordinate = format!("{}/{}", record.namespace, record.name);
        let resolver = self.models.as_ref().with_context(|| {
            format!("plugin {coordinate} has its model slot {slot:?} bound, but this runtime cannot reach inference")
        })?;
        let resolved = resolver.resolve(binding).await;
        match resolved {
            Ok(endpoint) => Ok((Some(model_calls::Session::new(endpoint)?), None)),
            Err(_) => {
                // Not logged: the error may name an endpoint or a key variable.
                tracing::warn!(
                    plugin = %coordinate,
                    profile_id = %binding.profile_id,
                    "a plugin's model binding is unusable; it runs without a model"
                );
                Ok((
                    None,
                    Some(format!(
                        "plugin {coordinate} ran without a model: its slot {slot:?} is bound to profile {:?}, which cannot be used; bind it to another profile with `gents plugin bind`, or unbind it with `gents plugin unbind`",
                        binding.profile_id
                    )),
                ))
            }
        }
    }

    pub async fn bind_input(
        &self,
        record: &InstalledPlugin,
        input: &serde_json::Value,
        context: &BindContext<'_>,
    ) -> Result<Option<BoundDir>, String> {
        let Some(binding) = &record.declaration.bind_dir else {
            return Ok(None);
        };
        let plugin = format!("{}/{}", record.namespace, record.name);
        let field = &binding.input_field;
        let requested = match input.get(field) {
            None | Some(serde_json::Value::Null) => return Ok(None),
            Some(serde_json::Value::String(path)) if path.trim().is_empty() => return Ok(None),
            Some(serde_json::Value::String(path)) => path,
            Some(_) => return Err(format!("plugin {plugin} needs {field:?} as a path string")),
        };
        let refuse = |reason: String| format!("plugin {plugin} cannot read {field:?}: {reason}");
        let home = self
            .home
            .as_deref()
            .ok_or_else(|| refuse("this runtime has no home".to_owned()))?;
        let user_home = allowed::user_home();
        let resolved = allowed::resolve(requested, context.workdir, user_home.as_deref(), home)
            .map_err(refuse)?;
        if resolved.is_dir {
            allowed::ensure_mount_safe(&resolved.target)
                .map_err(|error| refuse(format!("{error:#}")))?;
        } else if allowed::is_protected(&resolved.target) {
            return Err(refuse(
                "Runtime permission files cannot be shared with a plugin.".into(),
            ));
        }
        let scope = approval::workspace::allowed_scope(home, context.workdir, user_home.as_deref())
            .map_err(|error| refuse(format!("{error:#}")))?;
        let access = binding.call_access(input);
        let granted = scope.granted(&resolved.target);
        let covered = granted.is_some_and(|granted| granted >= access);
        let route = approval::workspace::binding_route(
            home,
            || {
                Ok(approval::workspace::BindingAction::Plugin {
                    plugin: plugin.clone(),
                    artifact_digest: record.digest.clone(),
                    input_digest: json_digest(input),
                    path: resolved.target.clone(),
                    is_dir: resolved.is_dir,
                    access,
                })
            },
            covered || approval::workspace::within_workspace(&resolved.target),
            || Ok(input.clone()),
        )
        .map_err(|error| refuse(format!("{error:#}")))?;
        let mut consent = None;
        if route.is_some() || !covered {
            let allowed = match route {
                Some(decision) => {
                    let binding = decision.binding.clone();
                    let allowed = approval::workspace::consent_binding(
                        home,
                        decision,
                        context.interactive,
                        context.session_id.map(str::to_owned),
                    )
                    .await
                    .map_err(|error| refuse(format!("{error:#}")))?;
                    consent = Some(approval::workspace::action_consent(home, binding));
                    allowed
                }
                None => match approval::consent(context.interactive) {
                    approval::Consent::Deny => false,
                    approval::Consent::Ask => {
                        let session_id = context.session_id.map(str::to_owned);
                        let request =
                            approval::Request::new(&plugin, &resolved, access, session_id);
                        approval::ask(home, &request, approval::WAIT)
                            .await
                            .map_err(|error| refuse(format!("{error:#}")))?
                    }
                },
            };
            if !allowed {
                return Err(refuse(not_allowed(
                    &resolved,
                    access,
                    granted,
                    &binding.write_fields_set(input),
                    context,
                )));
            }
        }
        let folder_allowed =
            approval::workspace::allowed_scope(home, context.workdir, user_home.as_deref())
                .map_err(|error| refuse(format!("{error:#}")))?
                .granted(resolved.folder())
                .is_some();
        allowed::bind(&resolved, access, folder_allowed)
            .map(|bound| bound.with_consent(consent))
            .map(Some)
            .map_err(refuse)
    }

    pub async fn call_data_bound(
        &self,
        record: &InstalledPlugin,
        input: serde_json::Value,
        tool_root: Option<&Path>,
        tool_calls: bool,
    ) -> Result<PluginCall> {
        self.call_data_bound_for(record, input, tool_root, tool_calls, None)
            .await
    }

    /// [`Self::call_data_bound`] offering `data_calls.query` as `data` when
    /// the plugin declares data.
    pub async fn call_data_bound_for(
        &self,
        record: &InstalledPlugin,
        input: serde_json::Value,
        tool_root: Option<&Path>,
        tool_calls: bool,
        data: Option<DataCaller>,
    ) -> Result<PluginCall> {
        self.call_data_bound_jobs(record, input, tool_root, tool_calls, data, None)
            .await
    }

    /// [`Self::call_data_bound_for`] also offering `job_calls` as `jobs` when
    /// the plugin declares `job`: the first call of a job tool.
    pub async fn call_data_bound_jobs(
        &self,
        record: &InstalledPlugin,
        input: serde_json::Value,
        tool_root: Option<&Path>,
        tool_calls: bool,
        data: Option<DataCaller>,
        jobs: Option<JobCaller>,
    ) -> Result<PluginCall> {
        let session = crate::tools::call::lifecycle::runtime::current_tool_runtime_context();
        let workdir = session
            .as_ref()
            .and_then(|context| context.workspace_cwd.clone())
            .or_else(|| tool_root.map(Path::to_path_buf));
        let session_id = session.and_then(|context| context.session_id);
        let record = &self.resolve(
            &format!("{}/{}", record.namespace, record.name),
            Some(&record.digest),
        )?;
        let context = BindContext {
            workdir: workdir.as_deref(),
            session_id: session_id.as_deref(),
            interactive: approval::interactive(),
        };
        let bound = self
            .bind_input(record, &input, &context)
            .await
            .map_err(anyhow::Error::msg)?;
        self.run(
            record,
            input,
            bound,
            super::tool_calls::Session::for_call(tool_calls),
            data,
            jobs,
        )
        .await
    }

    pub async fn call_data_bound_with_receipt(
        &self,
        record: &InstalledPlugin,
        input: serde_json::Value,
        tool_root: Option<&Path>,
        tool_calls: bool,
    ) -> (Result<PluginCall>, PluginExecutionReceipt) {
        self.call_data_bound_with_receipt_for(record, input, tool_root, tool_calls, None)
            .await
    }

    /// [`Self::call_data_bound_with_receipt`] offering `data_calls.query` as
    /// `data` when the plugin declares data.
    pub async fn call_data_bound_with_receipt_for(
        &self,
        record: &InstalledPlugin,
        input: serde_json::Value,
        tool_root: Option<&Path>,
        tool_calls: bool,
        data: Option<DataCaller>,
    ) -> (Result<PluginCall>, PluginExecutionReceipt) {
        let refused = initial_receipt(record, &input);
        let result = self
            .call_data_bound_for(record, input, tool_root, tool_calls, data)
            .await;
        let receipt = receipt_of(&result, refused);
        (result, receipt)
    }

    /// [`Self::call_data_bound_jobs`] with the receipt of the attempt.
    pub async fn call_data_bound_jobs_with_receipt(
        &self,
        record: &InstalledPlugin,
        input: serde_json::Value,
        tool_root: Option<&Path>,
        tool_calls: bool,
        data: Option<DataCaller>,
        jobs: Option<JobCaller>,
    ) -> (Result<PluginCall>, PluginExecutionReceipt) {
        let refused = initial_receipt(record, &input);
        let result = self
            .call_data_bound_jobs(record, input, tool_root, tool_calls, data, jobs)
            .await;
        let receipt = receipt_of(&result, refused);
        (result, receipt)
    }

    /// One job step of `record`, with the receipt of the attempt. A step
    /// never gets a tool dispatcher (Lean `job_step_has_no_dispatcher`), and
    /// is offered `data_calls` and `job_calls` when the plugin declares them.
    pub async fn call_job_step_with_receipt(
        &self,
        record: &InstalledPlugin,
        input: serde_json::Value,
        data: Option<DataCaller>,
        jobs: JobCaller,
    ) -> (Result<PluginCall>, PluginExecutionReceipt) {
        let refused = initial_receipt(record, &input);
        let result = self.run(record, input, None, None, data, Some(jobs)).await;
        let receipt = receipt_of(&result, refused);
        (result, receipt)
    }

    pub fn resolve(&self, coordinate: &str, pinned: Option<&str>) -> Result<InstalledPlugin> {
        let (namespace, name) = store::parse_coordinate(coordinate)?;
        let home = self.home.as_deref().with_context(|| {
            format!("plugin {coordinate} is not installed: this runtime has no plugin directory")
        })?;
        let record = store::read_record(home, namespace, name)
            .with_context(|| format!("plugin {coordinate} is not installed"))?;
        if let Some(pinned) = pinned {
            anyhow::ensure!(
                record.digest == pinned,
                "plugin {coordinate} is installed as {}, not the pinned {pinned}; reinstall it or update the pin",
                record.digest
            );
        }
        Ok(record)
    }

    // Dropping this future does not stop an admitted call; its plugin wall-clock budget remains authoritative.
    pub async fn call(
        &self,
        record: &InstalledPlugin,
        input: serde_json::Value,
    ) -> Result<PluginCall> {
        self.run(record, input, None, None, None, None).await
    }

    /// [`Self::call`] offering `data_calls.query` as `caller` when the plugin
    /// declares data.
    pub async fn call_with_data(
        &self,
        record: &InstalledPlugin,
        input: serde_json::Value,
        caller: DataCaller,
    ) -> Result<PluginCall> {
        self.run(record, input, None, None, Some(caller), None)
            .await
    }

    /// [`Self::call`] offering `job_calls` as `caller` when the plugin
    /// declares `job` and the invocation is offered the service.
    pub async fn call_with_jobs(
        &self,
        record: &InstalledPlugin,
        input: serde_json::Value,
        caller: JobCaller,
    ) -> Result<PluginCall> {
        self.run(record, input, None, None, None, Some(caller))
            .await
    }

    pub async fn call_bound(
        &self,
        record: &InstalledPlugin,
        input: serde_json::Value,
        bound: BoundDir,
    ) -> Result<PluginCall> {
        self.run(record, input, Some(bound), None, None, None).await
    }

    /// [`Self::call`] granting `bound` and offering `data_calls.query` as
    /// `data`, each when given (a callback invocation).
    pub async fn call_bound_data(
        &self,
        record: &InstalledPlugin,
        input: serde_json::Value,
        bound: Option<BoundDir>,
        data: Option<DataCaller>,
    ) -> Result<PluginCall> {
        self.run(record, input, bound, None, data, None).await
    }

    async fn run(
        &self,
        record: &InstalledPlugin,
        input: serde_json::Value,
        bound: Option<BoundDir>,
        tools: Option<super::tool_calls::Session>,
        data: Option<DataCaller>,
        jobs: Option<JobCaller>,
    ) -> Result<PluginCall> {
        let mut receipt = initial_receipt(record, &input);
        let result: Result<PluginCall> = async {
            let coordinate = receipt.coordinate.clone();
            let current = self.resolve(&coordinate, Some(&record.digest))?;
            anyhow::ensure!(
                bound.is_none() || current == *record,
                "plugin {coordinate} was reinstalled while this call was being authorized; call again"
            );
            let record = &current;
            if let Some(bound) = &bound {
                bound.validate_invocation(&coordinate, &record.digest, &json_digest(&input))?;
            }
            let admitted = self.admit(record)?;
            let mut input = input.clone();
            if record.declaration.service.is_some() {
                if let Some(object) = input.as_object_mut() { object.remove("service"); }
                if bound.is_none() {
                    self.services.prepare(self.home.as_deref().context("service requires a runtime home")?, record, &mut input).await?;
                }
            }
            let (model, binding_note) = self.model_session(record).await?;
            let calls = HostCalls {
                model,
                http: http_calls::Session::for_grant(&coordinate, &admitted.runner.manifold)?,
                tools,
                data: data::Session::for_call(record.declaration.data.as_ref(), data),
                jobs: job_calls::Session::for_call(record.declaration.job.0, &coordinate, jobs),
            };
            let first_input = if calls.is_empty() {
                input.clone()
            } else {
                calls.prepare_input(input.clone())?
            };
            let (effective_input, manifold) = match bound.as_ref() {
                Some(bound) => admitted.runner.prepare_bound(&first_input, bound)?,
                None => (first_input.clone(), admitted.runner.manifold.clone()),
            };
            receipt.input_digest = json_digest(&effective_input);
            receipt.limits = Some(PluginExecutionLimits {
                fuel: admitted.budget.fuel,
                memory_bytes: admitted.budget.memory_bytes,
                wall_ms: admitted.budget.wall_clock.as_millis().try_into().unwrap_or(u64::MAX),
                max_output_bytes: admitted.budget.max_output_bytes as u64,
            });
            let bound_field = bound
                .as_ref()
                .and(record.declaration.bind_dir.as_ref())
                .map(|binding| binding.input_field.as_str());
            receipt.authority = Some(authority(
                &manifold,
                bound_field,
                calls.model.is_some(),
                calls.http.as_ref(),
                calls.tools.is_some(),
            ));
            receipt.verdict = PluginExecutionVerdict::ExecutionError;
            let round = artifact_round(admitted.runner.clone(), bound);
            let outcome = drive_with_calls(&coordinate, round, calls, input, admitted.budget)
            .await
            .inspect_err(|error| {
                if error.downcast_ref::<super::GuestTrap>().is_some() {
                    receipt.verdict = PluginExecutionVerdict::Failed;
                }
            })?;
            receipt.verdict = receipt_verdict(outcome.verdict);
            if outcome.verdict == super::PluginVerdict::Success {
                receipt.output_digest = Some(json_digest(&outcome.output));
            }
            Ok(PluginCall {
                coordinate,
                digest: record.digest.clone(),
                outcome,
                binding_note,
                receipt: receipt.clone(),
            })
        }.await;
        result.map_err(|source| PluginAttemptError { source, receipt }.into())
    }

    fn admit(&self, record: &InstalledPlugin) -> Result<Arc<Admitted>> {
        if let Some(admitted) = self.cached_admission(record) {
            return Ok(admitted);
        }
        let home = self
            .home
            .as_deref()
            .context("this runtime has no plugin directory")?;
        let digest_hex = record.digest.strip_prefix("sha256:").with_context(|| {
            format!("installed plugin digest {:?} is not sha256", record.digest)
        })?;
        let bytes = store::read_bytes(home, digest_hex)?;
        anyhow::ensure!(
            store::content_digest_hex(&bytes) == digest_hex,
            "installed plugin {}/{} bytes do not match their digest {}; reinstall it",
            record.namespace,
            record.name,
            record.digest
        );
        let afb = afterburner_afb::Afb::from_bytes(&bytes).map_err(|error| {
            anyhow::anyhow!(
                "installed plugin {}/{} is not a readable .afb: {error}",
                record.namespace,
                record.name
            )
        })?;
        anyhow::ensure!(
            super::authority::limits_consented(
                record.declaration.limits.as_ref(),
                None,
                record.granted.is_some()
            ),
            "plugin resource limits have no recorded consent; reinstall with --grant-authority"
        );
        let budget = PluginBudget::for_plugin(&afb, &record.declaration);
        let runner = PluginRunner::compile_within(&bytes, &record.declaration, &record.ceiling())?;
        let admitted = Arc::new(Admitted {
            granted: record.granted.clone(),
            declaration: record.declaration.clone(),
            runner: Arc::new(runner),
            budget,
            bytes: bytes.len() as u64,
        });
        let total = self
            .admitted_bytes
            .fetch_add(admitted.bytes, Ordering::Relaxed)
            + admitted.bytes;
        if total > ADMITTED_BYTES_BUDGET {
            self.admitted.clear();
            self.admitted_bytes.store(admitted.bytes, Ordering::Relaxed);
        }
        if let Some(previous) = self
            .admitted
            .insert(record.digest.clone(), admitted.clone())
        {
            self.admitted_bytes
                .fetch_sub(previous.bytes, Ordering::Relaxed);
        }
        Ok(admitted)
    }

    fn cached_admission(&self, record: &InstalledPlugin) -> Option<Arc<Admitted>> {
        self.admitted.get(&record.digest).filter(|admitted| {
            admitted.granted == record.granted && admitted.declaration == record.declaration
        })
    }

    #[cfg(test)]
    pub(crate) fn cached_admission_matches(&self, record: &InstalledPlugin) -> bool {
        self.cached_admission(record).is_some()
    }

    #[cfg(test)]
    pub(crate) fn admitted_len(&self) -> usize {
        self.admitted.len()
    }
}

pub async fn call_runner(
    coordinate: &str,
    runner: Arc<PluginRunner>,
    input: serde_json::Value,
    budget: PluginBudget,
    bound: Option<BoundDir>,
) -> Result<PluginOutcome> {
    drive(coordinate, runner, None, input, budget, bound, None).await
}

/// The receipt of a finished attempt: the call's own, the one its failure
/// carries, or `refused` when it failed before running.
fn receipt_of(
    result: &Result<PluginCall>,
    refused: PluginExecutionReceipt,
) -> PluginExecutionReceipt {
    match result {
        Ok(call) => call.receipt.clone(),
        Err(error) => error
            .downcast_ref::<PluginAttemptError>()
            .map(|error| error.receipt.clone())
            .unwrap_or(refused),
    }
}

async fn drive(
    coordinate: &str,
    runner: Arc<PluginRunner>,
    model: Option<model_calls::Session>,
    input: serde_json::Value,
    budget: PluginBudget,
    bound: Option<BoundDir>,
    tools: Option<super::tool_calls::Session>,
) -> Result<PluginOutcome> {
    let calls = HostCalls {
        model,
        http: http_calls::Session::for_grant(coordinate, &runner.manifold)?,
        tools,
        data: None,
        jobs: None,
    };
    drive_with_calls(
        coordinate,
        artifact_round(runner, bound),
        calls,
        input,
        budget,
    )
    .await
}

/// The round of an artifact: the compiled `.afb` over its bound directory.
fn artifact_round(runner: Arc<PluginRunner>, bound: Option<BoundDir>) -> rounds::Round {
    Arc::new(move |input, budget| match bound.as_ref() {
        Some(bound) => runner.call_bound(&input, &budget, bound),
        None => runner.call(&input, &budget),
    })
}

async fn drive_with_calls(
    coordinate: &str,
    round: rounds::Round,
    calls: HostCalls,
    input: serde_json::Value,
    budget: PluginBudget,
) -> Result<PluginOutcome> {
    if calls.is_empty() {
        tokio::task::spawn_blocking(move || round(input, budget))
            .await
            .with_context(|| format!("plugin {coordinate} stopped unexpectedly"))?
    } else {
        rounds::drive(calls, input, budget, round).await
    }
}
