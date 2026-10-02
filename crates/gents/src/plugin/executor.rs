//! The one path a running agent takes to call an installed plugin.
//!
//! A model tool and a graph stage both call through here, so the lookup of
//! the installed record, the digest pin, the recorded grant and the budget
//! are decided in one place. An admitted runner is cached by artifact digest
//! and grant, so repeated calls skip the store read, the parse and the
//! admission check; a new install or a changed grant is admitted afresh.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};

use super::model_calls::{self, ModelResolver};
use super::store::{self, InstalledPlugin};
use super::{allowed, approval};
use super::{BoundDir, Manifold, PluginBudget, PluginOutcome, PluginRunner};

/// Bytes of admitted artifacts kept in memory before the cache starts over.
// The whole cache is dropped past the budget; per-entry eviction is the
// upgrade if agents ever call more distinct plugins than fit.
const ADMITTED_BYTES_BUDGET: u64 = 512 * 1024 * 1024;

struct Admitted {
    granted: Option<Manifold>,
    declaration: crate::pack::PackPlugin,
    runner: PluginRunner,
    budget: PluginBudget,
    bytes: u64,
}

/// What a data-chosen path is checked against: the session's working folder
/// (`None` when the session has no explicit one; the folder is also ignored
/// when it is too broad, see [`allowed`]), and whether the operator can be asked about a path outside the allowed
/// folders (`interactive`, set by an interactive chat's calls).
pub struct BindContext<'a> {
    pub workdir: Option<&'a Path>,
    pub session_id: Option<&'a str>,
    pub interactive: bool,
}

impl BindContext<'_> {
    /// A call with nobody to ask, such as a graph node.
    pub fn headless(workdir: Option<&Path>) -> BindContext<'_> {
        BindContext {
            workdir,
            session_id: None,
            interactive: false,
        }
    }
}

/// The refusal sentence for a path no folder covers and nobody approved.
fn not_allowed(
    resolved: &allowed::Resolved,
    access: crate::pack::BindAccess,
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
    format!(
        "{} is outside the folders this call may {} ({asked}allow it with `gents plugin dirs add {}{flag}`)",
        resolved.target.display(),
        access.as_str().replace('_', " and "),
        resolved.folder().display()
    )
}

/// One completed call: which artifact ran and what it returned.
#[derive(Debug, Clone)]
pub struct PluginCall {
    pub coordinate: String,
    /// `sha256:<hex>` of the artifact that ran.
    pub digest: String,
    pub outcome: PluginOutcome,
    /// One sentence for the operator when the plugin's model binding could
    /// not be used and the plugin ran without a model; `None` otherwise.
    pub binding_note: Option<String>,
}

/// Calls installed plugins from one gents home.
pub struct PluginExecutor {
    home: Option<PathBuf>,
    models: Option<Arc<dyn ModelResolver>>,
    admitted: kovan_map::HopscotchMap<String, Arc<Admitted>>,
    admitted_bytes: AtomicU64,
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
    /// Plugins installed under `home`; `None` has none installed.
    pub fn new(home: Option<PathBuf>) -> Self {
        if let Some(home) = &home {
            allowed::protect(home);
        }
        Self {
            home,
            models: None,
            admitted: kovan_map::HopscotchMap::new(),
            admitted_bytes: AtomicU64::new(0),
        }
    }

    /// The gents home plugins are installed under, when there is one.
    pub fn home(&self) -> Option<&std::path::Path> {
        self.home.as_deref()
    }

    /// Lets the plugins whose model slot is bound call a model through
    /// `models` (see [`super::model_calls`]).
    pub fn with_models(mut self, models: Arc<dyn ModelResolver>) -> Self {
        self.models = Some(models);
        self
    }

    /// The model session for `record`, when its `model_slot` is bound for
    /// this installation; `None` when it is not, and the plugin then runs
    /// exactly as it does without model calls. A binding that no longer
    /// resolves (profile or backend gone or disabled, key missing) also gives
    /// `None`, with the sentence to show the operator, so a call that does
    /// not need the model still works.
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
                // The error is not logged: it may name an endpoint or a key variable.
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

    /// Binds the path in `input` under `record`'s declared `bind_dir` field
    /// for one call: exactly the file or folder it names, when `context`'s
    /// working folder or the operator's allowed folders cover it with the
    /// access the plugin declares, or when the operator approves it for this
    /// call. `None` when the plugin declares no binding or the input does
    /// not carry the field (the plugin then runs sealed, e.g. on inline
    /// data). The one function a graph node and a model tool both call; the
    /// error is one sentence for the caller.
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
        let scope = allowed::Scope::load(home, context.workdir, user_home.as_deref())
            .map_err(|error| refuse(format!("{error:#}")))?;
        let granted = scope.granted(&resolved.target);
        let covered = granted.is_some_and(|granted| granted >= binding.access);
        if !covered {
            let allowed = context.interactive && {
                let request = approval::Request::new(
                    &plugin,
                    &resolved,
                    binding.access,
                    context.session_id.map(str::to_owned),
                );
                approval::ask(home, &request, approval::WAIT)
                    .await
                    .map_err(|error| refuse(format!("{error:#}")))?
            };
            if !allowed {
                return Err(refuse(not_allowed(&resolved, binding.access, context)));
            }
        }
        let folder_allowed = allowed::Scope::load(home, context.workdir, user_home.as_deref())
            .map_err(|error| refuse(format!("{error:#}")))?
            .granted(resolved.folder())
            .is_some();
        allowed::bind(&resolved, binding.access, folder_allowed)
            .map(Some)
            .map_err(refuse)
    }

    /// [`Self::call`] for a model tool: binds the path the model named under
    /// the ambient session, then runs. The working folder is the session's
    /// workspace folder, else `tool_root`, the root the operator gave the
    /// agent's file tools; never the process's own current directory.
    pub async fn call_data_bound(
        &self,
        record: &InstalledPlugin,
        input: serde_json::Value,
        tool_root: Option<&Path>,
    ) -> Result<PluginCall> {
        let session = crate::tool_call_lifecycle::runtime::current_tool_runtime_context();
        let workdir = session
            .as_ref()
            .and_then(|context| context.workspace_cwd.clone())
            .or_else(|| tool_root.map(Path::to_path_buf));
        let session_id = session.and_then(|context| context.session_id);
        let context = BindContext {
            workdir: workdir.as_deref(),
            session_id: session_id.as_deref(),
            interactive: approval::interactive(),
        };
        match self
            .bind_input(record, &input, &context)
            .await
            .map_err(anyhow::Error::msg)?
        {
            Some(bound) => self.call_bound(record, input, bound).await,
            None => self.call(record, input).await,
        }
    }

    /// The installed record for `coordinate`, which must still be the
    /// artifact `pinned` names when a pin is given.
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

    /// Runs `record`'s artifact once on `input`, within its recorded grant.
    ///
    /// The call runs on the blocking pool. Dropping the future does not stop
    /// a call already running; the plugin's own wall-clock budget does.
    pub async fn call(
        &self,
        record: &InstalledPlugin,
        input: serde_json::Value,
    ) -> Result<PluginCall> {
        self.run(record, input, None).await
    }

    /// [`Self::call`] with `bound` granted for this one call, in the
    /// record's declared `bind_dir` input field (see
    /// [`PluginRunner::call_bound`]).
    pub async fn call_bound(
        &self,
        record: &InstalledPlugin,
        input: serde_json::Value,
        bound: BoundDir,
    ) -> Result<PluginCall> {
        self.run(record, input, Some(bound)).await
    }

    async fn run(
        &self,
        record: &InstalledPlugin,
        input: serde_json::Value,
        bound: Option<BoundDir>,
    ) -> Result<PluginCall> {
        let admitted = self.admit(record)?;
        let coordinate = format!("{}/{}", record.namespace, record.name);
        let (session, binding_note) = self.model_session(record).await?;
        let budget = admitted.budget;
        let bound = bound.map(Arc::new);
        let round: model_calls::Round = Arc::new(move |input, budget| match &bound {
            Some(bound) => admitted.runner.call_bound(&input, &budget, bound),
            None => admitted.runner.call(&input, &budget),
        });
        let outcome = match session {
            Some(session) => model_calls::drive(session, input, budget, round).await?,
            None => tokio::task::spawn_blocking(move || round(input, budget))
                .await
                .with_context(|| format!("plugin {coordinate} stopped unexpectedly"))??,
        };
        Ok(PluginCall {
            coordinate,
            digest: record.digest.clone(),
            outcome,
            binding_note,
        })
    }

    fn admit(&self, record: &InstalledPlugin) -> Result<Arc<Admitted>> {
        if let Some(admitted) = self.admitted.get(&record.digest) {
            if admitted.granted == record.granted && admitted.declaration == record.declaration {
                return Ok(admitted);
            }
        }
        let home = self
            .home
            .as_deref()
            .context("this runtime has no plugin directory")?;
        let digest_hex = record.digest.strip_prefix("sha256:").with_context(|| {
            format!("installed plugin digest {:?} is not sha256", record.digest)
        })?;
        let bytes = store::read_bytes(home, digest_hex)?;
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
            runner,
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

    #[cfg(test)]
    pub(crate) fn admitted_len(&self) -> usize {
        self.admitted.len()
    }
}
