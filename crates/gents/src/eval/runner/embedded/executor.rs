//! The embedded [`TrialExecutor`]: one trial, one fresh DefraDB home.
//!
//! Neither [`TrialExecutor::provision`] nor [`TrialExecutor::execute`] returns
//! an error: a trial whose home, pack or runtime never came up produces
//! [`TrialEvidence::infrastructure`], and the loop owes the slot another attempt.

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::config_client::{
    apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::defra_node::EmbeddedNode;
use crate::document_config::{InferenceSampling, PackConfig, TriggerObservation};
use crate::eval::runner::embedded::home::{
    boot_runtime, EmbeddedHome, RunningRuntime, RUNTIME_READY_TIMEOUT,
};
use crate::eval::runner::embedded::live::LiveObserver;
use crate::eval::runner::embedded::observe::{
    await_terminal, classify_request_outcome, collect_request_evidence, poll_request,
    request_evidence_from_query_data, InferenceCallEvidence, RequestEvidence, TerminalObservation,
};
use crate::eval::runner::executor::{
    Capture, CaptureResult, FileRef, FixtureDocument, InferenceBinding, Isolation, StageEvidence,
    StageSpec, TrialEvidence, TrialExecutor, TrialFixtures, TrialLocator, TrialSpec,
};
use crate::eval::{Anchor, OutcomeKind, ProviderReason, TrialUsage};
use crate::graphql::{
    escape_graphql_string, graphql_with_transaction_retry, validate_collection_identifier,
    validate_graphql_name,
};
use crate::lifecycle::{
    build_signed_request, ExecutionOrigin, RequestIdentity, RequestSigner, RequestSpec,
};
use crate::pack::{
    bind_pack_install_config, declared_paths, digest_declared_assets, load_pack_config,
    PackInferenceBindings, PackInstallOptions, PackManifest,
};
use crate::tool_surface::{BashMode, FileToolMode, ToolCeiling, ToolPolicySurface};
use crate::toolset::CommandExecutionMode;
use crate::{Collection, ConfigAccess, DocumentRuntimeOptions, RuntimeSnapshotObserver};

const GRACE: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(250);
const LIVE_EVERY: Duration = Duration::from_secs(5);

/// Runs each trial in its own embedded home under `runs_dir`.
pub struct EmbeddedExecutor {
    pub runtime_options: DocumentRuntimeOptions,
    pub runs_dir: PathBuf,
    provisioned: Mutex<HashMap<String, Provisioned>>,
}

struct Provisioned {
    home: EmbeddedHome,
    locator: TrialLocator,
}

impl EmbeddedExecutor {
    pub fn new(runtime_options: DocumentRuntimeOptions, runs_dir: PathBuf) -> Self {
        Self {
            runtime_options,
            runs_dir,
            provisioned: Mutex::new(HashMap::new()),
        }
    }

    async fn provision_home(&self, spec: &TrialSpec) -> Result<TrialLocator> {
        let dir = spec.trial_dir.join("home");
        let home = opened(move || async move { EmbeddedHome::create_retained(&dir).await }).await?;
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
    fn provisioned(&self) -> std::sync::MutexGuard<'_, HashMap<String, Provisioned>> {
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
        /// The trial's own latch on its event sources, so a seed stage writes
        /// only once the pack's triggers can see the write.
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

/// The process ceiling of an embedded trial: file tools, read-write, rooted
/// at the trial's own workspace, and host bash only when the definition asks
/// for it. Bash is capped at `WorkspaceWrite`, so no grant an agent holds can
/// write outside the trial root. Freezing still refuses a subject pack that
/// grants itself host bash under [`Isolation::Embedded`]; a Tools document the
/// subject writes at run time is narrowed to this ceiling.
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

/// Run `stages` while reporting the home's [`LiveObserver`] snapshot every
/// [`LIVE_EVERY`], then once more when they end.
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

    /// The loop abandoned a provisioned trial: shut its home down rather than
    /// leave a node running for a trial nobody will take.
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
        let home =
            match opened(move || async move { EmbeddedHome::open_retained(&dir).await }).await {
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
            let failure_kind = match terminal_state {
                Some(state) => classify_request_outcome(state, false, &evidence).map(outcome_kind),
                None => Some(OutcomeKind::Unknown),
            };
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

/// Which side of the provider boundary a failed `InferenceCall` names. `None`
/// is not a third answer about the provider: grading downgrades a provider
/// failure with no reason to Unknown rather than guessing.
pub fn provider_reason_from_failure(failure_reason: &str) -> Option<ProviderReason> {
    let reason = failure_reason.to_lowercase();
    if let Some(status) = http_status(&reason) {
        return Some(match status {
            408 | 429 => ProviderReason::Unavailable,
            400..=499 => ProviderReason::Rejected,
            _ => ProviderReason::Unavailable,
        });
    }
    if ["context", "policy", "content"]
        .iter()
        .any(|named| reason.contains(named))
    {
        return Some(ProviderReason::Rejected);
    }
    if ["connect", "timeout", "timed out", "rate"]
        .iter()
        .any(|named| reason.contains(named))
    {
        return Some(ProviderReason::Unavailable);
    }
    None
}

fn http_status(reason: &str) -> Option<u16> {
    reason
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .filter(|token| token.len() == 3 && token.bytes().all(|byte| byte.is_ascii_digit()))
        .filter_map(|token| token.parse::<u16>().ok())
        .find(|status| (400..600).contains(status))
}

/// Open a trial home from a `Send` future: `EmbeddedNode::build` is not `Send`,
/// so the open is polled to completion on a blocking thread of the runtime the
/// trial will run on.
async fn opened<Open, Opening>(open: Open) -> Result<EmbeddedHome>
where
    Open: FnOnce() -> Opening + Send + 'static,
    Opening: std::future::Future<Output = Result<EmbeddedHome>>,
{
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || runtime.block_on(open()))
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

/// Steps 1 and 2: the pack the run froze, the trial's inference binding, its
/// workspace root and its fixtures, into this trial's own home.
async fn install(spec: &TrialSpec, home: &EmbeddedHome, workspace: &Path) -> Result<()> {
    let (manifest, assets) = read_pack(&spec.pack_dir)?;
    let digest = digest_declared_assets(&manifest, |path| {
        assets
            .get(path)
            .map(Vec::as_slice)
            .with_context(|| format!("pack has no asset {path:?}"))
    })?;
    anyhow::ensure!(
        digest == spec.pack_digest,
        "pack digest mismatch: {} materialized as {digest}, but the run froze {}",
        spec.pack_dir.display(),
        spec.pack_digest
    );

    let node_did = home.did().to_string();
    let config = load_pack_config(
        &manifest,
        &PackInstallOptions {
            node_did: node_did.clone(),
        },
        &|path| {
            assets
                .get(path)
                .cloned()
                .with_context(|| format!("pack has no asset {path:?}"))
        },
        &|name| std::env::var(name).ok(),
    )?;
    let config = bind_inference_slots(&manifest, &config, &spec.inference)
        .context("binding the pack's inference slots to the frozen profile")?;
    let mut config = root_host_tools(config, workspace)?;
    config.node.default_agent_id = Some(spec.agent_id.clone());
    let access = ConfigAccess::local(home.node.clone());
    for path in &manifest.schemas {
        let bytes = assets
            .get(path)
            .with_context(|| format!("pack has no schema asset {path:?}"))?;
        let sdl =
            std::str::from_utf8(bytes).with_context(|| format!("decoding pack schema {path:?}"))?;
        install_schema(&access, sdl)
            .await
            .with_context(|| format!("installing pack schema {path:?}"))?;
    }
    // The binding first: a reference is only installable once what it names exists.
    apply(
        &access,
        "eval.trial.install_inference",
        &inference_plan(&spec.inference, &spec.trial_id, &node_did)?,
    )
    .await
    .context("installing the trial inference binding")?;
    apply(
        &access,
        "eval.trial.install_pack",
        &DesiredStateApplyPlan::from_pack_config(&config)?,
    )
    .await
    .context("installing the trial pack")?;

    install_workspace_root(&home.node, workspace).await?;
    install_fixtures(&access, &spec.fixtures, workspace, &node_did).await
}

fn bind_inference_slots(
    manifest: &PackManifest,
    config: &PackConfig,
    binding: &InferenceBinding,
) -> Result<PackConfig> {
    let profile_id = binding
        .profile
        .get("profile_id")
        .and_then(Value::as_str)
        .context("the trial's InferenceProfile document has no profile_id")?;
    let bindings: PackInferenceBindings = manifest
        .metadata
        .inference_slots
        .iter()
        .filter(|slot| !slot.optional)
        .map(|slot| (slot.name.clone(), profile_id.to_owned()))
        .collect();
    bind_pack_install_config(manifest, config, &bindings)
}

fn root_host_tools(mut config: PackConfig, workspace: &Path) -> Result<PackConfig> {
    let root = workspace
        .to_str()
        .with_context(|| format!("{} is not UTF-8", workspace.display()))?;
    for tools in &mut config.tools {
        if let Some(host) = tools.host.as_mut() {
            host.root.get_or_insert_with(|| root.to_owned());
        }
    }
    Ok(config)
}

async fn apply(
    access: &ConfigAccess,
    operation: &'static str,
    plan: &DesiredStateApplyPlan,
) -> Result<()> {
    access
        .transact_write(operation, |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
}

fn read_pack(pack_dir: &Path) -> Result<(PackManifest, BTreeMap<String, Vec<u8>>)> {
    let manifest_path = pack_dir.join("manifest.json");
    let manifest: PackManifest = serde_json::from_slice(
        &std::fs::read(&manifest_path)
            .with_context(|| format!("reading {}", manifest_path.display()))?,
    )
    .context("parsing pack manifest")?;
    let mut assets = BTreeMap::new();
    for path in declared_paths(&manifest) {
        let file = pack_dir.join(&path);
        let bytes = std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
        assets.insert(path, bytes);
    }
    Ok((manifest, assets))
}

/// Ownership is rewritten to the trial's own DID.
fn inference_plan(
    binding: &InferenceBinding,
    trial_id: &str,
    node_did: &str,
) -> Result<DesiredStateApplyPlan> {
    let mut backend = binding.backend.clone();
    own(&mut backend, "InferenceBackend", node_did)?;

    let mut sampling = match binding.sampling.clone() {
        Some(sampling) => sampling,
        None => serde_json::to_value(InferenceSampling {
            node_did: node_did.to_string(),
            sampling_id: format!("eval-{trial_id}"),
            ..Default::default()
        })?,
    };
    own(&mut sampling, "InferenceSampling", node_did)?;
    object(&mut sampling, "InferenceSampling")?.insert("seed".to_string(), json!(binding.seed));
    let sampling_id = sampling
        .get("sampling_id")
        .and_then(Value::as_str)
        .context("the trial's InferenceSampling document has no sampling_id")?
        .to_string();

    let mut profile = binding.profile.clone();
    own(&mut profile, "InferenceProfile", node_did)?;
    object(&mut profile, "InferenceProfile")?.insert("sampling_id".to_string(), json!(sampling_id));

    let mut documents = Vec::new();
    for (collection, value) in [
        (Collection::InferenceRetryPolicy, &binding.retry_policy),
        (Collection::InferenceExecution, &binding.execution),
    ] {
        if let Some(value) = value {
            let mut value = value.clone();
            own(&mut value, collection.graphql_type(), node_did)?;
            documents.push(desired(collection, value));
        }
    }
    documents.extend([
        desired(Collection::InferenceBackend, backend),
        desired(Collection::InferenceSampling, sampling),
        desired(Collection::InferenceProfile, profile),
    ]);
    DesiredStateApplyPlan::new(documents)
}

fn desired(collection: Collection, value: Value) -> DesiredStateApplyDocument {
    DesiredStateApplyDocument {
        collection,
        add: value.clone(),
        update: value,
    }
}

fn own(value: &mut Value, named: &str, node_did: &str) -> Result<()> {
    object(value, named)?.insert("node_did".to_string(), json!(node_did));
    Ok(())
}

fn object<'a>(value: &'a mut Value, named: &str) -> Result<&'a mut serde_json::Map<String, Value>> {
    value
        .as_object_mut()
        .with_context(|| format!("the trial's {named} document is not an object"))
}

async fn install_workspace_root(
    node: &std::sync::Arc<EmbeddedNode>,
    workspace: &Path,
) -> Result<()> {
    let root = escape_graphql_string(
        workspace
            .to_str()
            .with_context(|| format!("{} is not UTF-8", workspace.display()))?,
    );
    let updated_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            create_WorkspaceRoot(input: {{
                root_path: "{root}",
                display_name: "Eval trial workspace",
                enabled: true,
                updated_at: "{updated_at}"
            }}) {{ _docID }}
        }}"#
    );
    ConfigAccess::write_local(node, "eval.trial.workspace_root", &mutation)
        .await
        .context("publishing the trial workspace root")?;
    Ok(())
}

async fn install_fixtures(
    access: &ConfigAccess,
    fixtures: &TrialFixtures,
    workspace: &Path,
    trial_did: &str,
) -> Result<()> {
    for sdl in &fixtures.schemas {
        install_schema(access, sdl)
            .await
            .context("installing a fixture schema")?;
    }
    for fixture in &fixtures.documents {
        create_document(
            access,
            "eval.trial.fixture_document",
            &fixture.collection,
            &bind_fixture_owner(&fixture.document, trial_did),
        )
        .await?;
    }
    for file in &fixtures.files {
        let path = workspace_path(workspace, &file.path)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&path, &file.contents)
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

async fn install_schema(access: &ConfigAccess, sdl: &str) -> Result<()> {
    let plan = crate::config_client::preview_schema_install(access, sdl).await?;
    crate::config_client::apply_schema_install(access, sdl, &plan.artifact_digest).await?;
    Ok(())
}

pub(crate) fn bind_fixture_owner(value: &Value, trial_did: &str) -> Value {
    match value {
        Value::String(text) if text == "$trial" => Value::String(trial_did.into()),
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|v| bind_fixture_owner(v, trial_did))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), bind_fixture_owner(value, trial_did)))
                .collect(),
        ),
        _ => value.clone(),
    }
}

async fn create_document(
    access: &ConfigAccess,
    label: &'static str,
    collection: &str,
    document: &Value,
) -> Result<String> {
    validate_collection_identifier(collection)?;
    let mutation = format!(
        "mutation($input: {collection}MutationInputArg!) {{ create_{collection}(input: $input) {{ _docID }} }}"
    );
    let variables = json!({ "input": document });
    access
        .transact_write(label, |txn| {
            let (mutation, variables) = (&mutation, &variables);
            Box::pin(async move {
                let response = txn.execute_with_variables(mutation, variables).await?;
                crate::graphql::created_doc_id(&response, collection)
            })
        })
        .await
        .with_context(|| format!("creating a {collection} document"))
}

fn workspace_path(workspace: &Path, relative: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        Path::new(relative)
            .components()
            .all(|component| matches!(component, Component::Normal(_))),
        "workspace path {relative:?} must stay inside the trial workspace"
    );
    Ok(workspace.join(relative))
}

async fn run_stages(
    spec: &TrialSpec,
    cancel: &CancellationToken,
    home: &EmbeddedHome,
    runtime: &RunningRuntime,
    locator: &TrialLocator,
    workspace: &Path,
    ready: &watch::Receiver<Reconciled>,
) -> Vec<StageEvidence> {
    let mut stages = Vec::new();
    for authored in &spec.stages {
        let mut reviewed = authored.clone();
        if authored.review_previous {
            let feedback = stages
                .last()
                .ok_or_else(|| anyhow::anyhow!("no previous stage"))
                .and_then(|previous| spec.review.feedback(previous));
            match feedback {
                Ok(feedback) => reviewed.prompt.push_str(&format!("\n\n{feedback}")),
                Err(error) => {
                    tracing::warn!(%error, "eval review could not be prepared");
                    stages.push(StageEvidence {
                        stage_id: authored.stage_id.clone(),
                        request_id: None,
                        terminal_state: None,
                        failure_kind: Some(OutcomeKind::Grader),
                        provider_reason: None,
                        messages: Vec::new(),
                        tool_calls: Vec::new(),
                        inference_calls: Vec::new(),
                        captures: Default::default(),
                        prods: 0,
                    });
                    break;
                }
            }
        }
        let stage = &reviewed;
        spec.progress.stage_started(&stage.stage_id);
        let evidence = run_stage(
            spec, cancel, home, runtime, locator, workspace, stage, ready,
        )
        .await;
        let mut summary = spec.review.summarize(&evidence);
        summary["usage"] =
            serde_json::to_value(usage_of(&evidence.inference_calls)).unwrap_or(Value::Null);
        spec.progress.stage_result(&stage.stage_id, summary);
        spec.progress.stage_ended(&stage.stage_id);
        let failed = evidence.failure_kind.is_some();
        stages.push(evidence);
        if failed {
            break;
        }
    }
    stages
}

async fn run_stage(
    spec: &TrialSpec,
    cancel: &CancellationToken,
    home: &EmbeddedHome,
    runtime: &RunningRuntime,
    locator: &TrialLocator,
    workspace: &Path,
    stage: &StageSpec,
    ready: &watch::Receiver<Reconciled>,
) -> StageEvidence {
    let observed = submit_and_observe(spec, cancel, home, runtime, locator, stage, ready).await;
    StageEvidence {
        stage_id: stage.stage_id.clone(),
        request_id: observed.request_id,
        terminal_state: observed.terminal_state,
        provider_reason: provider_reason(observed.failure_kind, &observed.evidence),
        failure_kind: observed.failure_kind,
        messages: observed.evidence.messages,
        tool_calls: observed.evidence.tool_calls,
        inference_calls: observed.evidence.inference_calls,
        captures: run_captures(
            &home.node,
            &locator.trial_node_did,
            &locator.session_id,
            workspace,
            &stage.captures,
        )
        .await,
        prods: observed.prods,
    }
}

/// What one stage's request did, before its captures are read.
struct ObservedStage {
    request_id: Option<String>,
    prods: u32,
    terminal_state: Option<RequestLifecycleState>,
    failure_kind: Option<OutcomeKind>,
    evidence: RequestEvidence,
}

impl ObservedStage {
    /// A stage whose request never reached the home is
    /// [`OutcomeKind::Infrastructure`], not `Runtime`: a request that was never
    /// written says nothing about the subject, so the slot is retried rather
    /// than scored.
    fn unsubmitted() -> Self {
        Self {
            request_id: None,
            prods: 0,
            terminal_state: None,
            failure_kind: Some(OutcomeKind::Infrastructure),
            evidence: RequestEvidence::default(),
        }
    }

    /// A stage cancelled before it had a request.
    fn cancelled() -> Self {
        Self {
            failure_kind: Some(OutcomeKind::Runtime),
            ..Self::unsubmitted()
        }
    }
}

#[derive(Debug)]
enum StageSubmission {
    Submitted {
        request_id: String,
        remaining: Duration,
    },
    Failed(OutcomeKind),
}

async fn observe_stage_terminal(
    node: &std::sync::Arc<EmbeddedNode>,
    cancel: &CancellationToken,
    runtime: &RunningRuntime,
    request_id: &str,
    remaining: Duration,
) -> (Result<TerminalObservation>, bool) {
    let watch_started = std::time::Instant::now();
    let stopped;
    let observed = tokio::select! {
        observed = await_terminal(node, request_id, remaining, GRACE, POLL) => {
            stopped = false;
            observed
        }
        () = cancel.cancelled() => {
            stopped = true;
            interrupt_and_settle(node, request_id).await
        }
        // Nothing is left to settle the request; one last unbounded read first,
        // since the request may have settled within the same poll.
        () = runtime_exited(runtime) => {
            let row = poll_request(node, request_id).await.ok().flatten();
            let settled = row.and_then(|row| {
                let terminal_state = RequestLifecycleState::parse(row.lifecycle_state.as_deref()?)
                    .ok()
                    .filter(|state| state.is_terminal())?;
                Some(TerminalObservation {
                    terminal_state,
                    session_id: row.session_id,
                    interrupted_on_deadline: false,
                    elapsed: watch_started.elapsed(),
                })
            });
            stopped = settled.is_none();
            settled.ok_or_else(|| anyhow!("the trial runtime exited before the request settled"))
        }
    };
    (observed, stopped)
}

async fn submit_and_observe(
    spec: &TrialSpec,
    cancel: &CancellationToken,
    home: &EmbeddedHome,
    runtime: &RunningRuntime,
    locator: &TrialLocator,
    stage: &StageSpec,
    ready: &watch::Receiver<Reconciled>,
) -> ObservedStage {
    let stage_started = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let deadline = Duration::from_secs(stage.deadline_secs);
    let started = Instant::now();
    let submit = async {
        match &stage.seed {
            Some(seed) => {
                seed_and_await_fire(home, ready, seed, deadline, &spec.trial_id, &stage.stage_id)
                    .await
            }
            None => {
                let request_id = uuid::Uuid::new_v4().to_string();
                submit_stage(&home.node, locator, &spec.agent_id, stage, &request_id)
                    .await
                    .map(|()| StageSubmission::Submitted {
                        request_id,
                        remaining: deadline,
                    })
            }
        }
    };
    // A cancel that lands between the request write and its return ends here
    // without interrupt_and_settle; the trial home is torn down with it.
    let submitted = tokio::select! {
        biased;
        () = cancel.cancelled() => return ObservedStage::cancelled(),
        submitted = submit => submitted,
    };
    let (request_id, remaining) = match submitted {
        Ok(StageSubmission::Submitted {
            request_id,
            remaining,
        }) => (request_id, remaining),
        Ok(StageSubmission::Failed(kind)) => {
            return ObservedStage {
                failure_kind: Some(kind),
                ..ObservedStage::unsubmitted()
            };
        }
        Err(error) => {
            tracing::warn!(
                error = %format!("{error:#}"),
                trial_id = %spec.trial_id,
                stage_id = %stage.stage_id,
                "eval stage could not be submitted"
            );
            return ObservedStage::unsubmitted();
        }
    };

    let (observed, stopped) =
        observe_stage_terminal(&home.node, cancel, runtime, &request_id, remaining).await;
    let observed = match observed {
        Ok(observed) => Some(observed),
        Err(error) => {
            tracing::warn!(
                error = %format!("{error:#}"),
                trial_id = %spec.trial_id,
                stage_id = %stage.stage_id,
                "eval stage could not be observed to a terminal state"
            );
            None
        }
    };

    let settling = stage.settle && !stopped && !runtime.handle.is_finished() && observed.is_some();
    if settling {
        let budget = deadline.saturating_sub(started.elapsed());
        tokio::select! {
            biased;
            () = cancel.cancelled() => {}
            quiet = settle_home(&home.node, budget) => match quiet {
                Ok(true) => {}
                Ok(false) => tracing::warn!(
                    trial_id = %spec.trial_id,
                    stage_id = %stage.stage_id,
                    "eval stage deadline ended before the trial home was quiet; capturing it with requests still running"
                ),
                Err(error) => tracing::warn!(
                    error = %format!("{error:#}"),
                    trial_id = %spec.trial_id,
                    stage_id = %stage.stage_id,
                    "eval stage could not observe the trial home settling"
                ),
            },
        }
    }

    let (prods, prod_failed) = if settling {
        continue_until_done(spec, cancel, home, locator, stage, deadline, started).await
    } else {
        (0, false)
    };

    let mut collected = true;
    let mut evidence = collect_request_evidence(&home.node, &request_id)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(
                error = %format!("{error:#}"),
                trial_id = %spec.trial_id,
                stage_id = %stage.stage_id,
                "eval stage evidence could not be collected"
            );
            collected = false;
            RequestEvidence::default()
        });
    // The stage's own request decides how the stage ended; later requests in
    // the session are what the subject did next, not a second verdict on it.
    let failure_kind = if prod_failed {
        Some(OutcomeKind::Infrastructure)
    } else {
        stage_failure_kind(observed.as_ref(), collected, stopped, &evidence)
    };
    if settling && collected {
        if let Err(error) =
            extend_with_later_requests(&home.node, &locator.session_id, &request_id, &mut evidence)
                .await
        {
            tracing::warn!(
                error = %format!("{error:#}"),
                trial_id = %spec.trial_id,
                stage_id = %stage.stage_id,
                "eval stage evidence of later session requests could not be collected"
            );
            return ObservedStage {
                request_id: Some(request_id),
                prods,
                terminal_state: observed.as_ref().map(|observed| observed.terminal_state),
                failure_kind: Some(OutcomeKind::Infrastructure),
                evidence,
            };
        }
    }

    ObservedStage {
        request_id: Some(request_id),
        prods,
        terminal_state: observed.as_ref().map(|observed| observed.terminal_state),
        failure_kind: match failure_kind {
            None => {
                trigger_failure(
                    &home.node,
                    &locator.trial_node_did,
                    &stage_started,
                    None,
                    &spec.trial_id,
                    &stage.stage_id,
                )
                .await
            }
            failed => failed,
        },
        evidence,
    }
}

/// The stage's continuation prods: while the `until` capture holds no row and
/// fewer than `max` prods were sent, send the prompt into the trial session,
/// wait for it and for the home to settle. Bounded by the stage's deadline and
/// by cancellation.
async fn continue_until_done(
    spec: &TrialSpec,
    cancel: &CancellationToken,
    home: &EmbeddedHome,
    locator: &TrialLocator,
    stage: &StageSpec,
    deadline: Duration,
    started: Instant,
) -> (u32, bool) {
    let Some(continuation) = &stage.continuation else {
        return (0, false);
    };
    let until = stage.captures.iter().find_map(|capture| match capture {
        Capture::Documents {
            name,
            collection,
            filter,
            fields,
        } if name == &continuation.until => Some((collection, filter, fields)),
        _ => None,
    });
    let Some((collection, filter, fields)) = until else {
        return (0, false);
    };
    let prod = StageSpec {
        prompt: continuation.prompt.clone(),
        ..stage.clone()
    };
    let mut prods = 0;
    while prods < continuation.max && !cancel.is_cancelled() {
        match capture_documents(
            &home.node,
            collection,
            filter,
            fields,
            &locator.trial_node_did,
        )
        .await
        {
            Ok(rows) if !rows.is_empty() => break,
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    error = %format!("{error:#}"),
                    trial_id = %spec.trial_id,
                    "eval continuation could not read its until capture"
                );
                return (prods, true);
            }
        }
        let remaining = deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break;
        }
        let request_id = uuid::Uuid::new_v4().to_string();
        if let Err(error) =
            submit_stage(&home.node, locator, &spec.agent_id, &prod, &request_id).await
        {
            tracing::warn!(
                error = %format!("{error:#}"),
                trial_id = %spec.trial_id,
                "eval continuation prod could not be submitted"
            );
            return (prods, true);
        }
        prods += 1;
        tracing::info!(target: "gents::eval", trial_id = %spec.trial_id, prods, "continuation prod sent");
        tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            _ = await_terminal(&home.node, &request_id, remaining, GRACE, POLL) => {}
        }
        let remaining = deadline.saturating_sub(started.elapsed());
        tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            _ = settle_home(&home.node, remaining) => {}
        }
    }
    (prods, false)
}

/// How long every request in a settling trial home must stay terminal before
/// the home counts as quiet: a trigger fire, a queued delivery or a goal
/// continuation writes its request a moment after the document that causes it.
const SETTLE_QUIET: Duration = Duration::from_secs(60);

const SETTLE_POLL: Duration = Duration::from_secs(2);

/// Wait, within `budget`, until the home has been quiet for [`SETTLE_QUIET`].
/// `Ok(false)` when the budget ran out first.
async fn settle_home(node: &EmbeddedNode, budget: Duration) -> Result<bool> {
    let started = Instant::now();
    let mut quiet: Option<(Instant, usize)> = None;
    loop {
        let (total, running) = request_counts(node).await?;
        let now = Instant::now();
        quiet = match quiet {
            Some((since, seen)) if running == 0 && seen == total => Some((since, seen)),
            _ if running == 0 => Some((now, total)),
            _ => None,
        };
        if quiet.is_some_and(|(since, _)| now.duration_since(since) >= SETTLE_QUIET) {
            return Ok(true);
        }
        if started.elapsed() >= budget {
            return Ok(false);
        }
        tokio::time::sleep(SETTLE_POLL.min(budget.saturating_sub(started.elapsed()))).await;
    }
}

/// Every request in the home, and how many of them are not terminal.
async fn request_counts(node: &EmbeddedNode) -> Result<(usize, usize)> {
    let response = graphql_with_transaction_retry(
        node,
        "{ AgentRequest { lifecycle_state } }",
        "eval trial settle",
    )
    .await?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let running = rows
        .iter()
        .filter(|row| {
            !row.get("lifecycle_state")
                .and_then(Value::as_str)
                .and_then(|state| RequestLifecycleState::parse(state).ok())
                .is_some_and(|state| state.is_terminal())
        })
        .count();
    Ok((rows.len(), running))
}

/// Append evidence from requests the trial session received after this stage
/// began. Each request contributes its own tool calls once; earlier setup
/// requests and calls in child sessions remain outside the stage.
async fn extend_with_later_requests(
    node: &Arc<EmbeddedNode>,
    session_id: &str,
    request_id: &str,
    evidence: &mut RequestEvidence,
) -> Result<()> {
    let requests = session_requests(node, session_id).await?;
    let later = requests
        .iter()
        .skip_while(|request| request.request_id != request_id)
        .skip(1);
    for request in later {
        let more = collect_request_evidence(node, &request.request_id).await?;
        evidence.messages.extend(more.messages);
        evidence.tool_calls.extend(more.tool_calls);
        evidence.inference_calls.extend(more.inference_calls);
    }
    Ok(())
}

fn stage_failure_kind(
    observed: Option<&TerminalObservation>,
    collected: bool,
    stopped: bool,
    evidence: &RequestEvidence,
) -> Option<OutcomeKind> {
    if !collected {
        return Some(OutcomeKind::Infrastructure);
    }
    if stopped {
        return Some(OutcomeKind::Runtime);
    }
    match observed {
        Some(observed) => classify_request_outcome(
            observed.terminal_state,
            observed.interrupted_on_deadline,
            evidence,
        )
        .map(outcome_kind),
        None => Some(OutcomeKind::Infrastructure),
    }
}

fn outcome_kind(classification: &str) -> OutcomeKind {
    match classification {
        "deadline" => OutcomeKind::Deadline,
        "tool" => OutcomeKind::Tool,
        "provider" => OutcomeKind::Provider,
        "runtime" => OutcomeKind::Runtime,
        "infrastructure" => OutcomeKind::Infrastructure,
        _ => OutcomeKind::Unknown,
    }
}

fn provider_reason(
    failure_kind: Option<OutcomeKind>,
    evidence: &RequestEvidence,
) -> Option<ProviderReason> {
    if failure_kind != Some(OutcomeKind::Provider) {
        return None;
    }
    evidence
        .inference_calls
        .iter()
        .filter_map(|call| call.failure_reason.as_deref())
        .find_map(provider_reason_from_failure)
}

/// A trigger in the trial home whose last fire was a failure fails a stage
/// that otherwise completed: a subject-causable error is [`OutcomeKind::Runtime`],
/// an unacknowledged fire or an unreadable status is
/// [`OutcomeKind::Infrastructure`]. A subject-causable error is never downgraded
/// by an unacknowledged fire recorded alongside it.
///
/// Only failures attempted since `since` (whole-second RFC 3339) count; a
/// trigger writes its status asynchronously after the fire. A seed wait
/// supplies its physical source ID so another fire in the same trial, even in
/// the same timestamp second, is unrelated.
async fn trigger_failure(
    node: &EmbeddedNode,
    node_did: &str,
    since: &str,
    source_doc_id: Option<&str>,
    trial_id: &str,
    stage_id: &str,
) -> Option<OutcomeKind> {
    let node_did = escape_graphql_string(node_did);
    let since = escape_graphql_string(since);
    let unacknowledged = crate::trigger_engine::UNACKNOWLEDGED_STATUS;
    let source_filter = source_doc_id.map_or_else(String::new, |doc_id| {
        format!(
            r#", last_fired_source_doc_id: {{ _eq: "{}" }}"#,
            escape_graphql_string(doc_id)
        )
    });
    let query = format!(
        r#"{{ Trigger(filter: {{ node_did: {{ _eq: "{node_did}" }}, last_status: {{ _in: ["error", "{unacknowledged}"] }}, last_attempt_at: {{ _geq: "{since}" }}{source_filter} }}) {{ trigger_id last_attempt_at last_fired_source_doc_id last_status last_error }} }}"#
    );
    let failing = match graphql_with_transaction_retry(node, &query, "eval trial trigger status")
        .await
    {
        Ok(response) => match errored_triggers(response.data.as_ref()) {
            Some(errored) => errored,
            None => {
                tracing::warn!(
                    trial_id = %trial_id,
                    stage_id = %stage_id,
                    "eval trial trigger status read had no Trigger list or a row that did not parse"
                );
                return Some(OutcomeKind::Infrastructure);
            }
        },
        Err(error) => {
            tracing::warn!(
                error = %format!("{error:#}"),
                trial_id = %trial_id,
                stage_id = %stage_id,
                "eval trial trigger status could not be read"
            );
            return Some(OutcomeKind::Infrastructure);
        }
    };
    if failing.is_empty() {
        return None;
    }
    let kind = if failing
        .iter()
        .any(|trigger| trigger.last_status.as_deref() == Some("error"))
    {
        OutcomeKind::Runtime
    } else {
        OutcomeKind::Infrastructure
    };
    tracing::warn!(
        triggers = ?failing,
        outcome = kind.as_str(),
        trial_id = %trial_id,
        stage_id = %stage_id,
        "an eval trial trigger recorded a failed fire; the stage fails"
    );
    Some(kind)
}

/// The `Trigger` rows of a status read, or `None` when the response holds no
/// list of them.
fn errored_triggers(data: Option<&Value>) -> Option<Vec<TriggerObservation>> {
    serde_json::from_value(data?.get("Trigger")?.clone()).ok()
}

async fn runtime_exited(runtime: &RunningRuntime) {
    while !runtime.handle.is_finished() {
        tokio::time::sleep(POLL).await;
    }
}

/// Cancellation reaches a running stage through the same latch a deadline
/// uses: the request is interrupted, then watched until it settles, so the
/// evidence names a state the home actually reached.
async fn interrupt_and_settle(
    node: &std::sync::Arc<EmbeddedNode>,
    request_id: &str,
) -> Result<TerminalObservation> {
    crate::interrupt::interrupt_request(node, request_id)
        .await
        .context("interrupting a cancelled stage")?;
    await_terminal(node, request_id, Duration::ZERO, GRACE, POLL).await
}

/// One stage is one `AgentRequest` in the trial's session, built the way
/// `gents request` builds an interactive one: `local_self` admission, signed as
/// the registered trial node, and the session created implicitly by its
/// first request rather than by a separate `AgentSession` write. The CLI's
/// `valid_until` and `retry` are unset for a request that is nobody's retry,
/// which is exactly what `RequestSpec::new` already carries.
async fn submit_stage(
    node: &std::sync::Arc<EmbeddedNode>,
    locator: &TrialLocator,
    agent_id: &str,
    stage: &StageSpec,
    request_id: &str,
) -> Result<()> {
    let create = build_signed_request(
        RequestSpec::new(
            gents_protocol::request_admission::RequestPurpose::Normal,
            RequestIdentity {
                request_id: request_id.to_string(),
                node_did: locator.trial_node_did.clone(),
                requester_did: None,
                agent_id: agent_id.to_string(),
                session_id: locator.session_id.clone(),
                content: stage.prompt.clone(),
                execution_origin: ExecutionOrigin::Interactive,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            },
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                &locator.trial_node_did,
            ),
        ),
        RequestSigner::RegisteredTarget,
    )
    .await?;
    let mutation = create
        .graphql_mutation()
        .map_err(|error| anyhow!("building the stage request mutation: {error}"))?;
    ConfigAccess::write_local(node, "eval.trial.submit_stage", &mutation).await?;
    Ok(())
}

/// A seed stage: once the runtime's event sources are reconciled, write the
/// document and take the request the pack's trigger fires for it.
async fn seed_and_await_fire(
    home: &EmbeddedHome,
    ready: &watch::Receiver<Reconciled>,
    seed: &FixtureDocument,
    deadline: Duration,
    trial_id: &str,
    stage_id: &str,
) -> Result<StageSubmission> {
    let reconciled = tokio::time::timeout(
        RUNTIME_READY_TIMEOUT,
        ready.clone().wait_for(Option::is_some),
    )
    .await
    .context("waiting for the trial runtime's event sources")?
    .context("the trial runtime stopped before its event sources were reconciled")?
    .clone();
    if let Some(Err(error)) = reconciled {
        anyhow::bail!("the trial runtime's event sources did not reconcile: {error}");
    }
    let access = ConfigAccess::local(home.node.clone());
    let since = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let doc_id = create_document(
        &access,
        "eval.trial.seed_document",
        &seed.collection,
        &bind_fixture_owner(&seed.document, home.did()),
    )
    .await?;
    tracing::debug!(collection = %seed.collection, doc_id = %doc_id, "eval seed document written");
    let started = Instant::now();
    loop {
        if let Some(request_id) = fired_request(&home.node, &doc_id).await? {
            return Ok(StageSubmission::Submitted {
                request_id,
                remaining: deadline.saturating_sub(started.elapsed()),
            });
        }
        if let Some(kind) = trigger_failure(
            &home.node,
            home.did(),
            &since,
            Some(&doc_id),
            trial_id,
            stage_id,
        )
        .await
        {
            return resolve_seed_failure(&home.node, &doc_id, kind, deadline, started).await;
        }
        anyhow::ensure!(
            started.elapsed() < deadline,
            "no trigger fired a request within {deadline:?} of seeding {}",
            seed.collection
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Request publication and trigger bookkeeping are observed in separate read
/// snapshots; a matching request committed during the status read wins.
async fn resolve_seed_failure(
    node: &EmbeddedNode,
    doc_id: &str,
    kind: OutcomeKind,
    deadline: Duration,
    started: Instant,
) -> Result<StageSubmission> {
    Ok(match fired_request(node, doc_id).await? {
        Some(request_id) => StageSubmission::Submitted {
            request_id,
            remaining: deadline.saturating_sub(started.elapsed()),
        },
        None => StageSubmission::Failed(kind),
    })
}

/// The earliest request an event trigger fired for the seed document, keyed by
/// the recorded `caused_by_source_doc_id`.
async fn fired_request(node: &EmbeddedNode, doc_id: &str) -> Result<Option<String>> {
    let query =
        r#"{ AgentRequest(order: { created_at: ASC }) { request_id caused_by_source_doc_id } }"#;
    let response = graphql_with_transaction_retry(node, query, "eval trial fired request").await?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|row| row.get("caused_by_source_doc_id").and_then(Value::as_str) == Some(doc_id))
        .find_map(|row| row.get("request_id")?.as_str())
        .map(ToOwned::to_owned))
}

/// Step 5: what the trial left behind, read out of its own home and workspace.
/// A capture that cannot be read is absent from the map rather than empty: an
/// empty capture says the subject produced nothing, a failed read says the
/// harness broke.
async fn run_captures(
    node: &Arc<EmbeddedNode>,
    trial_did: &str,
    session_id: &str,
    workspace: &Path,
    captures: &[Capture],
) -> BTreeMap<String, CaptureResult> {
    let mut results = BTreeMap::new();
    for capture in captures {
        match capture {
            Capture::Schema { name, collections } => {
                let access = crate::config_client::ConfigAccess::local(node.clone());
                let mut versions = Vec::new();
                let mut failed = false;
                for collection in collections {
                    match access.collection_version(collection).await {
                        Ok(Some(version)) => versions.push(version),
                        Ok(None) => {}
                        Err(error) => {
                            tracing::warn!(error = %format!("{error:#}"), capture = %name,
                                "eval schema capture failed; recording no capture");
                            failed = true;
                            break;
                        }
                    }
                }
                if !failed {
                    results.insert(
                        name.clone(),
                        CaptureResult::Schema {
                            collections: versions,
                        },
                    );
                }
            }
            Capture::Documents {
                name,
                collection,
                filter,
                fields,
            } => match capture_documents(
                node,
                collection,
                &bind_capture_session(filter, session_id),
                fields,
                trial_did,
            )
            .await
            {
                Ok(rows) => {
                    results.insert(name.clone(), CaptureResult::Documents { rows });
                }
                Err(error) => tracing::warn!(
                    error = %format!("{error:#}"),
                    capture = %name,
                    "eval document capture failed; the stage records no capture under that name"
                ),
            },
            Capture::File { name, glob } => {
                if let Some(result) = capture_files(workspace, glob) {
                    results.insert(name.clone(), result);
                }
            }
        }
    }
    results
}

/// `$session` in document captures denotes the durable trial session.
fn bind_capture_session(value: &Value, session_id: &str) -> Value {
    match value {
        Value::String(text) if text == "$session" => Value::String(session_id.into()),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| bind_capture_session(v, session_id))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, v)| (key.clone(), bind_capture_session(v, session_id)))
                .collect(),
        ),
        other => other.clone(),
    }
}

async fn capture_documents(
    node: &EmbeddedNode,
    collection: &str,
    filter: &Value,
    fields: &[String],
    trial_did: &str,
) -> Result<Vec<Value>> {
    let query = documents_capture_query(collection, filter, fields, trial_did)?;
    let response = graphql_with_transaction_retry(node, &query, "eval trial capture").await?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get(collection))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

pub(super) fn documents_capture_query(
    collection: &str,
    filter: &Value,
    fields: &[String],
    trial_did: &str,
) -> Result<String> {
    validate_collection_identifier(collection)?;
    validate_filter_names(filter)?;
    let mut selected = vec!["_docID".to_string()];
    let filter_fields = filter
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, _)| !key.starts_with('_'))
        .map(|(key, _)| key.clone());
    for field in fields.iter().cloned().chain(filter_fields) {
        validate_graphql_name(&field)?;
        if !selected.contains(&field) {
            selected.push(field);
        }
    }
    Ok(format!(
        "query {{ {collection}(filter: {}) {{ {} }} }}",
        render_graphql_input(filter, trial_did),
        selected.join(" ")
    ))
}

fn validate_filter_names(filter: &Value) -> Result<()> {
    match filter {
        Value::Object(map) => map.iter().try_for_each(|(key, nested)| {
            validate_graphql_name(key)?;
            validate_filter_names(nested)
        }),
        Value::Array(items) => items.iter().try_for_each(validate_filter_names),
        _ => Ok(()),
    }
}

pub(crate) fn render_graphql_input(value: &Value, trial_did: &str) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(text) => format!(
            "\"{}\"",
            escape_graphql_string(&text.replace("$trial", trial_did))
        ),
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(|item| render_graphql_input(item, trial_did))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(key, nested)| format!("{key}: {}", render_graphql_input(nested, trial_did)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// The workspace files a capture's glob matches, sorted by path, or `None` when
/// the glob could not be run at all.
fn capture_files(workspace: &Path, glob: &str) -> Option<CaptureResult> {
    match collect_files(workspace, glob) {
        Ok((files, outside_workspace)) => Some(CaptureResult::Files {
            files,
            outside_workspace,
        }),
        Err(error) => {
            tracing::warn!(
                error = %format!("{error:#}"),
                glob = %glob,
                "eval file capture was refused or failed; the stage records no capture under that name"
            );
            None
        }
    }
}

/// The matched files, and the matches that were refused.
///
/// A glob with no parent component can still reach outside the workspace
/// through a symlink inside it, so every match is resolved and checked against
/// the resolved workspace; one that lands outside is skipped and named in the
/// second list rather than silently dropped.
fn collect_files(workspace: &Path, glob: &str) -> Result<(Vec<FileRef>, Vec<String>)> {
    anyhow::ensure!(
        Path::new(glob)
            .components()
            .all(|component| matches!(component, Component::Normal(_))),
        "file capture glob {glob:?} must stay inside the trial workspace"
    );
    let resolved_workspace = workspace
        .canonicalize()
        .with_context(|| format!("resolving {}", workspace.display()))?;
    let root = glob::Pattern::escape(
        workspace
            .to_str()
            .with_context(|| format!("{} is not UTF-8", workspace.display()))?,
    );
    let mut files = Vec::new();
    let mut outside_workspace = Vec::new();
    for matched in glob::glob(&format!("{root}/{glob}")).context("compiling a capture glob")? {
        let path = matched.context("reading a capture glob match")?;
        if !path.is_file() {
            continue;
        }
        let relative = path
            .strip_prefix(workspace)
            .with_context(|| format!("{} is outside the trial workspace", path.display()))?
            .to_str()
            .with_context(|| format!("{} is not UTF-8", path.display()))?
            .to_string();
        let inside = path
            .canonicalize()
            .is_ok_and(|resolved| resolved.starts_with(&resolved_workspace));
        if !inside {
            tracing::warn!(
                path = %path.display(),
                workspace = %resolved_workspace.display(),
                "eval file capture skipped a match that resolves outside the trial workspace"
            );
            outside_workspace.push(relative);
            continue;
        }
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        files.push(FileRef {
            path: relative,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            bytes: bytes.len() as u64,
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    outside_workspace.sort();
    Ok((files, outside_workspace))
}

struct SessionRequest {
    request_id: String,
    lifecycle_state: String,
}

async fn session_requests(node: &EmbeddedNode, session_id: &str) -> Result<Vec<SessionRequest>> {
    let session_id = escape_graphql_string(session_id);
    let scope =
        crate::session::public_request_filter(&format!(r#"session_id: {{ _eq: "{session_id}" }}"#));
    let query = format!(
        r#"{{ AgentRequest(filter: {{ {scope} }}, order: {{ created_at: ASC }}) {{ request_id lifecycle_state }} }}"#
    );
    let response = graphql_with_transaction_retry(node, &query, "eval trial session").await?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|row| {
            Some(SessionRequest {
                request_id: row.get("request_id")?.as_str()?.to_string(),
                lifecycle_state: row
                    .get("lifecycle_state")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect())
}

/// Every inference call in an isolated home belongs to its trial.
async fn home_usage(node: &EmbeddedNode) -> TrialUsage {
    let result = async {
        let response = graphql_with_transaction_retry(
            node,
            "{ InferenceCall { call_seq call_state failure_reason prompt_tokens completion_tokens } }",
            "eval trial total usage",
        ).await?;
        let data = response.data.context("trial usage query omitted data")?;
        anyhow::ensure!(data.get("InferenceCall").is_some_and(Value::is_array), "trial usage query omitted inference calls");
        Ok::<_, anyhow::Error>(usage_of(&request_evidence_from_query_data(&data).inference_calls))
    }.await;
    result.unwrap_or_else(|error| {
        tracing::warn!(error = %format!("{error:#}"), "eval trial usage could not be collected");
        TrialUsage::default()
    })
}

pub(super) fn usage_of<'a>(
    calls: impl IntoIterator<Item = &'a InferenceCallEvidence>,
) -> TrialUsage {
    let mut input_tokens = Some(0u64);
    let mut output_tokens = Some(0u64);
    for call in calls {
        input_tokens = input_tokens
            .zip(call.prompt_tokens)
            .map(|(total, tokens)| total.saturating_add(tokens));
        output_tokens = output_tokens
            .zip(call.completion_tokens)
            .map(|(total, tokens)| total.saturating_add(tokens));
    }
    TrialUsage {
        input_tokens,
        output_tokens,
    }
}

fn anchor(stages: &[StageEvidence]) -> Anchor {
    Anchor {
        terminal_states: stages
            .iter()
            .filter_map(|stage| stage.terminal_state)
            .collect(),
        requests: count(
            stages
                .iter()
                .filter(|stage| stage.request_id.is_some())
                .count(),
        ),
        inference_calls: count(
            stages
                .iter()
                .map(|stage| stage.inference_calls.len())
                .sum::<usize>(),
        ),
    }
}

fn count(total: usize) -> u32 {
    u32::try_from(total).unwrap_or(u32::MAX)
}
