use rapidhash::{HashSetExt, RapidHashSet};
use std::ops::RangeInclusive;

use anyhow::Context;
use gents_protocol::job::{DynamicMode, PluginJobSettings};
use gents_protocol::job_settings::{
    policy_valid, MAX_LIVE, MAX_MESSAGES_PER_SLOT, MAX_SPAWNS, MAX_WALL_SECS, RESULT_BYTES,
};

use crate::documents::Tools;

/// Decodes one `integrations.plugins[].job` block; unknown fields are refused.
pub fn parse(value: serde_json::Value) -> anyhow::Result<PluginJobSettings> {
    serde_json::from_value(value).context(
        "job is not a valid block; its fields are agent_targets, max_live, max_spawns, \
         max_messages_per_slot, result_bytes, max_wall_secs and policy",
    )
}

/// What a job block at `check` means to whoever just installed or wired
/// `plugin`, and the call that changes it: nothing else tells them a workflow
/// pack does nothing dynamic as shipped (a pack cannot ship `run`).
pub fn dynamic_check_notice(plugin: &str) -> String {
    format!(
        "{plugin}: its job block has policy.dynamic check, so a workflow script the model writes is checked but every spawn and message it asks for is refused until dynamic is run. Set it on the Tools that select {plugin}: config [\"tools\",\"get\"], then [\"tools\",\"edit\"] with set.integrations sent back and that plugin's job.policy.dynamic \"run\" (operator: gents config tools set --file tools.json)"
    )
}

/// [`dynamic_check_notice`] for every job block at `check` in `tools`.
pub fn dynamic_check_notices<'a>(tools: impl IntoIterator<Item = &'a Tools>) -> Vec<String> {
    tools
        .into_iter()
        .flat_map(|tools| tools.integrations.iter())
        .flat_map(|integrations| integrations.plugins.iter().flatten())
        .filter(|plugin| {
            plugin
                .job
                .as_ref()
                .is_some_and(|job| job.policy.dynamic == DynamicMode::Check)
        })
        .map(|plugin| dynamic_check_notice(&plugin.plugin))
        .collect()
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
            crate::pack::resolve::parse_pack_spec(pack),
            Ok(spec) if spec.version.is_none()
        ) {
            errors.push(format!(
                "job.policy.trusted_packs entry {pack:?} is not a pack coordinate; name it as \
                 name or namespace/name in snake_case, without @version"
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

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::job::JobCeiling;
    use gents_protocol::job_settings::{clamp, effective_ceiling};
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
}
