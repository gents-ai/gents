use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config_client::ConfigAccess;
use crate::documents::{EvalDefinition, EvalSplit};
use crate::eval::checks::CHECK_REGISTRY_VERSION;
use crate::eval::runner::executor::{Capture, InferenceBinding, Isolation};
use crate::eval::{
    create_run, load_run, CellSpec, RunOrigin, RunRecord, DENOMINATOR_POLICY_V1, TAXONOMY_VERSION,
};
use crate::Collection;

pub(crate) mod documents;
pub(crate) mod pack;
pub(crate) mod snapshot;
pub(crate) mod validate;

use self::documents::{definition_ref, inference_binding, load_definition, read_document};
use self::pack::{load_pack, materialize_pack, refuse_host_bash, validate_cell};
use self::snapshot::{
    read_frozen_definition, read_sidecar, sidecar_path, write_frozen_definition, write_sidecar,
};
use self::validate::{
    select_cases, validate_cell_identity, validate_evaluator, validate_purpose, validate_seed_range,
};

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

pub fn freeze_refused(error: &anyhow::Error) -> Option<&FreezeRefused> {
    error.downcast_ref::<FreezeRefused>()
}

fn refused(reason: impl Into<String>) -> anyhow::Error {
    anyhow::Error::from(FreezeRefused(reason.into()))
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
) -> Result<FrozenRun> {
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

/// A run_id reuses the run already frozen for it; a different request for the
/// same id is refused. The evaluator DID records who asked and is not compared.
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
pub(crate) mod tests;
