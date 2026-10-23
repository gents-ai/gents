use rapidhash::{HashSetExt, RapidHashSet};
use std::ops::RangeInclusive;

use anyhow::Context;
use gents_protocol::job::{DynamicMode, JobCeiling, JobPolicy, PluginJobSettings};

use crate::document_config::{PackConfig, Tools};
use crate::pack::{InstalledVia, PackMetadata};

const MAX_LIVE: RangeInclusive<u32> = 1..=64;
const MAX_SPAWNS: RangeInclusive<u32> = 1..=1024;
const MAX_MESSAGES_PER_SLOT: RangeInclusive<u32> = 0..=16;
const RESULT_BYTES: RangeInclusive<u64> = 1024..=1024 * 1024;
const MAX_WALL_SECS: RangeInclusive<u64> = 60..=PluginJobSettings::MAX_WALL_SECS_CAP;

/// Decodes one `integrations.plugins[].job` block; unknown fields are refused.
pub fn parse(value: serde_json::Value) -> anyhow::Result<PluginJobSettings> {
    serde_json::from_value(value).context(
        "job is not a valid block; its fields are agent_targets, max_live, max_spawns, \
         max_messages_per_slot, result_bytes, max_wall_secs and policy",
    )
}

/// Lean `Policy.valid`: `dynamic_targets` must lie within `agent_targets`
/// unless dynamic scripts are off.
pub fn policy_valid(agent_targets: &[String], policy: &JobPolicy) -> bool {
    policy.dynamic == DynamicMode::Off
        || policy
            .dynamic_targets
            .iter()
            .all(|target| agent_targets.contains(target))
}

/// Lean `packJobBlockAdmissible`: a pack-shipped job block installs only on an
/// operator install, with targets and trusted packs drawn from the pack and its
/// dependencies, dynamic scripts off or check, and no project roots.
pub fn job_block_admissible(
    via: InstalledVia,
    closure: &[String],
    targets_from: &[String],
    trusted: &[String],
    dynamic: DynamicMode,
    project_roots: &[String],
) -> bool {
    via == InstalledVia::Operator
        && targets_from
            .iter()
            .chain(trusted)
            .all(|coordinate| closure.contains(coordinate))
        && matches!(dynamic, DynamicMode::Off | DynamicMode::Check)
        && project_roots.is_empty()
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
                &job.policy.project_roots,
            ),
            "plugin {:?} ships a job block this install cannot grant. A pack-shipped job \
             block needs an operator install, job.agent_targets that name AgentTargets shipped \
             by {coordinate} or its dependencies, job.policy.trusted_packs within {coordinate} \
             and its dependencies, job.policy.dynamic off or check, and no \
             job.policy.project_roots. Next: drop the job block from the pack, or install the \
             pack with `gents pack install` and wire the block into Tools as the operator",
            plugin.plugin
        );
    }
    Ok(())
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

/// Every violation of the block's rules, each naming the field and the fix.
pub fn validate(settings: &PluginJobSettings) -> Vec<String> {
    let mut errors = Vec::new();
    if settings.agent_targets.is_empty() {
        errors.push(
            "job.agent_targets is empty; list the AgentTarget ids the plugin may start, \
             or remove the job block to allow non-job verbs only"
                .to_owned(),
        );
    }
    let mut seen = RapidHashSet::new();
    for target in &settings.agent_targets {
        if target.trim().is_empty() || !seen.insert(target) {
            errors.push(format!(
                "job.agent_targets entry {target:?} is blank or repeated; list each AgentTarget id once"
            ));
        }
    }
    bounded(&mut errors, "max_live", settings.max_live, &MAX_LIVE);
    bounded(&mut errors, "max_spawns", settings.max_spawns, &MAX_SPAWNS);
    bounded(
        &mut errors,
        "max_messages_per_slot",
        settings.max_messages_per_slot,
        &MAX_MESSAGES_PER_SLOT,
    );
    bounded(
        &mut errors,
        "result_bytes",
        settings.result_bytes,
        &RESULT_BYTES,
    );
    bounded(
        &mut errors,
        "max_wall_secs",
        settings.max_wall_secs,
        &MAX_WALL_SECS,
    );
    let policy = &settings.policy;
    if !policy_valid(&settings.agent_targets, policy) {
        errors.push(
            "job.policy.dynamic_targets names ids outside job.agent_targets; add them to \
             agent_targets or set job.policy.dynamic to off"
                .to_owned(),
        );
    }
    for pack in &policy.trusted_packs {
        if !matches!(
            crate::pack_resolve::parse_pack_spec(pack),
            Ok(spec) if spec.version.is_none()
        ) {
            errors.push(format!(
                "job.policy.trusted_packs entry {pack:?} is not a pack coordinate; name it as \
                 name or namespace/name in snake_case, without @version"
            ));
        }
    }
    for root in &policy.project_roots {
        if !std::path::Path::new(root).is_absolute() {
            errors.push(format!(
                "job.policy.project_roots entry {root:?} is not absolute; give the full path"
            ));
        }
    }
    errors
}

fn bounded<T>(errors: &mut Vec<String>, field: &str, value: T, range: &RangeInclusive<T>)
where
    T: Copy + PartialOrd + std::fmt::Display,
{
    if !range.contains(&value) {
        errors.push(format!(
            "job.{field} is {value}; it must be within {}..={}",
            range.start(),
            range.end()
        ));
    }
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
mod tests {
    use super::*;
    use serde_json::json;

    fn block(overrides: serde_json::Value) -> PluginJobSettings {
        let mut value = json!({"agent_targets": ["a", "b"]});
        for (key, field) in overrides.as_object().unwrap() {
            value[key] = field.clone();
        }
        parse(value).unwrap()
    }

    fn errors(overrides: serde_json::Value) -> String {
        validate(&block(overrides)).join("; ")
    }

    #[test]
    fn absent_job_block_is_the_empty_ceiling() {
        assert_eq!(
            effective_ceiling(None),
            JobCeiling {
                targets: Vec::new(),
                max_live: 0,
                max_spawns: 0,
                max_messages_per_slot: 0,
                result_bytes: 0,
                max_wall_secs: 0,
            }
        );
        let settings = block(json!({}));
        assert!(validate(&settings).is_empty());
        assert_eq!(
            effective_ceiling(Some(&settings)),
            JobCeiling {
                targets: vec!["a".into(), "b".into()],
                max_live: 8,
                max_spawns: 128,
                max_messages_per_slot: 4,
                result_bytes: 65_536,
                max_wall_secs: 7_200,
            }
        );
    }

    #[test]
    fn job_bounds_are_enforced_at_both_edges() {
        for (field, low, high) in [
            ("max_live", 1_u64, 64_u64),
            ("max_spawns", 1, 1024),
            ("max_messages_per_slot", 0, 16),
            ("result_bytes", 1024, 1024 * 1024),
            ("max_wall_secs", 60, 86_400),
        ] {
            for edge in [low, high] {
                assert_eq!(errors(json!({ field: edge })), "", "{field}={edge}");
            }
            for beyond in [low.checked_sub(1), Some(high + 1)].into_iter().flatten() {
                let message = errors(json!({ field: beyond }));
                assert!(message.contains(&format!("job.{field}")), "{message}");
                assert!(message.contains(&format!("{low}..={high}")), "{message}");
            }
        }
        let wild = block(json!({"max_live": 0, "result_bytes": 1u64 << 40}));
        let clamped = clamp(wild);
        assert_eq!((clamped.max_live, clamped.result_bytes), (1, 1024 * 1024));
    }

    #[test]
    fn dynamic_targets_subset_is_checked_only_when_dynamic_is_not_off() {
        let policy =
            |dynamic: &str| json!({"policy": {"dynamic": dynamic, "dynamic_targets": ["c"]}});
        assert_eq!(errors(policy("off")), "");
        for dynamic in ["check", "run"] {
            let message = errors(policy(dynamic));
            assert!(message.contains("job.policy.dynamic_targets"), "{message}");
        }
        assert_eq!(errors(json!({"policy": {"dynamic": "off"}})), "");
        assert_eq!(errors(json!({"policy": {"dynamic_targets": ["a"]}})), "");
    }

    #[test]
    fn duplicate_or_empty_agent_targets_are_refused() {
        assert!(errors(json!({"agent_targets": []})).contains("job.agent_targets"));
        assert!(errors(json!({"agent_targets": ["a", "a"]})).contains("job.agent_targets"));
        assert!(errors(json!({"agent_targets": ["a", " "]})).contains("job.agent_targets"));
    }

    #[test]
    fn relative_project_roots_are_refused() {
        let message = errors(json!({"policy": {"project_roots": ["work/repo"]}}));
        assert!(message.contains("job.policy.project_roots"), "{message}");
        assert_eq!(
            errors(json!({"policy": {"project_roots": ["/work/repo"]}})),
            ""
        );
    }

    #[test]
    fn policy_valid_is_dynamic_off_or_dynamic_targets_within_agent_targets() {
        let agent_targets = vec!["a".to_owned(), "b".to_owned()];
        let policy = |dynamic, targets: &[&str]| JobPolicy {
            dynamic,
            dynamic_targets: targets.iter().map(|t| (*t).to_owned()).collect(),
            ..JobPolicy::default()
        };
        assert!(policy_valid(
            &agent_targets,
            &policy(DynamicMode::Off, &["c"])
        ));
        for mode in [DynamicMode::Check, DynamicMode::Run] {
            assert!(policy_valid(&agent_targets, &policy(mode, &["a"])));
            assert!(!policy_valid(&agent_targets, &policy(mode, &["c"])));
            assert!(policy_valid(&agent_targets, &policy(mode, &[])));
        }
    }

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn job_block_admissible_is_lean_pack_job_block_admissible() {
        use DynamicMode::{Check, Off, Run};
        use InstalledVia::{Operator, SelfConfig};
        let case = |via, from: &[&str], trusted: &[&str], dynamic, roots: &[&str]| {
            let closure = names(&["p", "q"]);
            let (from, trusted, roots) = (names(from), names(trusted), names(roots));
            job_block_admissible(via, &closure, &from, &trusted, dynamic, &roots)
        };
        assert!(case(Operator, &["p", "q"], &["p"], Off, &[]));
        assert!(case(Operator, &[], &[], Check, &[]));
        assert!(!case(SelfConfig, &["p"], &["p"], Check, &[]));
        assert!(!case(Operator, &["z"], &[], Check, &[]));
        assert!(!case(Operator, &[], &["z"], Check, &[]));
        assert!(!case(Operator, &[], &[], Run, &[]));
        assert!(!case(Operator, &[], &[], Check, &["/work/repo"]));
    }

    #[test]
    fn wire_targets_from_trusts_the_pack_and_unions_targets_in_lean_order() {
        let trusting = |packs: &[&str]| JobPolicy {
            trusted_packs: names(packs),
            ..JobPolicy::default()
        };
        let (wired, job, caller) = wire_targets_from(
            "p",
            &names(&["a", "b"]),
            trusting(&["q"]),
            &names(&["b", "c"]),
            &names(&["c", "d"]),
        );
        assert_eq!(wired.trusted_packs, names(&["p", "q"]));
        assert_eq!(job, names(&["c", "a", "b"]));
        assert_eq!(caller, names(&["d", "c", "a", "b"]));
        let (wired, _, _) = wire_targets_from("p", &[], trusting(&["q", "p"]), &[], &[]);
        assert_eq!(wired.trusted_packs, names(&["q", "p"]));
    }
}
