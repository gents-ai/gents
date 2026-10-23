use std::path::Path;

use gents::document_config::{Agent, PluginToolRef, Tools};
use gents::pack::home_install::recorded_provenance;
use gents::pack::{read_home_install, InstalledVia, PackKind};
use gents::plugin::store::{parse_coordinate, read_record};
use gents::tool_surface::{BashMode, FileToolMode};
use gents_protocol::job::PluginJobSettings;
use serde::Serialize;
use serde_json::{json, Value};

use crate::commands::tools::agent_tools_document;
use crate::shared::ConfigExportBundle;

#[derive(Serialize)]
struct Row {
    check: &'static str,
    severity: &'static str,
    subject: String,
    next_call: String,
}

fn row(check: &'static str, severity: &'static str, subject: String, next_call: String) -> Row {
    Row {
        check,
        severity,
        subject,
        next_call,
    }
}

/// `checks.plugin_jobs`: per agent, whether each wired job plugin can run its
/// jobs. Failures set `ok: false`; warnings do not.
pub(super) fn diagnose_plugin_jobs(home: &Path, bundle: Option<&ConfigExportBundle>) -> Value {
    let agents: Vec<(&str, Vec<Row>)> = bundle
        .into_iter()
        .flat_map(|bundle| {
            bundle
                .config
                .agents
                .iter()
                .map(|agent| (agent.agent_id.as_str(), agent_rows(home, bundle, agent)))
        })
        .collect();
    let ok = agents
        .iter()
        .all(|(_, rows)| rows.iter().all(|row| row.severity != "fail"));
    json!({
        "ok": ok,
        "agents": agents
            .iter()
            .map(|(agent_id, rows)| json!({"agent_id": agent_id, "checks": rows}))
            .collect::<Vec<_>>(),
    })
}

fn agent_rows(home: &Path, bundle: &ConfigExportBundle, agent: &Agent) -> Vec<Row> {
    let mut rows = Vec::new();
    match agent_tools_document(bundle, &agent.node_did, agent) {
        Ok((_, Some(tools))) => {
            let plugins = tools
                .integrations
                .iter()
                .flat_map(|integrations| integrations.plugins.iter().flatten());
            for entry in plugins {
                plugin_rows(home, bundle, agent, tools, entry, &mut rows);
            }
        }
        Ok(_) => {}
        Err(error) => rows.push(row(
            "tools_unreadable",
            "fail",
            error.to_string(),
            format!("gents tools explain --agent {}", agent.agent_id),
        )),
    }
    rows
}

fn plugin_rows(
    home: &Path,
    bundle: &ConfigExportBundle,
    agent: &Agent,
    tools: &Tools,
    entry: &PluginToolRef,
    rows: &mut Vec<Row>,
) {
    let plugin = entry.plugin.as_str();
    let record = match parse_coordinate(plugin)
        .and_then(|(namespace, name)| read_record(home, namespace, name))
    {
        Ok(record) => record,
        Err(error) => {
            rows.push(row(
                "plugin_record_unreadable",
                "fail",
                format!("{plugin}: {error:#}"),
                format!("gents plugin install {plugin}"),
            ));
            return;
        }
    };
    let owner = record.owner_pack_coordinate.as_deref();
    let wire = format!("gents pack wire {plugin} --agent {}", agent.agent_id);
    let Some(block) = &entry.job else {
        if record.declaration.job.0 {
            rows.push(row(
                "job_block_missing",
                "fail",
                format!("{plugin} declares job steps but has no job block, so it serves non-job verbs only"),
                format!("{wire} --targets-from {}", owner.unwrap_or("<pack>")),
            ));
        }
        return;
    };

    let caller_targets = tools
        .agents
        .as_ref()
        .map_or(&[][..], |agents| agents.target_ids.as_slice());
    for id in &block.agent_targets {
        let Some(target) = find_target(bundle, agent, id) else {
            rows.push(row(
                "job_target_unavailable",
                "fail",
                format!("{plugin}: job target {id} is not an AgentTarget"),
                wire.clone(),
            ));
            continue;
        };
        let destination = bundle
            .config
            .agents
            .iter()
            .find(|a| a.node_did == target.target_node_did && a.agent_id == target.agent_id);
        let problem = if target.target_node_did == target.node_did && destination.is_none() {
            Some("names an agent that is not configured")
        } else if destination.is_some_and(|a| !a.enabled) {
            Some("names a disabled agent")
        } else if !caller_targets.contains(id) {
            Some("is not in the agent's AgentTools target list")
        } else {
            None
        };
        if let Some(problem) = problem {
            rows.push(row(
                "job_target_unavailable",
                "fail",
                format!("{plugin}: job target {id} {problem}"),
                wire.clone(),
            ));
        }
        if let Some(destination) = destination {
            inference_rows(bundle, destination, plugin, block, rows);
        }
    }

    // `engine_guard` refuses a plugin with no owning pack, so this fails too.
    let engine_reason = match owner {
        Some(owner) => not_operator(home, owner),
        None => Some("it has no owning pack, so its jobs are refused".to_owned()),
    };
    if let Some(reason) = engine_reason {
        let engine = owner.map_or_else(
            || format!("{}/{}", record.namespace, record.name),
            str::to_owned,
        );
        rows.push(row(
            "engine_not_operator_installed",
            "fail",
            format!("{plugin}: {reason}"),
            format!("gents pack install {engine} --grant-authority"),
        ));
    }
    for pack in &block.policy.trusted_packs {
        if let Some(reason) = not_operator(home, pack) {
            rows.push(row(
                "trusted_pack_not_operator",
                "warn",
                format!("{plugin}: trusted pack {reason}, so its scripts run as dynamic"),
                format!("gents pack install {pack}"),
            ));
        }
    }
    for id in &block.policy.dynamic_targets {
        let tools =
            find_target(bundle, agent, id)
                .and_then(|target| {
                    bundle.config.agents.iter().find(|a| {
                        a.node_did == target.target_node_did && a.agent_id == target.agent_id
                    })
                })
                .and_then(|a| agent_tools_document(bundle, &a.node_did, a).ok())
                .and_then(|(_, tools)| tools);
        if tools.is_some_and(reads_files_and_reaches_network) {
            rows.push(row(
                "dynamic_target_exfiltration",
                "warn",
                format!("{plugin}: dynamic target {id} can read files and reach the network"),
                "narrow job.policy.dynamic_targets to wf-read-only".into(),
            ));
        }
    }
    let widened: Vec<&str> = block
        .agent_targets
        .iter()
        .filter(|id| caller_targets.contains(id))
        .map(String::as_str)
        .collect();
    if tools
        .agents
        .as_ref()
        .is_some_and(|a| a.enabled == Some(true))
        && !widened.is_empty()
    {
        rows.push(row(
            "agent_new_widened",
            "warn",
            format!(
                "{plugin}: {} are also reachable by agent_new",
                widened.join(", ")
            ),
            format!("gents tools explain --agent {}", agent.agent_id),
        ));
    }
}

fn find_target<'a>(
    bundle: &'a ConfigExportBundle,
    agent: &Agent,
    id: &str,
) -> Option<&'a gents::document_config::AgentTargetDocument> {
    bundle
        .config
        .agent_targets
        .iter()
        .find(|t| t.node_did == agent.node_did && t.target_id == id)
}

/// A target agent's inference profile and backend, and the backend's capacity
/// against the block's `max_live`.
fn inference_rows(
    bundle: &ConfigExportBundle,
    destination: &Agent,
    plugin: &str,
    block: &PluginJobSettings,
    rows: &mut Vec<Row>,
) {
    let config = &bundle.config;
    let profile = config.inference_profiles.iter().find(|p| {
        p.node_did == destination.node_did && p.profile_id == destination.inference_profile_id
    });
    let backend = profile.and_then(|p| {
        config
            .inference_backends
            .iter()
            .find(|b| b.node_did == p.node_did && b.backend_id == p.backend_id)
    });
    let unavailable = match (profile, backend) {
        (None, _) => Some(format!(
            "inference profile {} is missing",
            destination.inference_profile_id
        )),
        (Some(profile), None) => Some(format!("backend {} is missing", profile.backend_id)),
        (_, Some(backend)) if !backend.enabled => {
            Some(format!("backend {} is disabled", backend.backend_id))
        }
        _ => None,
    };
    if let Some(problem) = unavailable {
        rows.push(row(
            "persona_inference_unavailable",
            "fail",
            format!(
                "{plugin}: target agent {} has no usable inference: {problem}",
                destination.agent_id
            ),
            "gents config profile / gents config backend".into(),
        ));
    } else if let Some(backend) = backend {
        let capacity = backend.max_concurrent.unwrap_or(1);
        if capacity < i64::from(block.max_live) {
            rows.push(row(
                "backend_concurrency",
                "warn",
                format!(
                    "{plugin}: backend {} allows {capacity} concurrent calls, below max_live {}",
                    backend.backend_id, block.max_live
                ),
                format!(
                    "gents config backend set --backend-id {} --max-concurrent {}",
                    backend.backend_id, block.max_live
                ),
            ));
        }
    }
}

/// C5.4: a record-less Documents-kind pack (a dependency) reads `SelfConfig`.
fn not_operator(home: &Path, coordinate: &str) -> Option<String> {
    let provenance = read_home_install(home, coordinate).map(|record| {
        recorded_provenance(
            record
                .as_ref()
                .map_or(PackKind::Documents, |record| record.kind.clone()),
            record.map(|record| record.installed_via),
        )
    });
    match provenance {
        Ok(InstalledVia::Operator) => None,
        Ok(InstalledVia::SelfConfig) => Some(format!("{coordinate} is not operator-installed")),
        Err(error) => Some(format!(
            "{coordinate} has an unreadable install record: {error}"
        )),
    }
}

fn reads_files_and_reaches_network(tools: &Tools) -> bool {
    let host = tools.host.as_ref();
    let reads = host.is_some_and(|host| {
        host.files
            .as_ref()
            .is_some_and(|f| f.mode != FileToolMode::Off)
            || host.bash.as_ref().is_some_and(|b| b.mode != BashMode::Off)
    });
    let network = tools
        .remote
        .as_ref()
        .is_some_and(|remote| remote.services.iter().any(|s| !s.tool_names.is_empty()));
    reads && network
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::document_config::AgentContext;
    use gents::plugin::store::write_record;

    const NODE: &str = "did:key:node";

    fn world() -> Value {
        json!({
            "node": {"node_did": NODE},
            "agents": [
                {"agent_id": "caller", "node_did": NODE, "context_id": "ctx", "inference_profile_id": "p"},
                {"agent_id": "worker", "node_did": NODE, "inference_profile_id": "p"},
                {"agent_id": "web", "node_did": NODE, "context_id": "ctx-web", "inference_profile_id": "p"},
            ],
            "contexts": [
                {"context_id": "ctx", "node_did": NODE, "tools_id": "tools"},
                {"context_id": "ctx-web", "node_did": NODE, "tools_id": "tools-web"},
            ],
            "tools": [
                {
                    "tools_id": "tools", "node_did": NODE,
                    "agents": {"target_ids": ["wf-read-only"]},
                    "integrations": {"plugins": [{
                        "plugin": "acme/runner", "digest": "sha256:aa",
                        "job": {
                            "agent_targets": ["wf-read-only"], "max_live": 2,
                            "policy": {
                                "dynamic": "check",
                                "dynamic_targets": ["wf-read-only"],
                                "trusted_packs": ["acme/engine"],
                            },
                        },
                    }]},
                },
                {
                    "tools_id": "tools-web", "node_did": NODE,
                    "host": {"files": {"mode": "ReadOnly"}},
                    "remote": {"services": [{
                        "mcp_service_id": "web", "tool_names": ["web_collect_evidence"],
                    }]},
                },
            ],
            "agent_targets": [
                {"target_id": "wf-read-only", "node_did": NODE, "target_node_did": NODE,
                 "agent_id": "worker", "name": "wf-read-only"},
                {"target_id": "wf-read-only-web", "node_did": NODE, "target_node_did": NODE,
                 "agent_id": "web", "name": "wf-read-only-web"},
            ],
            "inference_backends": [{
                "backend_id": "b", "node_did": NODE, "name": "b",
                "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:1/v1",
                "auth": {"kind": "unauthenticated"}, "max_concurrent": 4,
            }],
            "inference_profiles": [{
                "profile_id": "p", "node_did": NODE, "backend_id": "b", "model_name": "m",
            }],
        })
    }

    fn plugin_record(job: bool) -> gents::plugin::store::InstalledPlugin {
        serde_json::from_value(json!({
            "namespace": "acme", "name": "runner", "version": "1.0.0",
            "digest": "sha256:aa", "language": "rust",
            "declaration": {
                "name": "runner", "description": "Runs", "artifact": "plugins/runner.afb",
                "language": "rust", "input_schema": {"type": "object"}, "job": job
            },
            "owner_pack_coordinate": "acme/engine",
        }))
        .unwrap()
    }

    fn install(home: &Path, coordinate: &str, via: InstalledVia) {
        gents::pack::write_home_install(
            home,
            &serde_json::from_value(json!({
                "coordinate": coordinate, "version": "1.0.0", "digest": "sha256:bb",
                "kind": "plugins", "assets": "packs/.materialized/acme/engine/bb",
                "plugins": [], "installed_at": "2026-01-01T00:00:00Z",
                "installed_via": via,
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn healthy_home() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        write_record(home.path(), &plugin_record(true)).unwrap();
        install(home.path(), "acme/engine", InstalledVia::Operator);
        home
    }

    fn bundle(config: Value) -> ConfigExportBundle {
        ConfigExportBundle {
            format: "test".into(),
            node_did: NODE.into(),
            exported_at: "2026-01-01T00:00:00Z".into(),
            access_mode: "test".into(),
            config: serde_json::from_value(config).unwrap(),
        }
    }

    /// The `(check, severity)` pairs reported for `caller`, plus the whole value.
    fn run(home: &Path, config: Value) -> (Vec<(String, String)>, Value) {
        let report = diagnose_plugin_jobs(home, Some(&bundle(config)));
        let caller = report["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|agent| agent["agent_id"] == "caller")
            .unwrap();
        let rows = caller["checks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    c["check"].as_str().unwrap().to_owned(),
                    c["severity"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        (rows, report)
    }

    fn plugin_entry(config: &mut Value) -> &mut Value {
        &mut config["tools"][0]["integrations"]["plugins"][0]
    }

    fn only(rows: &[(String, String)], check: &str, severity: &str) {
        assert_eq!(rows, [(check.to_owned(), severity.to_owned())]);
    }

    #[test]
    fn a_healthy_job_agent_has_one_row_and_no_checks() {
        let home = healthy_home();
        let (rows, report) = run(home.path(), world());
        assert!(rows.is_empty(), "{report}");
        assert_eq!(report["ok"], true);
        assert_eq!(report["agents"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn a_missing_bundle_reports_nothing() {
        let report = diagnose_plugin_jobs(Path::new("/nonexistent"), None);
        assert_eq!(report, json!({"ok": true, "agents": []}));
    }

    #[test]
    fn job_block_missing_names_targets_from() {
        let home = healthy_home();
        let mut config = world();
        plugin_entry(&mut config)
            .as_object_mut()
            .unwrap()
            .remove("job");
        let (rows, report) = run(home.path(), config);
        only(&rows, "job_block_missing", "fail");
        let next_call = report["agents"][0]["checks"][0]["next_call"]
            .as_str()
            .unwrap();
        assert_eq!(
            next_call,
            "gents pack wire acme/runner --agent caller --targets-from acme/engine"
        );
    }

    #[test]
    fn job_target_unavailable_covers_missing_disabled_and_unlisted_targets() {
        let home = healthy_home();
        type Mutation = fn(&mut Value);
        let cases: [(&str, Mutation); 3] = [
            ("not an AgentTarget", |config| {
                plugin_entry(config)["job"]["agent_targets"] = json!(["wf-gone"]);
            }),
            ("disabled agent", |config| {
                config["agents"][1]["enabled"] = json!(false);
            }),
            ("target list", |config| {
                config["tools"][0]["agents"]["target_ids"] = json!([]);
            }),
        ];
        for (needle, change) in cases {
            let mut config = world();
            change(&mut config);
            // dynamic_targets are not job targets: keep the case to one check.
            plugin_entry(&mut config)["job"]["policy"]["dynamic_targets"] = json!([]);
            let (rows, report) = run(home.path(), config);
            only(&rows, "job_target_unavailable", "fail");
            let check = &report["agents"][0]["checks"][0];
            assert!(
                check["subject"].as_str().unwrap().contains(needle),
                "{check}"
            );
            assert_eq!(
                check["next_call"],
                "gents pack wire acme/runner --agent caller"
            );
        }
    }

    #[test]
    fn persona_inference_unavailable_when_the_backend_is_disabled_or_missing() {
        let home = healthy_home();
        let mut disabled = world();
        disabled["inference_backends"][0]["enabled"] = json!(false);
        only(
            &run(home.path(), disabled).0,
            "persona_inference_unavailable",
            "fail",
        );
        let mut missing = world();
        missing["inference_backends"] = json!([]);
        only(
            &run(home.path(), missing).0,
            "persona_inference_unavailable",
            "fail",
        );
        let mut no_profile = world();
        no_profile["inference_profiles"] = json!([]);
        only(
            &run(home.path(), no_profile).0,
            "persona_inference_unavailable",
            "fail",
        );
    }

    #[test]
    fn engine_not_operator_installed_fails_and_names_grant_authority() {
        let home = healthy_home();
        install(home.path(), "acme/engine", InstalledVia::SelfConfig);
        let (rows, report) = run(home.path(), world());
        assert!(rows.contains(&("engine_not_operator_installed".into(), "fail".into())));
        assert!(rows.contains(&("trusted_pack_not_operator".into(), "warn".into())));
        let check = &report["agents"][0]["checks"][0];
        assert_eq!(
            check["next_call"],
            "gents pack install acme/engine --grant-authority"
        );
        assert_eq!(report["ok"], false);
    }

    #[test]
    fn a_plugin_with_no_owning_pack_fails_the_engine_check() {
        let home = healthy_home();
        let mut record = plugin_record(true);
        record.owner_pack_coordinate = None;
        write_record(home.path(), &record).unwrap();
        let (rows, report) = run(home.path(), world());
        assert!(rows.contains(&("engine_not_operator_installed".into(), "fail".into())));
        let check = &report["agents"][0]["checks"][0];
        assert_eq!(
            check["next_call"],
            "gents pack install acme/runner --grant-authority"
        );
        assert_eq!(report["ok"], false);
    }

    #[test]
    fn a_wired_plugin_with_no_record_fails() {
        let home = tempfile::tempdir().unwrap();
        let (rows, report) = run(home.path(), world());
        only(&rows, "plugin_record_unreadable", "fail");
        assert_eq!(
            report["agents"][0]["checks"][0]["next_call"],
            "gents plugin install acme/runner"
        );
        assert_eq!(report["ok"], false);
    }

    #[test]
    fn dependency_only_trusted_pack_warns() {
        let home = healthy_home();
        let mut config = world();
        plugin_entry(&mut config)["job"]["policy"]["trusted_packs"] =
            json!(["acme/engine", "acme/dependency"]);
        let (rows, report) = run(home.path(), config);
        only(&rows, "trusted_pack_not_operator", "warn");
        let check = &report["agents"][0]["checks"][0];
        assert!(check["subject"]
            .as_str()
            .unwrap()
            .contains("acme/dependency"));
        assert_eq!(check["next_call"], "gents pack install acme/dependency");
    }

    #[test]
    fn dynamic_target_exfiltration_warns_for_a_web_reading_target() {
        let home = healthy_home();
        let mut config = world();
        let job = &mut plugin_entry(&mut config)["job"];
        job["agent_targets"] = json!(["wf-read-only", "wf-read-only-web"]);
        job["policy"]["dynamic_targets"] = json!(["wf-read-only-web"]);
        config["tools"][0]["agents"]["target_ids"] = json!(["wf-read-only", "wf-read-only-web"]);
        let (rows, report) = run(home.path(), config);
        only(&rows, "dynamic_target_exfiltration", "warn");
        assert_eq!(report["ok"], true);
        let next_call = report["agents"][0]["checks"][0]["next_call"]
            .as_str()
            .unwrap();
        assert!(next_call.contains("wf-read-only"));
    }

    #[test]
    fn backend_concurrency_warns_below_max_live() {
        let home = healthy_home();
        let mut config = world();
        config["inference_backends"][0]["max_concurrent"] = json!(1);
        let (rows, report) = run(home.path(), config);
        only(&rows, "backend_concurrency", "warn");
        assert_eq!(report["ok"], true);
        let next_call = report["agents"][0]["checks"][0]["next_call"]
            .as_str()
            .unwrap();
        assert!(next_call.ends_with("--max-concurrent 2"), "{next_call}");
    }

    #[test]
    fn agent_new_widened_warns_when_agent_new_reaches_job_targets() {
        let home = healthy_home();
        let mut config = world();
        config["tools"][0]["agents"]["enabled"] = json!(true);
        only(&run(home.path(), config).0, "agent_new_widened", "warn");
    }

    #[test]
    fn failures_degrade_status_warnings_do_not() {
        let home = healthy_home();
        let mut warn = world();
        warn["tools"][0]["agents"]["enabled"] = json!(true);
        assert_eq!(run(home.path(), warn).1["ok"], true);
        let mut fail = world();
        plugin_entry(&mut fail)
            .as_object_mut()
            .unwrap()
            .remove("job");
        assert_eq!(run(home.path(), fail).1["ok"], false);
    }

    #[test]
    fn an_unreadable_tools_document_is_a_failure_with_its_error() {
        let home = healthy_home();
        let mut config = world();
        config["contexts"][0]["tools_id"] = json!("gone");
        let (rows, report) = run(home.path(), config);
        only(&rows, "tools_unreadable", "fail");
        assert_eq!(report["ok"], false);
    }

    #[test]
    fn wired_agent_passes_job_target_unavailable() {
        let home = healthy_home();
        let plugin = read_record(home.path(), "acme", "runner").unwrap();
        let context: AgentContext = serde_json::from_value(
            json!({"context_id": "ctx", "node_did": NODE, "tools_id": "tools"}),
        )
        .unwrap();
        let empty = Tools {
            tools_id: "tools".into(),
            node_did: NODE.into(),
            ..Tools::default()
        };
        let (wired, _, report) = gents::document_config::wire_plugin(
            &empty,
            &context,
            &plugin,
            None,
            &[],
            &[(
                "acme/engine".into(),
                vec!["wf-read-only".into()],
                InstalledVia::Operator,
            )],
        )
        .unwrap();
        assert!(report.job.is_some(), "{report:?}");
        let mut config = world();
        config["tools"][0] = serde_json::to_value(&wired).unwrap();
        config["inference_backends"][0]["max_concurrent"] =
            json!(PluginJobSettings::DEFAULT_MAX_LIVE);
        let (rows, report) = run(home.path(), config);
        assert!(rows.is_empty(), "{rows:?} {report}");
    }
}
