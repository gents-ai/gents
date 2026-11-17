use super::*;
use gents_loop::tool_policy::presentation::configuration_valid;
use gents_protocol::tool_names::*;

impl Tools {
    pub fn validate_hidden_tools(&self, merged: &crate::MergedSurfaceTools) -> anyhow::Result<()> {
        let all_selected = self
            .hidden_tool_names
            .iter()
            .all(|name| self.selects_invocation(name, merged));
        let script_ready = self
            .integrations
            .as_ref()
            .and_then(|group| group.plugins.as_ref())
            .is_some_and(|plugins| {
                plugins
                    .iter()
                    .any(|plugin| plugin.tool_name() == "script" && plugin.tool_calls)
            });
        let gateway_hidden = self
            .hidden_tool_names
            .iter()
            .any(|name| gents_loop::tool_policy::presentation::is_gateway(name));
        anyhow::ensure!(configuration_valid(!self.hidden_tool_names.is_empty(), all_selected, script_ready, gateway_hidden),
            "Tools {} hidden_tool_names requires selected exact invocation names and a visible script with tool_calls: true; script, config, load_skill and spawn_process must stay visible; unselected names: {:?}",
            self.tools_id, self.hidden_tool_names.iter().filter(|name| !self.selects_invocation(name, merged)).collect::<Vec<_>>());
        Ok(())
    }

    fn selects_invocation(&self, name: &str, merged: &crate::MergedSurfaceTools) -> bool {
        let host = self.host.as_ref();
        let files = host
            .and_then(|host| host.files.as_ref())
            .map(|files| files.mode)
            .unwrap_or_default();
        let bash = host
            .and_then(|host| host.bash.as_ref())
            .map(|bash| bash.mode)
            .unwrap_or_default();
        let built_ins = self.built_ins.as_ref();
        let datastore = self.datastore.as_ref();
        let agents = self.agents.as_ref();
        self.selects_named_invocation(name, merged)
            || match name {
                LIST_FILES_TOOL_NAME | READ_FILE_TOOL_NAME | GLOB_TOOL_NAME | GREP_TOOL_NAME => {
                    files != FileToolMode::Off
                }
                WRITE_FILE_TOOL_NAME | EDIT_FILE_TOOL_NAME => files == FileToolMode::ReadWrite,
                BASH_TOOL_NAME => bash == BashMode::ReadOnly,
                BASH_UNRESTRICTED_TOOL_NAME => bash == BashMode::Unrestricted,
                SCHEMA_TOOL_NAME => built_ins
                    .and_then(|group| group.enable_schema_tool)
                    .unwrap_or(false),
                "p2p" => built_ins
                    .and_then(|group| group.enable_p2p_tool)
                    .unwrap_or(false),
                SESSION_HISTORY_TOOL_NAME => built_ins
                    .and_then(|group| group.enable_session_history_tool)
                    .unwrap_or(true),
                CONTEXT_BUDGET_TOOL_NAME => built_ins
                    .and_then(|group| group.enable_context_budget)
                    .unwrap_or(true),
                DEFRA_QUERY_TOOL_NAME => datastore
                    .and_then(|group| group.enable_defra_query)
                    .unwrap_or(false),
                gents_protocol::plugin::WRITE_TOOL_NAME => datastore
                    .and_then(|group| group.write_collections.as_ref())
                    .is_some_and(|collections| !collections.is_empty()),
                gents_protocol::goal_names::GET_GOAL_TOOL_NAME
                | gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME => built_ins
                    .and_then(|group| group.enable_goal_tools)
                    .unwrap_or(false),
                gents_protocol::goal_names::CREATE_GOAL_TOOL_NAME => {
                    built_ins.is_some_and(|group| {
                        group.enable_goal_tools.unwrap_or(false)
                            && group.enable_goal_creation.unwrap_or(false)
                    })
                }
                CONFIG_TOOL_NAME | "provider_limits" => self
                    .self_config
                    .as_ref()
                    .and_then(|group| group.enabled)
                    .unwrap_or(false),
                LSP_TOOL_NAME => self
                    .integrations
                    .as_ref()
                    .is_some_and(|group| group.lsp.is_some()),
                AGENT_NEW_TOOL_NAME => agents.is_some_and(|group| {
                    group.enabled.unwrap_or(false) && !group.target_ids.is_empty()
                }),
                name if AGENT_TOOL_NAMES.contains(&name) => {
                    agents.and_then(|group| group.enabled).unwrap_or(false)
                }
                SPAWN_PROCESS_TOOL_NAME
                | WAIT_PROCESS_TOOL_NAME
                | LIST_PROCESSES_TOOL_NAME
                | READ_PROCESS_TOOL_NAME
                | CANCEL_PROCESS_TOOL_NAME => {
                    host.is_some_and(|host| {
                        host.bash.as_ref().is_some_and(|bash| {
                            bash.background_enabled && bash.mode != BashMode::Off
                        })
                    }) || self.remote.as_ref().is_some_and(|remote| {
                        remote
                            .services
                            .iter()
                            .any(|service| !service.background_tool_names.is_empty())
                    }) || self
                        .integrations
                        .as_ref()
                        .and_then(|group| group.plugins.as_ref())
                        .is_some_and(|plugins| {
                            plugins
                                .iter()
                                .any(|plugin| plugin.tool_name() == "script" && plugin.tool_calls)
                        })
                }
                _ => false,
            }
    }

    fn selects_named_invocation(&self, name: &str, merged: &crate::MergedSurfaceTools) -> bool {
        let host = self.host.as_ref();
        host.is_some_and(|host| host.cli.iter().any(|cli| cli.name == name))
            || merged.write_tools.iter().any(|tool| tool.tool_name == name)
            || merged.query_tools.iter().any(|tool| tool.tool_name == name)
            || self
                .integrations
                .as_ref()
                .and_then(|group| group.plugins.as_ref())
                .is_some_and(|plugins| plugins.iter().any(|plugin| plugin.tool_name() == name))
            || self.remote.as_ref().is_some_and(|remote| {
                remote
                    .invocation_names(
                        &remote
                            .services
                            .iter()
                            .map(|service| service.mcp_service_id.clone())
                            .collect::<Vec<_>>(),
                    )
                    .iter()
                    .any(|selected| selected == name)
            })
    }
}

impl RemoteTools {
    pub fn flat_tool_name(service: &str, tool: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update((service.len() as u64).to_be_bytes());
        hash.update(service.as_bytes());
        hash.update(tool.as_bytes());
        format!("mcp_{:x}", hash.finalize())[..64].to_string()
    }

    pub fn invocation_names(&self, allowed: &[String]) -> Vec<String> {
        let mut names = Vec::new();
        let mut discovery = false;
        for service in &self.services {
            let Some(service) = self.selected_service(&service.mcp_service_id, allowed) else {
                continue;
            };
            match service.style {
                RemoteToolStyle::Discovery => discovery = true,
                RemoteToolStyle::Flat => names.extend(
                    service
                        .tool_names
                        .iter()
                        .map(|tool| Self::flat_tool_name(&service.mcp_service_id, tool)),
                ),
            }
        }
        if discovery {
            names.extend(META_TOOL_NAMES.iter().map(|name| name.to_string()));
        }
        names
    }
}
