use super::*;
use crate::pack::InstalledVia;
use crate::plugin::store::InstalledPlugin;
use gents_documents::plugin_job_settings;
use gents_documents::{AgentContext, AgentTools, BuiltInTools};
use gents_protocol::job::{DynamicMode, PluginJobSettings};
use serde_json::json;
use std::collections::BTreeSet;

fn installed(job: bool, owner: Option<&str>) -> InstalledPlugin {
    serde_json::from_value(json!({
        "namespace": "acme", "name": "runner", "version": "1.0.0",
        "digest": "sha256:aa", "language": "rust",
        "declaration": {
            "name": "runner", "description": "Runs", "artifact": "plugins/runner.afb",
            "language": "rust", "input_schema": {"type": "object"}, "job": job
        },
        "owner_pack_coordinate": owner,
    }))
    .unwrap()
}

fn start(enabled: Option<bool>) -> (Tools, AgentContext) {
    let tools = Tools {
        agents: Some(AgentTools {
            target_ids: vec![],
            enabled,
        }),
        ..Tools::default()
    };
    let context = serde_json::from_value(json!({
        "context_id": "ctx", "node_did": "did:key:example",
        "skill_ids": ["existing"]
    }))
    .unwrap();
    (tools, context)
}

fn strs(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

fn set(items: &[String]) -> BTreeSet<&str> {
    items.iter().map(String::as_str).collect()
}

fn job(value: serde_json::Value) -> PluginJobSettings {
    serde_json::from_value(value).unwrap()
}

fn block(tools: &Tools) -> Option<&PluginJobSettings> {
    tools
        .integrations
        .as_ref()
        .unwrap()
        .plugins
        .as_ref()
        .unwrap()[0]
        .job
        .as_ref()
}

fn existing(tools: &mut Tools, block: PluginJobSettings) {
    tools.integrations = Some(IntegrationTools {
        plugins: Some(vec![PluginToolRef {
            plugin: "acme/runner".to_owned(),
            digest: Some("sha256:old".to_owned()),
            tool_calls: false,
            input_fields: vec![],
            job: Some(block),
        }]),
        ..IntegrationTools::default()
    });
}

#[test]
fn no_reviewed_block_and_no_targets_wires_without_a_job_block() {
    let (tools, context) = start(None);
    let (tools, _, report) =
        wire_plugin(&tools, &context, &installed(true, None), None, &[], &[]).unwrap();
    let refs = tools
        .integrations
        .as_ref()
        .unwrap()
        .plugins
        .as_ref()
        .unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].plugin, "acme/runner");
    assert_eq!(refs[0].digest.as_deref(), Some("sha256:aa"));
    assert!(refs[0].job.is_none());
    assert!(report.job.is_none());
    assert_eq!(
        report.next_call.as_deref(),
        Some("gents config pack wire acme/runner --agent <agent> --targets-from <pack>")
    );
}

#[test]
fn targets_from_without_reviewed_block_builds_a_valid_block() {
    let (tools, context) = start(None);
    let (tools, _, report) = wire_plugin(
        &tools,
        &context,
        &installed(true, Some("acme/engine")),
        None,
        &[],
        &[(
            "acme/content".to_owned(),
            strs(&["wf-a"]),
            InstalledVia::Operator,
        )],
    )
    .unwrap();
    let block = block(&tools).unwrap();
    assert_eq!(set(&block.agent_targets), BTreeSet::from(["wf-a"]));
    assert_eq!(
        set(&block.policy.trusted_packs),
        BTreeSet::from(["acme/engine", "acme/content"])
    );
    assert_eq!(block.policy.dynamic, DynamicMode::Check);
    assert!(block.policy.dynamic_targets.is_empty());
    assert_eq!(block.max_live, PluginJobSettings::DEFAULT_MAX_LIVE);
    assert_eq!(block.max_spawns, PluginJobSettings::DEFAULT_MAX_SPAWNS);
    assert_eq!(
        block.max_messages_per_slot,
        PluginJobSettings::DEFAULT_MAX_MESSAGES_PER_SLOT
    );
    assert_eq!(block.result_bytes, PluginJobSettings::DEFAULT_RESULT_BYTES);
    assert_eq!(
        block.max_wall_secs,
        PluginJobSettings::DEFAULT_MAX_WALL_SECS
    );
    assert!(plugin_job_settings::validate(block).is_empty());
    assert_eq!(report.job.as_ref(), Some(block));
    assert!(report.next_call.is_none());
    // A block left at check says what that refuses and the call that
    // changes it (packs#61 item 6).
    let warning = report
        .warnings
        .iter()
        .find(|warning| warning.starts_with("dynamic_check: "))
        .expect("a check block is reported");
    assert!(
        warning.contains("every spawn and message it asks for is refused")
            && warning.contains("[\"tools\",\"edit\"]")
            && warning.contains("acme/runner"),
        "{warning}"
    );

    // The node owner's consent raises check to run and records it.
    let (mut tools, mut report) = (tools, report);
    consent_dynamic_run(&mut tools, "acme/runner", &mut report);
    assert_eq!(
        self::block(&tools).unwrap().policy.dynamic,
        DynamicMode::Run
    );
    assert_eq!(
        report.job.as_ref().map(|job| job.policy.dynamic),
        Some(DynamicMode::Run)
    );
    assert!(report
        .warnings
        .iter()
        .all(|warning| !warning.starts_with("dynamic_check: ")));
    assert!(report
        .changed
        .iter()
        .any(|change| change.contains("set job.policy.dynamic run for acme/runner")));
}

#[test]
fn engine_reviewed_block_with_wf_read_only_passes() {
    let reviewed = job(json!({
        "agent_targets": ["wf-read-only"],
        "policy": {
            "dynamic": "check", "dynamic_targets": ["wf-read-only"],
            "trusted_packs": ["gents/rhai_workflows"]
        }
    }));
    let (tools, context) = start(None);
    let (tools, _, _) = wire_plugin(
        &tools,
        &context,
        &installed(true, Some("gents/rhai_workflows")),
        Some(&reviewed),
        &[],
        &[],
    )
    .unwrap();
    let block = block(&tools).unwrap();
    assert_eq!(block, &reviewed);
    assert!(plugin_job_settings::validate(block).is_empty());
}

#[test]
fn merge_never_widens() {
    let (mut tools, context) = start(None);
    existing(
        &mut tools,
        job(json!({
            "agent_targets": ["a", "c"], "max_live": 2, "max_spawns": 10,
            "max_messages_per_slot": 9, "result_bytes": 2048, "max_wall_secs": 100,
            "policy": {"dynamic": "off", "dynamic_targets": ["a", "c"]}
        })),
    );
    let reviewed = job(json!({
        "agent_targets": ["b", "c"], "max_live": 8,
        "policy": {"dynamic": "check", "dynamic_targets": ["b", "c"]}
    }));
    let (tools, _, _) = wire_plugin(
        &tools,
        &context,
        &installed(true, None),
        Some(&reviewed),
        &[],
        &[],
    )
    .unwrap();
    let block = block(&tools).unwrap();
    assert_eq!(set(&block.agent_targets), BTreeSet::from(["a", "b", "c"]));
    assert_eq!(block.max_live, 2);
    assert_eq!(block.max_spawns, 10);
    assert_eq!(block.max_messages_per_slot, 4);
    assert_eq!(block.result_bytes, 2048);
    assert_eq!(block.max_wall_secs, 100);
    assert_eq!(block.policy.dynamic, DynamicMode::Off);
    assert_eq!(block.policy.dynamic_targets, strs(&["c"]));
}

#[test]
fn agents_enabled_unchanged() {
    for enabled in [None, Some(false)] {
        let (tools, context) = start(enabled);
        let (tools, _, _) = wire_plugin(
            &tools,
            &context,
            &installed(true, None),
            None,
            &[],
            &[(
                "acme/content".to_owned(),
                strs(&["wf-a"]),
                InstalledVia::Operator,
            )],
        )
        .unwrap();
        assert_eq!(tools.agents.unwrap().enabled, enabled);
    }
}

#[test]
fn agent_new_widened_warning_when_enabled() {
    let (tools, context) = start(Some(true));
    let (tools, _, report) = wire_plugin(
        &tools,
        &context,
        &installed(true, None),
        None,
        &[],
        &[(
            "acme/content".to_owned(),
            strs(&["wf-a"]),
            InstalledVia::Operator,
        )],
    )
    .unwrap();
    assert_eq!(tools.agents.unwrap().enabled, Some(true));
    assert!(report
        .warnings
        .iter()
        .any(|warning| warning.starts_with("agent_new_widened") && warning.contains("wf-a")));
}

#[test]
fn non_operator_targets_from_pack_warns() {
    let (tools, context) = start(None);
    let (_, _, report) = wire_plugin(
        &tools,
        &context,
        &installed(true, None),
        None,
        &[],
        &[(
            "acme/dep".to_owned(),
            strs(&["wf-a"]),
            InstalledVia::SelfConfig,
        )],
    )
    .unwrap();
    assert!(report.warnings.iter().any(|warning| {
        warning.starts_with("trusted_pack_not_operator") && warning.contains("acme/dep")
    }));
}

#[test]
fn skill_ids_added_to_context() {
    let (tools, context) = start(None);
    let skills = strs(&["create-workflow"]);
    let (_, context, _) =
        wire_plugin(&tools, &context, &installed(true, None), None, &skills, &[]).unwrap();
    assert_eq!(context.skill_ids, strs(&["existing", "create-workflow"]));
    let (_, context, _) =
        wire_plugin(&tools, &context, &installed(true, None), None, &skills, &[]).unwrap();
    assert_eq!(context.skill_ids, strs(&["existing", "create-workflow"]));
}

fn sorted(mut tools: Tools) -> Tools {
    if let Some(agents) = tools.agents.as_mut() {
        agents.target_ids.sort();
    }
    tools
}

fn content_pack() -> (String, Vec<String>, InstalledVia) {
    (
        "acme/content".to_owned(),
        strs(&["wf-a", "wf-b"]),
        InstalledVia::Operator,
    )
}

#[test]
fn wiring_never_grants_p2p_collections() {
    let mut plugin = installed(false, Some("amygdala/fleet-board"));
    plugin.data_collection_fields = [("FleetBoardPost".to_owned(), strs(&["post_id"]))]
        .into_iter()
        .collect();
    let (mut tools, context) = start(None);
    tools.built_ins = Some(BuiltInTools {
        enable_p2p_tool: Some(true),
        ..BuiltInTools::default()
    });
    let (wired, _, report) = wire_plugin(&tools, &context, &plugin, None, &[], &[]).unwrap();
    assert_eq!(wired.built_ins, tools.built_ins);
    assert!(!report.changed.iter().any(|line| line.contains("P2P")));
    assert!(serde_json::from_value::<BuiltInTools>(
        serde_json::json!({"enable_p2p_tool": true, "p2p_collections": ["FleetBoardPost"]})
    )
    .is_err());
}

#[test]
fn wire_then_remove_restores_documents() {
    let reviewed = job(json!({"agent_targets": ["wf-a"]}));
    let skills = strs(&["create-workflow"]);
    let plugin = installed(true, Some("acme/engine"));
    for wired_from in [vec![], vec![content_pack()]] {
        let (tools, context) = start(None);
        let (wired, wired_context, _) = wire_plugin(
            &tools,
            &context,
            &plugin,
            Some(&reviewed),
            &skills,
            &wired_from,
        )
        .unwrap();
        assert_ne!(wired, tools);
        let (restored, restored_context, report) =
            unwire_plugin(&wired, &wired_context, &plugin, &skills, &[]).unwrap();
        assert_eq!(sorted(restored), sorted(tools));
        assert_eq!(restored_context, context);
        assert!(report.job.is_none());
        assert!(report.next_call.is_none());
        assert!(!report.changed.is_empty());
    }
}

#[test]
fn shared_targets_and_skills_survive_one_removal() {
    let (tools, context) = start(None);
    let skills = strs(&["create-workflow"]);
    let runner = installed(true, None);
    let mut other = installed(true, None);
    other.name = "other".to_owned();
    let (tools, context, _) = wire_plugin(
        &tools,
        &context,
        &runner,
        Some(&job(json!({"agent_targets": ["shared", "runner-own"]}))),
        &skills,
        &[],
    )
    .unwrap();
    let (tools, context, _) = wire_plugin(
        &tools,
        &context,
        &other,
        Some(&job(json!({"agent_targets": ["shared", "other-own"]}))),
        &skills,
        &[],
    )
    .unwrap();

    let (tools, context, _) = unwire_plugin(&tools, &context, &runner, &skills, &[]).unwrap();
    let plugins = tools
        .integrations
        .as_ref()
        .unwrap()
        .plugins
        .as_ref()
        .unwrap();
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0].plugin, "acme/other");
    let target_ids = &tools.agents.as_ref().unwrap().target_ids;
    assert_eq!(set(target_ids), BTreeSet::from(["shared", "other-own"]));
    assert!(context.skill_ids.contains(&"create-workflow".to_owned()));

    let (tools, context, _) = unwire_plugin(&tools, &context, &other, &skills, &[]).unwrap();
    assert!(tools.agents.as_ref().unwrap().target_ids.is_empty());
    assert_eq!(context.skill_ids, strs(&["existing"]));
}

#[test]
fn targets_from_removal_keeps_the_plugin() {
    let (tools, context) = start(None);
    let plugin = installed(true, Some("acme/engine"));
    let extra = (
        "acme/extra".to_owned(),
        strs(&["wf-c"]),
        InstalledVia::Operator,
    );
    let (tools, context, _) = wire_plugin(
        &tools,
        &context,
        &plugin,
        None,
        &[],
        &[content_pack(), extra],
    )
    .unwrap();
    let (tools, _, report) =
        unwire_plugin(&tools, &context, &plugin, &[], &[content_pack()]).unwrap();
    let kept = &tools
        .integrations
        .as_ref()
        .unwrap()
        .plugins
        .as_ref()
        .unwrap()[0];
    assert_eq!(kept.plugin, "acme/runner");
    assert_eq!(kept.digest.as_deref(), Some("sha256:aa"));
    let block = block(&tools).unwrap();
    assert_eq!(set(&block.agent_targets), BTreeSet::from(["wf-c"]));
    assert_eq!(
        set(&block.policy.trusted_packs),
        BTreeSet::from(["acme/engine", "acme/extra"])
    );
    assert!(plugin_job_settings::validate(block).is_empty());
    assert_eq!(
        set(&tools.agents.as_ref().unwrap().target_ids),
        BTreeSet::from(["wf-c"])
    );
    assert_eq!(report.job.as_ref(), Some(block));
    assert!(report.next_call.is_none());
}

#[test]
fn removing_the_last_target_drops_the_job_block() {
    let (tools, context) = start(None);
    let plugin = installed(true, Some("acme/engine"));
    let (tools, context, _) =
        wire_plugin(&tools, &context, &plugin, None, &[], &[content_pack()]).unwrap();
    let (tools, _, report) =
        unwire_plugin(&tools, &context, &plugin, &[], &[content_pack()]).unwrap();
    assert!(block(&tools).is_none());
    assert!(report.job.is_none());
    assert!(report.next_call.unwrap().contains("non-job verbs only"));
    assert!(tools.agents.unwrap().target_ids.is_empty());
}

#[test]
fn remove_never_changes_enabled_or_dynamic() {
    for (removed, dynamic_targets) in [("b", vec!["a"]), ("a", vec![])] {
        let (mut tools, context) = start(Some(true));
        tools.agents.as_mut().unwrap().target_ids = strs(&["a", "b"]);
        existing(
            &mut tools,
            job(json!({
                "agent_targets": ["a", "b"],
                "policy": {"dynamic": "run", "dynamic_targets": ["a"]}
            })),
        );
        let pack = (
            "acme/content".to_owned(),
            strs(&[removed]),
            InstalledVia::Operator,
        );
        let (tools, _, _) =
            unwire_plugin(&tools, &context, &installed(true, None), &[], &[pack]).unwrap();
        let block = block(&tools).unwrap();
        assert_eq!(block.policy.dynamic, DynamicMode::Run);
        assert_eq!(block.policy.dynamic_targets, strs(&dynamic_targets));
        assert_eq!(tools.agents.unwrap().enabled, Some(true));
    }
}
