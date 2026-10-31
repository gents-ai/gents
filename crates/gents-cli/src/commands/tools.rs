use rapidhash::RapidHashSet;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::ValueEnum;
use gents::tools::surface::{measured_mcp_services_for_access, RuntimeToolAvailability};
use gents::{AgentToolSurfaceConfig, ToolCeiling};
use serde::Serialize;
use serde_json::{json, Value};

use crate::cli::args::{ToolCeilingArg, ToolExplainArgs, ToolsCommand};
use crate::commands::status::fetch_runtime_status;
use crate::config_writes::ConfigAccess;
use crate::{
    build_config_export_bundle, format_tool_ceiling, host_tool_ceiling, parse_cli_tool_arg,
    print_json, read_init_config, resolve_config_access, resolve_node_did,
};

pub(crate) async fn dispatch(command: ToolsCommand) -> Result<()> {
    match command {
        ToolsCommand::Explain(args) => explain(args).await,
    }
}

async fn explain(args: ToolExplainArgs) -> Result<()> {
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let host = match &access {
        ConfigAccess::Graphql(endpoint) => {
            let status = fetch_runtime_status(endpoint, Duration::from_secs(10))
                .await
                .context(
                    "the GraphQL endpoint has no gents runtime status to read its tool ceiling",
                )?;
            let live = status
                .get("node_did")
                .and_then(Value::as_str)
                .context("runtime status has no node_did")?;
            if args.graphql.is_none() && args.node_did.is_none() {
                if let Some(expected) = read_init_config(&home_dir)?.map(|config| config.node_did) {
                    anyhow::ensure!(
                        expected == live,
                        "home {} belongs to node {expected}, but the runtime its runtime.json names \
                         serves node {live}; restart `gents server --home {}` to serve this home, or \
                         pass --graphql to explain that node",
                        home_dir.display(),
                        shell_quoted_path(&home_dir)?
                    );
                }
            }
            if let Some(explicit) = args.node_did.as_deref() {
                anyhow::ensure!(
                    explicit == live,
                    "the runtime at this endpoint serves node {live}, so its tool ceiling does not \
                     apply to --node-did {explicit}; omit --node-did or point --graphql at that node"
                );
            }
            HostCeiling::from_status(&status)?
        }
        ConfigAccess::Local(_) => HostCeiling::configured(&home_dir)?,
    };
    let node_did = match (args.node_did.as_deref(), host.live_node_did.as_deref()) {
        (None, Some(live)) => live.to_owned(),
        (explicit, _) => resolve_node_did(args.home.as_deref(), explicit)?,
    };
    let tool_ceiling = &host.ceiling;
    let bundle = build_config_export_bundle(&access, &node_did).await?;
    let available_services = measured_mcp_services_for_access(
        &access,
        &node_did,
        &bundle.config.tool_service_registries,
    )
    .await?;
    let availability =
        RuntimeToolAvailability::from_online_mcp_services(available_services.clone());
    let enabled_agent_ids = bundle
        .config
        .agents
        .iter()
        .filter(|agent| agent.enabled)
        .map(|agent| agent.agent_id.clone())
        .collect::<BTreeSet<_>>();
    let enabled_agent_id_set = enabled_agent_ids
        .iter()
        .cloned()
        .collect::<RapidHashSet<_>>();
    let mut configuration_issues = BTreeMap::new();
    let mut agents = Vec::new();
    for agent in &bundle.config.agents {
        if args
            .agent_id
            .as_deref()
            .is_some_and(|id| agent.agent_id != id)
        {
            continue;
        }
        if !agent.enabled {
            configuration_issues.insert(agent.agent_id.clone(), "agent is disabled".to_owned());
        }
        let resolved = (|| -> Result<_> {
            let (context, tools) = agent_tools_document(&bundle, &node_did, agent)?;
            let tools_id = context.and_then(|context| context.tools_id.as_deref());
            let config = if let Some(tools) = tools {
                AgentToolSurfaceConfig::from_tools_documents(
                    &agent.agent_id,
                    tools,
                    &bundle.config.datastore_tool_surfaces,
                    &bundle.config.agent_targets,
                    tool_ceiling,
                    Vec::new(),
                )?
            } else {
                AgentToolSurfaceConfig::from_tools_documents(
                    &agent.agent_id,
                    &gents::documents::Tools::default(),
                    &[],
                    &[],
                    tool_ceiling,
                    Vec::new(),
                )?
            };
            Ok((tools_id, config))
        })();
        let (tools_id, config) = match resolved {
            Ok(value) => value,
            Err(error) => {
                configuration_issues.insert(agent.agent_id.clone(), format!("{error:#}"));
                continue;
            }
        };
        let explanation = config.explain_with_runtime_availability(
            availability.clone(),
            &node_did,
            &enabled_agent_id_set,
        );
        agents.push(json!({
            "agent_id": agent.agent_id,
            "display_name": agent.display_name,
            "enabled": agent.enabled,
            "context_id": agent.context_id,
            "tools_id": tools_id,
            "tools_source": if tools_id.is_some() { "document" } else { "default_no_tools_binding" },
            "surface": explanation,
        }));
    }

    if let Some(only_agent_id) = args.agent_id.as_deref() {
        let found = agents.iter().any(|row| {
            row.get("agent_id")
                .and_then(Value::as_str)
                .is_some_and(|value| value == only_agent_id)
        }) || configuration_issues.contains_key(only_agent_id);
        if !found {
            anyhow::bail!("agent {only_agent_id} was not found for node {node_did}");
        }
    }

    let home_hint = args.home.as_deref().map(shell_quoted_path).transpose()?;
    let output = ExplainReport {
        node_did: &node_did,
        host_tool_ceiling: HostCeilingReport {
            tool_ceiling: format_tool_ceiling(host.mode),
            tool_root: host.root.as_deref(),
            cli_tools: host
                .ceiling
                .cli_tools()
                .iter()
                .map(|tool| tool.name.as_str())
                .collect(),
            live: host.live_node_did.is_some(),
        },
        agents,
        configuration_issues,
        runtime_availability: json!({
            "measured_available_mcp_service_ids": available_services,
            "local_target_eligibility": {
                "source": "enabled_agent_configuration",
                "enabled_agent_ids": enabled_agent_ids.iter().cloned().collect::<Vec<_>>(),
                "runtime_readiness_verified": false
            },
        }),
        operator_surfaces: json!({
            "included_in_model_tool_surface": false,
            "note": "Server HTTP routes and optional external /mcp are binary/operator surfaces; they are not included in per-agent model-callable tool_names."
        }),
        next: host.live_node_did.is_none().then(|| {
            let server = match &home_hint {
                Some(home) => format!("gents server --home {home}"),
                None => "gents server".to_owned(),
            };
            format!(
                "No runtime serves this home, so this is the configured init.json ceiling; a \
                 server started with --tool-ceiling, --tool-root or --cli-tool enforces its \
                 own. Start `{server}` and rerun for the live ceiling."
            )
        }),
        ceiling_source: host.source,
        agent_roots: host.live_node_did.is_some().then_some(
            "Agent Tools roots are admitted under tool_root on this CLI host's filesystem; \
             explained from another host, a root the server admits can appear under \
             configuration_issues.",
        ),
        ceiling_scope: "host_native_file_bash_cli_only",
        ceiling_note: "This ceiling currently clamps host-native file/bash/CLI tools, not every \
                       model-callable built-in read, MCP, agent, or operator HTTP surface.",
        access_mode: access.mode(),
        home: host
            .home
            .clone()
            .unwrap_or_else(|| home_dir.display().to_string()),
    };
    print_json(&output)?;
    Ok(())
}

/// The agent's Context, then that Context's Tools document, each when named.
/// Plugin entries, manifests and targets are not resolved here.
pub(crate) fn agent_tools_document<'a>(
    bundle: &'a crate::shared::ConfigExportBundle,
    node_did: &str,
    agent: &gents::documents::Agent,
) -> Result<(
    Option<&'a gents::documents::AgentContext>,
    Option<&'a gents::documents::Tools>,
)> {
    let context = agent
        .context_id
        .as_deref()
        .map(|id| {
            bundle
                .config
                .contexts
                .iter()
                .find(|context| context.node_did == node_did && context.context_id == id)
                .with_context(|| format!("referenced AgentContext {id} is missing"))
        })
        .transpose()?;
    let tools = context
        .and_then(|context| context.tools_id.as_deref())
        .map(|id| {
            bundle
                .config
                .tools
                .iter()
                .find(|tools| tools.node_did == node_did && tools.tools_id == id)
                .with_context(|| format!("referenced Tools {id} is missing"))
        })
        .transpose()?;
    Ok((context, tools))
}

#[derive(Serialize)]
struct ExplainReport<'a> {
    node_did: &'a str,
    host_tool_ceiling: HostCeilingReport<'a>,
    agents: Vec<Value>,
    configuration_issues: BTreeMap<String, String>,
    runtime_availability: Value,
    operator_surfaces: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<String>,
    ceiling_source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_roots: Option<&'static str>,
    ceiling_scope: &'static str,
    ceiling_note: &'static str,
    access_mode: &'static str,
    home: String,
}

#[derive(Serialize)]
struct HostCeilingReport<'a> {
    tool_ceiling: &'static str,
    tool_root: Option<&'a str>,
    cli_tools: Vec<&'a str>,
    live: bool,
}

fn shell_quoted_path(path: &Path) -> Result<String> {
    let text = path
        .to_str()
        .with_context(|| format!("home {} is not UTF-8", path.display()))?;
    Ok(shlex::try_quote(text)
        .with_context(|| format!("home {text} cannot be shell-quoted"))?
        .into_owned())
}

struct HostCeiling {
    mode: ToolCeilingArg,
    root: Option<String>,
    ceiling: ToolCeiling,
    source: &'static str,
    live_node_did: Option<String>,
    home: Option<String>,
}

impl HostCeiling {
    fn from_status(status: &Value) -> Result<Self> {
        let field = |name: &str| status.get(name).and_then(Value::as_str).map(str::to_owned);
        let mode_text = field("tool_ceiling").context("runtime status has no tool_ceiling")?;
        let mode = ToolCeilingArg::from_str(&mode_text, false)
            .map_err(|error| anyhow::anyhow!("runtime tool_ceiling {mode_text}: {error}"))?;
        let root = field("tool_root");
        if let (Some(error), false) = (field("tool_root_error"), mode == ToolCeilingArg::MetaOnly) {
            anyhow::bail!(
                "the runtime's {mode_text} ceiling has no usable root ({error}), so it builds no \
                 host tools to explain; fix --tool-root (or tool_root in init.json) and restart \
                 the server"
            );
        }
        let mut ceiling = host_tool_ceiling(mode, root.as_deref().map(Path::new))?;
        for registration in status
            .get("cli_tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            ceiling = ceiling.with_cli_tool(parse_cli_tool_arg(registration)?);
        }
        Ok(Self {
            ceiling,
            mode,
            root,
            source: "runtime_status",
            live_node_did: Some(field("node_did").context("runtime status has no node_did")?),
            home: field("home"),
        })
    }

    fn configured(home_dir: &Path) -> Result<Self> {
        let Some(config) = read_init_config(home_dir)? else {
            return Ok(Self {
                mode: ToolCeilingArg::MetaOnly,
                root: None,
                ceiling: ToolCeiling::meta_only(),
                source: "default_no_init_json",
                live_node_did: None,
                home: None,
            });
        };
        Ok(Self {
            ceiling: host_tool_ceiling(
                config.tool_ceiling,
                config.tool_root.as_deref().map(Path::new),
            )
            .context("init.json tool ceiling")?,
            mode: config.tool_ceiling,
            root: config.tool_root,
            source: "init_json",
            live_node_did: None,
            home: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::ConfigExportBundle;

    fn bundle(node_did: &str) -> ConfigExportBundle {
        ConfigExportBundle {
            format: "test".into(),
            node_did: node_did.into(),
            exported_at: "2026-01-01T00:00:00Z".into(),
            access_mode: "test".into(),
            config: serde_json::from_value(json!({
                "node": {"node_did": node_did},
                "agents": [
                    agent_json(node_did, "with-tools", Some("ctx-tools")),
                    agent_json(node_did, "no-context", None),
                    agent_json(node_did, "missing-tools", Some("ctx-missing")),
                ],
                "contexts": [
                    {"context_id": "ctx-tools", "node_did": node_did, "tools_id": "tools-a"},
                    {"context_id": "ctx-missing", "node_did": node_did, "tools_id": "gone"},
                ],
                "tools": [{"node_did": node_did, "tools_id": "tools-a"}],
            }))
            .unwrap(),
        }
    }

    fn agent_json(node_did: &str, agent_id: &str, context_id: Option<&str>) -> Value {
        json!({
            "agent_id": agent_id,
            "node_did": node_did,
            "context_id": context_id,
            "inference_profile_id": "profile",
        })
    }

    #[test]
    fn agent_tools_document_follows_context_to_tools() {
        let bundle = bundle("did:key:node");
        let agent = |id: &str| {
            bundle
                .config
                .agents
                .iter()
                .find(|agent| agent.agent_id == id)
                .unwrap()
        };

        let (context, tools) =
            agent_tools_document(&bundle, "did:key:node", agent("with-tools")).unwrap();
        assert_eq!(context.unwrap().context_id, "ctx-tools");
        assert_eq!(tools.unwrap().tools_id, "tools-a");

        let (context, tools) =
            agent_tools_document(&bundle, "did:key:node", agent("no-context")).unwrap();
        assert!(context.is_none() && tools.is_none());

        let error =
            agent_tools_document(&bundle, "did:key:node", agent("missing-tools")).unwrap_err();
        assert_eq!(error.to_string(), "referenced Tools gone is missing");
    }
}
