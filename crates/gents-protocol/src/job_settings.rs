use std::ops::RangeInclusive;

use crate::job::{DynamicMode, JobCeiling, JobPolicy, PluginJobSettings};

pub const MAX_LIVE: RangeInclusive<u32> = 1..=64;
pub const MAX_SPAWNS: RangeInclusive<u32> = 1..=1024;
pub const MAX_MESSAGES_PER_SLOT: RangeInclusive<u32> = 0..=16;
pub const RESULT_BYTES: RangeInclusive<u64> = 1024..=1024 * 1024;
pub const MAX_WALL_SECS: RangeInclusive<u64> = 60..=PluginJobSettings::MAX_WALL_SECS_CAP;

/// Lean `Policy.valid`: `dynamic_targets` must lie within `agent_targets`
/// unless dynamic scripts are off.
pub fn policy_valid(agent_targets: &[String], policy: &JobPolicy) -> bool {
    policy.dynamic == DynamicMode::Off
        || policy
            .dynamic_targets
            .iter()
            .all(|target| agent_targets.contains(target))
}

/// Lean `consentedDynamic`: the node owner's consent raises a block at `check`
/// to `run`; `off` stays off and no consent changes nothing.
pub fn consented_dynamic(consent: bool, mode: DynamicMode) -> DynamicMode {
    if consent && mode == DynamicMode::Check {
        DynamicMode::Run
    } else {
        mode
    }
}

/// Lean `wireTargetsFrom`: wiring a pack trusts it, adds its targets to the
/// job's and adds the job's targets to the caller's.
pub fn wire_targets_from(
    pack: &str,
    pack_targets: &[String],
    mut policy: JobPolicy,
    job_targets: &[String],
    caller_targets: &[String],
) -> (JobPolicy, Vec<String>, Vec<String>) {
    fn insert(item: &str, mut list: Vec<String>) -> Vec<String> {
        if !list.iter().any(|existing| existing == item) {
            list.insert(0, item.to_owned());
        }
        list
    }
    fn union(first: &[String], second: &[String]) -> Vec<String> {
        first
            .iter()
            .rev()
            .fold(second.to_vec(), |list, item| insert(item, list))
    }
    policy.trusted_packs = insert(pack, policy.trusted_packs);
    let job = union(job_targets, pack_targets);
    let caller = union(caller_targets, &job);
    (policy, job, caller)
}

/// Pulls every bounded number into its allowed range.
pub fn clamp(mut settings: PluginJobSettings) -> PluginJobSettings {
    settings.max_live = settings.max_live.clamp(*MAX_LIVE.start(), *MAX_LIVE.end());
    settings.max_spawns = settings
        .max_spawns
        .clamp(*MAX_SPAWNS.start(), *MAX_SPAWNS.end());
    settings.max_messages_per_slot = settings
        .max_messages_per_slot
        .clamp(*MAX_MESSAGES_PER_SLOT.start(), *MAX_MESSAGES_PER_SLOT.end());
    settings.result_bytes = settings
        .result_bytes
        .clamp(*RESULT_BYTES.start(), *RESULT_BYTES.end());
    settings.max_wall_secs = settings
        .max_wall_secs
        .clamp(*MAX_WALL_SECS.start(), *MAX_WALL_SECS.end());
    settings
}

/// The ceiling a plugin runs under; an absent block grants no targets and no
/// spawns, so only non-job verbs work.
pub fn effective_ceiling(block: Option<&PluginJobSettings>) -> JobCeiling {
    match block {
        None => JobCeiling {
            targets: Vec::new(),
            max_live: 0,
            max_spawns: 0,
            max_messages_per_slot: 0,
            result_bytes: 0,
            max_wall_secs: 0,
        },
        Some(settings) => JobCeiling {
            targets: settings.agent_targets.clone(),
            max_live: settings.max_live,
            max_spawns: settings.max_spawns,
            max_messages_per_slot: settings.max_messages_per_slot,
            result_bytes: settings.result_bytes,
            max_wall_secs: settings.max_wall_secs,
        },
    }
}

#[cfg(test)]
mod conformance {
    use super::*;

    #[test]
    fn consented_dynamic_matches_lean() {
        gents_lean_vocab::families::plugin_job_consented_dynamic::replay(|i| {
            consented_dynamic(i.consent, i.mode)
        });
    }
}
