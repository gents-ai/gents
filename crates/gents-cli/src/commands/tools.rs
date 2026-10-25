use gents_protocol::error_chain::ErrorChain;
use rapidhash::RapidHashSet;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use clap::ValueEnum;
use gents::config_client::GraphqlEndpoint;
use gents::tool_surface::{measured_mcp_services_for_access, RuntimeToolAvailability};
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

#[derive(Debug, thiserror::Error)]
pub(crate) enum ToolsCommandError {
    #[error("home {} is not UTF-8", path.display())]
    HomeNotUtf8 { path: PathBuf },
    #[error("home {text} cannot be shell-quoted")]
    NotShellQuotable {
        text: String,
        #[source]
        source: shlex::QuoteError,
    },
    #[error("referenced AgentContext {id} is missing")]
    ContextMissing { id: String },
    #[error("referenced Tools {id} is missing")]
    ToolsMissing { id: String },
    #[error(
        "home {} belongs to node {expected}, but the runtime its runtime.json names serves node {live}; restart `gents server --home {home_arg}` to serve this home, or pass --graphql to explain that node",
        .home.display()
    )]
    HomeNodeMismatch {
        home: PathBuf,
        expected: String,
        live: String,
        home_arg: String,
    },
    #[error(
        "the runtime at this endpoint serves node {live}, so its tool ceiling does not apply to --node-did {explicit}; omit --node-did or point --graphql at that node"
    )]
    NodeDidMismatch { live: String, explicit: String },
    #[error("agent {agent_id} was not found for node {node_did}")]
    AgentNotFound { agent_id: String, node_did: String },
    #[error("runtime status has no tool_ceiling")]
    StatusMissingToolCeiling,
    #[error("runtime status has no node_did")]
    StatusMissingNodeDid,
    #[error("runtime tool_ceiling {mode}: {error}")]
    RuntimeToolCeiling { mode: String, error: String },
    #[error(
        "the runtime's {mode} ceiling has no usable root ({error}), so it builds no host tools to explain; fix --tool-root (or tool_root in init.json) and restart the server"
    )]
    UnusableRuntimeRoot { mode: String, error: String },
    #[error("the GraphQL endpoint has no gents runtime status to read its tool ceiling")]
    RuntimeStatusUnavailable(#[source] Box<crate::commands::status::StatusError>),
    #[error("init.json tool ceiling")]
    InitToolCeiling(#[source] crate::resolve_helpers::ResolveHelpersError),
    #[error(transparent)]
    HomeState(#[from] crate::home_state::HomeStateError),
    #[error(transparent)]
    ResolveHelpers(#[from] crate::resolve_helpers::ResolveHelpersError),
}

pub(crate) async fn dispatch(command: ToolsCommand) -> Result<()> {
    match command {
        ToolsCommand::Explain(args) => explain(args).await,
    }
}

async fn explain(args: ToolExplainArgs) -> Result<()> {
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let host = match &access {
        ConfigAccess::Graphql(endpoint) => HostCeiling::live(endpoint).await?,
        ConfigAccess::Local(_) => HostCeiling::configured(&home_dir)?,
    };
    if let (None, None, Some(live)) = (
        args.graphql.as_deref(),
        args.node_did.as_deref(),
        host.live_node_did.as_deref(),
    ) {
        if let Some(expected) = read_init_config(&home_dir)?.map(|config| config.node_did) {
            if expected != live {
                return Err(ToolsCommandError::HomeNodeMismatch {
                    home: home_dir.clone(),
                    expected,
                    live: live.to_owned(),
                    home_arg: shell_quoted_path(&home_dir)?,
                }
                .into());
            }
        }
    }
    let node_did = match (args.node_did.as_deref(), host.live_node_did.as_deref()) {
        (Some(explicit), Some(live)) if explicit != live => {
            return Err(ToolsCommandError::NodeDidMismatch {
                live: live.to_owned(),
                explicit: explicit.to_owned(),
            }
            .into());
        }
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
                configuration_issues.insert(
                    agent.agent_id.clone(),
                    ErrorChain(error.as_ref()).to_string(),
                );
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
            return Err(ToolsCommandError::AgentNotFound {
                agent_id: only_agent_id.to_owned(),
                node_did: node_did.clone(),
            }
            .into());
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
    agent: &gents::document_config::Agent,
) -> Result<
    (
        Option<&'a gents::document_config::AgentContext>,
        Option<&'a gents::document_config::Tools>,
    ),
    ToolsCommandError,
> {
    let context = agent
        .context_id
        .as_deref()
        .map(|id| {
            bundle
                .config
                .contexts
                .iter()
                .find(|context| context.node_did == node_did && context.context_id == id)
                .ok_or_else(|| ToolsCommandError::ContextMissing { id: id.to_owned() })
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
                .ok_or_else(|| ToolsCommandError::ToolsMissing { id: id.to_owned() })
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

fn shell_quoted_path(path: &Path) -> Result<String, ToolsCommandError> {
    let text = path
        .to_str()
        .ok_or_else(|| ToolsCommandError::HomeNotUtf8 {
            path: path.to_path_buf(),
        })?;
    shlex::try_quote(text)
        .map(|quoted| quoted.into_owned())
        .map_err(|source| ToolsCommandError::NotShellQuotable {
            text: text.to_owned(),
            source,
        })
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
    async fn live(endpoint: &GraphqlEndpoint) -> Result<Self, ToolsCommandError> {
        let status = fetch_runtime_status(endpoint, Duration::from_secs(10))
            .await
            .map_err(|error| ToolsCommandError::RuntimeStatusUnavailable(Box::new(error)))?;
        let field = |name: &str| status.get(name).and_then(Value::as_str).map(str::to_owned);
        let mode_text = field("tool_ceiling").ok_or(ToolsCommandError::StatusMissingToolCeiling)?;
        let mode = ToolCeilingArg::from_str(&mode_text, false).map_err(|error| {
            ToolsCommandError::RuntimeToolCeiling {
                mode: mode_text.clone(),
                error,
            }
        })?;
        let root = field("tool_root");
        if let (Some(error), false) = (field("tool_root_error"), mode == ToolCeilingArg::MetaOnly) {
            return Err(ToolsCommandError::UnusableRuntimeRoot {
                mode: mode_text,
                error,
            });
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
            live_node_did: Some(field("node_did").ok_or(ToolsCommandError::StatusMissingNodeDid)?),
            home: field("home"),
        })
    }

    fn configured(home_dir: &Path) -> Result<Self, ToolsCommandError> {
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
            .map_err(ToolsCommandError::InitToolCeiling)?,
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
