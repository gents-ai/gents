use super::*;
use serde_json::json;

#[test]
fn minimal_tools_round_trip_without_disabled_group_boilerplate() {
    let authored = json!({
        "tools_id": "coding", "node_did": "did:key:example",
        "host": {"root": ".", "files": {"mode": "ReadWrite"}}
    });
    let tools: Tools = serde_json::from_value(authored.clone()).unwrap();
    assert!(tools.remote.is_none());
    assert!(tools.host.as_ref().unwrap().bash.is_none());
    assert_eq!(serde_json::to_value(&tools).unwrap(), authored);
    assert_eq!(
        serde_json::from_value::<Tools>(serde_json::to_value(&tools).unwrap()).unwrap(),
        tools
    );
}

#[test]
fn missing_and_null_remote_defaults_grant_no_tools() {
    let tools: Tools = serde_json::from_value(json!({
        "tools_id": "research", "node_did": "did:key:example",
        "remote": {"services": [{"mcp_service_id": "web", "tool_names": null, "style": null}]}
    }))
    .unwrap();
    let service = &tools.remote.as_ref().unwrap().services[0];
    assert!(service.tool_names.is_empty());
    assert!(service.background_tool_names.is_empty());
    assert!(!service.required);
    assert_eq!(service.style, RemoteToolStyle::Discovery);
    assert_eq!(
        serde_json::to_value(tools).unwrap(),
        json!({
            "tools_id": "research", "node_did": "did:key:example",
            "remote": {"services": [{"mcp_service_id": "web"}]}
        })
    );
}

fn document(groups: serde_json::Value) -> Tools {
    let mut value = groups;
    value["tools_id"] = "tools".into();
    value["node_did"] = "owner".into();
    serde_json::from_value(value).unwrap()
}

#[test]
fn integrations_plugins_job_block_errors_name_the_field() {
    let tools = |job: serde_json::Value| {
        document(json!({"integrations":{"plugins":[{"plugin":"ns/runner","job":job}]}}))
    };
    assert!(tools(json!({"agent_targets":["a"]})).validate().is_ok());
    for (job, field) in [
        (json!({"agent_targets":[]}), "job.agent_targets"),
        (json!({"agent_targets":["a"],"max_live":0}), "job.max_live"),
        (
            json!({"agent_targets":["a"],"policy":{"trusted_packs":["not a pack"]}}),
            "job.policy.trusted_packs",
        ),
        (
            json!({"agent_targets":["a"],"policy":{"trusted_packs":["Bad-Pack"]}}),
            "job.policy.trusted_packs",
        ),
        (
            json!({"agent_targets":["a"],"policy":{"trusted_packs":["ns/pack@1.0.0"]}}),
            "job.policy.trusted_packs",
        ),
    ] {
        let message = tools(job).validate().unwrap_err().to_string();
        assert!(
            message.contains(&format!("integrations.plugins[ns/runner].{field}")),
            "{message}"
        );
    }
}

#[test]
fn a_0_21_job_policy_with_project_roots_still_loads_and_drops_them() {
    let tools = document(
        json!({"integrations":{"plugins":[{"plugin":"ns/runner","job":{
            "agent_targets":["a"],
            "policy":{"trusted_packs":["ns/pack"],"project_roots":["/work"]},
        }}]}}),
    );
    tools.validate().unwrap();
    let policy =
        &serde_json::to_value(&tools).unwrap()["integrations"]["plugins"][0]["job"]["policy"];
    assert_eq!(policy["trusted_packs"], json!(["ns/pack"]), "{policy}");
    assert!(policy.get("project_roots").is_none(), "{policy}");
}

mod wire {
    use super::super::*;
    use crate::documents::AgentContext;
    use crate::pack::InstalledVia;
    use crate::plugin::store::InstalledPlugin;
    use crate::plugin_job::settings;
    use gents_protocol::job::{DynamicMode, JobPolicy, PluginJobSettings};
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
            Some("gents pack wire acme/runner --agent <agent> --targets-from <pack>")
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
        assert!(settings::validate(block).is_empty());
        assert_eq!(report.job.as_ref(), Some(block));
        assert!(report.next_call.is_none());
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
        assert!(settings::validate(block).is_empty());
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
    fn targets_from_goes_through_wire_targets_from() {
        let (tools, context) = start(None);
        let pack_targets = strs(&["wf-a", "wf-b"]);
        let (tools, _, _) = wire_plugin(
            &tools,
            &context,
            &installed(true, Some("acme/engine")),
            None,
            &[],
            &[(
                "acme/content".to_owned(),
                pack_targets.clone(),
                InstalledVia::Operator,
            )],
        )
        .unwrap();
        let policy = JobPolicy {
            trusted_packs: strs(&["acme/engine"]),
            ..JobPolicy::default()
        };
        let (policy, job_targets, caller_targets) =
            settings::wire_targets_from("acme/content", &pack_targets, policy, &[], &[]);
        let block = block(&tools).unwrap();
        assert_eq!(block.policy, policy);
        assert_eq!(block.agent_targets, job_targets);
        assert_eq!(tools.agents.unwrap().target_ids, caller_targets);
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
    fn wiring_grants_the_plugins_pack_collections_for_p2p_when_enabled() {
        let mut plugin = installed(false, Some("amygdala/fleet-board"));
        plugin.data_collection_fields = [
            ("FleetBoardPost".to_owned(), strs(&["post_id"])),
            ("DeploymentNote".to_owned(), strs(&["text"])),
        ]
        .into_iter()
        .collect();
        let (tools, context) = start(None);
        let (unchanged, _, report) =
            wire_plugin(&tools, &context, &plugin, None, &[], &[]).unwrap();
        assert!(unchanged.built_ins.is_none());
        assert!(!report.changed.iter().any(|line| line.contains("P2P")));

        let mut enabled = tools.clone();
        enabled.built_ins = Some(BuiltInTools {
            enable_p2p_tool: Some(true),
            p2p_collections: strs(&["DeploymentNote"]),
            ..BuiltInTools::default()
        });
        let (wired, wired_context, report) =
            wire_plugin(&enabled, &context, &plugin, None, &[], &[]).unwrap();
        assert_eq!(
            wired.built_ins.as_ref().unwrap().p2p_collections,
            strs(&["DeploymentNote", "FleetBoardPost"])
        );
        assert!(report
            .changed
            .contains(&"granted P2P pairing of FleetBoardPost".to_owned()));
        let (again, _, report) =
            wire_plugin(&wired, &wired_context, &plugin, None, &[], &[]).unwrap();
        assert_eq!(again.built_ins, wired.built_ins);
        assert!(!report.changed.iter().any(|line| line.contains("P2P")));
        let (unwired, _, _) = unwire_plugin(&wired, &wired_context, &plugin, &[], &[]).unwrap();
        assert_eq!(unwired.built_ins, wired.built_ins);
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
        assert!(settings::validate(block).is_empty());
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
}
