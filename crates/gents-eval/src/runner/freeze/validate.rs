use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Result;

use super::{refused, RunRequest};
use crate::scoring::PILOT_PURPOSE;
use gents_documents::EvalDefinition;

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

/// Refuses a definition whose subject names a core pack that is not one, or
/// one `archives` (the running build's) does not carry: every trial would
/// install it, so a run that cannot is refused before any trial starts.
pub(crate) fn validate_core_packs(
    definition: &gents_documents::EvalDefinition,
    archives: gents::core_packs::Archives<'_>,
) -> Result<()> {
    for coordinate in &definition.subject.core_packs {
        let Some(pack) = gents_pack::pack::core_packs::CorePack::of_coordinate(coordinate) else {
            return Err(refused(format!(
                "eval definition {} names {coordinate:?} in subject core_packs, which is not a core pack ({})",
                definition.definition_id,
                gents_pack::pack::core_packs::CorePack::ALL
                    .map(gents_pack::pack::core_packs::CorePack::coordinate)
                    .join(", ")
            )));
        };
        if gents::core_packs::release_archive(archives, pack).is_none() {
            return Err(refused(format!(
                "eval definition {} installs {} on its subject, but {}",
                definition.definition_id,
                pack.coordinate(),
                gents::core_packs::not_carried(pack)
            )));
        }
    }
    Ok(())
}

/// Refuses a cell whose agent cannot install the definition's core packs:
/// a trial installs each through the agent's own config tool, as init
/// installs one through the Engineer's, so the agent's Tools must grant
/// self-config with pack install and pack authority. Without the grant every
/// trial would fail at setup, so the run is refused before any starts.
pub(crate) fn validate_core_pack_installer(
    definition: &gents_documents::EvalDefinition,
    cell_id: &str,
    agent_slot: &str,
    authored: &gents_pack::pack::AuthoredPackConfig,
) -> Result<()> {
    if definition.subject.core_packs.is_empty() {
        return Ok(());
    }
    let rows = |key: &str| authored.value[key].as_array().into_iter().flatten();
    let grant = authored
        .agent(agent_slot)
        .and_then(|agent| {
            rows("contexts").find(|context| {
                context["context_id"].is_string() && context["context_id"] == agent["context_id"]
            })
        })
        .and_then(|context| {
            rows("tools").find(|tools| {
                tools["tools_id"].is_string() && tools["tools_id"] == context["tools_id"]
            })
        })
        .map(|tools| &tools["self_config"]);
    let granted = |key: &str| grant.is_some_and(|grant| grant[key] == true);
    if [
        "enable_self_config",
        "enable_pack_install",
        "enable_pack_authority",
    ]
    .into_iter()
    .all(granted)
    {
        return Ok(());
    }
    Err(refused(format!(
        "cell {cell_id:?} agent {agent_slot:?} cannot install the core packs eval definition {} names: its Tools must grant self_config with enable_self_config, enable_pack_install and enable_pack_authority",
        definition.definition_id
    )))
}
