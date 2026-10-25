use rapidhash::{HashMapExt, RapidHashMap};
use std::collections::BTreeMap;
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
use gents_protocol::error_chain::ErrorChain;

const GRACE: Duration = Duration::from_secs(30);

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
                error = %ErrorChain(error.as_ref()),
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
                error = %ErrorChain(error.as_ref()),
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
                    error = %ErrorChain(error.as_ref()),
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
                    error = %ErrorChain(error.as_ref()),
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
                    error = %ErrorChain(error.as_ref()),
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
                        error = %ErrorChain(error.as_ref()),
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
/// so the open is polled on a blocking thread of `runtime`.
async fn opened<Open, Opening>(runtime: &tokio::runtime::Handle, open: Open) -> Result<EmbeddedHome>
where
    Open: FnOnce() -> Opening + Send + 'static,
    Opening: std::future::Future<Output = Result<EmbeddedHome>>,
{
    crate::storage_backend::drive_unsendable(runtime, open)
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
        error = %ErrorChain(error.as_ref()),
        trial_id = %trial_id,
        "trial infrastructure failure"
    );
    TrialEvidence::infrastructure(locator)
}

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
    // A reference is only installable once what it names exists.
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
    Ok(bind_pack_install_config(manifest, config, &bindings)?)
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
    Ok(access
        .transact_write(operation, |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await?)
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
    Ok(DesiredStateApplyPlan::new(documents)?)
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
                .and_then(|previous| spec.review.feedback(previous).map_err(anyhow::Error::from));
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
        let env = StageEnv {
            cancel,
            home,
            runtime,
            locator,
            workspace,
            ready,
        };
        let evidence = run_stage(spec, &env, stage).await;
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

#[derive(Clone, Copy)]
struct StageEnv<'a> {
    cancel: &'a CancellationToken,
    home: &'a EmbeddedHome,
    runtime: &'a RunningRuntime,
    locator: &'a TrialLocator,
    workspace: &'a Path,
    ready: &'a watch::Receiver<Reconciled>,
}

async fn run_stage(spec: &TrialSpec, env: &StageEnv<'_>, stage: &StageSpec) -> StageEvidence {
    let StageEnv {
        cancel,
        home,
        runtime,
        locator,
        workspace,
        ready,
    } = *env;
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

struct ObservedStage {
    request_id: Option<String>,
    prods: u32,
    terminal_state: Option<RequestLifecycleState>,
    failure_kind: Option<OutcomeKind>,
    evidence: RequestEvidence,
}

impl ObservedStage {
    fn unsubmitted() -> Self {
        Self {
            request_id: None,
            prods: 0,
            terminal_state: None,
            failure_kind: Some(OutcomeKind::Infrastructure),
            evidence: RequestEvidence::default(),
        }
    }

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
        observed = await_terminal(node, request_id, remaining, GRACE) => {
            stopped = false;
            observed
        }
        () = cancel.cancelled() => {
            stopped = true;
            interrupt_and_settle(node, request_id).await
        }
        // Nothing is left to settle the request; the request may have settled
        // within the same poll, so one last unbounded read first.
        () = runtime_exited(runtime) => {
            let row = poll_request(node, request_id).await.ok().flatten();
            let settled = row.and_then(|row| {
                let terminal_state = RequestLifecycleState::parse(row.lifecycle_state.as_deref()?)
                    .ok()
                    .filter(|state| state.is_terminal())?;
                Some(TerminalObservation {
                    terminal_state: Some(terminal_state),
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
                error = %ErrorChain(error.as_ref()),
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
                error = %ErrorChain(error.as_ref()),
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
                    error = %ErrorChain(error.as_ref()),
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
                error = %ErrorChain(error.as_ref()),
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
                error = %ErrorChain(error.as_ref()),
                trial_id = %spec.trial_id,
                stage_id = %stage.stage_id,
                "eval stage evidence of later session requests could not be collected"
            );
            return ObservedStage {
                request_id: Some(request_id),
                prods,
                terminal_state: observed
                    .as_ref()
                    .and_then(|observed| observed.terminal_state),
                failure_kind: Some(OutcomeKind::Infrastructure),
                evidence,
            };
        }
    }

    ObservedStage {
        request_id: Some(request_id),
        prods,
        terminal_state: observed
            .as_ref()
            .and_then(|observed| observed.terminal_state),
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
                    error = %ErrorChain(error.as_ref()),
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
                error = %ErrorChain(error.as_ref()),
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
            _ = await_terminal(&home.node, &request_id, remaining, GRACE) => {}
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

/// How long every request must stay terminal before the home counts as quiet:
/// a trigger fire, a queued delivery or a goal continuation writes its request
/// a moment after the document that causes it.
const SETTLE_QUIET: Duration = Duration::from_secs(60);

const SETTLE_POLL: Duration = Duration::from_secs(2);

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
                error = %ErrorChain(&error),
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

fn errored_triggers(data: Option<&Value>) -> Option<Vec<TriggerObservation>> {
    serde_json::from_value(data?.get("Trigger")?.clone()).ok()
}

async fn runtime_exited(runtime: &RunningRuntime) {
    while !runtime.handle.is_finished() {
        tokio::time::sleep(POLL).await;
    }
}

async fn interrupt_and_settle(
    node: &std::sync::Arc<EmbeddedNode>,
    request_id: &str,
) -> Result<TerminalObservation> {
    crate::interrupt::interrupt_request(node, request_id)
        .await
        .context("interrupting a cancelled stage")?;
    await_terminal(node, request_id, Duration::ZERO, GRACE).await
}

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
                            tracing::warn!(error = %ErrorChain(&error), capture = %name,
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
                    error = %ErrorChain(error.as_ref()),
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
    anyhow::ensure!(
        !gents_protocol::schemas::is_credential_collection(collection)
            || collection == gents_protocol::schemas::INFERENCE_BACKEND_NAME,
        "credential collection {collection:?} cannot be retained as eval capture evidence"
    );
    validate_filter_names(filter)?;
    for field in fields {
        anyhow::ensure!(
            !crate::defra_query::is_restricted_field(collection, field),
            "field {field:?} on {collection:?} is restricted and cannot be captured"
        );
    }
    let mut filter_fields = Vec::new();
    crate::defra_query::query::collect_filter_field_keys(filter, &mut filter_fields);
    for field in &filter_fields {
        anyhow::ensure!(
            !crate::defra_query::is_restricted_field(collection, field),
            "field {field:?} on {collection:?} is restricted and cannot be used in a capture filter"
        );
    }
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

fn capture_files(workspace: &Path, glob: &str) -> Option<CaptureResult> {
    match collect_files(workspace, glob) {
        Ok((files, outside_workspace)) => Some(CaptureResult::Files {
            files,
            outside_workspace,
        }),
        Err(error) => {
            tracing::warn!(
                error = %ErrorChain(error.as_ref()),
                glob = %glob,
                "eval file capture was refused or failed; the stage records no capture under that name"
            );
            None
        }
    }
}

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
        tracing::warn!(error = %ErrorChain(error.as_ref()), "eval trial usage could not be collected");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_seed_wait_resolves_as_soon_as_reconciliation_reports_an_error() {
        let home = EmbeddedHome::create_temp("seed-reconcile-error")
            .await
            .unwrap();
        let (sender, ready) = watch::channel(None);
        let observer = EventSourcesReady(sender);
        let seed = FixtureDocument {
            collection: "MissingSeedCollection".into(),
            document: json!({"label": "not submitted"}),
        };
        let error = {
            let waiting = seed_and_await_fire(
                &home,
                &ready,
                &seed,
                Duration::from_secs(2),
                "trial",
                "stage",
            );
            tokio::pin!(waiting);
            assert!(futures::poll!(waiting.as_mut()).is_pending());
            observer.on_event_sources_reconciled(1, "fixture", Err("missing seed collection"));
            let std::task::Poll::Ready(result) = futures::poll!(waiting.as_mut()) else {
                panic!("the reported reconciliation error did not end the seed wait");
            };
            result.unwrap_err()
        };
        assert_eq!(
            error.to_string(),
            "the trial runtime's event sources did not reconcile: missing seed collection"
        );
        close(home).await;
    }

    fn assert_failure_class(
        observed: Option<OutcomeKind>,
        kind: OutcomeKind,
        class: crate::eval::EvidenceClass,
    ) {
        assert_eq!(observed, Some(kind));
        let modeled = crate::lean_vocab_test::lean_eval_outcome_cases()
            .iter()
            .find(|case| case.kind == kind.as_str() && case.provider_reason.is_none())
            .expect("the Lean model classifies every outcome without a provider reason");
        assert_eq!(modeled.class, class.as_str());
        assert_eq!(crate::eval::classify(kind, None).as_str(), modeled.class);
    }

    #[test]
    fn a_trial_ceiling_grants_bash_only_when_the_definition_asks_for_it() {
        let workspace = Path::new("/runs/r/trials/t/workspace");
        let without = trial_tool_ceiling(workspace, false);
        assert_eq!(
            (without.file_tools(), without.bash(), without.root()),
            (FileToolMode::ReadWrite, BashMode::Off, Some(workspace))
        );
        let with = trial_tool_ceiling(workspace, true);
        assert_eq!(
            (with.file_tools(), with.bash(), with.root()),
            (
                FileToolMode::ReadWrite,
                BashMode::Unrestricted,
                Some(workspace)
            )
        );
        assert_eq!(
            with.policy().bash.execution_mode,
            CommandExecutionMode::WorkspaceWrite
        );
    }

    #[test]
    fn provider_reason_from_failure_maps_the_table() {
        assert_eq!(
            provider_reason_from_failure("HTTP 503"),
            Some(ProviderReason::Unavailable)
        );
        assert_eq!(
            provider_reason_from_failure("HTTP 429 rate limited"),
            Some(ProviderReason::Unavailable)
        );
        assert_eq!(
            provider_reason_from_failure("HTTP 408 request timeout"),
            Some(ProviderReason::Unavailable)
        );
        assert_eq!(
            provider_reason_from_failure("HTTP 400 context length"),
            Some(ProviderReason::Rejected)
        );
        assert_eq!(
            provider_reason_from_failure("content policy"),
            Some(ProviderReason::Rejected)
        );
        assert_eq!(provider_reason_from_failure("weird"), None);
        assert_eq!(provider_reason_from_failure("model gpt-4o-500k"), None);
    }

    #[tokio::test]
    async fn schema_capture_reads_active_indexes_and_records_absent_collections() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        ConfigAccess::local(node.clone())
            .add_schema("type SearchNote { body: String @fulltext(language: \"english\") }")
            .await
            .unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let captures = run_captures(
            &node,
            "did:test",
            "session-test",
            workspace.path(),
            &[
                Capture::Schema {
                    name: "present".into(),
                    collections: vec!["SearchNote".into()],
                },
                Capture::Schema {
                    name: "absent".into(),
                    collections: vec!["NoSuchCollection".into()],
                },
            ],
        )
        .await;
        let CaptureResult::Schema { collections } = &captures["present"] else {
            panic!("schema evidence")
        };
        assert_eq!(collections[0]["FullTextIndexes"][0]["FieldName"], "body");
        assert_eq!(
            captures["absent"],
            CaptureResult::Schema {
                collections: vec![]
            }
        );
    }

    #[tokio::test]
    async fn delegation_case_capture_excludes_session_title_requests() {
        let home = EmbeddedHome::create_temp("eval-normal-request-capture")
            .await
            .unwrap();
        let did = escape_graphql_string(home.did());
        for purpose in ["normal", "title-audit"] {
            let purpose = escape_graphql_string(purpose);
            ConfigAccess::write_local(
                &home.node,
                "eval.test.normal_request_capture",
                &format!(r#"mutation {{ create_AgentRequest(input: {{request_id: "{purpose}", purpose: "{purpose}", node_did: "{did}", agent_id: "{did}:research-helper", content: "TRAIN-111"}}) {{ _docID }} }}"#),
            )
            .await
            .unwrap();
        }
        let case: Value = serde_json::from_slice(
            &std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join(
                "tests/fixtures/configurator_evals/ladder/l5_agents_tools/cases/train_minimal_self_target.json",
            ))
            .unwrap(),
        )
        .unwrap();
        let capture = case["stages"][1]["capture"]
            .as_array()
            .unwrap()
            .iter()
            .find(|capture| capture["name"] == "helper-request")
            .unwrap();
        let fields: Vec<String> = serde_json::from_value(capture["fields"].clone()).unwrap();
        let mut all_purposes = capture["filter"].clone();
        all_purposes.as_object_mut().unwrap().remove("purpose");
        let all = capture_documents(
            &home.node,
            "AgentRequest",
            &all_purposes,
            &fields,
            home.did(),
        )
        .await
        .unwrap();
        assert_eq!(all.len(), 2);
        let normal = capture_documents(
            &home.node,
            "AgentRequest",
            &capture["filter"],
            &fields,
            home.did(),
        )
        .await
        .unwrap();
        assert_eq!(normal.len(), 1);
        assert_eq!(normal[0]["request_id"], "normal");
        home.node.shutdown().await;
    }

    #[tokio::test]
    async fn document_capture_excludes_other_sessions_with_the_same_sequence() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        let access = ConfigAccess::local(node.clone());
        access
            .add_schema("type CaptureHeader { session_id: String sequence: Int }")
            .await
            .unwrap();
        let session_id = "trial\"session";
        for session in [session_id, "background-session"] {
            access.write("eval.test.capture_session", &format!(
                "mutation {{ create_CaptureHeader(input: {{session_id: \"{}\", sequence: 1}}) {{ _docID }} }}",
                escape_graphql_string(session)
            )).await.unwrap();
        }
        let workspace = tempfile::tempdir().unwrap();
        let captures = run_captures(
            &node,
            "did:test",
            session_id,
            workspace.path(),
            &[Capture::Documents {
                name: "source_headers".into(),
                collection: "CaptureHeader".into(),
                filter: json!({"session_id": {"_eq": "$session"}, "sequence": {"_eq": 1}}),
                fields: vec!["session_id".into(), "sequence".into()],
            }],
        )
        .await;
        let CaptureResult::Documents { rows } = &captures["source_headers"] else {
            panic!("document capture")
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["session_id"], session_id);
        assert_eq!(rows[0]["sequence"], 1);
        assert!(rows[0]["_docID"].as_str().is_some());
        assert_eq!(
            bind_capture_session(&json!("literal $session text"), session_id),
            json!("literal $session text")
        );
        node.shutdown().await;
    }

    #[test]
    fn capture_query_renders_filter_and_escapes_strings() {
        let query = documents_capture_query(
            "AgentRequest",
            &json!({"requester_did": {"_eq": "$trial"}}),
            &["content".to_string()],
            r#"did:x"y"#,
        )
        .unwrap();
        assert!(
            query.contains(r#"filter: {requester_did: {_eq: "did:x\"y"}}"#),
            "{query}"
        );
        assert!(
            query.contains("{ _docID content requester_did }"),
            "{query}"
        );
    }

    #[test]
    fn document_captures_reject_secret_fields_and_oauth_documents() {
        let error = documents_capture_query(
            "InferenceBackend",
            &json!({"node_did": {"_eq": "did:test"}}),
            &["backend_id".into(), "auth".into()],
            "did:test",
        )
        .unwrap_err();
        assert!(error.to_string().contains("InferenceBackend"));
        assert!(error.to_string().contains("restricted"));

        let error = documents_capture_query(
            "InferenceBackend",
            &json!({"_or": [{"auth": {"_eq": {"kind": "api_key"}}}]}),
            &["backend_id".into()],
            "did:test",
        )
        .unwrap_err();
        assert!(error.to_string().contains("restricted"));

        let error = documents_capture_query(
            "OAuthCredential",
            &json!({}),
            &["credential_id".into()],
            "did:test",
        )
        .unwrap_err();
        assert!(error.to_string().contains("credential collection"));

        let safe = documents_capture_query(
            "Task",
            &json!({"node_did": {"_eq": "did:test"}}),
            &["task_id".into(), "display_name".into()],
            "did:test",
        )
        .unwrap();
        assert!(safe.contains("task_id display_name"));
    }

    #[test]
    fn generated_document_capture_cases_match_lean() {
        use std::collections::BTreeSet;

        let cases = crate::lean_vocab_test::lean_eval_document_capture_cases();
        assert!(!cases.is_empty());
        for case in cases {
            let mut filter_fields = Vec::new();
            crate::defra_query::query::collect_filter_field_keys(&case.filter, &mut filter_fields);
            assert_eq!(
                filter_fields.into_iter().collect::<BTreeSet<_>>(),
                case.filter_fields.iter().cloned().collect::<BTreeSet<_>>(),
                "{case:?}"
            );
            let query =
                documents_capture_query(&case.collection, &case.filter, &case.fields, "did:test");
            assert_eq!(query.is_ok(), case.allowed, "{case:?}: {query:?}");
        }
    }

    #[tokio::test]
    async fn l1_backend_capture_grades_metadata_without_retaining_auth() {
        let home = EmbeddedHome::create_temp("eval-backend-metadata")
            .await
            .unwrap();
        let plan = DesiredStateApplyPlan::new(vec![desired(
            Collection::InferenceBackend,
            json!({
                "node_did": home.did(), "backend_id": "metadata-backend", "name": "metadata",
                "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:9/v1",
                "auth": {"kind": "api_key", "key": "eval-secret-sentinel"}, "enabled": true
            }),
        )])
        .unwrap();
        apply(
            &ConfigAccess::local(home.node.clone()),
            "eval.test.backend_metadata",
            &plan,
        )
        .await
        .unwrap();

        let case: Value = serde_json::from_slice(&std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(
                "tests/fixtures/configurator_evals/ladder/l1_inference/cases/val_minimal_limits.json",
            ),
        ).unwrap()).unwrap();
        let stage = &case["stages"][1];
        let capture: Capture = serde_json::from_value(
            stage["capture"]
                .as_array()
                .unwrap()
                .iter()
                .find(|capture| capture["name"] == "backends")
                .unwrap()
                .clone(),
        )
        .unwrap();
        let mut evidence = crate::eval::runner::ScriptedExecutor::passed_evidence(
            home.did(),
            "configure",
            "session-metadata",
            Vec::new(),
        )
        .stages
        .remove(0);
        evidence.captures = run_captures(
            &home.node,
            home.did(),
            "session-metadata",
            home.path(),
            &[capture],
        )
        .await;
        let CaptureResult::Documents { rows } = &evidence.captures["backends"] else {
            panic!("backend metadata capture must contain documents");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["backend_id"], "metadata-backend");
        assert!(rows[0].get("auth").is_none());
        assert!(!serde_json::to_string(&evidence.captures)
            .unwrap()
            .contains("eval-secret-sentinel"));

        let check = stage["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["check"] == "captured_rows_count")
            .unwrap();
        let registry = crate::eval::checks::CheckRegistry::builtin();
        let verdict = registry
            .get("captured_rows_count")
            .unwrap()
            .evaluate(&check["params"], &evidence);
        assert_eq!(verdict.kind, OutcomeKind::Passed, "{:?}", verdict.raw);
        home.node.shutdown().await;
    }

    fn captured(workspace: &Path, glob: &str) -> (Vec<FileRef>, Vec<String>) {
        match capture_files(workspace, glob) {
            Some(CaptureResult::Files {
                files,
                outside_workspace,
            }) => (files, outside_workspace),
            other => panic!("expected a file capture, got {other:?}"),
        }
    }

    #[test]
    fn file_capture_refuses_parent_components() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("report.txt"), b"hello").unwrap();
        std::fs::write(dir.path().join("outside.txt"), b"secret").unwrap();

        assert_eq!(capture_files(&workspace, "../*.txt"), None);
        assert_eq!(capture_files(&workspace, "/etc/*"), None);
        let (found, outside) = captured(&workspace, "*.txt");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "report.txt");
        assert_eq!(found[0].bytes, 5);
        assert!(outside.is_empty());
    }

    #[test]
    fn file_capture_does_not_follow_a_symlink_out_of_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("mine.txt"), b"mine").unwrap();
        let secret = dir.path().join("secret.txt");
        std::fs::write(&secret, b"not the trial's").unwrap();
        std::os::unix::fs::symlink(&secret, workspace.join("escape.txt")).unwrap();

        let (files, outside) = captured(&workspace, "*.txt");
        assert_eq!(
            files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            ["mine.txt"],
            "only the file the trial itself holds is captured"
        );
        assert_eq!(
            outside,
            ["escape.txt"],
            "the skipped match is recorded, not silently dropped"
        );
    }

    #[test]
    fn a_file_capture_that_cannot_be_read_is_omitted() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let locked = workspace.join("locked.txt");
        std::fs::write(&locked, b"unreadable").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&locked).is_ok() {
            return;
        }

        assert_eq!(
            capture_files(&workspace, "*.txt"),
            None,
            "a capture that cannot be read is absent, not empty"
        );

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (files, outside) = captured(&workspace, "*.txt");
        assert_eq!(
            files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            ["locked.txt"]
        );
        assert!(outside.is_empty());
    }

    #[test]
    fn an_observation_that_failed_is_infrastructure() {
        let evidence = RequestEvidence::default();

        assert_eq!(
            stage_failure_kind(None, true, false, &evidence),
            Some(OutcomeKind::Infrastructure),
            "an observation that never returned says nothing about the subject"
        );
        assert_eq!(
            provider_reason(Some(OutcomeKind::Infrastructure), &evidence),
            None
        );
        assert_eq!(
            stage_failure_kind(None, false, false, &evidence),
            Some(OutcomeKind::Infrastructure),
            "evidence that could not be collected is infrastructure too"
        );
        assert_eq!(
            stage_failure_kind(None, true, true, &evidence),
            Some(OutcomeKind::Runtime),
            "a cancelled stage is the request not finishing"
        );
    }

    #[test]
    fn a_lease_failure_uses_the_existing_infrastructure_outcome() {
        let cases: Vec<Value> = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/eval_request_outcome.json"
        )))
        .unwrap();
        let case = cases
            .iter()
            .find(|case| {
                case["state"] == "failed"
                    && case["deadline"] == false
                    && case["request_reason"] == "execution lease expired"
                    && case["inference_reason"].is_null()
                    && case["inference_failed"] == false
                    && case["tool_failed"] == false
            })
            .unwrap();
        let expected = OutcomeKind::parse(case["outcome"].as_str().unwrap()).unwrap();
        let evidence = RequestEvidence {
            failure_reason: Some(case["request_reason"].as_str().unwrap().into()),
            ..Default::default()
        };
        let terminal = TerminalObservation {
            terminal_state: Some(RequestLifecycleState::Failed),
            session_id: Some("session".into()),
            interrupted_on_deadline: false,
            elapsed: Duration::ZERO,
        };
        let outcome = stage_failure_kind(Some(&terminal), true, false, &evidence);
        assert_eq!(outcome, Some(expected));
        assert_eq!(provider_reason(outcome, &evidence), None);
        assert_failure_class(outcome, expected, crate::eval::EvidenceClass::NotEvidence);
    }

    #[test]
    fn a_deadline_without_terminal_evidence_keeps_its_deadline_outcome() {
        let cases: Vec<Value> = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/eval_request_outcome.json"
        )))
        .unwrap();
        let cases: Vec<_> = cases
            .iter()
            .filter(|case| {
                case["state"].is_null()
                    && case["request_reason"].is_null()
                    && case["inference_reason"].is_null()
                    && case["inference_failed"] == false
                    && case["tool_failed"] == false
            })
            .collect();
        assert_eq!(cases.len(), 2);
        for case in cases {
            let observed = TerminalObservation {
                terminal_state: None,
                session_id: Some("session".into()),
                interrupted_on_deadline: case["deadline"].as_bool().unwrap(),
                elapsed: Duration::ZERO,
            };
            let expected = OutcomeKind::parse(case["outcome"].as_str().unwrap()).unwrap();
            assert_eq!(
                stage_failure_kind(Some(&observed), true, false, &RequestEvidence::default()),
                Some(expected),
                "{case}"
            );
        }
    }

    #[tokio::test]
    async fn a_cancelled_seed_stage_is_runtime_before_any_request_exists() {
        let home = EmbeddedHome::create_temp("seed-cancel").await.unwrap();
        let cancel = CancellationToken::new();
        let stage = StageSpec {
            stage_id: "fire".into(),
            prompt: String::new(),
            seed: Some(FixtureDocument {
                collection: "NoSuchItem".into(),
                document: json!({}),
            }),
            deadline_secs: 120,
            settle: false,
            continuation: None,
            review_previous: false,
            captures: Vec::new(),
        };
        let locator = TrialLocator {
            trial_node_did: home.did().to_string(),
            session_id: "s".into(),
            home_hint: None,
        };
        let (_ready_tx, never_ready) = watch::channel(None);
        let canceller = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            canceller.cancel();
        });
        let observed = tokio::time::timeout(
            Duration::from_secs(5),
            submit_and_observe(
                &TrialSpec::empty_for_tests("t1"),
                &cancel,
                &home,
                &exited_runtime(),
                &locator,
                &stage,
                &never_ready,
            ),
        )
        .await
        .expect("the cancel ends the stage at once");
        assert_eq!(
            (observed.request_id, observed.failure_kind),
            (None, Some(OutcomeKind::Runtime))
        );
        assert_eq!(observed.terminal_state, None);
    }

    #[tokio::test]
    async fn installing_a_pack_binds_its_inference_slot_to_the_frozen_profile() {
        let dir = tempfile::tempdir().unwrap();
        let pack_dir = dir.path().join("pack");
        write_slot_pack(&pack_dir, "gents:inference-slot:primary");
        let home = EmbeddedHome::create_temp("slot-binding").await.unwrap();
        let workspace = dir.path().join("workspace");
        let spec = TrialSpec {
            pack_digest: materialized_digest(&pack_dir),
            pack_dir,
            inference: frozen_binding(json!("frozen-profile")),
            agent_id: "subject".into(),
            ..TrialSpec::empty_for_tests("t1")
        };

        install(&spec, &home, &workspace).await.unwrap();

        let response = home
            .node
            .execute("query { Agent { agent_id inference_profile_id } }")
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let rows = response.data.as_ref().unwrap()["Agent"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["agent_id"], "subject");
        assert_eq!(
            rows[0]["inference_profile_id"], "frozen-profile",
            "the slot marker never reaches the trial home"
        );
        home.node.shutdown().await;
    }

    #[tokio::test]
    async fn a_trial_subject_supplies_an_omitted_default_agent() {
        let dir = tempfile::tempdir().unwrap();
        let pack_dir = dir.path().join("pack");
        write_slot_pack(&pack_dir, "gents:inference-slot:primary");
        let path = pack_dir.join("pack_config.json");
        let config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(config["node"].get("default_agent_id").is_none());
        let bytes = serde_json::to_vec(&config).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let spec = TrialSpec {
            pack_digest: materialized_digest(&pack_dir),
            pack_dir,
            agent_id: "subject".into(),
            inference: frozen_binding(json!("frozen-profile")),
            ..TrialSpec::empty_for_tests("subject-default")
        };
        let home = EmbeddedHome::create_temp("subject-default").await.unwrap();
        install(&spec, &home, &dir.path().join("workspace"))
            .await
            .unwrap();

        let access = ConfigAccess::local(home.node.clone());
        let response = access
            .execute("query { Node { default_agent_id } }")
            .await
            .unwrap();
        assert_eq!(response["data"]["Node"][0]["default_agent_id"], "subject");
        let agent = crate::Gents::from_default_agent_documents(
            home.node.clone(),
            home.identity.clone(),
            DocumentRuntimeOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(agent.default_agent_id(), "subject");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        home.node.shutdown().await;
    }

    #[tokio::test]
    async fn a_declared_slot_the_binding_cannot_fill_is_infrastructure() {
        let dir = tempfile::tempdir().unwrap();
        let runs_dir = dir.path().join("runs");
        let pack_dir = runs_dir
            .join("run-1")
            .join("cells")
            .join("base")
            .join("pack");
        write_slot_pack(&pack_dir, "gents:inference-slot:primary");
        let executor = EmbeddedExecutor::new(
            DocumentRuntimeOptions::default(),
            runs_dir.clone(),
            crate::test_support::runtime_handle(),
        );
        let spec = TrialSpec {
            pack_digest: materialized_digest(&pack_dir),
            pack_dir,
            inference: frozen_binding(Value::Null),
            trial_dir: runs_dir.join("run-1").join("trials").join("t1"),
            ..TrialSpec::empty_for_tests("t1")
        };

        let home = EmbeddedHome::create_temp("unbindable-slot").await.unwrap();
        let error = install(&spec, &home, &dir.path().join("workspace"))
            .await
            .unwrap_err();
        let reason = format!("{}", ErrorChain(error.as_ref()));
        assert!(
            reason.contains("inference slots") && reason.contains("profile_id"),
            "{reason}"
        );
        home.node.shutdown().await;

        executor.provision(&spec).await;
        let evidence = executor.execute(&spec, CancellationToken::new()).await;
        assert!(evidence.stages.is_empty(), "{:?}", evidence.stages);
        assert_eq!(evidence.anchor.requests, 0);
    }

    #[tokio::test]
    async fn subject_schemas_are_installed_before_documents_without_fixture_copies() {
        let dir = tempfile::tempdir().unwrap();
        let pack_dir = dir.path().join("pack");
        write_slot_pack(&pack_dir, "gents:inference-slot:primary");
        let schema_path = "schemas/submission.graphql";
        let sdl = "type EvalSubmission { message: String }";
        std::fs::create_dir_all(pack_dir.join("schemas")).unwrap();
        std::fs::write(pack_dir.join(schema_path), sdl).unwrap();
        let manifest_path = pack_dir.join("manifest.json");
        let mut manifest: Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest["schemas"] = json!([schema_path]);
        manifest["assets"]
            .as_array_mut()
            .unwrap()
            .push(json!(schema_path));
        std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let config_path = pack_dir.join("pack_config.json");
        let mut config: Value =
            serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
        config["datastore_tool_surfaces"] = json!([{
            "surface_id": "submissions",
            "entries": [{
                "tool_name": "submit_message",
                "collection": "EvalSubmission",
                "description": "Record a submission.",
                "fields": [{"name": "message", "required": true}]
            }]
        }]);
        std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
        let spec = TrialSpec {
            pack_digest: materialized_digest(&pack_dir),
            pack_dir,
            agent_id: "subject".into(),
            inference: frozen_binding(json!("frozen-profile")),
            fixtures: TrialFixtures {
                documents: vec![FixtureDocument {
                    collection: "EvalSubmission".into(),
                    document: json!({"message": "from the fixture"}),
                }],
                ..Default::default()
            },
            ..TrialSpec::empty_for_tests("subject-schema")
        };
        let home = EmbeddedHome::create_temp("subject-schema").await.unwrap();
        let workspace = dir.path().join("workspace");
        install(&spec, &home, &workspace).await.unwrap();
        let access = ConfigAccess::local(home.node.clone());
        let response = access
            .execute("query { EvalSubmission { message } }")
            .await
            .unwrap();
        assert_eq!(
            response["data"]["EvalSubmission"],
            json!([{"message": "from the fixture"}])
        );

        let fixtures = TrialFixtures {
            schemas: vec![sdl.into()],
            ..Default::default()
        };
        install_fixtures(&access, &fixtures, &workspace, home.did())
            .await
            .unwrap();
        let incompatible = TrialFixtures {
            schemas: vec!["type EvalSubmission { message: Int }".into()],
            ..Default::default()
        };
        let error = install_fixtures(&access, &incompatible, &workspace, home.did())
            .await
            .unwrap_err();
        assert!(format!("{}", ErrorChain(error.as_ref())).contains("EvalSubmission"));
        assert_eq!(
            access
                .execute("query { EvalSubmission { message } }")
                .await
                .unwrap(),
            response
        );
        home.node.shutdown().await;
    }

    fn materialized_digest(pack_dir: &Path) -> String {
        let (manifest, assets) = read_pack(pack_dir).unwrap();
        digest_declared_assets(&manifest, |path| {
            assets.get(path).map(Vec::as_slice).context("missing asset")
        })
        .unwrap()
    }

    fn frozen_binding(profile_id: Value) -> InferenceBinding {
        let mut profile = json!({
            "node_did": "did:key:frozen",
            "backend_id": "frozen-backend",
            "model_name": "frozen-model",
        });
        if !profile_id.is_null() {
            profile["profile_id"] = profile_id;
        }
        InferenceBinding {
            profile,
            backend: json!({
                "node_did": "did:key:frozen",
                "backend_id": "frozen-backend",
                "name": "Frozen backend",
                "provider_kind": "OpenAiCompatible",
                "endpoint": "http://127.0.0.1:1/v1",
                "auth": {"kind": "unauthenticated"},
            }),
            sampling: None,
            execution: None,
            retry_policy: None,
            seed: 7,
        }
    }

    fn write_slot_pack(root: &Path, inference_profile_id: &str) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join("README.md"), "# slot fixture\n").unwrap();
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_vec(&json!({
                "manifest_version": 1,
                "name": "slot_fixture",
                "version": "1.0.0",
                "description": "A pack whose agent references an inference slot.",
                "authors": ["gents-ai contributors"],
                "kind": "documents",
                "assets": ["README.md", "pack_config.json"],
                "config": "pack_config.json",
                "inference_slots": [{
                    "name": "primary",
                    "description": "Runs the subject agent.",
                    "agents": ["subject"],
                }],
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            root.join("pack_config.json"),
            serde_json::to_vec(&json!({
                "node": {},
                "agents": [{
                    "agent_id": "subject",
                    "display_name": "Subject",
                    "inference_profile_id": inference_profile_id,
                }],
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn trial_teardown_settles_scheduled_inference_without_expiring_its_lease() {
        use crate::support::streaming_backend::{
            MockStreamingBackend, StreamPlan, StreamResponse, StreamScript,
        };

        for cancel_stage in [false, true] {
            let marker = "scheduled-trial-teardown";
            let backend = MockStreamingBackend::start_with_plans(
                "frozen-model",
                vec![StreamPlan::new(
                    marker,
                    vec![StreamResponse::Stream(StreamScript::paused(
                        marker,
                        ["received before shutdown"],
                    ))],
                )],
            )
            .unwrap();
            let dir = tempfile::tempdir().unwrap();
            let pack_dir = dir.path().join("pack");
            write_slot_pack(&pack_dir, "gents:inference-slot:primary");
            let mut inference = frozen_binding(json!("frozen-profile"));
            inference.backend["endpoint"] = json!(backend.endpoint());
            let spec = TrialSpec {
                pack_digest: materialized_digest(&pack_dir),
                pack_dir,
                agent_id: "subject".into(),
                inference,
                ..TrialSpec::empty_for_tests("scheduled-teardown")
            };
            let home = EmbeddedHome::create_temp("scheduled-teardown")
                .await
                .unwrap();
            install(&spec, &home, &dir.path().join("workspace"))
                .await
                .unwrap();
            let (runtime, _agent) = boot_runtime(
                &home,
                home.identity.clone(),
                DocumentRuntimeOptions::default(),
            )
            .await
            .unwrap();
            let request_id = uuid::Uuid::new_v4().to_string();
            let request = build_signed_request(
                RequestSpec::new(
                    gents_protocol::request_admission::RequestPurpose::Normal,
                    RequestIdentity {
                        request_id: request_id.clone(),
                        node_did: home.did().into(),
                        requester_did: None,
                        agent_id: "subject".into(),
                        session_id: uuid::Uuid::new_v4().to_string(),
                        content: marker.into(),
                        execution_origin: ExecutionOrigin::Scheduled,
                        created_at: chrono::Utc::now()
                            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    },
                    gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                        home.did(),
                    ),
                ),
                RequestSigner::RegisteredTarget,
            )
            .await
            .unwrap();
            ConfigAccess::write_local(
                &home.node,
                "test.eval_scheduled_shutdown",
                &request.graphql_mutation().unwrap(),
            )
            .await
            .unwrap();
            tokio::time::timeout(Duration::from_secs(30), async {
                while backend.observed_chunks(marker) == 0 {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("scheduled inference must reach its paused provider stream");
            let before = graphql_with_transaction_retry(
                &home.node,
                &super::super::observe::evidence_query(&request_id),
                "test.eval_scheduled_inference",
            )
            .await
            .unwrap();
            let before = request_evidence_from_query_data(before.data.as_ref().unwrap());
            assert!(before
                .inference_calls
                .iter()
                .any(|call| call.call_state.as_deref() == Some("running")));

            if cancel_stage {
                let cancel = CancellationToken::new();
                cancel.cancel();
                let (terminal, stopped) = tokio::time::timeout(
                    Duration::from_secs(30),
                    observe_stage_terminal(
                        &home.node,
                        &cancel,
                        &runtime,
                        &request_id,
                        Duration::from_secs(120),
                    ),
                )
                .await
                .expect("stage cancellation must settle active inference");
                assert!(stopped);
                assert_eq!(
                    terminal.unwrap().terminal_state,
                    Some(RequestLifecycleState::Interrupted)
                );
            }

            tokio::time::timeout(Duration::from_secs(30), runtime.shutdown())
                .await
                .expect("trial shutdown must settle active inference")
                .unwrap();
            let state = poll_request(&home.node, &request_id)
                .await
                .unwrap()
                .unwrap();
            let terminal_state =
                RequestLifecycleState::parse(state.lifecycle_state.as_deref().unwrap()).unwrap();
            assert!(terminal_state.is_terminal());
            let evidence = collect_request_evidence(&home.node, &request_id)
                .await
                .unwrap();
            assert!(!evidence.inference_calls.is_empty());
            assert!(evidence.inference_calls.iter().all(|call| {
                matches!(
                    call.call_state.as_deref(),
                    Some("completed" | "failed" | "cancelled")
                ) && call.ended_at.is_some()
            }));
            assert_ne!(
                classify_request_outcome(Some(terminal_state), false, &evidence),
                Some("infrastructure"),
                "{evidence:?}"
            );
            close(home).await;
        }
    }

    #[tokio::test]
    async fn provision_then_execute_on_a_bad_pack_digest_is_infrastructure() {
        let dir = tempfile::tempdir().unwrap();
        let runs_dir = dir.path().join("runs");
        let pack_dir = runs_dir
            .join("run-1")
            .join("cells")
            .join("base")
            .join("pack");
        write_minimal_pack(&pack_dir);
        let executor = EmbeddedExecutor::new(
            DocumentRuntimeOptions::default(),
            runs_dir.clone(),
            crate::test_support::runtime_handle(),
        );
        let spec = TrialSpec {
            pack_dir,
            pack_digest: "sha256:wrong".to_string(),
            trial_dir: runs_dir.join("run-1").join("trials").join("t1"),
            ..TrialSpec::empty_for_tests("t1")
        };

        let (manifest, assets) = read_pack(&spec.pack_dir).unwrap();
        let materialized = digest_declared_assets(&manifest, |path| {
            assets.get(path).map(Vec::as_slice).context("missing asset")
        })
        .unwrap();
        assert!(materialized.starts_with("sha256:"));
        assert_ne!(materialized, spec.pack_digest);

        let locator = executor.provision(&spec).await;
        assert!(locator.trial_node_did.starts_with("did:"));
        assert_eq!(locator.home_hint.as_deref(), Some("run-1/trials/t1"));
        assert!(spec.trial_dir.join("workspace").is_dir());

        let evidence = executor.execute(&spec, CancellationToken::new()).await;
        assert_eq!(evidence.locator.trial_node_did, locator.trial_node_did);
        assert!(evidence.stages.is_empty());
        assert_eq!(evidence.anchor.requests, 0);

        let reopened = EmbeddedHome::open_retained(&spec.trial_dir.join("home"))
            .await
            .expect("the trial home survives the close");
        assert_eq!(reopened.did(), locator.trial_node_did);
        reopened.node.shutdown().await;
    }

    #[tokio::test]
    async fn trial_usage_includes_child_requests_and_preserves_unknown_tokens() {
        let home = EmbeddedHome::create_temp("trial-total-usage")
            .await
            .unwrap();
        for (id, input, output) in [("stage-call", 11, 7), ("automation-call", 101, 13)] {
            let id = escape_graphql_string(id);
            crate::config_client::ConfigAccess::write_local(
                &home.node,
                "eval.test.usage",
                &format!(r#"mutation {{ create_InferenceCall(input: {{ call_id: "{id}", request_id: "{id}", call_seq: 0, call_state: "completed", prompt_tokens: {input}, completion_tokens: {output} }}) {{ _docID }} }}"#),
            ).await.unwrap();
        }
        let total = home_usage(&home.node).await;
        assert_eq!(total.input_tokens, Some(112));
        assert_eq!(total.output_tokens, Some(20));
        crate::config_client::ConfigAccess::write_local(
            &home.node,
            "eval.test.usage_missing",
            r#"mutation { create_InferenceCall(input: { call_id: "unreported", request_id: "child", call_seq: 1, call_state: "failed", prompt_tokens: null, completion_tokens: 3 }) { _docID } }"#,
        ).await.unwrap();
        let total = home_usage(&home.node).await;
        assert_eq!(total.input_tokens, None);
        assert_eq!(total.output_tokens, Some(23));
        close(home).await;
    }

    #[tokio::test]
    async fn recollect_preserves_missing_terminal_evidence_as_infrastructure() {
        let cases: Vec<Value> = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/eval_request_outcome.json"
        )))
        .unwrap();
        let case = cases
            .iter()
            .find(|case| {
                case["state"].is_null()
                    && case["deadline"] == false
                    && case["request_reason"].is_null()
                    && case["inference_reason"].is_null()
                    && case["inference_failed"] == false
                    && case["tool_failed"] == false
            })
            .unwrap();
        let expected = OutcomeKind::parse(case["outcome"].as_str().unwrap()).unwrap();
        let runs = tempfile::tempdir().unwrap();
        let home = EmbeddedHome::create_retained(&runs.path().join("trial/home"))
            .await
            .unwrap();
        for state in ["pending", "processing"] {
            seed_request_with_state(&home, &format!("retained-{state}"), state).await;
        }
        let locator = TrialLocator {
            trial_node_did: home.did().to_owned(),
            session_id: "s-1".into(),
            home_hint: Some("trial".into()),
        };
        close(home).await;

        let executor = EmbeddedExecutor::new(
            DocumentRuntimeOptions::default(),
            runs.path().to_owned(),
            crate::test_support::runtime_handle(),
        );
        let evidence = executor.recollect(&locator, &[]).await.unwrap();
        assert_eq!(evidence.stages.len(), 2);
        assert!(evidence.anchor.terminal_states.is_empty());
        for stage in evidence.stages {
            assert_eq!(
                serde_json::to_value(stage.terminal_state).unwrap(),
                case["observed_terminal_state"]
            );
            assert_eq!(stage.failure_kind, Some(expected));
            assert_eq!(stage.provider_reason, None);
        }
    }

    #[tokio::test]
    async fn recollect_refuses_a_home_hint_outside_the_runs_directory() {
        let dir = tempfile::tempdir().unwrap();
        let runs_dir = dir.path().join("runs");
        let outside = dir.path().join("outside");
        close(
            EmbeddedHome::create_retained(&outside.join("home"))
                .await
                .unwrap(),
        )
        .await;
        let executor = EmbeddedExecutor::new(
            DocumentRuntimeOptions::default(),
            runs_dir.clone(),
            crate::test_support::runtime_handle(),
        );
        let locator = |hint: &str| TrialLocator {
            trial_node_did: "did:key:zAny".to_string(),
            session_id: "s-1".to_string(),
            home_hint: Some(hint.to_string()),
        };
        for hint in ["../outside", outside.to_str().unwrap()] {
            assert!(
                executor.recollect(&locator(hint), &[]).await.is_none(),
                "{hint}"
            );
        }
        std::fs::create_dir_all(&runs_dir).unwrap();
        std::fs::rename(&outside, runs_dir.join("inside")).unwrap();
        assert!(executor.recollect(&locator("inside"), &[]).await.is_some());
    }

    #[tokio::test]
    async fn recollect_refuses_a_home_hint_that_resolves_outside_the_runs_directory() {
        let dir = tempfile::tempdir().unwrap();
        let runs_dir = dir.path().join("runs");
        let outside = dir.path().join("outside");
        close(
            EmbeddedHome::create_retained(&outside.join("home"))
                .await
                .unwrap(),
        )
        .await;
        std::fs::create_dir_all(runs_dir.join("real")).unwrap();
        std::os::unix::fs::symlink(&outside, runs_dir.join("linked")).unwrap();
        std::os::unix::fs::symlink(outside.join("home"), runs_dir.join("real").join("home"))
            .unwrap();
        close(
            EmbeddedHome::create_retained(&runs_dir.join("ws").join("home"))
                .await
                .unwrap(),
        )
        .await;
        std::os::unix::fs::symlink(&outside, runs_dir.join("ws").join("workspace")).unwrap();
        let executor = EmbeddedExecutor::new(
            DocumentRuntimeOptions::default(),
            runs_dir,
            crate::test_support::runtime_handle(),
        );
        for hint in ["linked", "real", "ws"] {
            let locator = TrialLocator {
                trial_node_did: "did:key:zAny".to_string(),
                session_id: "s-1".to_string(),
                home_hint: Some(hint.to_string()),
            };
            assert!(executor.recollect(&locator, &[]).await.is_none(), "{hint}");
        }
    }

    #[tokio::test]
    async fn discarding_is_a_send_future() {
        fn assert_send<T>(_: T)
        where
            T: Send,
        {
        }
        let executor = EmbeddedExecutor::new(
            DocumentRuntimeOptions::default(),
            PathBuf::new(),
            crate::test_support::runtime_handle(),
        );
        assert_send(executor.discard("t1"));
    }

    #[tokio::test]
    async fn a_discarded_trial_is_forgotten_and_executes_as_unprovisioned() {
        let dir = tempfile::tempdir().unwrap();
        let runs_dir = dir.path().join("runs");
        let executor = EmbeddedExecutor::new(
            DocumentRuntimeOptions::default(),
            runs_dir.clone(),
            crate::test_support::runtime_handle(),
        );
        let spec = TrialSpec {
            trial_dir: runs_dir.join("run-1").join("trials").join("t1"),
            ..TrialSpec::empty_for_tests("t1")
        };

        let locator = executor.provision(&spec).await;
        assert!(locator.trial_node_did.starts_with("did:"));
        executor.discard(&spec.trial_id).await;

        let evidence = executor.execute(&spec, CancellationToken::new()).await;
        assert_eq!(evidence.locator.trial_node_did, "did:unprovisioned");
        assert!(evidence.stages.is_empty());
    }

    #[tokio::test]
    async fn a_stage_that_could_not_be_submitted_still_runs_its_captures() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let home = EmbeddedHome::create_temp("submit-failure").await.unwrap();
        seed_request(&home, "seeded").await;

        let spec = TrialSpec::empty_for_tests("t1");
        let locator = TrialLocator {
            trial_node_did: "did:key:zUnregistered".to_string(),
            session_id: "s-1".to_string(),
            home_hint: None,
        };
        let stage = StageSpec {
            stage_id: "only".to_string(),
            prompt: "hello".to_string(),
            seed: None,
            deadline_secs: 1,
            settle: false,
            continuation: None,
            review_previous: false,
            captures: vec![
                Capture::Documents {
                    name: "requests".to_string(),
                    collection: "AgentRequest".to_string(),
                    filter: json!({"request_id": {"_eq": "seeded"}}),
                    fields: vec!["agent_id".to_string()],
                },
                Capture::Documents {
                    name: "unreadable".to_string(),
                    collection: "NoSuchCollection".to_string(),
                    filter: json!({}),
                    fields: Vec::new(),
                },
            ],
        };

        let runtime = exited_runtime();
        let evidence = run_stage(
            &spec,
            &StageEnv {
                cancel: &CancellationToken::new(),
                home: &home,
                runtime: &runtime,
                locator: &locator,
                workspace: &workspace,
                ready: &watch::channel(None).1,
            },
            &stage,
        )
        .await;

        assert_eq!(evidence.stage_id, "only");
        assert_eq!(evidence.request_id, None);
        assert_failure_class(
            evidence.failure_kind,
            OutcomeKind::Infrastructure,
            crate::eval::EvidenceClass::NotEvidence,
        );
        let CaptureResult::Documents { rows } = &evidence.captures["requests"] else {
            panic!("expected a document capture, got {:?}", evidence.captures);
        };
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["agent_id"], "seeded-agent");
        assert!(
            !evidence.captures.contains_key("unreadable"),
            "{:?}",
            evidence.captures
        );
        home.node.shutdown().await;
    }

    #[tokio::test]
    async fn a_runtime_that_exits_mid_stage_fails_the_stage_as_runtime_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let home = EmbeddedHome::create_temp("runtime-exit").await.unwrap();
        let locator = TrialLocator {
            trial_node_did: home.did().to_string(),
            session_id: "s-1".to_string(),
            home_hint: None,
        };
        let stage = StageSpec {
            stage_id: "only".to_string(),
            prompt: "hello".to_string(),
            seed: None,
            deadline_secs: 600,
            settle: false,
            continuation: None,
            review_previous: false,
            captures: Vec::new(),
        };
        let runtime = exited_runtime();

        let evidence = tokio::time::timeout(
            Duration::from_secs(30),
            run_stage(
                &TrialSpec {
                    agent_id: "subject".to_string(),
                    ..TrialSpec::empty_for_tests("t1")
                },
                &StageEnv {
                    cancel: &CancellationToken::new(),
                    home: &home,
                    runtime: &runtime,
                    locator: &locator,
                    workspace: &workspace,
                    ready: &watch::channel(None).1,
                },
                &stage,
            ),
        )
        .await
        .expect("a dead runtime is not waited out to the stage deadline");

        assert!(evidence.request_id.is_some(), "the stage was submitted");
        assert_eq!(evidence.failure_kind, Some(OutcomeKind::Runtime));
        assert_eq!(evidence.provider_reason, None);
        home.node.shutdown().await;
    }

    #[tokio::test]
    async fn an_errored_trigger_fails_a_completed_stage_as_runtime() {
        let home = EmbeddedHome::create_temp("trigger-error").await.unwrap();
        let since = "2026-06-01T00:00:00Z";
        assert_eq!(
            trigger_failure(&home.node, home.did(), since, None, "t1", "only").await,
            None
        );

        seed_trigger(&home, "healthy", "fired", None, since).await;
        assert_eq!(
            trigger_failure(&home.node, home.did(), since, None, "t1", "only").await,
            None
        );

        seed_trigger(
            &home,
            "broken",
            "error",
            Some("template did not render"),
            since,
        )
        .await;
        assert_eq!(
            trigger_failure(&home.node, home.did(), since, None, "t1", "only").await,
            Some(OutcomeKind::Runtime)
        );
        assert_eq!(
            trigger_failure(
                &home.node,
                "did:key:zSomeoneElse",
                since,
                None,
                "t1",
                "only"
            )
            .await,
            None,
            "another node's trigger is not this trial's"
        );
        home.node.shutdown().await;
    }

    #[tokio::test]
    async fn a_seed_template_failure_ends_the_stage_before_its_deadline() {
        let backend = crate::support::streaming_backend::MockStreamingBackend::start_with_plans(
            "frozen-model",
            Vec::new(),
        )
        .unwrap();
        let mut inference = frozen_binding(json!("frozen-profile"));
        inference.backend["endpoint"] = json!(backend.endpoint());
        let dir = tempfile::tempdir().unwrap();
        let pack_dir = dir.path().join("pack");
        write_slot_pack(&pack_dir, "gents:inference-slot:primary");
        let path = pack_dir.join("pack_config.json");
        let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        config["tasks"] = json!([{
            "task_id": "seed-task",
            "agent_id": "subject",
            "prompt_template": "{{ doc.payload.required }}"
        }]);
        config["event_sources"] = json!([{
            "event_source_id": "seed-source",
            "source_collection": "EvalSeed"
        }]);
        config["triggers"] = json!([{
            "trigger_id": "seed-trigger",
            "task_id": "seed-task",
            "source": {"kind": "event", "event_source_id": "seed-source"}
        }]);
        std::fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
        let spec = TrialSpec {
            pack_digest: materialized_digest(&pack_dir),
            pack_dir,
            agent_id: "subject".into(),
            inference,
            ..TrialSpec::empty_for_tests("seed-template-failure")
        };
        let home = EmbeddedHome::create_temp("seed-template-failure")
            .await
            .unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        install_schema(
            &ConfigAccess::local(home.node.clone()),
            "type EvalSeed { payload: JSON }",
        )
        .await
        .unwrap();
        install(&spec, &home, &workspace).await.unwrap();
        let (event_sources_ready, ready) = watch::channel(None);
        let options = DocumentRuntimeOptions {
            runtime_snapshot_observer: Some(Arc::new(EventSourcesReady(event_sources_ready))),
            ..Default::default()
        };
        let (runtime, _agent) = crate::eval::runner::embedded::home::boot_runtime_within(
            &home,
            home.identity.clone(),
            options,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
        let locator = TrialLocator {
            trial_node_did: home.did().to_owned(),
            session_id: "seed-session".into(),
            home_hint: None,
        };
        let stage = StageSpec {
            stage_id: "seed".into(),
            prompt: String::new(),
            seed: Some(FixtureDocument {
                collection: "EvalSeed".into(),
                document: json!({"payload": {}}),
            }),
            deadline_secs: 600,
            settle: false,
            continuation: None,
            review_previous: false,
            captures: vec![Capture::Documents {
                name: "trigger".into(),
                collection: "Trigger".into(),
                filter: json!({"trigger_id": {"_eq": "seed-trigger"}}),
                fields: vec![
                    "last_status".into(),
                    "last_error".into(),
                    "last_fired_source_doc_id".into(),
                ],
            }],
        };
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            run_stage(
                &spec,
                &StageEnv {
                    cancel: &CancellationToken::new(),
                    home: &home,
                    runtime: &runtime,
                    locator: &locator,
                    workspace: &workspace,
                    ready: &ready,
                },
                &stage,
            ),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(30), runtime.shutdown())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "fixture runtime did not stop; stage: {result:?}; event sources: {:?}",
                    *ready.borrow()
                )
            })
            .unwrap();
        home.node.shutdown().await;

        let evidence = result.expect("the failed trigger must end the seed wait promptly");
        assert_failure_class(
            evidence.failure_kind,
            OutcomeKind::Runtime,
            crate::eval::EvidenceClass::Fail,
        );
        assert_eq!(evidence.request_id, None);
        assert!(evidence.inference_calls.is_empty());
        assert!(backend.observed_completion_bodies().is_empty());
        let CaptureResult::Documents { rows } = &evidence.captures["trigger"] else {
            panic!("the failed stage must retain its trigger evidence");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["last_status"], "error");
        assert!(
            rows[0]["last_error"]
                .as_str()
                .is_some_and(|error| error.contains("template:")),
            "{rows:?}"
        );
        assert!(rows[0]["last_fired_source_doc_id"].as_str().is_some());
    }

    #[tokio::test]
    async fn a_seed_ignores_unrelated_and_stale_trigger_failures() {
        let home = EmbeddedHome::create_temp("seed-trigger-scope")
            .await
            .unwrap();
        let since = "2026-06-01T00:00:00Z";
        let source = "seed-\"current";
        let unacknowledged = crate::trigger_engine::UNACKNOWLEDGED_STATUS;
        for (trigger_id, status, doc_id, attempted) in [
            ("unrelated", "error", "another-seed", since),
            ("stale", "error", source, "2026-01-01T00:00:00Z"),
            (
                "unrelated-unacknowledged",
                unacknowledged,
                "another-seed",
                since,
            ),
            (
                "stale-unacknowledged",
                unacknowledged,
                source,
                "2026-01-01T00:00:00Z",
            ),
        ] {
            seed_trigger(
                &home,
                trigger_id,
                status,
                Some("unrelated failure"),
                attempted,
            )
            .await;
            crate::document_config::update_trigger_runtime_fields(
                &home.node,
                home.did(),
                trigger_id,
                crate::document_config::TriggerRuntimeUpdate {
                    last_fired_source_doc_id: Some(doc_id.into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }
        assert_eq!(
            trigger_failure(&home.node, home.did(), since, Some(source), "t1", "seed").await,
            None,
            "another document's fire, and one attempted before the stage, is not this stage's"
        );
        crate::document_config::update_trigger_runtime_fields(
            &home.node,
            home.did(),
            "stale",
            crate::document_config::TriggerRuntimeUpdate {
                last_attempt_at: Some(since.into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_failure_class(
            trigger_failure(&home.node, home.did(), since, Some(source), "t1", "seed").await,
            OutcomeKind::Runtime,
            crate::eval::EvidenceClass::Fail,
        );
        home.node.shutdown().await;
    }

    #[tokio::test]
    async fn an_unreadable_seed_trigger_status_is_infrastructure() {
        let node = EmbeddedNode::builder().build().await.unwrap();
        let failure = trigger_failure(
            &node,
            "did:key:trial",
            "2026-06-01T00:00:00Z",
            Some("seed"),
            "t1",
            "seed",
        )
        .await;
        node.shutdown().await;
        assert_failure_class(
            failure,
            OutcomeKind::Infrastructure,
            crate::eval::EvidenceClass::NotEvidence,
        );
    }

    #[tokio::test]
    async fn a_seed_unacknowledged_trigger_is_infrastructure() {
        let home = EmbeddedHome::create_temp("seed-trigger-unacknowledged")
            .await
            .unwrap();
        let since = "2026-06-01T00:00:00Z";
        let source = "seed-current";
        seed_trigger(
            &home,
            "unacknowledged",
            crate::trigger_engine::UNACKNOWLEDGED_STATUS,
            Some("acknowledgment channel closed"),
            since,
        )
        .await;
        crate::document_config::update_trigger_runtime_fields(
            &home.node,
            home.did(),
            "unacknowledged",
            crate::document_config::TriggerRuntimeUpdate {
                last_fired_source_doc_id: Some(source.into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_failure_class(
            trigger_failure(&home.node, home.did(), since, Some(source), "t1", "seed").await,
            OutcomeKind::Infrastructure,
            crate::eval::EvidenceClass::NotEvidence,
        );
        home.node.shutdown().await;
    }

    #[tokio::test]
    async fn an_unacknowledged_seed_fire_does_not_downgrade_a_recorded_error() {
        let home = EmbeddedHome::create_temp("seed-trigger-precedence")
            .await
            .unwrap();
        let since = "2026-06-01T00:00:00Z";
        let source = "seed-current";
        for (trigger_id, status) in [
            ("lost", crate::trigger_engine::UNACKNOWLEDGED_STATUS),
            ("caused", "error"),
        ] {
            seed_trigger(
                &home,
                trigger_id,
                status,
                Some("recorded alongside the other"),
                since,
            )
            .await;
            crate::document_config::update_trigger_runtime_fields(
                &home.node,
                home.did(),
                trigger_id,
                crate::document_config::TriggerRuntimeUpdate {
                    last_fired_source_doc_id: Some(source.into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }
        assert_failure_class(
            trigger_failure(&home.node, home.did(), since, Some(source), "t1", "seed").await,
            OutcomeKind::Runtime,
            crate::eval::EvidenceClass::Fail,
        );
        home.node.shutdown().await;
    }

    #[tokio::test]
    async fn a_request_published_between_seed_observations_takes_precedence() {
        let home = EmbeddedHome::create_temp("seed-observation-race")
            .await
            .unwrap();
        let access = ConfigAccess::local(home.node.clone());
        install_schema(&access, "type RaceSeed { label: String }")
            .await
            .unwrap();
        let doc_id = create_document(
            &access,
            "eval.test.seed_race",
            "RaceSeed",
            &json!({"label": "race"}),
        )
        .await
        .unwrap();
        assert_eq!(fired_request(&home.node, &doc_id).await.unwrap(), None);

        create_document(
            &access,
            "eval.test.seed_race_lineage",
            "AgentRequest",
            &json!({
                "request_id": "fired",
                "node_did": home.did(),
                "requester_did": home.did(),
                "agent_id": "seeded-agent",
                "session_id": "s-1",
                "content": "x",
                "execution_origin": "event",
                "lifecycle_state": "completed",
                "created_at": "2026-01-01T00:00:00Z",
                "caused_by_source_doc_id": doc_id,
            }),
        )
        .await
        .unwrap();
        let since = "2026-06-01T00:00:00Z";
        seed_trigger(
            &home,
            "seed-trigger",
            "error",
            Some("earlier attempt"),
            since,
        )
        .await;
        crate::document_config::update_trigger_runtime_fields(
            &home.node,
            home.did(),
            "seed-trigger",
            crate::document_config::TriggerRuntimeUpdate {
                last_fired_source_doc_id: Some(doc_id.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let failure = trigger_failure(&home.node, home.did(), since, Some(&doc_id), "t1", "seed")
            .await
            .expect("the second observation sees the delayed trigger failure");
        let resolved = resolve_seed_failure(
            &home.node,
            &doc_id,
            failure,
            Duration::from_secs(600),
            Instant::now(),
        )
        .await
        .unwrap();
        home.node.shutdown().await;
        let StageSubmission::Submitted {
            request_id,
            remaining,
        } = resolved
        else {
            panic!("the committed request must take precedence: {resolved:?}");
        };
        assert_eq!(request_id, "fired");
        assert!(!remaining.is_zero());
    }

    #[tokio::test]
    async fn a_trigger_error_from_before_the_stage_does_not_fail_it() {
        let home = EmbeddedHome::create_temp("trigger-stale").await.unwrap();
        seed_trigger(&home, "stale", "error", Some("old"), "2026-01-01T00:00:00Z").await;

        assert_eq!(
            trigger_failure(
                &home.node,
                home.did(),
                "2026-06-01T00:00:00Z",
                None,
                "t1",
                "only"
            )
            .await,
            None
        );
        home.node.shutdown().await;
    }

    async fn seed_trigger(
        home: &EmbeddedHome,
        trigger_id: &str,
        status: &str,
        error: Option<&str>,
        last_attempt_at: &str,
    ) {
        let node_did = escape_graphql_string(home.did());
        let trigger_id = escape_graphql_string(trigger_id);
        let status = escape_graphql_string(status);
        let last_attempt_at = escape_graphql_string(last_attempt_at);
        let last_error = error.map_or("null".to_string(), |error| {
            format!("\"{}\"", escape_graphql_string(error))
        });
        let mutation = format!(
            r#"mutation {{ create_Trigger(input: {{ trigger_id: "{trigger_id}", node_did: "{node_did}", task_id: "task", enabled: true, last_status: "{status}", last_error: {last_error}, last_attempt_at: "{last_attempt_at}" }}) {{ _docID }} }}"#
        );
        let response =
            crate::ConfigAccess::write_local_response(&home.node, "eval.test.trigger", &mutation)
                .await
                .unwrap();
        assert!(response.errors.is_empty(), "{:?}", response.errors);
    }

    #[tokio::test]
    async fn a_request_that_settled_before_its_runtime_exited_keeps_its_outcome() {
        let home = EmbeddedHome::create_temp("settled-then-exit")
            .await
            .unwrap();
        let request_id = "settled-request";
        seed_request(&home, request_id).await;
        let runtime = exited_runtime();
        while !runtime.handle.is_finished() {
            tokio::task::yield_now().await;
        }
        assert!(runtime.handle.is_finished());

        tokio::time::pause();
        let (observed, stopped) = observe_stage_terminal(
            &home.node,
            &CancellationToken::new(),
            &runtime,
            request_id,
            Duration::from_secs(600),
        )
        .await;
        tokio::time::resume();

        let observed = observed.expect("settled request should remain observable after exit");
        assert_eq!(
            observed.terminal_state,
            Some(RequestLifecycleState::Completed)
        );
        assert!(!stopped);

        let evidence = collect_request_evidence(&home.node, request_id)
            .await
            .expect("evidence should be gathered after virtual time resumes");
        assert_eq!(
            stage_failure_kind(Some(&observed), true, stopped, &evidence),
            None
        );
        home.node.shutdown().await;
    }

    fn exited_runtime() -> RunningRuntime {
        RunningRuntime {
            shutdown: tokio::sync::watch::channel(false).0,
            handle: tokio::spawn(async { Err(anyhow!("runtime killed")) }),
            node_did: String::new(),
        }
    }

    async fn seed_request(home: &EmbeddedHome, request_id: &str) {
        seed_request_with_state(home, request_id, "completed").await;
    }

    async fn seed_request_with_state(home: &EmbeddedHome, request_id: &str, state: &str) {
        let node_did = escape_graphql_string(home.did());
        let request_id = escape_graphql_string(request_id);
        let state = escape_graphql_string(state);
        let mutation = format!(
            r#"mutation {{ create_AgentRequest(input: {{ request_id: "{request_id}", purpose: "normal", node_did: "{node_did}", requester_did: "{node_did}", agent_id: "seeded-agent", session_id: "s-1", content: "x", execution_origin: "interactive", lifecycle_state: "{state}", created_at: "2026-01-01T00:00:00Z" }}) {{ _docID }} }}"#
        );
        let response = home.node.execute(&mutation).await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
    }

    fn write_minimal_pack(root: &Path) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_vec(&json!({
                "manifest_version": 1,
                "name": "digest_fixture",
                "version": "1.0.0",
                "description": "A pack that loads, so only the digest can disagree.",
                "authors": ["gents-ai contributors"],
                "kind": "documents",
                "assets": ["pack_config.json"],
                "config": "pack_config.json",
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            root.join("pack_config.json"),
            serde_json::to_vec(&json!({"node": {}})).unwrap(),
        )
        .unwrap();
    }
}
