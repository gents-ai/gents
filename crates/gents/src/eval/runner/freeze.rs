use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::config_client::{
    desired_state_document_digest, read_desired_state_record_in_txn, ConfigAccess,
};
use crate::document_config::{
    BackendAuth, EvalDefinition, EvalSplit, InferenceBackend, InferenceExecution, InferenceProfile,
    InferenceRetryPolicy, InferenceSampling, PackConfig,
};
use crate::eval::checks::CHECK_REGISTRY_VERSION;
use crate::eval::documents::EvalError;
use crate::eval::runner::executor::{Capture, InferenceBinding, Isolation};
use crate::eval::scoring::PILOT_PURPOSE;
use crate::eval::{
    create_run, load_run, CellSpec, DefinitionRef, RunOrigin, RunRecord, SubjectRef,
    DENOMINATOR_POLICY_V1, TAXONOMY_VERSION,
};
use crate::pack::{declared_paths, digest_declared_assets, PackInstallOptions, PackManifest};
use crate::tool_surface::BashMode;
use crate::Collection;
use gents_protocol::error_chain::ErrorChain;

type Result<T, E = EvalError> = std::result::Result<T, E>;

#[derive(Clone, Debug)]
pub enum CellSource {
    Directory(PathBuf),
}

#[derive(Clone, Debug)]
pub struct CellRequest {
    pub cell_id: String,
    pub label: String,
    pub source: CellSource,
    pub agent_id: String,
    pub inference_profile_id: String,
}

#[derive(Clone, Debug)]
pub struct RunRequest {
    pub run_id: String,
    pub owner: String,
    pub evaluator_did: String,
    pub definition_id: String,
    pub split: EvalSplit,
    pub case_ids: Option<Vec<String>>,
    pub cells: Vec<CellRequest>,
    pub trials_per_case: u32,
    pub seed_base: i64,
    pub deadline_secs: Option<u64>,
    pub concurrency: u32,
    pub max_infra_retries: u32,
    pub breaker_threshold: u32,
    pub purpose: String,
    pub source_commit: String,
    pub source_dirty: bool,
    pub captures: Vec<Capture>,
    pub runs_dir: PathBuf,
}

#[derive(Clone, Debug)]
pub struct FrozenCell {
    pub spec: CellSpec,
    pub pack_dir: PathBuf,
    pub inference: InferenceBinding,
    pub tools_unrestricted_bash: bool,
}

#[derive(Clone, Debug)]
pub struct FrozenRun {
    pub record: RunRecord,
    pub run_dir: PathBuf,
    pub definition: EvalDefinition,
    pub cells: Vec<FrozenCell>,
    pub captures: Vec<Capture>,
    pub breaker_threshold: u32,
}

#[derive(Debug)]
pub struct FreezeRefused(pub String);

impl std::fmt::Display for FreezeRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for FreezeRefused {}

pub fn freeze_refused<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a FreezeRefused> {
    std::iter::successors(Some(error), |error| error.source()).find_map(|error| {
        match error.downcast_ref::<EvalError>() {
            Some(EvalError::FreezeRefused(found)) => Some(found),
            _ => error.downcast_ref::<FreezeRefused>(),
        }
    })
}

fn refused(reason: impl Into<String>) -> EvalError {
    EvalError::FreezeRefused(FreezeRefused(reason.into()))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct RunSidecar {
    breaker_threshold: u32,
    #[serde(default)]
    captures: Vec<Capture>,
}

/// Validate `request`, materialize its packs and write its run; returns the
/// run already frozen for `request.run_id` when one exists.
pub async fn freeze(
    access: &ConfigAccess,
    request: &RunRequest,
    isolation: Isolation,
) -> anyhow::Result<FrozenRun> {
    validate_purpose(&request.purpose)?;
    validate_evaluator(&request.evaluator_did)?;
    validate_cell_identity(request)?;
    validate_seed_range(request)?;
    let definition = load_definition(access, &request.owner, &request.definition_id).await?;
    let case_ids = select_cases(request, &definition)?;

    let run_dir = request.runs_dir.join(&request.run_id);
    let mut validated = Vec::with_capacity(request.cells.len());
    for cell in &request.cells {
        validated.push(validate_cell(access, request, cell, isolation, &run_dir).await?);
    }

    let origin = RunOrigin {
        definition: definition_ref(&definition)?,
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
    write_frozen_definition(&run_dir, &definition)?;

    let record = match existing {
        Some(record) => record,
        None => {
            create_run(
                access,
                &request.run_id,
                &request.owner,
                &request.evaluator_did,
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

    let definition = match read_frozen_definition(&run_dir, &record.origin.definition)? {
        Some(definition) => definition,
        None => {
            let definition_id = &record.origin.definition.definition_id;
            let definition: EvalDefinition =
                read_document(access, Collection::EvalDefinition, owner, definition_id)
                    .await?
                    .ok_or_else(|| {
                        refused(format!(
                            "run {run_id} names no eval definition {definition_id:?}"
                        ))
                    })?;
            if definition_ref(&definition)?.digest != record.origin.definition.digest {
                return Err(refused(format!(
                    "eval definition {definition_id:?} changed since run {run_id} froze it"
                )));
            }
            definition
        }
    };

    let mut cells = Vec::with_capacity(record.origin.cells.len());
    for spec in &record.origin.cells {
        let pack_dir = run_dir.join("cells").join(&spec.cell_id).join("pack");
        let pack = load_pack(&CellSource::Directory(pack_dir.clone()), owner).map_err(|error| {
            refused(format!(
                "cell {:?} materialized pack: {}",
                spec.cell_id,
                ErrorChain(error.as_ref())
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
        let unrestricted = refuse_host_bash(&pack.config, isolation)?;
        cells.push(FrozenCell {
            spec: spec.clone(),
            pack_dir,
            inference,
            tools_unrestricted_bash: unrestricted.is_some(),
        });
    }

    // An origin frozen before `breaker_threshold` joined it holds the default,
    // indistinguishable from a run that chose the default, so only there does
    // `run.json` still decide.
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

fn validate_purpose(purpose: &str) -> Result<(), EvalError> {
    if purpose == "eval" || purpose == PILOT_PURPOSE {
        return Ok(());
    }
    if purpose
        .strip_prefix("optimization:")
        .is_some_and(|job_id| !job_id.trim().is_empty())
    {
        return Ok(());
    }
    Err(refused(format!(
        "purpose {purpose:?} must be \"eval\", \"{PILOT_PURPOSE}\" or \"optimization:<job_id>\""
    )))
}

fn validate_evaluator(evaluator_did: &str) -> Result<(), EvalError> {
    if evaluator_did.trim().is_empty() {
        return Err(refused(
            "the run request names no evaluator DID; the launching home's own DID is required",
        ));
    }
    Ok(())
}

pub(crate) fn directory_name(kind: &str, value: &str) -> Result<(), EvalError> {
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

/// A cell is one directory under the run and one row in its origin; a
/// duplicate id would record two digests against one materialized pack.
fn validate_cell_identity(request: &RunRequest) -> Result<(), EvalError> {
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

/// A trial's seed is `seed_base + trial_index`, so the last one must fit an i64.
fn validate_seed_range(request: &RunRequest) -> Result<(), EvalError> {
    let last_index = i64::from(request.trials_per_case.saturating_sub(1));
    if request.seed_base.checked_add(last_index).is_some() {
        return Ok(());
    }
    Err(refused(format!(
        "seed_base {} with trials_per_case {} seeds past i64::MAX",
        request.seed_base, request.trials_per_case
    )))
}

async fn read_document<T>(
    access: &ConfigAccess,
    collection: Collection,
    owner: &str,
    id: &str,
) -> Result<Option<T>>
where
    T: serde::de::DeserializeOwned,
{
    let found = access
        .transact_readonly("eval.freeze.read_configuration", |txn| {
            Box::pin(
                async move { read_desired_state_record_in_txn(txn, collection, owner, id).await },
            )
        })
        .await?;
    found
        .map(|(_, value)| {
            serde_json::from_value(value).map_err(|source| EvalError::DecodeDocument {
                kind: collection.graphql_type().to_owned(),
                id: id.to_owned(),
                source,
            })
        })
        .transpose()
}

pub(crate) async fn load_definition(
    access: &ConfigAccess,
    owner: &str,
    definition_id: &str,
) -> Result<EvalDefinition> {
    let definition: EvalDefinition =
        read_document(access, Collection::EvalDefinition, owner, definition_id)
            .await?
            .ok_or_else(|| refused(format!("no eval definition {definition_id:?} for {owner}")))?;
    definition.validate().map_err(|error| {
        refused(format!(
            "eval definition {:?}: {}",
            definition.definition_id,
            ErrorChain(&error)
        ))
    })?;
    Ok(definition)
}

pub(crate) fn definition_ref(definition: &EvalDefinition) -> Result<DefinitionRef> {
    Ok(DefinitionRef {
        definition_id: definition.definition_id.clone(),
        comparability_version: definition.comparability_version,
        digest: desired_state_document_digest(&serde_json::to_value(definition)?)?,
    })
}

fn select_cases(
    request: &RunRequest,
    definition: &EvalDefinition,
) -> Result<Vec<String>, EvalError> {
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

pub(crate) struct LoadedPack {
    pub(crate) digest: String,
    pub(crate) config: PackConfig,
    pub(crate) manifest: PackManifest,
    pub(crate) files: BTreeMap<String, Vec<u8>>,
}

pub(crate) fn load_pack(source: &CellSource, owner: &str) -> anyhow::Result<LoadedPack> {
    let options = PackInstallOptions {
        node_did: owner.to_owned(),
    };
    let CellSource::Directory(root) = source;
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
    let digest = digest_declared_assets(&manifest, |path| present.get(path).map(Vec::as_slice))?;
    let config = crate::pack::load_pack_config(
        &manifest,
        &options,
        &|path| asset(path).cloned(),
        &|name| std::env::var(name).ok(),
    )?;
    let files = declared_paths(&manifest)
        .into_iter()
        .map(|path| {
            let bytes = asset(&path)?.clone();
            Ok((path, bytes))
        })
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    Ok(LoadedPack {
        digest,
        config,
        manifest,
        files,
    })
}

fn copyable_files(root: &Path) -> Result<BTreeMap<String, Vec<u8>>, EvalError> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|source| EvalError::ReadPackDirectory {
            path: dir.clone(),
            source,
        })? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(EvalError::PackSymlink { path });
            }
            if kind.is_dir() {
                pending.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|source| EvalError::PackPathOutside {
                    path: path.clone(),
                    source,
                })?
                .to_str()
                .ok_or_else(|| EvalError::PackPathNotUtf8 { path: path.clone() })?
                .to_owned();
            files.insert(
                relative,
                std::fs::read(&path).map_err(|source| EvalError::ReadPath {
                    path: path.clone(),
                    source,
                })?,
            );
        }
    }
    Ok(files)
}

async fn validate_cell(
    access: &ConfigAccess,
    request: &RunRequest,
    cell: &CellRequest,
    isolation: Isolation,
    run_dir: &Path,
) -> anyhow::Result<(FrozenCell, LoadedPack)> {
    let pack = load_pack(&cell.source, &request.owner).map_err(|error| {
        refused(format!(
            "cell {:?} pack: {}",
            cell.cell_id,
            ErrorChain(error.as_ref())
        ))
    })?;

    if !pack
        .config
        .agents
        .iter()
        .any(|behavior| behavior.agent_id == cell.agent_id)
    {
        return Err(refused(format!(
            "cell {:?} pack has no agent {:?}",
            cell.cell_id, cell.agent_id
        ))
        .into());
    }

    let inference = inference_binding(
        access,
        &request.owner,
        &cell.cell_id,
        &cell.inference_profile_id,
    )
    .await?;

    let unrestricted = refuse_host_bash(&pack.config, isolation)?;

    Ok((
        FrozenCell {
            spec: CellSpec {
                cell_id: cell.cell_id.clone(),
                label: cell.label.clone(),
                subject: SubjectRef {
                    pack_digest: pack.digest.clone(),
                    agent_id: cell.agent_id.clone(),
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
    let execution: Option<InferenceExecution> = match &profile.execution_id {
        Some(execution_id) => Some(
            read_document(access, Collection::InferenceExecution, owner, execution_id)
                .await?
                .ok_or_else(|| {
                    refused(format!(
                        "inference profile {:?} names no execution {execution_id:?}",
                        profile.profile_id
                    ))
                })?,
        ),
        None => None,
    };
    let retry_policy: Option<InferenceRetryPolicy> =
        match execution.as_ref().and_then(|e| e.retry_policy_id.as_ref()) {
            Some(retry_policy_id) => Some(
                read_document(
                    access,
                    Collection::InferenceRetryPolicy,
                    owner,
                    retry_policy_id,
                )
                .await?
                .ok_or_else(|| {
                    refused(format!(
                        "inference profile {:?} names no retry policy {retry_policy_id:?}",
                        profile.profile_id
                    ))
                })?,
            ),
            None => None,
        };

    if matches!(backend.auth, BackendAuth::NodeOAuth { .. }) {
        return Err(refused(format!(
            "backend {:?} authenticates with node_oauth; an eval run must not \
             spend the launching principal's subscription credential",
            backend.backend_id
        )));
    }

    Ok(InferenceBinding {
        profile: serde_json::to_value(&profile)?,
        backend: serde_json::to_value(&backend)?,
        sampling: sampling.as_ref().map(serde_json::to_value).transpose()?,
        execution: execution.as_ref().map(serde_json::to_value).transpose()?,
        retry_policy: retry_policy
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?,
        seed: 0,
    })
}

/// Under [`Isolation::Embedded`] the only host bash mode a pack may grant is
/// [`BashMode::Off`]: an embedded trial shares this host, and `ReadOnly` is an
/// argv allowlist with this host's network and no confining root, not a
/// boundary the executor can enforce. Freezing and resuming both go through
/// here, so a run frozen under a sandboxed executor cannot be resumed into an
/// embedded one and quietly lose the guarantee it was frozen with.
fn refuse_host_bash(
    config: &PackConfig,
    isolation: Isolation,
) -> Result<Option<String>, EvalError> {
    if isolation == Isolation::Embedded {
        if let Some((tools_id, mode)) = host_bash(config) {
            return Err(refused(format!(
                "tools {tools_id:?} grants host bash (mode {mode:?}); an embedded trial shares \
                 this host, so only Off is allowed"
            )));
        }
    }
    Ok(unrestricted_bash(config))
}

fn host_bash(config: &PackConfig) -> Option<(String, BashMode)> {
    config.tools.iter().find_map(|tools| {
        let bash = tools.host.as_ref().and_then(|host| host.bash.as_ref())?;
        (bash.mode != BashMode::Off).then(|| (tools.tools_id.clone(), bash.mode))
    })
}

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

fn materialize_pack(cell: &FrozenCell, pack: &LoadedPack) -> Result<(), EvalError> {
    if cell.pack_dir.join("manifest.json").exists() {
        return Ok(());
    }
    let cell_dir = cell
        .pack_dir
        .parent()
        .ok_or_else(|| EvalError::NoCellDirectory {
            path: cell.pack_dir.clone(),
        })?;
    let staging = cell_dir.join("pack.tmp");
    for stale in [&staging, &cell.pack_dir] {
        if stale.exists() {
            std::fs::remove_dir_all(stale).map_err(|source| EvalError::RemovePath {
                path: stale.to_path_buf(),
                source,
            })?;
        }
    }
    std::fs::create_dir_all(&staging).map_err(|source| EvalError::CreateDir {
        path: staging.clone(),
        source,
    })?;
    write_pack_files(&staging, &pack.files)?;
    std::fs::rename(&staging, &cell.pack_dir).map_err(|source| EvalError::Rename {
        from: staging.clone(),
        to: cell.pack_dir.clone(),
        source,
    })
}

pub(crate) fn write_pack_files(
    dir: &Path,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(), EvalError> {
    for (path, bytes) in files {
        let destination = dir.join(path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|source| EvalError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        std::fs::write(&destination, bytes).map_err(|source| EvalError::WriteFile {
            path: destination.clone(),
            source,
        })?;
    }
    Ok(())
}

fn sidecar_path(run_dir: &Path) -> PathBuf {
    run_dir.join("run.json")
}

fn read_sidecar(run_dir: &Path) -> Result<Option<RunSidecar>, EvalError> {
    let path = sidecar_path(run_dir);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).map_err(|source| EvalError::ReadPath {
        path: path.clone(),
        source,
    })?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|source| EvalError::ParsePath { path, source })
}

pub(crate) fn frozen_captures(run_dir: &Path) -> Result<Option<Vec<Capture>>, EvalError> {
    Ok(read_sidecar(run_dir)?.map(|sidecar| sidecar.captures))
}

fn write_sidecar(run_dir: &Path, sidecar: &RunSidecar) -> Result<(), EvalError> {
    std::fs::create_dir_all(run_dir).map_err(|source| EvalError::CreateDir {
        path: run_dir.to_path_buf(),
        source,
    })?;
    let path = sidecar_path(run_dir);
    super::files::write_json_atomically(&path, sidecar).map_err(|source| EvalError::WriteRecord {
        path,
        source: Box::new(source),
    })
}

pub const DEFINITION_FILE: &str = "definition.json";
fn write_frozen_definition(run_dir: &Path, definition: &EvalDefinition) -> Result<(), EvalError> {
    std::fs::create_dir_all(run_dir).map_err(|source| EvalError::CreateDir {
        path: run_dir.to_path_buf(),
        source,
    })?;
    let path = run_dir.join(DEFINITION_FILE);
    super::files::write_json_atomically(&path, definition).map_err(|source| {
        EvalError::WriteRecord {
            path,
            source: Box::new(source),
        }
    })
}

pub fn read_frozen_definition(
    run_dir: &Path,
    frozen: &DefinitionRef,
) -> Result<Option<EvalDefinition>> {
    let path = run_dir.join(DEFINITION_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(EvalError::ReadPath { path, source }),
    };
    let definition: EvalDefinition =
        serde_json::from_slice(&bytes).map_err(|source| EvalError::ParsePath {
            path: path.clone(),
            source,
        })?;
    if definition_ref(&definition)?.digest != frozen.digest {
        return Err(refused(format!(
            "{} no longer digests to the definition its run froze ({})",
            path.display(),
            frozen.digest
        )));
    }
    Ok(Some(definition))
}

/// A run_id reuses the run already frozen for it; a different request for the
/// same id is refused. The evaluator DID records who asked and is not compared.
fn reuse_or_refuse(
    record: &RunRecord,
    origin: &RunOrigin,
    sidecar: &RunSidecar,
    run_dir: &Path,
) -> Result<(), EvalError> {
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
    use crate::document_config::ensure_node;
    use crate::eval::invalidate_run;
    use crate::eval::runner::embedded::EmbeddedHome;

    pub(crate) const OWNER: &str = "did:key:eval-owner";

    pub(crate) struct Launching {
        home: EmbeddedHome,
        pub(crate) access: ConfigAccess,
        dirs: TempDir,
    }

    impl Launching {
        pub(crate) async fn new() -> Self {
            let home = EmbeddedHome::create_temp("freeze").await.unwrap();
            let access = ConfigAccess::local(home.node.clone());
            ensure_node(&home.node, OWNER).await.unwrap();
            let launching = Self {
                home,
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
                        json!({"node_did": OWNER, "sampling_id": "sampling", "temperature": 0.0}),
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
                .transact_write("freeze.test_fixture", |txn| {
                    let plan = &plan;
                    Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
                })
                .await
                .unwrap();
        }

        pub(crate) async fn delete(&self, collection: Collection, id: &str) {
            self.access
                .transact_write("freeze.test_delete", |txn| {
                    Box::pin(async move {
                        let (doc_id, _) =
                            read_desired_state_record_in_txn(txn, collection, OWNER, id)
                                .await?
                                .ok_or_else(|| anyhow::anyhow!("document to delete"))?;
                        txn.execute(&format!(
                            r#"mutation {{ delete_{name}(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}) {{ _docID }} }}"#,
                            name = collection.graphql_type(),
                            doc_id = crate::graphql::escape_graphql_string(&doc_id),
                        ))
                        .await
                        .map(|_| ())
                        .map_err(anyhow::Error::from)
                    })
                })
                .await
                .unwrap();
        }

        pub(crate) fn runs_dir(&self) -> PathBuf {
            self.dirs.path().join("eval/runs")
        }

        pub(crate) fn evaluator_did(&self) -> String {
            self.home.did().to_string()
        }

        pub(crate) fn pack(&self, name: &str, bash_mode: &str) -> PathBuf {
            let root = self.dirs.path().join(name);
            write_fixture_pack(&root, bash_mode);
            root
        }

        fn request(&self, run_id: &str, pack: &Path) -> RunRequest {
            RunRequest {
                run_id: run_id.into(),
                owner: OWNER.into(),
                evaluator_did: self.evaluator_did(),
                definition_id: "monitor-findings".into(),
                split: EvalSplit::Validation,
                case_ids: None,
                cells: vec![CellRequest {
                    cell_id: "baseline".into(),
                    label: "baseline".into(),
                    source: CellSource::Directory(pack.to_path_buf()),
                    agent_id: "monitor".into(),
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
            "node_did": OWNER,
            "comparability_version": 1,
            "subject": {"kind": "agent", "inference_slots": ["primary"]},
            "cases": [
                case("disk-warning", "validation"),
                case("train-case", "train"),
            ],
        })
    }

    #[test]
    fn frozen_files_preserve_complete_snapshots_for_existing_readers() {
        let dir = tempfile::tempdir().unwrap();
        let before = RunSidecar {
            breaker_threshold: 5,
            captures: Vec::new(),
        };
        let original: EvalDefinition = serde_json::from_value(definition()).unwrap();
        write_sidecar(dir.path(), &before).unwrap();
        write_frozen_definition(dir.path(), &original).unwrap();
        let sidecar_reader = std::fs::File::open(sidecar_path(dir.path())).unwrap();
        let definition_reader = std::fs::File::open(dir.path().join(DEFINITION_FILE)).unwrap();

        let after = RunSidecar {
            breaker_threshold: 10,
            ..before.clone()
        };
        let mut revised = original.clone();
        revised.comparability_version += 1;
        write_sidecar(dir.path(), &after).unwrap();
        write_frozen_definition(dir.path(), &revised).unwrap();

        assert_eq!(
            serde_json::from_reader::<_, RunSidecar>(sidecar_reader).unwrap(),
            before
        );
        assert_eq!(
            serde_json::from_reader::<_, EvalDefinition>(definition_reader).unwrap(),
            original
        );
        assert_eq!(read_sidecar(dir.path()).unwrap(), Some(after));
        assert_eq!(
            read_frozen_definition(dir.path(), &definition_ref(&revised).unwrap()).unwrap(),
            Some(revised)
        );
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
            "node_did": OWNER,
            "backend_id": backend_id,
            "name": "Workstation",
            "provider_kind": provider_kind,
            "endpoint": "http://127.0.0.1:8000/v1",
            "auth": auth,
        })
    }

    fn profile(profile_id: &str, backend_id: &str, sampling_id: Option<&str>) -> Value {
        json!({
            "node_did": OWNER,
            "profile_id": profile_id,
            "backend_id": backend_id,
            "model_name": "test-model",
            "sampling_id": sampling_id,
        })
    }

    pub(crate) const FIXTURE_PROMPT: &str = "Watch the mailbox.\n";

    pub(crate) fn write_fixture_pack(root: &Path, bash_mode: &str) {
        std::fs::create_dir_all(root.join("agents/monitor")).unwrap();
        std::fs::write(root.join("README.md"), "# monitor fixture\n").unwrap();
        std::fs::write(root.join("agents/monitor/system_prompt.md"), FIXTURE_PROMPT).unwrap();
        let manifest = json!({
            "manifest_version": 1,
            "name": "monitor_fixture",
            "version": "1.0.0",
            "description": "Freeze fixture: one monitor behavior.",
            "authors": ["gents-ai contributors"],
            "kind": "documents",
            "assets": [
                "README.md",
                "agents/monitor/system_prompt.md",
                "pack_config.json",
            ],
            "config": "pack_config.json",
            "inference_slots": [{
                "name": "primary",
                "description": "Runs the monitor behavior.",
                "agents": ["monitor"],
            }],
        });
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let config = json!({
            "node": {},
            "agents": [{
                "agent_id": "monitor",
                "display_name": "Monitor",
                "context_id": "monitor-context",
                "inference_profile_id": "gents:inference-slot:primary",
            }],
            "contexts": [{
                "context_id": "monitor-context",
                "display_name": "Monitor",
                "system_prompt": "./agents/monitor/system_prompt.md",
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

    fn refusal(error: &(dyn std::error::Error + 'static)) -> String {
        freeze_refused(error)
            .unwrap_or_else(|| panic!("expected a FreezeRefused, got {}", ErrorChain(error)))
            .0
            .clone()
    }

    fn sidecar(run_dir: &Path) -> Value {
        serde_json::from_slice(&std::fs::read(run_dir.join("run.json")).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn freeze_writes_the_run_materializes_the_pack_and_is_idempotent() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");
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
        assert_eq!(cell.spec.subject.agent_id, "monitor");
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
            .join("agents/monitor/system_prompt.md")
            .exists());
        assert_eq!(sidecar(&frozen.run_dir)["breaker_threshold"], 5);

        let again = freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();
        assert_eq!(again.record, frozen.record, "a frozen run is reused whole");
    }

    #[tokio::test]
    async fn freeze_records_the_requested_evaluator_and_refuses_an_empty_one() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");
        let request = launching.request("run-1", &pack);

        let frozen = freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();
        assert_eq!(frozen.record.evaluator_did, request.evaluator_did);
        assert_ne!(
            frozen.record.evaluator_did, frozen.record.owner,
            "the launching home is not the owning principal in this fixture"
        );
        let stored = load_run(&launching.access, OWNER, "run-1")
            .await
            .unwrap()
            .expect("the frozen run");
        assert_eq!(stored.evaluator_did, request.evaluator_did);

        for empty in ["", "   "] {
            let mut anonymous = launching.request("run-2", &pack);
            anonymous.evaluator_did = empty.into();
            let error = freeze(&launching.access, &anonymous, Isolation::Embedded)
                .await
                .unwrap_err();
            assert!(
                refusal(error.as_ref()).contains("evaluator DID"),
                "{}",
                ErrorChain(error.as_ref())
            );
        }
    }

    #[tokio::test]
    async fn freeze_refuses_a_duplicate_or_unsafe_cell_id() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");

        let mut duplicate = launching.request("run-1", &pack);
        duplicate.cells.push(duplicate.cells[0].clone());
        let error = freeze(&launching.access, &duplicate, Isolation::Embedded)
            .await
            .unwrap_err();
        let reason = refusal(error.as_ref());
        assert!(
            reason.contains("baseline") && reason.contains("twice"),
            "{reason}"
        );

        let mut empty = launching.request("run-1", &pack);
        empty.cells.clear();
        let error = freeze(&launching.access, &empty, Isolation::Embedded)
            .await
            .unwrap_err();
        assert!(
            refusal(error.as_ref()).contains("no cell"),
            "{}",
            ErrorChain(error.as_ref())
        );

        for escaping in ["..", "../sibling", "nested/cell", ""] {
            let mut unsafe_id = launching.request("run-1", &pack);
            unsafe_id.cells[0].cell_id = escaping.into();
            let error = freeze(&launching.access, &unsafe_id, Isolation::Embedded)
                .await
                .unwrap_err();
            let reason = refusal(error.as_ref());
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
        assert!(
            refusal(error.as_ref()).contains("run_id"),
            "{}",
            ErrorChain(error.as_ref())
        );
    }

    #[tokio::test]
    async fn freeze_materializes_only_the_declared_assets() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");
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
            .join("agents/monitor/system_prompt.md")
            .exists());
        assert!(!materialized.join("scratch.txt").exists());
        assert!(!materialized.join(".git").exists());
        assert!(
            !materialized.with_file_name("pack.tmp").exists(),
            "the staging directory is renamed into place, never left behind"
        );
    }

    #[tokio::test]
    async fn freeze_refuses_a_seed_range_past_i64() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");

        let mut overflowing = launching.request("run-1", &pack);
        overflowing.seed_base = i64::MAX;
        overflowing.trials_per_case = 2;
        let error = freeze(&launching.access, &overflowing, Isolation::Embedded)
            .await
            .unwrap_err();
        let reason = refusal(error.as_ref());
        assert!(
            reason.contains("seed_base") && reason.contains("trials_per_case"),
            "{reason}"
        );
        assert!(
            !launching.runs_dir().join("run-1").exists(),
            "a refused request writes nothing"
        );

        let mut fitting = launching.request("run-2", &pack);
        fitting.seed_base = i64::MAX;
        fitting.trials_per_case = 1;
        let frozen = freeze(&launching.access, &fitting, Isolation::Embedded)
            .await
            .unwrap();
        assert_eq!(frozen.record.origin.seed_base, i64::MAX);
    }

    #[tokio::test]
    async fn freeze_refuses_a_changed_origin_for_an_existing_run_id() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");
        let request = launching.request("run-1", &pack);
        freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();

        let mut changed = request.clone();
        changed.trials_per_case = 3;
        let error = freeze(&launching.access, &changed, Isolation::Embedded)
            .await
            .unwrap_err();
        assert!(
            refusal(error.as_ref()).contains("different origin"),
            "{}",
            ErrorChain(error.as_ref())
        );
    }

    #[tokio::test]
    async fn freeze_refuses_an_unknown_case_and_a_case_off_the_split() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");

        let mut unknown = launching.request("run-1", &pack);
        unknown.case_ids = Some(vec!["nope".into()]);
        let error = freeze(&launching.access, &unknown, Isolation::Embedded)
            .await
            .unwrap_err();
        assert!(
            refusal(error.as_ref()).contains("nope"),
            "{}",
            ErrorChain(error.as_ref())
        );

        let mut off_split = launching.request("run-2", &pack);
        off_split.case_ids = Some(vec!["train-case".into()]);
        let error = freeze(&launching.access, &off_split, Isolation::Embedded)
            .await
            .unwrap_err();
        let reason = refusal(error.as_ref());
        assert!(
            reason.contains("train-case") && reason.contains("split"),
            "{reason}"
        );
    }

    #[tokio::test]
    async fn freeze_refuses_any_host_bash_under_embedded_but_not_under_process() {
        let launching = Launching::new().await;

        for (run_id, mode) in [("embedded-ro", "ReadOnly"), ("embedded-rw", "Unrestricted")] {
            let pack = launching.pack(&format!("bash-{mode}"), mode);
            let error = freeze(
                &launching.access,
                &launching.request(run_id, &pack),
                Isolation::Embedded,
            )
            .await
            .unwrap_err();
            let reason = refusal(error.as_ref());
            assert!(
                reason.contains("monitor-tools") && reason.contains(mode),
                "{mode}: {reason}"
            );
            assert!(
                !launching.runs_dir().join(run_id).exists(),
                "a refused request leaves no run directory behind: {run_id}"
            );
        }

        let off = launching.pack("pack", "Off");
        let frozen = freeze(
            &launching.access,
            &launching.request("embedded-off", &off),
            Isolation::Embedded,
        )
        .await
        .unwrap();
        assert!(!frozen.cells[0].tools_unrestricted_bash);

        let unrestricted = launching.pack("bash-Unrestricted", "Unrestricted");
        let frozen = freeze(
            &launching.access,
            &launching.request("process", &unrestricted),
            Isolation::Process,
        )
        .await
        .unwrap();
        assert!(frozen.cells[0].tools_unrestricted_bash);

        let read_only = launching.pack("bash-ReadOnly", "ReadOnly");
        let frozen = freeze(
            &launching.access,
            &launching.request("process-read-only", &read_only),
            Isolation::Process,
        )
        .await
        .unwrap();
        assert!(
            !frozen.cells[0].tools_unrestricted_bash,
            "read-only bash is host bash, but it is not unrestricted bash"
        );
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
                        json!({"kind": "node_oauth"}),
                    ),
                ),
                (
                    Collection::InferenceProfile,
                    profile("borrowed", "subscription", None),
                ),
            ])
            .await;
        let pack = launching.pack("pack", "Off");
        let mut request = launching.request("run-1", &pack);
        request.cells[0].inference_profile_id = "borrowed".into();

        let error = freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap_err();
        assert!(
            refusal(error.as_ref()).contains("node_oauth"),
            "{}",
            ErrorChain(error.as_ref())
        );
    }

    #[tokio::test]
    async fn freeze_refuses_a_profile_whose_sampling_document_is_missing() {
        let launching = Launching::new().await;
        launching
            .install(vec![
                (
                    Collection::InferenceSampling,
                    json!({"node_did": OWNER, "sampling_id": "spare", "temperature": 1.0}),
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
        let pack = launching.pack("pack", "Off");
        let mut request = launching.request("run-1", &pack);
        request.cells[0].inference_profile_id = "dangling".into();

        let error = freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap_err();
        assert!(
            refusal(error.as_ref()).contains("spare"),
            "{}",
            ErrorChain(error.as_ref())
        );
    }

    #[tokio::test]
    async fn freeze_refuses_an_invalidated_run() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");
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
        assert!(
            refusal(error.as_ref()).contains("invalidated"),
            "{}",
            ErrorChain(error.as_ref())
        );
    }

    #[tokio::test]
    async fn freeze_refuses_a_malformed_purpose() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");
        for malformed in ["optimization:", "other"] {
            let mut request = launching.request("run-1", &pack);
            request.purpose = malformed.into();
            let error = freeze(&launching.access, &request, Isolation::Embedded)
                .await
                .unwrap_err();
            assert!(
                refusal(error.as_ref()).contains(malformed),
                "{}",
                ErrorChain(error.as_ref())
            );
        }
        for (run_id, purpose) in [
            ("plain", "eval"),
            ("tuned", "optimization:job-1"),
            ("draft", PILOT_PURPOSE),
        ] {
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
        let pack = launching.pack("pack", "Off");
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
        assert!(
            refusal(error.as_ref()).contains("capture"),
            "{}",
            ErrorChain(error.as_ref())
        );
    }

    #[tokio::test]
    async fn freeze_writes_the_definition_it_froze_and_resume_reads_it_back() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");
        let frozen = freeze(
            &launching.access,
            &launching.request("run-def", &pack),
            Isolation::Embedded,
        )
        .await
        .unwrap();
        let path = frozen.run_dir.join(DEFINITION_FILE);
        assert!(path.is_file());
        let read = read_frozen_definition(&frozen.run_dir, &frozen.record.origin.definition)
            .unwrap()
            .expect("the file is there");
        assert_eq!(read, frozen.definition);

        let mut edited = serde_json::to_value(&frozen.definition).unwrap();
        edited["title"] = json!("edited after the freeze");
        launching
            .install(vec![(Collection::EvalDefinition, edited)])
            .await;
        let thawed = thaw(
            &launching.access,
            OWNER,
            "run-def",
            &launching.runs_dir(),
            Isolation::Embedded,
        )
        .await
        .unwrap();
        assert_eq!(thawed.definition, frozen.definition);

        let mut tampered = serde_json::to_value(&frozen.definition).unwrap();
        tampered["comparability_version"] = json!(99);
        std::fs::write(&path, serde_json::to_vec(&tampered).unwrap()).unwrap();
        let error =
            read_frozen_definition(&frozen.run_dir, &frozen.record.origin.definition).unwrap_err();
        assert!(
            refusal(&error).contains("no longer digests"),
            "{}",
            ErrorChain(&error)
        );
        let error = thaw(
            &launching.access,
            OWNER,
            "run-def",
            &launching.runs_dir(),
            Isolation::Embedded,
        )
        .await
        .unwrap_err();
        assert!(
            refusal(&error).contains("no longer digests"),
            "{}",
            ErrorChain(&error)
        );
    }

    #[tokio::test]
    async fn a_run_frozen_without_the_file_falls_back_to_the_live_definition() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");
        let frozen = freeze(
            &launching.access,
            &launching.request("run-old", &pack),
            Isolation::Embedded,
        )
        .await
        .unwrap();
        std::fs::remove_file(frozen.run_dir.join(DEFINITION_FILE)).unwrap();
        assert_eq!(
            read_frozen_definition(&frozen.run_dir, &frozen.record.origin.definition).unwrap(),
            None
        );
        let thawed = thaw(
            &launching.access,
            OWNER,
            "run-old",
            &launching.runs_dir(),
            Isolation::Embedded,
        )
        .await
        .unwrap();
        assert_eq!(thawed.definition, frozen.definition);
    }
}
