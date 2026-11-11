use gents_protocol::job::DynamicMode;

use crate::documents::{PackConfig, Tools};
use crate::pack::{InstalledVia, PackMetadata};

/// Lean `packJobBlockAdmissible`: a pack-shipped job block installs only on an
/// install that records `Operator` (`installer_provenance`), with targets and trusted packs drawn from the pack and its
/// dependencies, and dynamic scripts off or check.
pub fn job_block_admissible(
    via: InstalledVia,
    closure: &[String],
    targets_from: &[String],
    trusted: &[String],
    dynamic: DynamicMode,
) -> bool {
    via == InstalledVia::Operator
        && targets_from
            .iter()
            .chain(trusted)
            .all(|coordinate| closure.contains(coordinate))
        && matches!(dynamic, DynamicMode::Off | DynamicMode::Check)
}

/// Decides every job block of a pack's Tools document through
/// [`job_block_admissible`]. `shipped` lists the pack and each resolved
/// dependency with the configuration it ships, so a target id is traced to the
/// coordinate that ships it.
pub fn pack_job_block_admissible(
    tools_doc: &Tools,
    pack: &PackMetadata,
    coordinate: &str,
    shipped: &[(String, &PackConfig)],
    installed_via: InstalledVia,
) -> anyhow::Result<()> {
    let closure: Vec<String> = std::iter::once(coordinate.to_owned())
        .chain(pack.dependencies.iter().cloned())
        .chain(shipped.iter().map(|(shipper, _)| shipper.clone()))
        .collect();
    let plugins = tools_doc
        .integrations
        .iter()
        .flat_map(|integrations| integrations.plugins.iter().flatten());
    for plugin in plugins {
        let Some(job) = &plugin.job else { continue };
        let targets_from: Vec<String> = job
            .agent_targets
            .iter()
            .map(|id| {
                shipped
                    .iter()
                    .find(|(_, config)| config.agent_targets.iter().any(|t| &t.target_id == id))
                    .map_or_else(
                        || format!("<no pack ships {id}>"),
                        |(shipper, _)| shipper.clone(),
                    )
            })
            .collect();
        anyhow::ensure!(
            job_block_admissible(
                installed_via,
                &closure,
                &targets_from,
                &job.policy.trusted_packs,
                job.policy.dynamic,
            ),
            "plugin {:?} ships a job block this install cannot grant. A pack-shipped job \
             block needs an operator install or an agent install under pack authority \
             (self_config.enable_pack_authority), job.agent_targets that name AgentTargets shipped \
             by {coordinate} or its dependencies, job.policy.trusted_packs within {coordinate} \
             and its dependencies, and job.policy.dynamic off or check; a pack never ships run, which is set on the selecting \
             Tools after install. Next: an agent that holds pack authority repeats this install \
             (config [\"tools\",\"get\"] shows self_config.enable_pack_authority); without it, the node's User \
             installs the pack with `gents config pack install`, or you drop the job block from \
             the pack and install the rest",
            plugin.plugin
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn job_block_admissible_is_lean_pack_job_block_admissible() {
        use DynamicMode::{Check, Off, Run};
        use InstalledVia::{Operator, SelfConfig};
        let case = |via, from: &[&str], trusted: &[&str], dynamic| {
            let closure = names(&["p", "q"]);
            let (from, trusted) = (names(from), names(trusted));
            job_block_admissible(via, &closure, &from, &trusted, dynamic)
        };
        assert!(case(Operator, &["p", "q"], &["p"], Off));
        assert!(case(Operator, &[], &[], Check));
        assert!(!case(SelfConfig, &["p"], &["p"], Check));
        assert!(!case(Operator, &["z"], &[], Check));
        assert!(!case(Operator, &[], &["z"], Check));
        assert!(!case(Operator, &[], &[], Run));
    }
}
