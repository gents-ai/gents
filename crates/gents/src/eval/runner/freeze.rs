//! Freezing a run: the request is validated, each cell's pack is materialized
//! under the run directory, and one `EvalRun` row records the comparability
//! facts every later stage reads.
//!
//! Freezing is idempotent: the same request for the same `run_id` returns the
//! run that is already there. Anything else about that `run_id` — a different
//! origin, a different capture list, an invalidation — is a [`FreezeRefused`]
//! rather than a second interpretation of the same id.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config_client::{
    desired_state_document_digest, read_desired_state_record_in_txn, ConfigAccess,
};
use crate::document_config::{
    BackendAuth, EvalDefinition, EvalSplit, InferenceBackend, InferenceProfile, InferenceSampling,
    PackConfig,
};
use crate::eval::checks::CHECK_REGISTRY_VERSION;
use crate::eval::runner::executor::{Capture, InferenceBinding, Isolation};
use crate::eval::{
    create_run, load_run, CellSpec, DefinitionRef, RunOrigin, RunRecord, SubjectRef,
    DENOMINATOR_POLICY_V1, TAXONOMY_VERSION,
};
use crate::pack::{
    declared_paths, digest_declared_assets, resolve_pack, PackInstallOptions, PackManifest,
};
use crate::tool_surface::BashMode;
use crate::Collection;

/// Where a cell's subject pack comes from.
#[derive(Clone, Debug)]
pub enum CellSource {
    InstalledPack { name: String },
    Directory(PathBuf),
}

/// One arm of the comparison.
#[derive(Clone, Debug)]
pub struct CellRequest {
    pub cell_id: String,
    pub label: String,
    pub source: CellSource,
    pub behavior_id: String,
    pub inference_profile_id: String,
}

/// Everything an operator chose about a run, before any of it is checked.
#[derive(Clone, Debug)]
pub struct RunRequest {
    pub run_id: String,
    pub owner: String,
    pub definition_id: String,
    pub split: EvalSplit,
    /// `None` selects every case on the split.
    pub case_ids: Option<Vec<String>>,
    pub cells: Vec<CellRequest>,
    pub trials_per_case: u32,
    pub seed_base: i64,
    pub deadline_secs: Option<u64>,
    pub concurrency: u32,
    pub max_infra_retries: u32,
    pub breaker_threshold: u32,
    /// `"eval"` or `"optimization:<job_id>"`.
    pub purpose: String,
    pub source_commit: String,
    pub source_dirty: bool,
    /// What to read out of each finished trial home. Not comparability data,
    /// so it rides beside the run rather than in its origin.
    pub captures: Vec<Capture>,
    /// `<launching home>/eval/runs`.
    pub runs_dir: PathBuf,
}

/// A validated cell: its frozen spec, its materialized pack and the inference
/// documents every trial of the cell runs against.
#[derive(Clone, Debug)]
pub struct FrozenCell {
    pub spec: CellSpec,
    pub pack_dir: PathBuf,
    pub inference: InferenceBinding,
    pub tools_unrestricted_bash: bool,
}

/// A run that exists: its row, its directory, the definition it froze and its
/// cells.
#[derive(Clone, Debug)]
pub struct FrozenRun {
    pub record: RunRecord,
    pub run_dir: PathBuf,
    pub definition: EvalDefinition,
    pub cells: Vec<FrozenCell>,
    pub captures: Vec<Capture>,
    /// How many consecutive trials may produce no evidence before the loop
    /// stops the run. Frozen beside the run in `run.json`.
    pub breaker_threshold: u32,
}

/// A request that will not become a run, and why.
#[derive(Debug)]
pub struct FreezeRefused(pub String);

impl std::fmt::Display for FreezeRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for FreezeRefused {}

pub fn freeze_refused(error: &anyhow::Error) -> Option<&FreezeRefused> {
    error.downcast_ref::<FreezeRefused>()
}

fn refused(reason: impl Into<String>) -> anyhow::Error {
    anyhow::Error::from(FreezeRefused(reason.into()))
}

/// The run-level settings that have no slot on the frozen [`RunOrigin`]:
/// `breaker_threshold` (an M1 amendment to request) and the request's captures
/// (M3 moves them onto the definition). Neither is comparability data, so both
/// live beside the run rather than widening the replicating row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct RunSidecar {
    breaker_threshold: u32,
    captures: Vec<Capture>,
}

/// Validate `request`, materialize its packs and write its run.
///
/// Returns the run that exists for `request.run_id`: a new one, or the one
/// already frozen from the same request.
pub async fn freeze(
    access: &ConfigAccess,
    request: &RunRequest,
    isolation: Isolation,
) -> Result<FrozenRun> {
    validate_purpose(&request.purpose)?;
    validate_cell_identity(request)?;
    let definition = load_definition(access, request).await?;
    let case_ids = select_cases(request, &definition)?;

    let run_dir = request.runs_dir.join(&request.run_id);
    let mut validated = Vec::with_capacity(request.cells.len());
    for cell in &request.cells {
        validated.push(validate_cell(access, request, cell, isolation, &run_dir).await?);
    }

    let origin = RunOrigin {
        definition: DefinitionRef {
            definition_id: definition.definition_id.clone(),
            comparability_version: definition.comparability_version,
            digest: desired_state_document_digest(&serde_json::to_value(&definition)?)?,
        },
        split: request.split,
        case_ids,
        cells: validated
            .iter()
            .map(|(cell, _)| cell.spec.clone())
            .collect(),
        trials_per_case: request.trials_per_case,
        seed_base: request.seed_base,
        deadline_secs: request.deadline_secs,
        concurrency: request.concurrency,
        denominator_policy: DENOMINATOR_POLICY_V1.to_owned(),
        taxonomy_version: TAXONOMY_VERSION.to_owned(),
        max_infra_retries: request.max_infra_retries,
        check_registry_version: CHECK_REGISTRY_VERSION.to_owned(),
        source_commit: request.source_commit.clone(),
        source_dirty: request.source_dirty,
        purpose: request.purpose.clone(),
        breaker_threshold: request.breaker_threshold,
    };
    let sidecar = RunSidecar {
        breaker_threshold: request.breaker_threshold,
        captures: request.captures.clone(),
    };

    let existing = load_run(access, &request.owner, &request.run_id).await?;
    if let Some(record) = &existing {
        reuse_or_refuse(record, &origin, &sidecar, &run_dir)?;
    }

    for (cell, pack) in &validated {
        materialize_pack(cell, pack)?;
    }
    write_sidecar(&run_dir, &sidecar)?;

    let record = match existing {
        Some(record) => record,
        None => {
            create_run(
                access,
                &request.run_id,
                &request.owner,
                &request.owner,
                &origin,
            )
            .await?
        }
    };
    tracing::info!(
        run_id = %record.run_id,
        cells = validated.len(),
        cases = record.origin.case_ids.len(),
        "eval run frozen with materialized packs"
    );
    Ok(FrozenRun {
        record,
        run_dir,
        definition,
        cells: validated.into_iter().map(|(cell, _)| cell).collect(),
        captures: request.captures.clone(),
        breaker_threshold: request.breaker_threshold,
    })
}

/// Rebuild the run `run_id` from its row and the directory it already wrote.
///
/// Resuming re-reads what freezing decided rather than deciding it again: the
/// packs under the run directory are the run's own copy, and the definition is
/// only held to the digest the run froze. What is checked again is what could
/// have changed underneath and would change what a trial means: the
/// definition's digest, each materialized pack's digest, the inference
/// documents the cells name, and — because the resuming executor need not be
/// the one the run froze under — `isolation` against the host bash each pack
/// grants.
pub(crate) async fn thaw(
    access: &ConfigAccess,
    owner: &str,
    run_id: &str,
    runs_dir: &Path,
    isolation: Isolation,
) -> Result<FrozenRun> {
    let record = load_run(access, owner, run_id)
        .await?
        .ok_or_else(|| refused(format!("no eval run {run_id:?} for {owner}")))?;
    if record.invalidated.is_some() {
        return Err(refused(format!("run {run_id} is invalidated")));
    }
    let run_dir = runs_dir.join(run_id);
    let sidecar = read_sidecar(&run_dir)?.ok_or_else(|| {
        refused(format!(
            "run {run_id} has no {}",
            sidecar_path(&run_dir).display()
        ))
    })?;

    let definition_id = &record.origin.definition.definition_id;
    let definition: EvalDefinition =
        read_document(access, Collection::EvalDefinition, owner, definition_id)
            .await?
            .ok_or_else(|| {
                refused(format!(
                    "run {run_id} names no eval definition {definition_id:?}"
                ))
            })?;
    let digest = desired_state_document_digest(&serde_json::to_value(&definition)?)?;
    if digest != record.origin.definition.digest {
        return Err(refused(format!(
            "eval definition {definition_id:?} changed since run {run_id} froze it"
        )));
    }

    let mut cells = Vec::with_capacity(record.origin.cells.len());
    for spec in &record.origin.cells {
        let pack_dir = run_dir.join("cells").join(&spec.cell_id).join("pack");
        let pack = load_pack(&CellSource::Directory(pack_dir.clone()), owner).map_err(|error| {
            refused(format!(
                "cell {:?} materialized pack: {error:#}",
                spec.cell_id
            ))
        })?;
        if pack.digest != spec.subject.pack_digest {
            return Err(refused(format!(
                "cell {:?} materialized pack no longer digests to what run {run_id} froze",
                spec.cell_id
            )));
        }
        let inference =
            inference_binding(access, owner, &spec.cell_id, &spec.inference_profile_id).await?;
        let unrestricted = refuse_unrestricted_bash(&pack.config, isolation)?;
        cells.push(FrozenCell {
            spec: spec.clone(),
            pack_dir,
            inference,
            tools_unrestricted_bash: unrestricted.is_some(),
        });
    }

    // The origin carries the breaker threshold now. A row frozen before the
    // field existed reads the documented default, so only there does
    // `run.json` still decide: an origin that holds anything but the default
    // was frozen with the field, and one that holds the default either agrees
    // with `run.json` or predates the field.
    let breaker_threshold =
        if record.origin.breaker_threshold != crate::eval::documents::default_breaker_threshold() {
            record.origin.breaker_threshold
        } else {
            sidecar.breaker_threshold
        };

    tracing::info!(run_id, cells = cells.len(), "eval run thawed for resume");
    Ok(FrozenRun {
        record,
        run_dir,
        definition,
        cells,
        captures: sidecar.captures,
        breaker_threshold,
    })
}

fn validate_purpose(purpose: &str) -> Result<()> {
    if purpose == "eval" {
        return Ok(());
    }
    if purpose
        .strip_prefix("optimization:")
        .is_some_and(|job_id| !job_id.trim().is_empty())
    {
        return Ok(());
    }
    Err(refused(format!(
        "purpose {purpose:?} must be \"eval\" or \"optimization:<job_id>\""
    )))
}

/// A run id and a cell id each name one directory the run owns, so each has to
/// be one ordinary path component: anything else writes outside the run.
fn directory_name(kind: &str, value: &str) -> Result<()> {
    let mut components = Path::new(value).components();
    let one_normal = matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none();
    if one_normal && !value.contains('/') && !value.contains('\\') {
        return Ok(());
    }
    Err(refused(format!(
        "{kind} {value:?} must be one ordinary path component"
    )))
}

/// A cell is one directory under the run and one row in its origin. Two cells
/// sharing an id would record two digests against one materialized pack, so the
/// second is refused rather than silently handed the first one's bytes.
fn validate_cell_identity(request: &RunRequest) -> Result<()> {
    directory_name("run_id", &request.run_id)?;
    if request.cells.is_empty() {
        return Err(refused(format!("run {} compares no cell", request.run_id)));
    }
    let mut seen = BTreeSet::new();
    for cell in &request.cells {
        directory_name("cell_id", &cell.cell_id)?;
        if !seen.insert(cell.cell_id.as_str()) {
            return Err(refused(format!(
                "cell_id {:?} is requested twice",
                cell.cell_id
            )));
        }
    }
    Ok(())
}

/// One configuration document of the launching home, in its canonical form.
async fn read_document<T: serde::de::DeserializeOwned>(
    access: &ConfigAccess,
    collection: Collection,
    owner: &str,
    id: &str,
) -> Result<Option<T>> {
    let found = access
        .transact("eval.freeze.read_configuration", |txn| {
            Box::pin(
                async move { read_desired_state_record_in_txn(txn, collection, owner, id).await },
            )
        })
        .await?;
    found
        .map(|(_, value)| {
            serde_json::from_value(value)
                .with_context(|| format!("decoding {} {id:?}", collection.graphql_type()))
        })
        .transpose()
}

async fn load_definition(access: &ConfigAccess, request: &RunRequest) -> Result<EvalDefinition> {
    let definition: EvalDefinition = read_document(
        access,
        Collection::EvalDefinition,
        &request.owner,
        &request.definition_id,
    )
    .await?
    .ok_or_else(|| {
        refused(format!(
            "no eval definition {:?} for {}",
            request.definition_id, request.owner
        ))
    })?;
    definition.validate().map_err(|error| {
        refused(format!(
            "eval definition {:?}: {error:#}",
            definition.definition_id
        ))
    })?;
    Ok(definition)
}

/// The cases this run compares, sorted, each once.
fn select_cases(request: &RunRequest, definition: &EvalDefinition) -> Result<Vec<String>> {
    let mut selected = match &request.case_ids {
        Some(requested) => {
            let mut selected = Vec::with_capacity(requested.len());
            for case_id in requested {
                let case = definition
                    .cases
                    .iter()
                    .find(|case| &case.case_id == case_id)
                    .ok_or_else(|| {
                        refused(format!(
                            "eval definition {:?} has no case {case_id:?}",
                            definition.definition_id
                        ))
                    })?;
                if case.split != request.split {
                    return Err(refused(format!(
                        "case {case_id:?} is on the {:?} split, not the requested {:?} split",
                        case.split, request.split
                    )));
                }
                selected.push(case.case_id.clone());
            }
            selected
        }
        None => definition
            .cases
            .iter()
            .filter(|case| case.split == request.split)
            .map(|case| case.case_id.clone())
            .collect(),
    };
    selected.sort();
    selected.dedup();
    if selected.is_empty() {
        return Err(refused(format!(
            "eval definition {:?} selects no case on the {:?} split",
            definition.definition_id, request.split
        )));
    }
    Ok(selected)
}

/// A cell's subject pack, whichever source it came from: what it declares, what
/// it digests to, and the files a trial home needs.
struct LoadedPack {
    digest: String,
    config: PackConfig,
    files: BTreeMap<String, Vec<u8>>,
}

fn load_pack(source: &CellSource, owner: &str) -> Result<LoadedPack> {
    let options = PackInstallOptions {
        agent_did: owner.to_owned(),
    };
    match source {
        CellSource::InstalledPack { name } => {
            let resolved = resolve_pack(name)?;
            let files = declared_paths(&resolved.manifest)
                .into_iter()
                .map(|path| {
                    let bytes = resolved.asset(&path)?.to_vec();
                    Ok((path, bytes))
                })
                .collect::<Result<BTreeMap<_, _>>>()?;
            Ok(LoadedPack {
                digest: resolved.digest.clone(),
                config: resolved.load_config(&options)?,
                files,
            })
        }
        CellSource::Directory(root) => {
            let manifest: PackManifest = serde_json::from_slice(
                &std::fs::read(root.join("manifest.json"))
                    .with_context(|| format!("reading {}/manifest.json", root.display()))?,
            )
            .context("parsing pack manifest")?;
            let present = copyable_files(root)?;
            let asset = |path: &str| {
                present
                    .get(path)
                    .with_context(|| format!("pack has no asset {path:?}"))
            };
            // The same digest an installed pack resolves to: over the declared
            // contents, so a pack compared across sources is one pack.
            let digest = digest_declared_assets(&manifest, |path| asset(path).map(Vec::as_slice))?;
            let config = crate::pack::load_pack_config(
                &manifest,
                &options,
                &|path| asset(path).cloned(),
                &|name| std::env::var(name).ok(),
            )?;
            // Only the declared assets travel: a trial must receive the bytes
            // the digest covers, not whatever else the authoring directory
            // happened to hold.
            let files = declared_paths(&manifest)
                .into_iter()
                .map(|path| {
                    let bytes = asset(&path)?.clone();
                    Ok((path, bytes))
                })
                .collect::<Result<BTreeMap<_, _>>>()?;
            Ok(LoadedPack {
                digest,
                config,
                files,
            })
        }
    }
}

/// Every regular file under `root`, keyed by its path relative to `root`.
///
/// This is the reader, not the copy: [`load_pack`] narrows the result to the
/// manifest's declared assets. A symlink is refused rather than followed,
/// because a pack's bytes are its own and not a pointer out of the run.
fn copyable_files(root: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("reading pack directory {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            anyhow::ensure!(
                !kind.is_symlink(),
                "pack entry {} is a symlink; a pack carries its own bytes",
                path.display()
            );
            if kind.is_dir() {
                pending.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .with_context(|| format!("{} is outside the pack", path.display()))?
                .to_str()
                .with_context(|| format!("pack path {} is not UTF-8", path.display()))?
                .to_owned();
            files.insert(
                relative,
                std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?,
            );
        }
    }
    Ok(files)
}

/// Everything one cell has to satisfy before the run exists: its pack resolves
/// and carries the behavior, its inference documents exist, its backend is not
/// a borrowed subscription, and an embedded trial is not handed the host shell.
async fn validate_cell(
    access: &ConfigAccess,
    request: &RunRequest,
    cell: &CellRequest,
    isolation: Isolation,
    run_dir: &Path,
) -> Result<(FrozenCell, LoadedPack)> {
    let pack = load_pack(&cell.source, &request.owner)
        .map_err(|error| refused(format!("cell {:?} pack: {error:#}", cell.cell_id)))?;

    if !pack
        .config
        .agent_behaviors
        .iter()
        .any(|behavior| behavior.behavior_id == cell.behavior_id)
    {
        return Err(refused(format!(
            "cell {:?} pack has no behavior {:?}",
            cell.cell_id, cell.behavior_id
        )));
    }

    let inference = inference_binding(
        access,
        &request.owner,
        &cell.cell_id,
        &cell.inference_profile_id,
    )
    .await?;

    let unrestricted = refuse_unrestricted_bash(&pack.config, isolation)?;

    Ok((
        FrozenCell {
            spec: CellSpec {
                cell_id: cell.cell_id.clone(),
                label: cell.label.clone(),
                subject: SubjectRef {
                    pack_digest: pack.digest.clone(),
                    behavior_id: cell.behavior_id.clone(),
                },
                inference_profile_id: cell.inference_profile_id.clone(),
            },
            pack_dir: run_dir.join("cells").join(&cell.cell_id).join("pack"),
            inference,
            tools_unrestricted_bash: unrestricted.is_some(),
        },
        pack,
    ))
}

/// The inference documents one cell runs against, copied verbatim, and the
/// guarantee that no run spends the launching principal's own subscription.
///
/// Both freezing and resuming read them here, so a run that would now borrow a
/// subscription credential is refused at either door.
async fn inference_binding(
    access: &ConfigAccess,
    owner: &str,
    cell_id: &str,
    inference_profile_id: &str,
) -> Result<InferenceBinding> {
    let profile: InferenceProfile = read_document(
        access,
        Collection::InferenceProfile,
        owner,
        inference_profile_id,
    )
    .await?
    .ok_or_else(|| {
        refused(format!(
            "cell {cell_id:?} names no inference profile {inference_profile_id:?}"
        ))
    })?;
    let backend: InferenceBackend = read_document(
        access,
        Collection::InferenceBackend,
        owner,
        &profile.backend_id,
    )
    .await?
    .ok_or_else(|| {
        refused(format!(
            "inference profile {:?} names no backend {:?}",
            profile.profile_id, profile.backend_id
        ))
    })?;
    let sampling: Option<InferenceSampling> = match &profile.sampling_id {
        Some(sampling_id) => Some(
            read_document(access, Collection::InferenceSampling, owner, sampling_id)
                .await?
                .ok_or_else(|| {
                    refused(format!(
                        "inference profile {:?} names no sampling document {sampling_id:?}",
                        profile.profile_id
                    ))
                })?,
        ),
        None => None,
    };

    if matches!(backend.auth, BackendAuth::PrincipalOAuth) {
        return Err(refused(format!(
            "backend {:?} authenticates with principal_oauth; an eval run must not \
             spend the launching principal's subscription credential",
            backend.backend_id
        )));
    }

    Ok(InferenceBinding {
        profile: serde_json::to_value(&profile)?,
        backend: serde_json::to_value(&backend)?,
        sampling: sampling.as_ref().map(serde_json::to_value).transpose()?,
        // The loop fills the per-trial seed: `seed_base + trial_index`.
        seed: 0,
    })
}

/// The id of the tools document granting unrestricted host bash, if the pack
/// grants it at all — and a refusal when the trial would run on this host.
///
/// Freezing and resuming both go through here, so a run frozen under a
/// sandboxed executor cannot be resumed into an embedded one and quietly lose
/// the guarantee it was frozen with.
fn refuse_unrestricted_bash(config: &PackConfig, isolation: Isolation) -> Result<Option<String>> {
    let unrestricted = unrestricted_bash(config);
    if let (Some(tools_id), Isolation::Embedded) = (&unrestricted, isolation) {
        return Err(refused(format!(
            "tools {tools_id:?} grants unrestricted bash; an embedded trial shares this host"
        )));
    }
    Ok(unrestricted)
}

/// The id of the first tools document granting unrestricted host bash, if any.
fn unrestricted_bash(config: &PackConfig) -> Option<String> {
    config
        .tools
        .iter()
        .find(|tools| {
            tools
                .host
                .as_ref()
                .and_then(|host| host.bash.as_ref())
                .is_some_and(|bash| bash.mode == BashMode::Unrestricted)
        })
        .map(|tools| tools.tools_id.clone())
}

/// Copy the pack's declared assets under the run, once.
///
/// A materialized pack is already the run's own copy, so a resumed freeze
/// leaves it alone. The marker for that has to be honest, so the copy lands in
/// a sibling `pack.tmp` and is renamed into place after its last byte: a crash
/// mid-copy leaves a staging directory that the next freeze discards, never a
/// half-written `pack/` that every later reader accepts as complete.
fn materialize_pack(cell: &FrozenCell, pack: &LoadedPack) -> Result<()> {
    if cell.pack_dir.join("manifest.json").exists() {
        return Ok(());
    }
    let cell_dir = cell
        .pack_dir
        .parent()
        .with_context(|| format!("{} has no cell directory", cell.pack_dir.display()))?;
    let staging = cell_dir.join("pack.tmp");
    for stale in [&staging, &cell.pack_dir] {
        if stale.exists() {
            std::fs::remove_dir_all(stale)
                .with_context(|| format!("removing {}", stale.display()))?;
        }
    }
    std::fs::create_dir_all(&staging).with_context(|| format!("creating {}", staging.display()))?;
    for (path, bytes) in &pack.files {
        let destination = staging.join(path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&destination, bytes)
            .with_context(|| format!("writing {}", destination.display()))?;
    }
    std::fs::rename(&staging, &cell.pack_dir).with_context(|| {
        format!(
            "renaming {} to {}",
            staging.display(),
            cell.pack_dir.display()
        )
    })
}

fn sidecar_path(run_dir: &Path) -> PathBuf {
    run_dir.join("run.json")
}

fn read_sidecar(run_dir: &Path) -> Result<Option<RunSidecar>> {
    let path = sidecar_path(run_dir);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .with_context(|| format!("parsing {}", path.display()))
}

fn write_sidecar(run_dir: &Path, sidecar: &RunSidecar) -> Result<()> {
    std::fs::create_dir_all(run_dir).with_context(|| format!("creating {}", run_dir.display()))?;
    let path = sidecar_path(run_dir);
    std::fs::write(&path, serde_json::to_vec_pretty(sidecar)?)
        .with_context(|| format!("writing {}", path.display()))
}

/// A `run_id` means one run. The same request reuses it; anything else about
/// it is refused rather than reinterpreted.
fn reuse_or_refuse(
    record: &RunRecord,
    origin: &RunOrigin,
    sidecar: &RunSidecar,
    run_dir: &Path,
) -> Result<()> {
    if record.invalidated.is_some() {
        return Err(refused(format!("run {} is invalidated", record.run_id)));
    }
    if &record.origin != origin {
        return Err(refused(format!(
            "run {} exists with a different origin",
            record.run_id
        )));
    }
    if read_sidecar(run_dir)?.is_some_and(|found| &found != sidecar) {
        return Err(refused(format!(
            "run {} exists with a different breaker threshold or capture list",
            record.run_id
        )));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::{json, Value};
    use tempfile::TempDir;

    use super::*;
    use crate::config_client::{
        apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    use crate::document_config::ensure_agent_principal;
    use crate::eval::invalidate_run;
    use crate::eval::runner::embedded::EmbeddedHome;

    pub(crate) const OWNER: &str = "did:key:eval-owner";

    /// A launching home with an eval definition, a profile, its sampling and a
    /// usable backend already installed.
    pub(crate) struct Launching {
        _home: EmbeddedHome,
        pub(crate) access: ConfigAccess,
        dirs: TempDir,
    }

    impl Launching {
        pub(crate) async fn new() -> Self {
            let home = EmbeddedHome::create_temp("freeze").await.unwrap();
            let access = ConfigAccess::Local(home.node.clone());
            ensure_agent_principal(home.node.as_ref(), OWNER)
                .await
                .unwrap();
            let launching = Self {
                _home: home,
                access,
                dirs: tempfile::tempdir().unwrap(),
            };
            launching
                .install(vec![
                    (Collection::EvalDefinition, definition()),
                    (
                        Collection::InferenceBackend,
                        backend(
                            "backend",
                            "OpenAiCompatible",
                            json!({"kind": "unauthenticated"}),
                        ),
                    ),
                    (
                        Collection::InferenceSampling,
                        json!({"agent_did": OWNER, "sampling_id": "sampling", "temperature": 0.0}),
                    ),
                    (
                        Collection::InferenceProfile,
                        profile("local", "backend", Some("sampling")),
                    ),
                ])
                .await;
            launching
        }

        pub(crate) async fn install(&self, documents: Vec<(Collection, Value)>) {
            let plan = DesiredStateApplyPlan::new(
                documents
                    .into_iter()
                    .map(|(collection, value)| DesiredStateApplyDocument {
                        collection,
                        add: value.clone(),
                        update: value,
                    })
                    .collect(),
            )
            .unwrap();
            self.access
                .transact("freeze.test_fixture", |txn| {
                    let plan = &plan;
                    Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
                })
                .await
                .unwrap();
        }

        /// Remove one document the way an operator editing configuration
        /// later would, leaving whatever still references it dangling.
        async fn delete(&self, collection: Collection, id: &str) {
            self.access
                .transact("freeze.test_delete", |txn| {
                    Box::pin(async move {
                        let (doc_id, _) =
                            read_desired_state_record_in_txn(txn, collection, OWNER, id)
                                .await?
                                .context("document to delete")?;
                        txn.execute(&format!(
                            r#"mutation {{ delete_{name}(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}) {{ _docID }} }}"#,
                            name = collection.graphql_type(),
                            doc_id = crate::graphql::escape_graphql_string(&doc_id),
                        ))
                        .await
                        .map(|_| ())
                    })
                })
                .await
                .unwrap();
        }

        pub(crate) fn runs_dir(&self) -> PathBuf {
            self.dirs.path().join("eval/runs")
        }

        /// A fixture pack written under this home's scratch directory.
        pub(crate) fn pack(&self, name: &str, bash_mode: &str) -> PathBuf {
            let root = self.dirs.path().join(name);
            write_fixture_pack(&root, bash_mode);
            root
        }

        fn request(&self, run_id: &str, pack: &Path) -> RunRequest {
            RunRequest {
                run_id: run_id.into(),
                owner: OWNER.into(),
                definition_id: "monitor-findings".into(),
                split: EvalSplit::Validation,
                case_ids: None,
                cells: vec![CellRequest {
                    cell_id: "baseline".into(),
                    label: "baseline".into(),
                    source: CellSource::Directory(pack.to_path_buf()),
                    behavior_id: "monitor".into(),
                    inference_profile_id: "local".into(),
                }],
                trials_per_case: 2,
                seed_base: 1000,
                deadline_secs: Some(600),
                concurrency: 1,
                max_infra_retries: 1,
                breaker_threshold: 5,
                purpose: "eval".into(),
                source_commit: "0deb7659c".into(),
                source_dirty: false,
                runs_dir: self.runs_dir(),
                captures: Vec::new(),
            }
        }
    }

    fn definition() -> Value {
        json!({
            "definition_id": "monitor-findings",
            "agent_did": OWNER,
            "comparability_version": 1,
            "subject": {"kind": "behavior", "inference_slots": ["primary"]},
            "cases": [
                case("disk-warning", "validation"),
                case("train-case", "train"),
            ],
        })
    }

    fn case(case_id: &str, split: &str) -> Value {
        json!({
            "case_id": case_id,
            "split": split,
            "stages": [{
                "stage_id": "check",
                "prompt": "Run the monitor.",
                "deadline_secs": 600,
                "checks": [{"check": "captured_rows_count", "params": {"name": "findings"}, "tier": "acceptance"}],
            }],
        })
    }

    fn backend(backend_id: &str, provider_kind: &str, auth: Value) -> Value {
        json!({
            "agent_did": OWNER,
            "backend_id": backend_id,
            "name": "Workstation",
            "provider_kind": provider_kind,
            "endpoint": "http://127.0.0.1:8000/v1",
            "auth": auth,
        })
    }

    fn profile(profile_id: &str, backend_id: &str, sampling_id: Option<&str>) -> Value {
        json!({
            "agent_did": OWNER,
            "profile_id": profile_id,
            "backend_id": backend_id,
            "model_name": "test-model",
            "sampling_id": sampling_id,
        })
    }

    /// The shape of `packs/pipeline`: a manifest, a README, the canonical
    /// config bundle and one behavior sidecar.
    fn write_fixture_pack(root: &Path, bash_mode: &str) {
        std::fs::create_dir_all(root.join("agent_behaviors/monitor")).unwrap();
        std::fs::write(root.join("README.md"), "# monitor fixture\n").unwrap();
        std::fs::write(
            root.join("agent_behaviors/monitor/system_prompt.md"),
            "Watch the mailbox.\n",
        )
        .unwrap();
        let manifest = json!({
            "manifest_version": 1,
            "name": "monitor_fixture",
            "version": "1.0.0",
            "description": "Freeze fixture: one monitor behavior.",
            "authors": ["gents-ai contributors"],
            "kind": "documents",
            "assets": [
                "README.md",
                "agent_behaviors/monitor/system_prompt.md",
                "pack_config.json",
            ],
            "config": "pack_config.json",
            "inference_slots": [{
                "name": "primary",
                "description": "Runs the monitor behavior.",
                "behaviors": ["monitor"],
            }],
        });
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let config = json!({
            "agent_principal": {},
            "agent_behaviors": [{
                "behavior_id": "monitor",
                "display_name": "Monitor",
                "context_id": "monitor-context",
                "inference_profile_id": "gents:inference-slot:primary",
            }],
            "contexts": [{
                "context_id": "monitor-context",
                "display_name": "Monitor",
                "system_prompt": "./agent_behaviors/monitor/system_prompt.md",
                "tools_id": "monitor-tools",
            }],
            "tools": [{
                "tools_id": "monitor-tools",
                "display_name": "Monitor tools",
                "host": {"bash": {"mode": bash_mode}},
            }],
        });
        std::fs::write(
            root.join("pack_config.json"),
            serde_json::to_vec_pretty(&config).unwrap(),
        )
        .unwrap();
    }

    fn refusal(error: &anyhow::Error) -> String {
        freeze_refused(error)
            .unwrap_or_else(|| panic!("expected a FreezeRefused, got {error:#}"))
            .0
            .clone()
    }

    fn sidecar(run_dir: &Path) -> Value {
        serde_json::from_slice(&std::fs::read(run_dir.join("run.json")).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn freeze_writes_the_run_materializes_the_pack_and_is_idempotent() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "ReadOnly");
        let request = launching.request("run-1", &pack);

        let frozen = freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();
        assert_eq!(frozen.record.origin.case_ids, ["disk-warning"]);
        assert_eq!(
            frozen.record.origin.denominator_policy,
            DENOMINATOR_POLICY_V1
        );
        assert_eq!(frozen.record.origin.taxonomy_version, TAXONOMY_VERSION);
        assert_eq!(
            frozen.record.origin.check_registry_version,
            CHECK_REGISTRY_VERSION
        );
        assert_eq!(frozen.definition.definition_id, "monitor-findings");

        let cell = &frozen.cells[0];
        assert_eq!(cell.spec.subject.behavior_id, "monitor");
        assert!(cell.spec.subject.pack_digest.starts_with("sha256:"));
        assert_eq!(
            frozen.record.origin.cells[0].subject.pack_digest,
            cell.spec.subject.pack_digest
        );
        assert!(!cell.tools_unrestricted_bash);
        assert_eq!(cell.inference.profile["profile_id"], "local");
        assert_eq!(cell.inference.backend["backend_id"], "backend");
        assert_eq!(
            cell.inference.sampling.as_ref().expect("sampling document")["sampling_id"],
            "sampling"
        );
        assert_eq!(cell.inference.seed, 0, "the loop seeds each trial");

        assert!(cell.pack_dir.join("manifest.json").exists());
        assert!(cell
            .pack_dir
            .join("agent_behaviors/monitor/system_prompt.md")
            .exists());
        assert_eq!(sidecar(&frozen.run_dir)["breaker_threshold"], 5);

        let again = freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();
        assert_eq!(again.record, frozen.record, "a frozen run is reused whole");
    }

    /// A cell id is a directory the run owns and a row in its origin, so two
    /// cells cannot share one and neither may step outside the run directory.
    #[tokio::test]
    async fn freeze_refuses_a_duplicate_or_unsafe_cell_id() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "ReadOnly");

        let mut duplicate = launching.request("run-1", &pack);
        duplicate.cells.push(duplicate.cells[0].clone());
        let error = freeze(&launching.access, &duplicate, Isolation::Embedded)
            .await
            .unwrap_err();
        let reason = refusal(&error);
        assert!(
            reason.contains("baseline") && reason.contains("twice"),
            "{reason}"
        );

        let mut empty = launching.request("run-1", &pack);
        empty.cells.clear();
        let error = freeze(&launching.access, &empty, Isolation::Embedded)
            .await
            .unwrap_err();
        assert!(refusal(&error).contains("no cell"), "{error:#}");

        for escaping in ["..", "../sibling", "nested/cell", ""] {
            let mut unsafe_id = launching.request("run-1", &pack);
            unsafe_id.cells[0].cell_id = escaping.into();
            let error = freeze(&launching.access, &unsafe_id, Isolation::Embedded)
                .await
                .unwrap_err();
            let reason = refusal(&error);
            assert!(
                reason.contains("cell_id") && reason.contains("path component"),
                "{escaping:?}: {reason}"
            );
        }

        let mut unsafe_run = launching.request("../escape", &pack);
        unsafe_run.cells[0].cell_id = "baseline".into();
        let error = freeze(&launching.access, &unsafe_run, Isolation::Embedded)
            .await
            .unwrap_err();
        assert!(refusal(&error).contains("run_id"), "{error:#}");
    }

    /// A pack's identity is its declared assets, so those are the bytes a trial
    /// receives. Whatever else the authoring directory holds — scratch files, a
    /// checkout's own metadata — is not part of the pack and must not travel.
    #[tokio::test]
    async fn freeze_materializes_only_the_declared_assets() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "ReadOnly");
        std::fs::write(pack.join("scratch.txt"), "not declared\n").unwrap();
        std::fs::create_dir_all(pack.join(".git")).unwrap();
        std::fs::write(pack.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();

        let frozen = freeze(
            &launching.access,
            &launching.request("run-1", &pack),
            Isolation::Embedded,
        )
        .await
        .unwrap();
        let materialized = &frozen.cells[0].pack_dir;
        assert!(materialized.join("manifest.json").exists());
        assert!(materialized
            .join("agent_behaviors/monitor/system_prompt.md")
            .exists());
        assert!(!materialized.join("scratch.txt").exists());
        assert!(!materialized.join(".git").exists());
        assert!(
            !materialized.with_file_name("pack.tmp").exists(),
            "the staging directory is renamed into place, never left behind"
        );
    }

    #[tokio::test]
    async fn freeze_refuses_a_changed_origin_for_an_existing_run_id() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "ReadOnly");
        let request = launching.request("run-1", &pack);
        freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();

        let mut changed = request.clone();
        changed.trials_per_case = 3;
        let error = freeze(&launching.access, &changed, Isolation::Embedded)
            .await
            .unwrap_err();
        assert!(refusal(&error).contains("different origin"), "{error:#}");
    }

    #[tokio::test]
    async fn freeze_refuses_an_unknown_case_and_a_case_off_the_split() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "ReadOnly");

        let mut unknown = launching.request("run-1", &pack);
        unknown.case_ids = Some(vec!["nope".into()]);
        let error = freeze(&launching.access, &unknown, Isolation::Embedded)
            .await
            .unwrap_err();
        assert!(refusal(&error).contains("nope"), "{error:#}");

        let mut off_split = launching.request("run-2", &pack);
        off_split.case_ids = Some(vec!["train-case".into()]);
        let error = freeze(&launching.access, &off_split, Isolation::Embedded)
            .await
            .unwrap_err();
        let reason = refusal(&error);
        assert!(
            reason.contains("train-case") && reason.contains("split"),
            "{reason}"
        );
    }

    #[tokio::test]
    async fn freeze_refuses_unrestricted_bash_under_embedded_but_not_under_process() {
        let launching = Launching::new().await;
        let pack = launching.pack("unrestricted", "Unrestricted");

        let error = freeze(
            &launching.access,
            &launching.request("embedded", &pack),
            Isolation::Embedded,
        )
        .await
        .unwrap_err();
        assert!(refusal(&error).contains("monitor-tools"), "{error:#}");

        let frozen = freeze(
            &launching.access,
            &launching.request("process", &pack),
            Isolation::Process,
        )
        .await
        .unwrap();
        assert!(frozen.cells[0].tools_unrestricted_bash);
    }

    #[tokio::test]
    async fn freeze_refuses_an_oauth_backend() {
        let launching = Launching::new().await;
        launching
            .install(vec![
                (
                    Collection::InferenceBackend,
                    backend(
                        "subscription",
                        "ClaudeCliSubscription",
                        json!({"kind": "principal_oauth"}),
                    ),
                ),
                (
                    Collection::InferenceProfile,
                    profile("borrowed", "subscription", None),
                ),
            ])
            .await;
        let pack = launching.pack("pack", "ReadOnly");
        let mut request = launching.request("run-1", &pack);
        request.cells[0].inference_profile_id = "borrowed".into();

        let error = freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap_err();
        assert!(refusal(&error).contains("principal_oauth"), "{error:#}");
    }

    /// A profile's sampling document can be deleted after the profile was
    /// authored, and a run must not silently freeze with the provider's
    /// defaults where the profile asked for chosen sampling.
    #[tokio::test]
    async fn freeze_refuses_a_profile_whose_sampling_document_is_missing() {
        let launching = Launching::new().await;
        launching
            .install(vec![
                (
                    Collection::InferenceSampling,
                    json!({"agent_did": OWNER, "sampling_id": "spare", "temperature": 1.0}),
                ),
                (
                    Collection::InferenceProfile,
                    profile("dangling", "backend", Some("spare")),
                ),
            ])
            .await;
        launching
            .delete(Collection::InferenceSampling, "spare")
            .await;
        let pack = launching.pack("pack", "ReadOnly");
        let mut request = launching.request("run-1", &pack);
        request.cells[0].inference_profile_id = "dangling".into();

        let error = freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap_err();
        assert!(refusal(&error).contains("spare"), "{error:#}");
    }

    #[tokio::test]
    async fn freeze_refuses_an_invalidated_run() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "ReadOnly");
        let request = launching.request("run-1", &pack);
        let origin = RunOrigin {
            definition: crate::eval::DefinitionRef {
                definition_id: "monitor-findings".into(),
                comparability_version: 1,
                digest: "sha256:whatever".into(),
            },
            split: EvalSplit::Validation,
            case_ids: vec!["disk-warning".into()],
            cells: Vec::new(),
            trials_per_case: 2,
            seed_base: 1000,
            deadline_secs: Some(600),
            concurrency: 1,
            denominator_policy: DENOMINATOR_POLICY_V1.into(),
            taxonomy_version: TAXONOMY_VERSION.into(),
            max_infra_retries: 1,
            check_registry_version: CHECK_REGISTRY_VERSION.into(),
            source_commit: "0deb7659c".into(),
            source_dirty: false,
            purpose: "eval".into(),
            breaker_threshold: 5,
        };
        create_run(&launching.access, "run-1", OWNER, OWNER, &origin)
            .await
            .unwrap();
        invalidate_run(&launching.access, OWNER, "run-1", OWNER, "grader bug")
            .await
            .unwrap();

        let error = freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap_err();
        assert!(refusal(&error).contains("invalidated"), "{error:#}");
    }

    #[tokio::test]
    async fn freeze_refuses_a_malformed_purpose() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "ReadOnly");
        for malformed in ["optimization:", "other"] {
            let mut request = launching.request("run-1", &pack);
            request.purpose = malformed.into();
            let error = freeze(&launching.access, &request, Isolation::Embedded)
                .await
                .unwrap_err();
            assert!(refusal(&error).contains(malformed), "{error:#}");
        }
        for (run_id, purpose) in [("plain", "eval"), ("tuned", "optimization:job-1")] {
            let mut request = launching.request(run_id, &pack);
            request.purpose = purpose.into();
            let frozen = freeze(&launching.access, &request, Isolation::Embedded)
                .await
                .unwrap();
            assert_eq!(frozen.record.origin.purpose, purpose);
        }
    }

    #[tokio::test]
    async fn freeze_persists_request_captures_and_refuses_a_changed_list() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "ReadOnly");
        let mut request = launching.request("run-1", &pack);
        request.captures = vec![Capture::Documents {
            name: "findings".into(),
            collection: "ExperimentFinding".into(),
            filter: json!({}),
            fields: vec!["finding_id".into()],
        }];

        let frozen = freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();
        assert_eq!(frozen.captures, request.captures);
        assert_eq!(
            sidecar(&frozen.run_dir)["captures"][0]["name"],
            "findings",
            "the capture list is frozen next to the run"
        );

        let mut changed = request.clone();
        changed.captures.clear();
        let error = freeze(&launching.access, &changed, Isolation::Embedded)
            .await
            .unwrap_err();
        assert!(refusal(&error).contains("capture"), "{error:#}");
    }
}
