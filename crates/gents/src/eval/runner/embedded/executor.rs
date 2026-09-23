//! The embedded [`TrialExecutor`]: one trial, one fresh DefraDB home.
//!
//! Provisioning creates the home and the workspace the trial will run in, so
//! the run can record where a trial lives before it runs. Execution installs
//! the frozen pack, the trial's inference binding and its fixtures into that
//! home, boots a runtime on it, submits one request per stage, and reads the
//! captures back out. Nothing the trial is told names a check, a tier, a split
//! or a case: grading happens afterwards, from the evidence alone.
//!
//! Neither [`TrialExecutor::provision`] nor [`TrialExecutor::execute`] returns
//! an error. A trial whose home, pack or runtime never came up produces
//! [`TrialEvidence::infrastructure`]: it learned nothing about its subject, and
//! the loop owes the slot another attempt.

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::config_client::{
    apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::defra_node::EmbeddedNode;
use crate::document_config::{InferenceSampling, PackConfig};
use crate::eval::runner::embedded::home::{boot_runtime, EmbeddedHome};
use crate::eval::runner::embedded::observe::{
    await_terminal, classify_request_outcome, collect_request_evidence, RequestEvidence,
    TerminalObservation,
};
use crate::eval::runner::executor::{
    Capture, CaptureResult, FileRef, InferenceBinding, Isolation, StageEvidence, StageSpec,
    TrialEvidence, TrialExecutor, TrialFixtures, TrialLocator, TrialSpec,
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
use crate::{Collection, ConfigAccess, DocumentRuntimeOptions};

/// How long a request is watched after it has been interrupted, on the stage
/// deadline or on cancellation, before the observation gives up on it settling.
const GRACE: Duration = Duration::from_secs(30);

/// How often a pending request's row is read while a stage runs.
const POLL: Duration = Duration::from_millis(250);

/// Runs each trial in its own embedded home under `runs_dir`.
pub struct EmbeddedExecutor {
    pub runtime_options: DocumentRuntimeOptions,
    /// A trial's home directory is `<runs_dir>/<run_id>/trials/<trial_id>`, so
    /// a [`TrialLocator::home_hint`] is a path relative to this directory and
    /// never an absolute path out of the run.
    pub runs_dir: PathBuf,
    /// What [`TrialExecutor::provision`] created, until `execute` takes it.
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
        let dir = spec.home_dir.join("home");
        let home = opened(move || async move { EmbeddedHome::create_retained(&dir).await }).await?;
        let workspace = workspace_dir(&spec.home_dir);
        if let Err(error) = std::fs::create_dir_all(&workspace) {
            close(home).await;
            return Err(error).with_context(|| format!("creating {}", workspace.display()));
        }
        let locator = TrialLocator {
            trial_agent_did: home.did().to_string(),
            session_id: uuid::Uuid::new_v4().to_string(),
            home_hint: spec
                .home_dir
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

    /// The homes [`TrialExecutor::provision`] created and `execute` has not
    /// taken yet.
    ///
    /// A poisoned map still holds trial nodes, so it is recovered rather than
    /// refused: whatever panicked did not panic in this data, and refusing it
    /// would strand a node that nothing can shut down any more.
    ///
    /// Callers take this guard in a `let` statement and drop it at the
    /// semicolon. It is a `std::sync::Mutex`, so holding it across an await
    /// would both block a worker thread and make the caller's future `!Send`.
    fn provisioned(&self) -> std::sync::MutexGuard<'_, HashMap<String, Provisioned>> {
        self.provisioned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Shut down and forget a home that will never run a trial.
    ///
    /// A home lives in the map from `provision` until `execute` takes it, so a
    /// run that abandons a provisioned trial — the loop failing to write its
    /// `EvalTrial` row, say — must say so, or the node runs until the process
    /// ends. [`TrialExecutor::discard`] is how the loop says it.
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

    /// Steps 1 to 3: check the pack, install it and everything the trial runs
    /// against, then boot. Any error here means no trial ran.
    async fn run(
        &self,
        spec: &TrialSpec,
        cancel: CancellationToken,
        home: EmbeddedHome,
        locator: TrialLocator,
    ) -> TrialEvidence {
        let workspace = workspace_dir(&spec.home_dir);
        if let Err(error) = install(spec, &home, &workspace).await {
            close(home).await;
            return infrastructure(&spec.trial_id, locator, &error);
        }
        // A failed boot has already stopped whatever it spawned, so the home is
        // ours to close.
        let runtime =
            match boot_runtime(&home, home.identity.clone(), self.runtime_options.clone()).await {
                Ok((runtime, _agent)) => runtime,
                Err(error) => {
                    close(home).await;
                    return infrastructure(&spec.trial_id, locator, &error);
                }
            };

        let stages = run_stages(spec, &cancel, &home, &locator, &workspace).await;

        if let Err(error) = runtime.shutdown().await {
            tracing::warn!(
                error = %format!("{error:#}"),
                trial_id = %spec.trial_id,
                "eval trial runtime did not shut down cleanly"
            );
        }
        close(home).await;
        let (usage, anchor) = (usage(&stages), anchor(&stages));
        TrialEvidence::new(locator, stages, usage, anchor)
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

    /// Reads a finished trial's home again, when the run directory still holds
    /// it. The home records which requests a session had, not which stage
    /// submitted them, so a rebuilt stage is named by its position in the
    /// session (`"0"`, `"1"`, …) rather than by the case's stage ids.
    ///
    /// That makes this capture evidence and not gradeable evidence: those
    /// positional ids match no case's `stage_id`, so grading over them would
    /// call every check a skipped prerequisite. A regrade has to map the
    /// positions onto the case first (M4). The home also does not record
    /// whether a request was interrupted on its stage's deadline, so nothing
    /// read back here is classified as a deadline.
    async fn recollect(&self, at: &TrialLocator, captures: &[Capture]) -> Option<TrialEvidence> {
        let home_dir = self.runs_dir.join(at.home_hint.as_deref()?);
        let dir = home_dir.join("home");
        let home =
            match opened(move || async move { EmbeddedHome::open_retained(&dir).await }).await {
                Ok(home) => home,
                Err(error) => {
                    tracing::warn!(
                        error = %format!("{error:#}"),
                        home = %home_dir.display(),
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

        let workspace = workspace_dir(&home_dir);
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
                stage_id: index.to_string(),
                request_id: Some(request.request_id),
                terminal_state,
                provider_reason: provider_reason(failure_kind, &evidence),
                failure_kind,
                messages: evidence.messages,
                tool_calls: evidence.tool_calls,
                inference_calls: evidence.inference_calls,
                captures: run_captures(&home.node, &at.trial_agent_did, &workspace, captures).await,
            });
        }
        close(home).await;
        let (usage, anchor) = (usage(&stages), anchor(&stages));
        Some(TrialEvidence::new(at.clone(), stages, usage, anchor))
    }
}

/// Which side of the provider boundary a failed `InferenceCall` names.
///
/// | the failure reason names                   | reason        |
/// |--------------------------------------------|---------------|
/// | HTTP 408 or 429                            | `Unavailable` |
/// | any other HTTP 4xx                         | `Rejected`    |
/// | HTTP 5xx                                   | `Unavailable` |
/// | "context", "policy", "content"             | `Rejected`    |
/// | "connect", "timeout", "timed out", "rate"  | `Unavailable` |
/// | anything else                              | `None`        |
///
/// Matching is case-insensitive. `None` is not a third answer about the
/// provider: grading downgrades a provider failure with no reason to Unknown
/// rather than guessing which side of the boundary failed.
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

/// The first 4xx or 5xx status in `reason`: three digits standing on their own,
/// so a model named `gpt-4o-500k` is not read as a server error.
fn http_status(reason: &str) -> Option<u16> {
    reason
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .filter(|token| token.len() == 3 && token.bytes().all(|byte| byte.is_ascii_digit()))
        .filter_map(|token| token.parse::<u16>().ok())
        .find(|status| (400..600).contains(status))
}

/// Open a trial home from a `Send` future.
///
/// `EmbeddedNode::build` holds a non-`Send` P2P setup value across an await, so
/// opening a home is not a `Send` future, while [`TrialExecutor`] hands the
/// runner loop `Send` ones. The open is therefore polled to completion on a
/// blocking thread of the runtime the trial will run on: every task the node
/// spawns stays on that runtime, and the opened home is itself `Send`, so only
/// the future ever needed the hop.
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

/// Close a trial home.
///
/// An `EmbeddedNode` that is only dropped tears itself down without anyone
/// waiting for it; awaiting its shutdown stops its background tasks and closes
/// the store cleanly first. Every path that finishes with a home ends here,
/// including the ones where no trial ever ran.
async fn close(home: EmbeddedHome) {
    home.node.shutdown().await;
}

/// A trial's files live beside its home, not inside the database directory.
fn workspace_dir(home_dir: &Path) -> PathBuf {
    home_dir.join("workspace")
}

fn unprovisioned() -> TrialLocator {
    TrialLocator {
        trial_agent_did: "did:unprovisioned".to_string(),
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
/// workspace root and its fixtures, all into this trial's own home.
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

    let agent_did = home.did().to_string();
    let config = load_pack_config(
        &manifest,
        &PackInstallOptions {
            agent_did: agent_did.clone(),
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
    let access = ConfigAccess::Local(home.node.clone());
    // The binding first: the pack's behaviors now reference the profile by id,
    // and a reference is only installable once what it names exists.
    apply(
        &access,
        "eval.trial.install_inference",
        &inference_plan(&spec.inference, &spec.trial_id, &agent_did)?,
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
    install_fixtures(&access, &home.node, &spec.fixtures, workspace).await
}

/// Bind every inference slot the pack declares to the profile the run froze.
///
/// A pack's behaviors may only reference a slot marker
/// (`gents:inference-slot:<name>`): [`crate::pack::bind_pack_install_config`]
/// is what turns those markers into a real profile id, and it is the same
/// binding `gents pack install` performs. A cell chooses one
/// `inference_profile_id`, so every slot of the subject binds to that one
/// profile: a cell is one arm of a comparison, not a per-slot model matrix.
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
        .map(|slot| (slot.name.clone(), profile_id.to_owned()))
        .collect();
    bind_pack_install_config(manifest, config, &bindings)
}

async fn apply(
    access: &ConfigAccess,
    operation: &'static str,
    plan: &DesiredStateApplyPlan,
) -> Result<()> {
    access
        .transact(operation, |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
}

/// The materialized pack: its manifest and the bytes of every asset that
/// manifest declares, which are exactly what the frozen digest covers.
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

/// The backend and profile the run froze, copied verbatim into this home, and
/// the sampling document that carries the trial's seed. Ownership is rewritten
/// to the trial's own DID: the documents describe what to call, not who calls.
fn inference_plan(
    binding: &InferenceBinding,
    trial_id: &str,
    agent_did: &str,
) -> Result<DesiredStateApplyPlan> {
    let mut backend = binding.backend.clone();
    own(&mut backend, "InferenceBackend", agent_did)?;

    let mut sampling = match binding.sampling.clone() {
        Some(sampling) => sampling,
        None => serde_json::to_value(InferenceSampling {
            agent_did: agent_did.to_string(),
            sampling_id: format!("eval-{trial_id}"),
            ..Default::default()
        })?,
    };
    own(&mut sampling, "InferenceSampling", agent_did)?;
    object(&mut sampling, "InferenceSampling")?.insert("seed".to_string(), json!(binding.seed));
    let sampling_id = sampling
        .get("sampling_id")
        .and_then(Value::as_str)
        .context("the trial's InferenceSampling document has no sampling_id")?
        .to_string();

    let mut profile = binding.profile.clone();
    own(&mut profile, "InferenceProfile", agent_did)?;
    object(&mut profile, "InferenceProfile")?.insert("sampling_id".to_string(), json!(sampling_id));

    DesiredStateApplyPlan::new(vec![
        desired(Collection::InferenceBackend, backend),
        desired(Collection::InferenceSampling, sampling),
        desired(Collection::InferenceProfile, profile),
    ])
}

fn desired(collection: Collection, value: Value) -> DesiredStateApplyDocument {
    DesiredStateApplyDocument {
        collection,
        add: value.clone(),
        update: value,
    }
}

fn own(value: &mut Value, named: &str, agent_did: &str) -> Result<()> {
    object(value, named)?.insert("agent_did".to_string(), json!(agent_did));
    Ok(())
}

fn object<'a>(value: &'a mut Value, named: &str) -> Result<&'a mut serde_json::Map<String, Value>> {
    value
        .as_object_mut()
        .with_context(|| format!("the trial's {named} document is not an object"))
}

/// The trial's workspace, published the way the configurator evals publish
/// theirs: an operator-owned root the trial's host tools may reach.
async fn install_workspace_root(node: &EmbeddedNode, workspace: &Path) -> Result<()> {
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

/// The state a case starts from: collections it declares, rows it seeds, files
/// it puts in the workspace. Fixture paths are input, so they stay inside it.
async fn install_fixtures(
    access: &ConfigAccess,
    node: &EmbeddedNode,
    fixtures: &TrialFixtures,
    workspace: &Path,
) -> Result<()> {
    for sdl in &fixtures.schemas {
        node.add_schema(sdl)
            .await
            .context("adding a fixture schema")?;
    }
    for fixture in &fixtures.documents {
        let collection = &fixture.collection;
        validate_collection_identifier(collection)?;
        // The document is arbitrary authored JSON, so it travels as a variable
        // and is never interpolated into the mutation.
        let mutation = format!(
            "mutation($input: {collection}MutationInputArg!) {{ create_{collection}(input: $input) {{ _docID }} }}"
        );
        let variables = json!({ "input": fixture.document });
        access
            .transact("eval.trial.fixture_document", |txn| {
                let (mutation, variables) = (&mutation, &variables);
                Box::pin(async move {
                    txn.execute_with_variables(mutation, variables)
                        .await
                        .map(|_| ())
                })
            })
            .await
            .with_context(|| format!("creating a {collection} fixture document"))?;
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

/// A fixture path names a file inside the trial's workspace, never a path out
/// of it.
fn workspace_path(workspace: &Path, relative: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        Path::new(relative)
            .components()
            .all(|component| matches!(component, Component::Normal(_))),
        "workspace path {relative:?} must stay inside the trial workspace"
    );
    Ok(workspace.join(relative))
}

/// Step 4: the stages in order, until one fails. A failed stage stops
/// submission; the stages after it are absent from the evidence, and grading
/// reads that absence as a skipped prerequisite.
async fn run_stages(
    spec: &TrialSpec,
    cancel: &CancellationToken,
    home: &EmbeddedHome,
    locator: &TrialLocator,
    workspace: &Path,
) -> Vec<StageEvidence> {
    let mut stages = Vec::new();
    for stage in &spec.stages {
        let evidence = run_stage(spec, cancel, home, locator, workspace, stage).await;
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
    locator: &TrialLocator,
    workspace: &Path,
    stage: &StageSpec,
) -> StageEvidence {
    let observed = submit_and_observe(spec, cancel, home, locator, stage).await;
    StageEvidence {
        stage_id: stage.stage_id.clone(),
        request_id: observed.request_id,
        terminal_state: observed.terminal_state,
        provider_reason: provider_reason(observed.failure_kind, &observed.evidence),
        failure_kind: observed.failure_kind,
        messages: observed.evidence.messages,
        tool_calls: observed.evidence.tool_calls,
        inference_calls: observed.evidence.inference_calls,
        // Captures run for a failed stage too, including one that was never
        // submitted: what the home holds is evidence either way.
        captures: run_captures(
            &home.node,
            &locator.trial_agent_did,
            workspace,
            &spec.captures,
        )
        .await,
    }
}

/// What one stage's request did, before its captures are read.
struct ObservedStage {
    /// `None` when the request was never submitted.
    request_id: Option<String>,
    terminal_state: Option<RequestLifecycleState>,
    failure_kind: Option<OutcomeKind>,
    evidence: RequestEvidence,
}

impl ObservedStage {
    /// A stage whose request never reached the home: the write failed, so the
    /// subject was never asked anything.
    ///
    /// [`OutcomeKind::Infrastructure`], not `Runtime`: a runtime failure is
    /// something the subject's own behavior can provoke and classifies as a
    /// failure against it, while a request that was never written says only
    /// that the harness broke. Reporting it as a failure would score the
    /// subject zero and close the slot instead of retrying it.
    fn unsubmitted() -> Self {
        Self {
            request_id: None,
            terminal_state: None,
            failure_kind: Some(OutcomeKind::Infrastructure),
            evidence: RequestEvidence::default(),
        }
    }
}

async fn submit_and_observe(
    spec: &TrialSpec,
    cancel: &CancellationToken,
    home: &EmbeddedHome,
    locator: &TrialLocator,
    stage: &StageSpec,
) -> ObservedStage {
    let request_id = uuid::Uuid::new_v4().to_string();
    if let Err(error) =
        submit_stage(&home.node, locator, &spec.behavior_id, stage, &request_id).await
    {
        tracing::warn!(
            error = %format!("{error:#}"),
            trial_id = %spec.trial_id,
            stage_id = %stage.stage_id,
            "eval stage could not be submitted"
        );
        return ObservedStage::unsubmitted();
    }

    let cancelled;
    let observed = tokio::select! {
        observed = await_terminal(
            &home.node,
            &request_id,
            Duration::from_secs(stage.deadline_secs),
            GRACE,
            POLL,
        ) => {
            cancelled = false;
            observed
        }
        () = cancel.cancelled() => {
            cancelled = true;
            interrupt_and_settle(&home.node, &request_id).await
        }
    };
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

    let mut collected = true;
    let evidence = collect_request_evidence(&home.node, &request_id)
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

    ObservedStage {
        request_id: Some(request_id),
        terminal_state: observed.as_ref().map(|observed| observed.terminal_state),
        failure_kind: stage_failure_kind(observed.as_ref(), collected, cancelled, &evidence),
        evidence,
    }
}

/// How a stage failed, when it did.
///
/// Evidence that could not be read is the harness failing, not the subject:
/// the stage may have gone perfectly and the read of its rows not have
/// arrived, so it is [`OutcomeKind::Infrastructure`] and the slot is owed
/// another attempt. An observation that failed outright is the same kind of
/// fault — the watch of the request broke, which says nothing about what the
/// request did. Cancellation stays on the runtime boundary: there the request
/// itself is what did not finish.
fn stage_failure_kind(
    observed: Option<&TerminalObservation>,
    collected: bool,
    cancelled: bool,
    evidence: &RequestEvidence,
) -> Option<OutcomeKind> {
    if !collected {
        return Some(OutcomeKind::Infrastructure);
    }
    if cancelled {
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

/// The same failure vocabulary the stage runner uses today, as the closed enum
/// grading reads.
fn outcome_kind(classification: &str) -> OutcomeKind {
    match classification {
        "deadline" => OutcomeKind::Deadline,
        "tool" => OutcomeKind::Tool,
        "provider" => OutcomeKind::Provider,
        "runtime" => OutcomeKind::Runtime,
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

/// Cancellation reaches a running stage through the same latch a deadline
/// uses: the request is interrupted, then watched until it settles, so the
/// evidence names a state the home actually reached.
async fn interrupt_and_settle(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<TerminalObservation> {
    crate::interrupt::interrupt_request(node, request_id)
        .await
        .context("interrupting a cancelled stage")?;
    await_terminal(node, request_id, Duration::ZERO, GRACE, POLL).await
}

/// One stage is one `AgentRequest` in the trial's session, built the way
/// `gents request` builds an interactive one: `local_self` admission, signed as
/// the registered trial principal, and the session created implicitly by its
/// first request rather than by a separate `AgentSession` write. The CLI's
/// `valid_until` and `retry` are unset for a request that is nobody's retry,
/// which is exactly what `RequestSpec::new` already carries.
async fn submit_stage(
    node: &EmbeddedNode,
    locator: &TrialLocator,
    behavior_id: &str,
    stage: &StageSpec,
    request_id: &str,
) -> Result<()> {
    let create = build_signed_request(
        RequestSpec::new(
            RequestIdentity {
                request_id: request_id.to_string(),
                agent_did: locator.trial_agent_did.clone(),
                requester_did: None,
                behavior_id: behavior_id.to_string(),
                session_id: locator.session_id.clone(),
                content: stage.prompt.clone(),
                execution_origin: ExecutionOrigin::Interactive,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            },
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                &locator.trial_agent_did,
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

/// Step 5: what the trial left behind, read out of its own home and workspace.
///
/// A capture that cannot be read is absent from the map rather than empty: an
/// empty capture is a fact about the subject — it produced no rows — and a
/// failed read is a fact about the harness. A check asked for a capture that
/// is not there reports `missing_capture`, which is no evidence about the
/// subject; a check handed an empty one would score it zero.
async fn run_captures(
    node: &EmbeddedNode,
    trial_did: &str,
    workspace: &Path,
    captures: &[Capture],
) -> BTreeMap<String, CaptureResult> {
    let mut results = BTreeMap::new();
    for capture in captures {
        match capture {
            Capture::Documents {
                name,
                collection,
                filter,
                fields,
            } => match capture_documents(node, collection, filter, fields, trial_did).await {
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

/// `_docID`, the fields the capture names, and the fields its filter selects
/// on. GraphQL has no "every field" selection, so a capture has to say what it
/// wants; a filter's own keys are included because a row captured by a field
/// should carry it.
fn documents_capture_query(
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
        // A leading underscore at the top level is a filter operator
        // (`_and`, `_or`, `_not`), not a field of the collection.
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

/// Every key of a frozen filter lands in identifier position, so they are all
/// checked before any of the filter is rendered into a query.
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

/// A capture's filter as a GraphQL input object. Strings are escaped and
/// quoted; `"$trial"` is the one runtime-supplied value, and it is replaced by
/// the trial's DID before escaping, never after.
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
/// the glob could not be run at all. A glob that would leave the workspace
/// matches nothing: a trial's evidence is what it produced, not what was
/// already on the host. A glob that was refused outright, or a match that could
/// not be read, records no capture rather than an empty one — the stage has no
/// evidence under that name, which is not the same as the subject producing no
/// files.
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
/// A glob with no parent component still reaches outside the workspace when
/// something inside it is a symlink: the trial can create one, and reading
/// through it would capture a host file as if the trial had produced it. So
/// every match is resolved and checked against the resolved workspace, and one
/// that lands outside is skipped and named in the second list rather than
/// silently dropped — an omission a check can see is not an empty capture.
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
    // The workspace root is a path, not a pattern: a run directory holding a
    // `[` or `*` must not change what the capture matches.
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
        // Named by where the glob found it, not by where it resolves to: the
        // capture reports what the trial's workspace offered.
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
    let query = format!(
        r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{session_id}" }} }}, order: {{ created_at: ASC }}) {{ request_id lifecycle_state }} }}"#
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

/// Step 6: totals over every inference call the trial made. A missing total is
/// `None`, never zero: one call that never reported its tokens makes the sum
/// unknown.
fn usage(stages: &[StageEvidence]) -> TrialUsage {
    let mut input_tokens = Some(0u64);
    let mut output_tokens = Some(0u64);
    for call in stages.iter().flat_map(|stage| &stage.inference_calls) {
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

/// What the trial is anchored to: the states its requests reached, in order,
/// and how many requests and inference calls there were.
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
        // A three-digit run inside a longer token is not a status.
        assert_eq!(provider_reason_from_failure("model gpt-4o-500k"), None);
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

    /// The matched files and the matches that were refused, for a glob that ran.
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

        // A refused glob records no capture at all: nothing of the host is
        // captured, and no empty capture reaches a check either.
        assert_eq!(capture_files(&workspace, "../*.txt"), None);
        assert_eq!(capture_files(&workspace, "/etc/*"), None);
        let (found, outside) = captured(&workspace, "*.txt");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "report.txt");
        assert_eq!(found[0].bytes, 5);
        assert!(outside.is_empty());
    }

    /// A trial can put a symlink in its own workspace, and a glob with no
    /// parent component then still reaches a host file. Reading through it
    /// would record that file as something the trial produced.
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

    /// A match the harness cannot read is the harness failing, so the stage
    /// records no capture under that name and a check asking for it reports
    /// `missing_capture`. An empty capture would instead say the trial produced
    /// nothing, and score it.
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
            // Running with privileges that ignore the mode, so the unreadable
            // match this test is about cannot be staged here.
            return;
        }

        assert_eq!(
            capture_files(&workspace, "*.txt"),
            None,
            "a capture that cannot be read is absent, not empty"
        );

        // And a capture the harness can read still lands, so the omission is
        // about the failure and not about the glob.
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

    /// The watch of a stage's request can itself fail while the request is
    /// fine. That is the harness breaking, not the subject failing, so the
    /// stage is infrastructure and the slot is owed another attempt — and it
    /// carries no provider reason, because no provider was observed at all.
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

        // The neighbouring arms keep their grades.
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

    /// A pack may only reference an inference slot, so installing one into a
    /// trial home has to bind that slot to the profile the run froze. Without
    /// the binding the behavior would reach the home still naming
    /// `gents:inference-slot:primary`, which is no profile at all.
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
            ..TrialSpec::empty_for_tests("t1")
        };

        install(&spec, &home, &workspace).await.unwrap();

        let response = home
            .node
            .execute("query { AgentBehavior { behavior_id inference_profile_id } }")
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let rows = response.data.as_ref().unwrap()["AgentBehavior"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["behavior_id"], "subject");
        assert_eq!(
            rows[0]["inference_profile_id"], "frozen-profile",
            "the slot marker never reaches the trial home"
        );
        home.node.shutdown().await;
    }

    /// The pack declares a slot and the frozen binding names no profile to bind
    /// it to, so nothing is installed and the trial learned nothing.
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
        let executor = EmbeddedExecutor::new(DocumentRuntimeOptions::default(), runs_dir.clone());
        let spec = TrialSpec {
            pack_digest: materialized_digest(&pack_dir),
            pack_dir,
            // A profile document with no `profile_id`: the declared slot has
            // nothing to bind to.
            inference: frozen_binding(Value::Null),
            home_dir: runs_dir.join("run-1").join("trials").join("t1"),
            ..TrialSpec::empty_for_tests("t1")
        };

        // The install refuses for that reason and no other.
        let home = EmbeddedHome::create_temp("unbindable-slot").await.unwrap();
        let error = install(&spec, &home, &dir.path().join("workspace"))
            .await
            .unwrap_err();
        let reason = format!("{error:#}");
        assert!(
            reason.contains("inference slots") && reason.contains("profile_id"),
            "{reason}"
        );
        home.node.shutdown().await;

        // And the trial records it as infrastructure rather than as evidence.
        executor.provision(&spec).await;
        let evidence = executor.execute(&spec, CancellationToken::new()).await;
        assert!(evidence.stages.is_empty(), "{:?}", evidence.stages);
        assert_eq!(evidence.anchor.requests, 0);
    }

    fn materialized_digest(pack_dir: &Path) -> String {
        let (manifest, assets) = read_pack(pack_dir).unwrap();
        digest_declared_assets(&manifest, |path| {
            assets.get(path).map(Vec::as_slice).context("missing asset")
        })
        .unwrap()
    }

    /// The inference documents a run freezes, with `profile_id` under test.
    fn frozen_binding(profile_id: Value) -> InferenceBinding {
        let mut profile = json!({
            "agent_did": "did:key:frozen",
            "backend_id": "frozen-backend",
            "model_name": "frozen-model",
        });
        if !profile_id.is_null() {
            profile["profile_id"] = profile_id;
        }
        InferenceBinding {
            profile,
            backend: json!({
                "agent_did": "did:key:frozen",
                "backend_id": "frozen-backend",
                "name": "Frozen backend",
                "provider_kind": "OpenAiCompatible",
                "endpoint": "http://127.0.0.1:1/v1",
                "auth": {"kind": "unauthenticated"},
            }),
            sampling: None,
            seed: 7,
        }
    }

    /// A pack in the only shape the loader accepts: one behavior, assigned to
    /// one declared inference slot, referencing it by marker.
    fn write_slot_pack(root: &Path, inference_profile_id: &str) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join("README.md"), "# slot fixture\n").unwrap();
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_vec(&json!({
                "manifest_version": 1,
                "name": "slot_fixture",
                "version": "1.0.0",
                "description": "A pack whose behavior references an inference slot.",
                "authors": ["gents-ai contributors"],
                "kind": "documents",
                "assets": ["README.md", "pack_config.json"],
                "config": "pack_config.json",
                "inference_slots": [{
                    "name": "primary",
                    "description": "Runs the subject behavior.",
                    "behaviors": ["subject"],
                }],
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            root.join("pack_config.json"),
            serde_json::to_vec(&json!({
                "agent_principal": {"default_behavior_id": "subject"},
                "agent_behaviors": [{
                    "behavior_id": "subject",
                    "display_name": "Subject",
                    "inference_profile_id": inference_profile_id,
                }],
            }))
            .unwrap(),
        )
        .unwrap();
    }

    /// The materialized pack is real and readable; only the digest the run
    /// froze disagrees with it. Nothing boots and no stage is submitted.
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
        let executor = EmbeddedExecutor::new(DocumentRuntimeOptions::default(), runs_dir.clone());
        let spec = TrialSpec {
            pack_dir,
            pack_digest: "sha256:wrong".to_string(),
            home_dir: runs_dir.join("run-1").join("trials").join("t1"),
            ..TrialSpec::empty_for_tests("t1")
        };

        // The fixture pack really loads; only the frozen digest disagrees.
        let (manifest, assets) = read_pack(&spec.pack_dir).unwrap();
        let materialized = digest_declared_assets(&manifest, |path| {
            assets.get(path).map(Vec::as_slice).context("missing asset")
        })
        .unwrap();
        assert!(materialized.starts_with("sha256:"));
        assert_ne!(materialized, spec.pack_digest);

        let locator = executor.provision(&spec).await;
        assert!(locator.trial_agent_did.starts_with("did:"));
        assert_eq!(locator.home_hint.as_deref(), Some("run-1/trials/t1"));
        assert!(spec.home_dir.join("workspace").is_dir());

        let evidence = executor.execute(&spec, CancellationToken::new()).await;
        assert_eq!(evidence.locator.trial_agent_did, locator.trial_agent_did);
        assert!(evidence.stages.is_empty());
        assert_eq!(evidence.anchor.requests, 0);

        // A trial that produced no evidence still leaves its home behind, and
        // closing it leaves the directory usable. (This does not prove the node
        // was shut down rather than dropped: the assertion passes either way.)
        let reopened = EmbeddedHome::open_retained(&spec.home_dir.join("home"))
            .await
            .expect("the trial home survives the close");
        assert_eq!(reopened.did(), locator.trial_agent_did);
        reopened.node.shutdown().await;
    }

    /// A provisioned home that will never run a trial has to be reclaimable, or
    /// its node runs on for the rest of the process.
    /// The loop calls `discard` from a `Send` context, so the provisioning
    /// guard must be dropped before the shutdown await. Holding it across the
    /// await makes this stop compiling rather than fail a review.
    #[test]
    fn discarding_is_a_send_future() {
        fn assert_send<T: Send>(_: T) {}
        let executor = EmbeddedExecutor::new(DocumentRuntimeOptions::default(), PathBuf::new());
        assert_send(executor.discard("t1"));
    }

    #[tokio::test]
    async fn a_discarded_trial_is_forgotten_and_executes_as_unprovisioned() {
        let dir = tempfile::tempdir().unwrap();
        let runs_dir = dir.path().join("runs");
        let executor = EmbeddedExecutor::new(DocumentRuntimeOptions::default(), runs_dir.clone());
        let spec = TrialSpec {
            home_dir: runs_dir.join("run-1").join("trials").join("t1"),
            ..TrialSpec::empty_for_tests("t1")
        };

        let locator = executor.provision(&spec).await;
        assert!(locator.trial_agent_did.starts_with("did:"));
        executor.discard(&spec.trial_id).await;

        let evidence = executor.execute(&spec, CancellationToken::new()).await;
        assert_eq!(evidence.locator.trial_agent_did, "did:unprovisioned");
        assert!(evidence.stages.is_empty());
    }

    /// A stage whose request never reached the home still reports what the home
    /// holds: the captures are the trial's evidence either way. The stage
    /// itself is infrastructure — the subject was never asked anything — so
    /// the slot is owed another attempt rather than scored zero.
    #[tokio::test]
    async fn a_stage_that_could_not_be_submitted_still_runs_its_captures() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let home = EmbeddedHome::create_temp("submit-failure").await.unwrap();
        seed_request(&home, "seeded").await;

        let mut spec = TrialSpec::empty_for_tests("t1");
        spec.captures = vec![
            Capture::Documents {
                name: "requests".to_string(),
                collection: "AgentRequest".to_string(),
                filter: json!({"request_id": {"_eq": "seeded"}}),
                fields: vec!["behavior_id".to_string()],
            },
            // No such collection in this home, so the read fails rather than
            // returning nothing.
            Capture::Documents {
                name: "unreadable".to_string(),
                collection: "NoSuchCollection".to_string(),
                filter: json!({}),
                fields: Vec::new(),
            },
        ];
        // Nothing has registered a signing identity for this DID, so building
        // the stage's signed request fails before anything is written.
        let locator = TrialLocator {
            trial_agent_did: "did:key:zUnregistered".to_string(),
            session_id: "s-1".to_string(),
            home_hint: None,
        };
        let stage = StageSpec {
            stage_id: "only".to_string(),
            prompt: "hello".to_string(),
            deadline_secs: 1,
        };

        let evidence = run_stage(
            &spec,
            &CancellationToken::new(),
            &home,
            &locator,
            &workspace,
            &stage,
        )
        .await;

        assert_eq!(evidence.stage_id, "only");
        assert_eq!(evidence.request_id, None);
        assert_eq!(evidence.failure_kind, Some(OutcomeKind::Infrastructure));
        let CaptureResult::Documents { rows } = &evidence.captures["requests"] else {
            panic!("expected a document capture, got {:?}", evidence.captures);
        };
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["behavior_id"], "seeded-behavior");
        // A capture that could not be read is absent, not empty: an empty one
        // would read as "the subject produced no rows".
        assert!(
            !evidence.captures.contains_key("unreadable"),
            "{:?}",
            evidence.captures
        );
        home.node.shutdown().await;
    }

    async fn seed_request(home: &EmbeddedHome, request_id: &str) {
        let agent_did = escape_graphql_string(home.did());
        let request_id = escape_graphql_string(request_id);
        let mutation = format!(
            r#"mutation {{ create_AgentRequest(input: {{ request_id: "{request_id}", agent_did: "{agent_did}", requester_did: "{agent_did}", behavior_id: "seeded-behavior", session_id: "s-1", content: "x", execution_origin: "interactive", lifecycle_state: "completed", created_at: "2026-01-01T00:00:00Z" }}) {{ _docID }} }}"#
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
            serde_json::to_vec(&json!({"agent_principal": {}})).unwrap(),
        )
        .unwrap();
    }
}
