use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::ValueEnum;
use gents::config_client::GraphqlEndpoint;
use gents::tool_surface::{measured_mcp_services_for_access, RuntimeToolAvailability};
use gents::{AgentToolSurfaceConfig, ToolCeiling};
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
    let host = match &*access {
        ConfigAccess::Graphql(endpoint) => HostCeiling::live(endpoint).await?,
        ConfigAccess::Local(_) => HostCeiling::configured(&home_dir)?,
    };
    if let (None, None, Some(live)) = (
        args.graphql.as_deref(),
        args.node_did.as_deref(),
        host.live_node_did.as_deref(),
    ) {
        if let Some(expected) = read_init_config(&home_dir)?.map(|config| config.node_did) {
            anyhow::ensure!(
                expected == live,
                "home {} belongs to node {expected}, but the runtime its runtime.json names \
                 serves node {live}; restart `gents server --home {}` to serve this home, or \
                 pass --graphql to explain that node",
                home_dir.display(),
                home_dir.display()
            );
        }
    }
    let node_did = match (args.node_did.as_deref(), host.live_node_did.as_deref()) {
        (Some(explicit), Some(live)) if explicit != live => anyhow::bail!(
            "the runtime at this endpoint serves node {live}, so its tool ceiling does not \
             apply to --node-did {explicit}; omit --node-did or point --graphql at that node"
        ),
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
    let enabled_agent_id_set = enabled_agent_ids.iter().cloned().collect::<HashSet<_>>();
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
            let tools_id = context.and_then(|context| context.tools_id.as_deref());
            let config = if let Some(id) = tools_id {
                let tools = bundle
                    .config
                    .tools
                    .iter()
                    .find(|tools| tools.node_did == node_did && tools.tools_id == id)
                    .with_context(|| format!("referenced Tools {id} is missing"))?;
                AgentToolSurfaceConfig::from_tools_documents(
                    &agent.agent_id,
                    tools,
                    &bundle.config.datastore_tool_surfaces,
                    &bundle.config.eth_tools,
                    &bundle.config.agent_targets,
                    tool_ceiling,
                    Vec::new(),
                )?
            } else {
                AgentToolSurfaceConfig::from_tools_documents(
                    &agent.agent_id,
                    &gents::document_config::Tools::default(),
                    &[],
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

    let output = json!({
        "node_did": node_did,
        "access_mode": access.mode(),
        "home": host.home.clone().unwrap_or_else(|| home_dir.display().to_string()),
        "host_tool_ceiling": {
            "tool_ceiling": format_tool_ceiling(host.mode),
            "tool_root": host.root,
            "cli_tools": host.ceiling.cli_tools().iter().map(|tool| &tool.name).collect::<Vec<_>>(),
            "source": host.source,
            "live": host.live_node_did.is_some(),
            "next": host.live_node_did.is_none().then(|| format!(
                "No runtime serves this home, so this is the configured init.json ceiling; a server started with --tool-ceiling, --tool-root or --cli-tool enforces its own. Start `{}` and rerun for the live ceiling.",
                match args.home.as_deref() {
                    Some(home) => format!("gents server --home {}", home.display()),
                    None => "gents server".to_owned(),
                }
            )),
            "agent_roots": host.live_node_did.is_some().then_some(
                "Agent Tools roots are admitted under tool_root on this CLI host's filesystem; explained from another host, a root the server admits can appear under configuration_issues."
            ),
            "scope": "host_native_file_bash_cli_only",
            "note": "This ceiling currently clamps host-native file/bash/CLI tools, not every model-callable built-in read, MCP, agent, or operator HTTP surface."
        },
        "runtime_availability": {
            "measured_available_mcp_service_ids": available_services,
            "local_target_eligibility": {
                "source": "enabled_agent_configuration",
                "enabled_agent_ids": enabled_agent_ids.iter().cloned().collect::<Vec<_>>(),
                "runtime_readiness_verified": false
            },
        },
        "agents": agents,
        "configuration_issues": configuration_issues,
        "operator_surfaces": {
            "included_in_model_tool_surface": false,
            "note": "Server HTTP routes and optional external /mcp are binary/operator surfaces; they are not included in per-agent model-callable tool_names."
        },
    });
    print_json(&output)?;
    Ok(())
}

/// The host ceiling explained agents are clamped by: the serving runtime's
/// own when one answers, since `gents server` flags override init.json.
struct HostCeiling {
    mode: ToolCeilingArg,
    root: Option<String>,
    ceiling: ToolCeiling,
    source: &'static str,
    live_node_did: Option<String>,
    home: Option<String>,
}

impl HostCeiling {
    async fn live(endpoint: &GraphqlEndpoint) -> Result<Self> {
        let status = fetch_runtime_status(endpoint, Duration::from_secs(10))
            .await
            .context("the GraphQL endpoint has no gents runtime status to read its tool ceiling")?;
        let field = |name: &str| status.get(name).and_then(Value::as_str).map(str::to_owned);
        let mode_text = field("tool_ceiling").context("runtime status has no tool_ceiling")?;
        let mode = ToolCeilingArg::from_str(&mode_text, false)
            .map_err(|error| anyhow::anyhow!("runtime tool_ceiling {mode_text}: {error}"))?;
        let root = field("tool_root");
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
