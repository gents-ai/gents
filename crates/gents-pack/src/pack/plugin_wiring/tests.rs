use super::*;
use crate::pack::InstalledVia;
use crate::plugin::store::InstalledPlugin;
use gents_documents::{AgentContext, AgentTools, BuiltInTools};
use gents_lean_vocab::families::pack_plugin_wiring as family;
use gents_lean_vocab::family::recode;
use gents_protocol::job::{DynamicMode, PluginJobSettings};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Wire,
    Unwire,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Warning {
    TrustedPackNotOperator,
    DynamicCheck,
    AgentNewWidened,
    NotWired,
}

type Surface = family::Surface<DynamicMode>;

fn installed_as(coordinate: &str, digest: &str, job: bool, owner: Option<&str>) -> InstalledPlugin {
    let (namespace, name) = coordinate.split_once('/').expect("plugin coordinate");
    serde_json::from_value(json!({
        "namespace": namespace, "name": name, "version": "1.0.0",
        "digest": digest, "language": "rust",
        "declaration": {
            "name": name, "description": "Runs", "artifact": format!("plugins/{name}.afb"),
            "language": "rust", "input_schema": {"type": "object"}, "job": job
        },
        "owner_pack_coordinate": owner,
    }))
    .unwrap()
}

fn installed(job: bool, owner: Option<&str>) -> InstalledPlugin {
    installed_as("acme/runner", "sha256:aa", job, owner)
}

fn context_with(skill_ids: &[String]) -> AgentContext {
    serde_json::from_value(json!({
        "context_id": "ctx", "node_did": "did:key:example", "skill_ids": skill_ids
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
    (tools, context_with(&["existing".to_owned()]))
}

fn strs(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

fn documents(surface: &Surface) -> (Tools, AgentContext) {
    let plugins: Vec<PluginToolRef> = surface
        .plugins
        .iter()
        .map(|entry| PluginToolRef {
            plugin: entry.plugin.clone(),
            digest: Some(entry.digest.clone()),
            tool_calls: entry.tool_calls,
            input_fields: Vec::new(),
            job: entry.job.as_ref().map(recode),
        })
        .collect();
    let tools = Tools {
        agents: Some(AgentTools {
            target_ids: surface.targets.clone(),
            enabled: surface.enabled,
        }),
        integrations: (!plugins.is_empty()).then(|| IntegrationTools {
            plugins: Some(plugins),
            ..IntegrationTools::default()
        }),
        built_ins: Some(BuiltInTools {
            enable_p2p_tool: Some(true),
            ..BuiltInTools::default()
        }),
        ..Tools::default()
    };
    (tools, context_with(&surface.skills))
}

fn surface(tools: &Tools, context: &AgentContext) -> Surface {
    let agents = tools.agents.clone().unwrap_or_default();
    family::Surface {
        plugins: tools
            .integrations
            .iter()
            .flat_map(|integrations| integrations.plugins.iter().flatten())
            .map(|entry| family::Entry {
                plugin: entry.plugin.clone(),
                digest: entry.digest.clone().unwrap_or_default(),
                tool_calls: entry.tool_calls,
                job: entry.job.as_ref().map(recode),
            })
            .collect(),
        targets: agents.target_ids,
        enabled: agents.enabled,
        skills: context.skill_ids.clone(),
    }
}

fn outside_wiring(tools: &Tools, context: &AgentContext) -> (Tools, AgentContext) {
    let mut tools = tools.clone();
    tools.agents = None;
    if let Some(integrations) = tools.integrations.as_mut() {
        integrations.plugins = None;
    }
    if tools.integrations == Some(IntegrationTools::default()) {
        tools.integrations = None;
    }
    let mut context = context.clone();
    context.skill_ids.clear();
    (tools, context)
}

fn warning(text: &str) -> Warning {
    match text.split_once(':').map(|(code, _)| code) {
        Some("trusted_pack_not_operator") => Warning::TrustedPackNotOperator,
        Some("dynamic_check") => Warning::DynamicCheck,
        Some("agent_new_widened") => Warning::AgentNewWidened,
        _ if text.ends_with("is not wired; nothing to remove") => Warning::NotWired,
        _ => panic!("a wiring warning the model does not name: {text}"),
    }
}

#[test]
fn wiring_conforms_to_the_model() {
    family::replay::<DynamicMode, Warning, Action>(|script| {
        let (mut tools, mut context) = documents(&script.start);
        script
            .steps
            .iter()
            .map(|step| {
                let plugin = installed_as(
                    &step.plugin.coordinate,
                    &step.plugin.digest,
                    step.plugin.runs_jobs,
                    step.plugin.owner_pack.as_deref(),
                );
                let reviewed: Option<PluginJobSettings> = step.reviewed.as_ref().map(recode);
                let sources: Vec<(String, Vec<String>, InstalledVia)> = step
                    .sources
                    .iter()
                    .map(|source| {
                        let via = if source.operator_installed {
                            InstalledVia::Operator
                        } else {
                            InstalledVia::SelfConfig
                        };
                        (source.pack.clone(), source.targets.clone(), via)
                    })
                    .collect();
                let (after, after_context, report) = match step.action {
                    Action::Wire => wire_plugin(
                        &tools,
                        &context,
                        &plugin,
                        reviewed.as_ref(),
                        &step.skills,
                        &sources,
                    )
                    .map(|(mut tools, context, mut report)| {
                        if step.consent_tool_calls {
                            consent_tool_calls(&mut tools, &step.plugin.coordinate, &mut report);
                        }
                        if step.consent_dynamic {
                            consent_dynamic_run(&mut tools, &step.plugin.coordinate, &mut report);
                        }
                        (tools, context, report)
                    }),
                    Action::Unwire => {
                        unwire_plugin(&tools, &context, &plugin, &step.skills, &sources)
                    }
                }
                .ok()?;
                assert_eq!(
                    outside_wiring(&after, &after_context),
                    outside_wiring(&tools, &context),
                    "{}",
                    script.name
                );
                tools = after;
                context = after_context;
                Some(family::Outcome {
                    surface: surface(&tools, &context),
                    job: report.job.as_ref().map(recode),
                    warnings: report.warnings.iter().map(|text| warning(text)).collect(),
                    next_call: report.next_call.is_some(),
                })
            })
            .collect()
    });
}

#[test]
fn the_report_names_the_next_call_and_what_check_refuses() {
    let (tools, context) = start(None);
    let (_, _, report) =
        wire_plugin(&tools, &context, &installed(true, None), None, &[], &[]).unwrap();
    assert_eq!(
        report.next_call.as_deref(),
        Some("gents config pack wire acme/runner --agent <agent> --targets-from <pack>")
    );

    let content = (
        "acme/content".to_owned(),
        strs(&["wf-a"]),
        InstalledVia::Operator,
    );
    let plugin = installed(true, Some("acme/engine"));
    let (tools, context, report) = wire_plugin(
        &tools,
        &context,
        &plugin,
        None,
        &[],
        std::slice::from_ref(&content),
    )
    .unwrap();
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

    let (mut consented, mut consented_report) = (tools.clone(), report);
    consent_dynamic_run(&mut consented, "acme/runner", &mut consented_report);
    assert!(consented_report
        .changed
        .iter()
        .any(|change| change.contains("set job.policy.dynamic run for acme/runner")));

    let (_, _, report) = unwire_plugin(&tools, &context, &plugin, &[], &[content]).unwrap();
    assert!(report.next_call.unwrap().contains("non-job verbs only"));
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

fn policy_block(mode: DynamicMode, targets: &[&str]) -> PluginJobSettings {
    PluginJobSettings {
        agent_targets: strs(&["a", "b", "c"]),
        policy: gents_protocol::job::JobPolicy {
            dynamic: mode,
            dynamic_targets: strs(targets),
            ..Default::default()
        },
        max_live: PluginJobSettings::DEFAULT_MAX_LIVE,
        max_spawns: PluginJobSettings::DEFAULT_MAX_SPAWNS,
        max_messages_per_slot: PluginJobSettings::DEFAULT_MAX_MESSAGES_PER_SLOT,
        result_bytes: PluginJobSettings::DEFAULT_RESULT_BYTES,
        max_wall_secs: PluginJobSettings::DEFAULT_MAX_WALL_SECS,
    }
}

#[test]
fn rewire_preserves_exact_operator_policy_and_keeps_other_merge_rules() {
    let mut existing = policy_block(DynamicMode::Run, &["c", "a", "b"]);
    existing.max_live = 2;
    existing.policy.trusted_packs = strs(&["operator/kept"]);
    let mut incoming = policy_block(DynamicMode::Check, &["a"]);
    incoming.max_spawns = 7;
    incoming.policy.trusted_packs = strs(&["pack/added"]);
    let merged = merge_job_blocks(&existing, &incoming).unwrap();
    assert_eq!(merged.policy.dynamic, existing.policy.dynamic);
    assert_eq!(
        merged.policy.dynamic_targets,
        existing.policy.dynamic_targets
    );
    assert_eq!(
        merged.policy.trusted_packs,
        strs(&["operator/kept", "pack/added"])
    );
    assert_eq!(merged.max_live, 2);
    assert_eq!(merged.max_spawns, 7);
    assert_eq!(merge_job_blocks(&merged, &incoming).unwrap(), merged);
}

#[test]
fn rewire_refuses_insufficient_mode_or_targets_with_explicit_edit_remedy() {
    for (existing_mode, incoming_mode, existing_targets, incoming_targets, missing) in [
        (
            DynamicMode::Off,
            DynamicMode::Check,
            vec!["a"],
            vec!["a"],
            false,
        ),
        (
            DynamicMode::Check,
            DynamicMode::Run,
            vec!["a"],
            vec!["a"],
            false,
        ),
        (
            DynamicMode::Run,
            DynamicMode::Run,
            vec!["a"],
            vec!["a", "b"],
            true,
        ),
    ] {
        let existing = policy_block(existing_mode, &existing_targets);
        let incoming = policy_block(incoming_mode, &incoming_targets);
        let (tools, context) = start(Some(false));
        let (mut tools, context, _) = wire_plugin(
            &tools,
            &context,
            &installed(true, None),
            Some(&existing),
            &[],
            &[],
        )
        .unwrap();
        tools.tools_id = "operator-tools".to_owned();
        let before = (tools.clone(), context.clone());
        let error = wire_plugin(
            &tools,
            &context,
            &installed(true, None),
            Some(&incoming),
            &strs(&["new-skill"]),
            &[],
        )
        .unwrap_err();
        assert_eq!((tools, context), before);
        assert!(
            matches!(
                merge_job_blocks(&existing, &incoming).err().as_ref(),
                Some(DynamicPolicyConflict::Targets { .. })
            ) == missing
        );
        let diagnostic = format!("{error:#}");
        assert!(
            diagnostic.contains("config tools show 'operator-tools'"),
            "{diagnostic}"
        );
        assert!(
            diagnostic.contains("config tools set --file tools.json"),
            "{diagnostic}"
        );
        assert!(
            diagnostic.contains("existing workflow policy is unchanged"),
            "{diagnostic}"
        );
        assert!(
            !diagnostic.contains("--enable-dynamic-jobs"),
            "{diagnostic}"
        );
        assert!(!diagnostic.contains("--allow-drop"), "{diagnostic}");
    }
}
