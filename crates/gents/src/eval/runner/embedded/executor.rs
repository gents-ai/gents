pub(super) mod capture;
pub(super) mod evidence;
pub(crate) mod install;
pub(super) mod outcome;
mod settle;
mod stage;
mod submit;

use rapidhash::{HashMapExt, RapidHashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use self::capture::run_captures;
use self::evidence::{anchor, home_usage, session_requests};
use self::install::install;
use self::outcome::{outcome_kind, provider_reason};
use self::stage::run_stages;
use crate::eval::runner::embedded::home::{boot_runtime, EmbeddedHome};
use crate::eval::runner::embedded::live::LiveObserver;
use crate::eval::runner::embedded::observe::{
    classify_request_outcome, collect_request_evidence, RequestEvidence,
};
use crate::eval::runner::executor::{
    Capture, Isolation, StageEvidence, TrialEvidence, TrialExecutor, TrialLocator, TrialSpec,
};
use crate::tool_surface::{BashMode, FileToolMode, ToolCeiling, ToolPolicySurface};
use crate::toolset::CommandExecutionMode;
use crate::{DocumentRuntimeOptions, RuntimeSnapshotObserver};

const POLL: Duration = Duration::from_millis(250);
const LIVE_EVERY: Duration = Duration::from_secs(5);

pub struct EmbeddedExecutor {
    pub runtime_options: DocumentRuntimeOptions,
    pub runs_dir: PathBuf,
    runtime: tokio::runtime::Handle,
    provisioned: Mutex<RapidHashMap<String, Provisioned>>,
}

struct Provisioned {
    home: EmbeddedHome,
    locator: TrialLocator,
}

impl EmbeddedExecutor {
    /// `runtime` drives trial homes, whose opening is not `Send`, on blocking threads.
    pub fn new(
        runtime_options: DocumentRuntimeOptions,
        runs_dir: PathBuf,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            runtime_options,
            runs_dir,
            runtime,
            provisioned: Mutex::new(RapidHashMap::new()),
        }
    }

    async fn provision_home(&self, spec: &TrialSpec) -> Result<TrialLocator> {
        let dir = spec.trial_dir.join("home");
        let home = opened(&self.runtime, move || async move {
            EmbeddedHome::create_retained(&dir).await
        })
        .await?;
        let workspace = workspace_dir(&spec.trial_dir);
        if let Err(error) = std::fs::create_dir_all(&workspace) {
            close(home).await;
            return Err(error).with_context(|| format!("creating {}", workspace.display()));
        }
        let locator = TrialLocator {
            trial_node_did: home.did().to_string(),
            session_id: uuid::Uuid::new_v4().to_string(),
            home_hint: spec
                .trial_dir
                .strip_prefix(&self.runs_dir)
                .ok()
                .and_then(Path::to_str)
                .map(ToOwned::to_owned),
        };
        let replaced = self.provisioned().insert(
            spec.trial_id.clone(),
            Provisioned {
                home,
                locator: locator.clone(),
            },
        );
        if let Some(replaced) = replaced {
            tracing::warn!(
                trial_id = %spec.trial_id,
                "eval trial was provisioned twice; the newer home replaces the older one"
            );
            close(replaced.home).await;
        }
        Ok(locator)
    }

    /// Callers take this guard in a `let` statement and drop it at the
    /// semicolon: holding it across an await would make the future `!Send`.
    fn provisioned(&self) -> std::sync::MutexGuard<'_, RapidHashMap<String, Provisioned>> {
        self.provisioned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub async fn discard(&self, trial_id: &str) {
        // A `let` statement, not an `if let`: on this edition an `if let`
        // scrutinee's temporaries live to the end of the whole expression, so
        // the guard would still be held across the shutdown below.
        let discarded = self.provisioned().remove(trial_id);
        if let Some(discarded) = discarded {
            tracing::warn!(trial_id = %trial_id, "eval trial home discarded unrun");
            close(discarded.home).await;
        }
    }

    async fn run(
        &self,
        spec: &TrialSpec,
        cancel: CancellationToken,
        home: EmbeddedHome,
        locator: TrialLocator,
    ) -> TrialEvidence {
        let workspace = workspace_dir(&spec.trial_dir);
        if let Err(error) = install(spec, &home, &workspace).await {
            close(home).await;
            return infrastructure(&spec.trial_id, locator, &error);
        }
        let (event_sources_ready, ready) = watch::channel(None);
        let mut options = self.runtime_options.clone();
        options.plugin_home = Some(home.path().to_path_buf());
        options.runtime_snapshot_observer = Some(Arc::new(EventSourcesReady(event_sources_ready)));
        options.tool_ceiling = trial_tool_ceiling(&workspace, spec.host_bash);
        let runtime = match boot_runtime(&home, home.identity.clone(), options).await {
            Ok((runtime, _agent)) => runtime,
            Err(error) => {
                close(home).await;
                return infrastructure(&spec.trial_id, locator, &error);
            }
        };

        let stages = observed_live(
            spec,
            &home,
            &locator,
            run_stages(spec, &cancel, &home, &runtime, &locator, &workspace, &ready),
        )
        .await;

        if let Err(error) = runtime.shutdown().await {
            tracing::warn!(
                error = %format!("{error:#}"),
                trial_id = %spec.trial_id,
                "eval trial runtime did not shut down cleanly"
            );
        }
        let usage = home_usage(&home.node).await;
        close(home).await;
        let anchor = anchor(&stages);
        TrialEvidence::new(locator, stages, usage, anchor)
    }
}

/// The process ceiling of an embedded trial. Bash is capped at
/// `WorkspaceWrite`, so no grant an agent holds can write outside the trial
/// root.
fn trial_tool_ceiling(workspace: &Path, host_bash: bool) -> ToolCeiling {
    let mut policy = ToolPolicySurface::ceiling_with_host_modes(
        FileToolMode::ReadWrite,
        if host_bash {
            BashMode::Unrestricted
        } else {
            BashMode::Off
        },
    );
    if host_bash {
        policy.bash.execution_mode = CommandExecutionMode::WorkspaceWrite;
    }
    ToolCeiling::readwrite(workspace).with_policy(policy)
}

async fn observed_live<Stages>(
    spec: &TrialSpec,
    home: &EmbeddedHome,
    locator: &TrialLocator,
    stages: Stages,
) -> Vec<StageEvidence>
where
    Stages: std::future::Future<Output = Vec<StageEvidence>>,
{
    if !spec.progress.attached() {
        return stages.await;
    }
    let observer = LiveObserver::new(&home.node, &locator.trial_node_did, &spec.stages).await;
    let report = || async {
        match observer.snapshot().await {
            Ok(snapshot) => spec.progress.live(snapshot),
            Err(error) => tracing::debug!(
                error = %format!("{error:#}"),
                trial_id = %spec.trial_id,
                "eval trial live snapshot was not read"
            ),
        }
    };
    // Joined, not selected: a slow read must never hold up the stages'
    // deadline and cancellation handling.
    let done = CancellationToken::new();
    let stages = async {
        let stages = stages.await;
        done.cancel();
        stages
    };
    let observe = async {
        let mut every = tokio::time::interval(LIVE_EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = done.cancelled() => break,
                _ = every.tick() => report().await,
            }
        }
    };
    let (stages, ()) = tokio::join!(stages, observe);
    report().await;
    stages
}

type Reconciled = Option<Result<(), String>>;

struct EventSourcesReady(watch::Sender<Reconciled>);

impl RuntimeSnapshotObserver for EventSourcesReady {
    fn on_generation_published(&self, _: u64, _: &str, _: &[String]) {}

    fn on_event_sources_reconciled(&self, generation: u64, _: &str, result: Result<(), &str>) {
        if generation >= 1 && self.0.borrow().is_none() {
            let _ = self.0.send(Some(result.map_err(str::to_owned)));
        }
    }
}

#[async_trait::async_trait]
impl TrialExecutor for EmbeddedExecutor {
    fn isolation(&self) -> Isolation {
        Isolation::Embedded
    }

    async fn provision(&self, spec: &TrialSpec) -> TrialLocator {
        match self.provision_home(spec).await {
            Ok(locator) => locator,
            Err(error) => {
                tracing::warn!(
                    error = %format!("{error:#}"),
                    trial_id = %spec.trial_id,
                    "eval trial home could not be provisioned"
                );
                unprovisioned()
            }
        }
    }

    async fn discard(&self, trial_id: &str) {
        EmbeddedExecutor::discard(self, trial_id).await;
    }

    async fn execute(&self, spec: &TrialSpec, cancel: CancellationToken) -> TrialEvidence {
        let taken = self.provisioned().remove(&spec.trial_id);
        let Some(Provisioned { home, locator }) = taken else {
            tracing::warn!(
                trial_id = %spec.trial_id,
                "eval trial was never provisioned; no home to run it in"
            );
            return TrialEvidence::infrastructure(unprovisioned());
        };
        self.run(spec, cancel, home, locator).await
    }

    async fn recollect(&self, at: &TrialLocator, captures: &[Capture]) -> Option<TrialEvidence> {
        let hint = at.home_hint.as_deref()?;
        let trial_dir = match super::inspect::retained_trial_dir(&self.runs_dir, hint) {
            Ok(dir) => dir,
            Err(error) => {
                tracing::warn!(home_hint = hint, %error, "eval trial home hint is invalid");
                return None;
            }
        };
        let dir = trial_dir.join("home");
        let workspace = workspace_dir(&trial_dir);
        let home = match opened(&self.runtime, move || async move {
            EmbeddedHome::open_retained(&dir).await
        })
        .await
        {
            Ok(home) => home,
            Err(error) => {
                tracing::warn!(
                    error = %format!("{error:#}"),
                    home = %trial_dir.display(),
                    "eval trial home is no longer readable"
                );
                return None;
            }
        };
        let requests = match session_requests(&home.node, &at.session_id).await {
            Ok(requests) => requests,
            Err(error) => {
                tracing::warn!(
                    error = %format!("{error:#}"),
                    session_id = %at.session_id,
                    "eval trial session could not be read back"
                );
                close(home).await;
                return None;
            }
        };

        let mut stages = Vec::new();
        for (index, request) in requests.into_iter().enumerate() {
            let evidence = collect_request_evidence(&home.node, &request.request_id)
                .await
                .unwrap_or_else(|error| {
                    tracing::warn!(
                        error = %format!("{error:#}"),
                        request_id = %request.request_id,
                        "eval trial request evidence could not be collected"
                    );
                    RequestEvidence::default()
                });
            let terminal_state = RequestLifecycleState::parse(&request.lifecycle_state)
                .ok()
                .filter(|state| state.is_terminal());
            let failure_kind =
                classify_request_outcome(terminal_state, false, &evidence).map(outcome_kind);
            stages.push(StageEvidence {
                prods: 0,
                stage_id: index.to_string(),
                request_id: Some(request.request_id),
                terminal_state,
                provider_reason: provider_reason(failure_kind, &evidence),
                failure_kind,
                messages: evidence.messages,
                tool_calls: evidence.tool_calls,
                inference_calls: evidence.inference_calls,
                captures: run_captures(
                    &home.node,
                    &at.trial_node_did,
                    &at.session_id,
                    &workspace,
                    captures,
                )
                .await,
            });
        }
        let usage = home_usage(&home.node).await;
        close(home).await;
        let anchor = anchor(&stages);
        Some(TrialEvidence::new(at.clone(), stages, usage, anchor))
    }
}

/// Open a trial home from a `Send` future: `EmbeddedNode::build` is not `Send`,
/// so the open is polled on a blocking thread of `runtime`.
async fn opened<Open, Opening>(runtime: &tokio::runtime::Handle, open: Open) -> Result<EmbeddedHome>
where
    Open: FnOnce() -> Opening + Send + 'static,
    Opening: std::future::Future<Output = Result<EmbeddedHome>>,
{
    crate::store::local::storage_backend::drive_unsendable(runtime, open)
        .await
        .context("opening a trial home")?
}

async fn close(home: EmbeddedHome) {
    home.node.shutdown().await;
}

fn workspace_dir(trial_dir: &Path) -> PathBuf {
    trial_dir.join("workspace")
}

fn unprovisioned() -> TrialLocator {
    TrialLocator {
        trial_node_did: "did:unprovisioned".to_string(),
        session_id: String::new(),
        home_hint: None,
    }
}

fn infrastructure(trial_id: &str, locator: TrialLocator, error: &anyhow::Error) -> TrialEvidence {
    tracing::warn!(
        error = %format!("{error:#}"),
        trial_id = %trial_id,
        "trial infrastructure failure"
    );
    TrialEvidence::infrastructure(locator)
}

#[cfg(test)]
mod tests;
