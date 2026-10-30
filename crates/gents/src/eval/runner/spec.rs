use std::path::Path;

use anyhow::{Context, Result};

use crate::document_config::{EvalCase, EvalFixtures};
use crate::eval::runner::{
    review, Capture, FixtureDocument, FixtureFile, FrozenCell, FrozenRun, PlannedTrial, ScriptKey,
    StageProgress, StageSpec, TrialFixtures, TrialSpec,
};

pub(super) fn trial_spec(
    frozen: &FrozenRun,
    planned: &PlannedTrial,
    case: &EvalCase,
    cell: &FrozenCell,
    wants_script_key: bool,
) -> Result<TrialSpec> {
    let mut inference = cell.inference.clone();
    inference.seed = planned.seed;
    Ok(TrialSpec {
        trial_id: planned.trial_id.clone(),
        pack_dir: cell.pack_dir.clone(),
        pack_digest: cell.spec.subject.pack_digest.clone(),
        agent_id: cell.spec.subject.agent_id.clone(),
        inference,
        fixtures: fixtures(
            &cell.pack_dir,
            frozen.definition.fixtures.as_ref(),
            case.fixtures.as_ref(),
        )?,
        host_bash: frozen.definition.subject.host_bash,
        stages: stage_specs(case, &frozen.captures),
        trial_dir: frozen.run_dir.join("trials").join(&planned.trial_id),
        script_key: wants_script_key.then(|| ScriptKey {
            cell_label: planned.cell_label.clone(),
            case_id: planned.case_id.clone(),
            trial_index: planned.trial_index,
            attempt: planned.attempt,
        }),
        progress: StageProgress::default(),
        review: review::StageReview::new(case),
    })
}

pub(super) fn stage_specs(case: &EvalCase, fallback: &[Capture]) -> Vec<StageSpec> {
    case.stages
        .iter()
        .map(|stage| StageSpec {
            stage_id: stage.stage_id.clone(),
            prompt: stage.prompt.clone(),
            seed: stage.seed.as_ref().map(|seed| FixtureDocument {
                collection: seed.collection.clone(),
                document: seed.document.clone(),
            }),
            deadline_secs: stage.deadline_secs,
            settle: stage.settle,
            continuation: stage.continuation.clone(),
            review_previous: stage.review_previous,
            captures: if stage.capture.is_empty() {
                fallback.to_vec()
            } else {
                stage.capture.iter().map(Capture::from).collect()
            },
        })
        .collect()
}

pub(super) fn fixtures(
    pack_dir: &Path,
    definition: Option<&EvalFixtures>,
    case: Option<&EvalFixtures>,
) -> Result<TrialFixtures> {
    let mut fixtures = TrialFixtures::default();
    for declared in [definition, case].into_iter().flatten() {
        fixtures.schemas.extend(declared.schemas.iter().cloned());
        fixtures
            .documents
            .extend(declared.documents.iter().map(|document| FixtureDocument {
                collection: document.collection.clone(),
                document: document.document.clone(),
            }));
        for asset in &declared.assets {
            fixtures.files.extend(asset_files(pack_dir, asset)?);
        }
        fixtures
            .files
            .extend(declared.files.iter().map(|file| FixtureFile {
                path: file.path.clone(),
                contents: file.contents.clone().into_bytes(),
            }));
    }
    Ok(fixtures)
}

fn asset_files(pack_dir: &Path, asset: &str) -> Result<Vec<FixtureFile>> {
    anyhow::ensure!(
        Path::new(asset)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_))),
        "fixture asset {asset:?} must be a path inside the pack"
    );
    let path = pack_dir.join(asset);
    if !path.is_dir() {
        return Ok(vec![FixtureFile {
            path: asset.to_owned(),
            contents: std::fs::read(&path)
                .with_context(|| format!("reading fixture asset {}", path.display()))?,
        }]);
    }
    let mut files = Vec::new();
    let mut pending = vec![path];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("reading fixture asset directory {}", dir.display()))?
        {
            let entry = entry?;
            let entry_path = entry.path();
            if entry.file_type()?.is_dir() {
                pending.push(entry_path);
                continue;
            }
            let relative = entry_path
                .strip_prefix(pack_dir)
                .context("fixture asset outside the pack")?
                .to_str()
                .with_context(|| format!("{} is not UTF-8", entry_path.display()))?
                .to_owned();
            files.push(FixtureFile {
                path: relative,
                contents: std::fs::read(&entry_path)
                    .with_context(|| format!("reading fixture asset {}", entry_path.display()))?,
            });
        }
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}
