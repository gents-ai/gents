use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Result;

use super::{refused, RunRequest};
use crate::document_config::EvalDefinition;
use crate::eval::scoring::PILOT_PURPOSE;

pub(super) fn validate_purpose(purpose: &str) -> Result<()> {
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

pub(super) fn validate_evaluator(evaluator_did: &str) -> Result<()> {
    if evaluator_did.trim().is_empty() {
        return Err(refused(
            "the run request names no evaluator DID; the launching home's own DID is required",
        ));
    }
    Ok(())
}

pub(crate) fn directory_name(kind: &str, value: &str) -> Result<()> {
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
pub(super) fn validate_cell_identity(request: &RunRequest) -> Result<()> {
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
pub(super) fn validate_seed_range(request: &RunRequest) -> Result<()> {
    let last_index = i64::from(request.trials_per_case.saturating_sub(1));
    if request.seed_base.checked_add(last_index).is_some() {
        return Ok(());
    }
    Err(refused(format!(
        "seed_base {} with trials_per_case {} seeds past i64::MAX",
        request.seed_base, request.trials_per_case
    )))
}

pub(super) fn select_cases(
    request: &RunRequest,
    definition: &EvalDefinition,
) -> Result<Vec<String>> {
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
