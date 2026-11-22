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
    Enable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Warning {
    TrustedPackNotOperator,
    DynamicCheck,
    AgentNewWidened,
    NotWired,
    DynamicModeKept,
    DynamicTargetsKept,
    DynamicRunRepinned,
}

fn policy_warning(code: PolicyWarningCode) -> Warning {
    match code {
        PolicyWarningCode::DynamicModeKept => Warning::DynamicModeKept,
        PolicyWarningCode::DynamicTargetsKept => Warning::DynamicTargetsKept,
        PolicyWarningCode::DynamicRunRepinned => Warning::DynamicRunRepinned,
    }
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
                        (tools, context, report)
                    }),
                    Action::Unwire => {
                        unwire_plugin(&tools, &context, &plugin, &step.skills, &sources)
                    }
                    Action::Enable => operator_enable_dynamic(
                        &tools,
                        &step.plugin.coordinate,
                        &step.enable_targets,
                    )
                    .map(|(tools, receipt)| {
                        let job = tools
                            .integrations
                            .iter()
                            .flat_map(|integrations| integrations.plugins.iter().flatten())
                            .find(|entry| entry.plugin == receipt.plugin)
                            .and_then(|entry| entry.job.clone());
                        assert!(job.as_ref().is_some_and(dynamic_enabled), "{}", script.name);
                        let report = WireReport {
                            changed: Vec::new(),
                            job,
                            warnings: Vec::new(),
                            policy_warnings: Vec::new(),
                            next_call: None,
                        };
                        (tools, context.clone(), report)
                    })
                    .map_err(anyhow::Error::from),
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
                    warnings: report
                        .warnings
                        .iter()
                        .map(|text| warning(text))
                        .chain(
                            report
                                .policy_warnings
                                .iter()
                                .map(|kept| policy_warning(kept.code)),
                        )
                        .collect(),
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

    let (_, receipt) = operator_enable_dynamic(&tools, "acme/runner", &strs(&["wf-a"])).unwrap();
    assert_eq!(
        (receipt.before.dynamic, receipt.after.dynamic),
        (DynamicMode::Check, DynamicMode::Run)
    );

    let (_, _, report) = unwire_plugin(&tools, &context, &plugin, &[], &[content]).unwrap();
    assert!(report.next_call.unwrap().contains("non-job verbs only"));
}

#[test]
fn a_kept_policy_warning_names_what_was_asked_and_each_surfaces_own_calls() {
    let (mut tools, context) = start(None);
    tools.tools_id = "engineer-tools".to_owned();
    let operator = PluginJobSettings {
        agent_targets: strs(&["a", "b"]),
        policy: gents_protocol::job::JobPolicy {
            dynamic: DynamicMode::Run,
            dynamic_targets: strs(&["b", "a"]),
            ..Default::default()
        },
        ..pack_block(&["a", "b"])
    };
    let plugin = installed(true, None);
    let (tools, context, _) =
        wire_plugin(&tools, &context, &plugin, Some(&operator), &[], &[]).unwrap();
    let (_, _, report) = wire_plugin(
        &tools,
        &context,
        &plugin,
        Some(&pack_block(&["a"])),
        &[],
        &[],
    )
    .unwrap();
    let [warning] = report.policy_warnings.as_slice() else {
        panic!("one kept-mode warning: {:?}", report.policy_warnings);
    };
    assert_eq!(warning.code, PolicyWarningCode::DynamicModeKept);
    assert_eq!(warning.tools_id, "engineer-tools");
    assert_eq!(
        warning.requested.as_ref().map(|policy| policy.dynamic),
        Some(DynamicMode::Check)
    );
    assert_eq!(warning.kept.dynamic, DynamicMode::Run);
    assert_eq!(warning.kept.dynamic_targets, strs(&["b", "a"]));
    assert!(
        warning
            .remedy
            .contains("gents config tools show engineer-tools")
            && warning.remedy.contains("gents config tools set --file"),
        "{}",
        warning.remedy
    );
    let tool = warning
        .clone()
        .rendered_for(RemedySurface::ConfigTool)
        .remedy;
    assert!(
        tool.contains(r#"config {"argv":["tools","get"],"target_id":"engineer-tools"}"#)
            && tool.contains(r#"["tools","edit"]"#)
            && !tool.contains("gents config"),
        "{tool}"
    );
}

fn pack_block(targets: &[&str]) -> PluginJobSettings {
    PluginJobSettings {
        agent_targets: strs(targets),
        max_live: PluginJobSettings::DEFAULT_MAX_LIVE,
        max_spawns: PluginJobSettings::DEFAULT_MAX_SPAWNS,
        max_messages_per_slot: PluginJobSettings::DEFAULT_MAX_MESSAGES_PER_SLOT,
        result_bytes: PluginJobSettings::DEFAULT_RESULT_BYTES,
        max_wall_secs: PluginJobSettings::DEFAULT_MAX_WALL_SECS,
        policy: gents_protocol::job::JobPolicy {
            dynamic: DynamicMode::Check,
            dynamic_targets: strs(targets),
            ..Default::default()
        },
    }
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
