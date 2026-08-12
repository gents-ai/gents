mod actions;
mod admit;
mod auth;
mod catalog;
mod client;
mod config;
mod edits;
mod encoding;
mod pool;
mod writethrough;
#[cfg(test)]
mod tests;

pub use admit::admit_command;
pub use auth::{
    lsp_action_authorized, lsp_advertised, lsp_apply_authorized, LspAction, LspMutationSource,
};
pub use catalog::{builtin_catalog, family_eligible, marker_matches, primary_for_file, CatalogServer};
pub use config::LspConfigDocument;
pub use pool::{LspPool, PoolKey};
pub use writethrough::LspWritethrough;

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::json;

use crate::llm::tool::{Tool, ToolDefinition};
use crate::tool_call_lifecycle::FailureClass;
use crate::tool_surface::FileToolMode;
use crate::toolset::shared::{ToolContext, ToolError};

use actions::ActionRequest;
use config::apply_overrides;

pub const LSP_TOOL_NAME: &str = "lsp";

#[derive(Clone)]
pub struct LspToolConfig {
    pub lsp: bool,
    pub file: FileToolMode,
    pub workspace: PathBuf,
    pub session_id: String,
    pub behavior_id: String,
    pub digest: String,
    pub servers: Vec<CatalogServer>,
}

#[derive(Clone)]
pub(crate) struct LspTool {
    config: LspToolConfig,
    pool: LspPool,
    context: ToolContext,
}

impl LspTool {
    pub fn new(config: LspToolConfig, pool: LspPool) -> Result<Self, anyhow::Error> {
        let context = ToolContext::new(config.workspace.clone(), false)?;
        Ok(Self {
            config,
            pool,
            context,
        })
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct LspArgs {
    action: String,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default)]
    symbol: Option<String>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    new_name: Option<String>,
    #[serde(default)]
    apply: Option<bool>,
    #[serde(default)]
    payload: Option<String>,
}

impl Tool for LspTool {
    const NAME: &'static str = LSP_TOOL_NAME;
    type Error = ToolError;
    type Args = LspArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: LSP_TOOL_NAME.to_string(),
            description: "Query language servers for diagnostics, navigation, symbols, renames, code actions, capabilities, and raw requests.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string" },
                    "file": { "type": "string" },
                    "line": { "type": "integer" },
                    "symbol": { "type": "string" },
                    "query": { "type": "string" },
                    "new_name": { "type": "string" },
                    "apply": { "type": "boolean" },
                    "payload": { "type": "string" }
                },
                "required": ["action"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let mut action = LspAction::parse(&args.action).ok_or_else(|| {
            ToolError::reported_failure(
                FailureClass::ArgumentInvalid,
                format!("unknown lsp action {}", args.action),
            )
        })?;
        if matches!(action, LspAction::CodeActionsList) && args.apply == Some(true) {
            action = LspAction::CodeActionsApply;
        }
        if matches!(action, LspAction::RequestRead)
            && args
                .query
                .as_deref()
                .is_some_and(|method| !actions::READ_REQUEST_METHODS.contains(&method))
        {
            action = LspAction::RequestWrite;
        }
        if !lsp_action_authorized(self.config.lsp, self.config.file, action) {
            return Err(ToolError::reported_failure(
                FailureClass::PolicyDenied,
                "lsp action is not authorized for this file-tool mode".into(),
            ));
        }
        let detected: Vec<CatalogServer> = self
            .config
            .servers
            .iter()
            .filter(|server| marker_matches(&self.config.workspace, &server.root_markers))
            .filter(|server| family_eligible(server, &self.config.workspace))
            .cloned()
            .collect();
        let lease = if action.may_cold_start() {
            let file = args.file.as_deref().unwrap_or("");
            let path = if file.is_empty() || file == "*" {
                None
            } else {
                edits::resolve_inbound_path(&self.context, file).ok()
            };
            let server = path
                .as_ref()
                .and_then(|path| primary_for_file(&detected, path))
                .or_else(|| detected.iter().find(|s| !s.is_linter))
                .cloned();
            if let Some(server) = server {
                let key = PoolKey {
                    session_id: self.config.session_id.clone(),
                    behavior_id: self.config.behavior_id.clone(),
                    workspace_root: self.config.workspace.clone(),
                    server_name: server.name.clone(),
                    config_digest: self.config.digest.clone(),
                };
                Some(
                    self.pool
                        .get_or_start(
                            key,
                            &server,
                            &self.config.workspace,
                            &self.config.workspace,
                            Some(crate::toolset::build_shell_env()),
                        )
                        .await
                        .map_err(|err| {
                            ToolError::reported_failure(FailureClass::ServiceUnavailable, err)
                        })?,
                )
            } else {
                None
            }
        } else if matches!(action, LspAction::Reload) {
            let key_prefix = (
                self.config.session_id.clone(),
                self.config.behavior_id.clone(),
                self.config.workspace.clone(),
                self.config.digest.clone(),
            );
            for server in &detected {
                let key = PoolKey {
                    session_id: key_prefix.0.clone(),
                    behavior_id: key_prefix.1.clone(),
                    workspace_root: key_prefix.2.clone(),
                    server_name: server.name.clone(),
                    config_digest: key_prefix.3.clone(),
                };
                self.pool.retire(&key).await;
            }
            None
        } else {
            None
        };
        actions::dispatch(
            &self.context,
            lease,
            &detected,
            ActionRequest {
                action,
                file: args.file,
                line: args.line,
                symbol: args.symbol,
                query: args.query,
                new_name: args.new_name,
                apply: args.apply,
                payload: args.payload,
            },
        )
        .await
    }

    fn into_dyn_error(error: Self::Error) -> crate::llm::tool::ToolError {
        error.into_dispatch_error()
    }
}

pub fn merge_catalog(raw_config: Option<&str>) -> Vec<CatalogServer> {
    let doc = LspConfigDocument::parse_operator(raw_config);
    apply_overrides(builtin_catalog(), &doc)
}

pub fn config_digest(workspace: &std::path::Path, servers: &[CatalogServer], extra: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(workspace.to_string_lossy().as_bytes());
    hasher.update(extra.as_bytes());
    for server in servers {
        hasher.update(server.name.as_bytes());
        hasher.update(server.command.as_bytes());
        hasher.update(server.priority.to_le_bytes());
    }
    format!("{:x}", hasher.finalize())
}

pub fn advertised(lsp: bool, file: FileToolMode) -> bool {
    lsp_advertised(lsp, file)
}
