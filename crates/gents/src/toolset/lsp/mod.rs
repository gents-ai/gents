mod actions;
mod admit;
mod auth;
mod catalog;
mod client;
mod config;
mod edits;
mod encoding;
mod pool;
#[cfg(test)]
mod tests;
mod uri;
mod writethrough;

pub use admit::admit_command;
pub use auth::{
    lsp_action_authorized, lsp_advertised, lsp_apply_authorized, LspAction, LspMutationSource,
};
pub use catalog::{builtin_catalog, primary_for_file, CatalogServer};
pub use config::LspConfigDocument;
pub use pool::{LspPool, PoolKey};
pub use writethrough::{LspWritethrough, MutationKind};

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::llm::tool::{Tool, ToolDefinition};
use crate::tool_call_lifecycle::FailureClass;
use crate::tool_surface::{FileToolMode, ToolPolicyBash, ToolPolicySurface};
use crate::toolset::shared::{ToolContext, ToolError};
use crate::toolset::{CommandConstraints, CommandNetworkMode};

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
    pub constraints: CommandConstraints,
    pub format_on_write: bool,
    pub diagnostics_on_write: bool,
    pub diagnostics_on_edit: bool,
    pub diagnostics_deduplicate: bool,
    pub idle_timeout: Duration,
}

pub fn constraints_from_effective_policy(
    policy: &ToolPolicySurface,
    lsp_network_overlay: Option<CommandNetworkMode>,
) -> CommandConstraints {
    constraints_from_effective_bash(&policy.bash, lsp_network_overlay)
}

pub fn constraints_from_effective_bash(
    bash: &ToolPolicyBash,
    lsp_network_overlay: Option<CommandNetworkMode>,
) -> CommandConstraints {
    let (allowed, deny_all) = match &bash.allowed_argv_prefixes {
        crate::tool_surface::EndpointScope::All => (Vec::new(), false),
        crate::tool_surface::EndpointScope::None => (Vec::new(), true),
        crate::tool_surface::EndpointScope::Only(_) => (bash.allowed_argv_prefixes.keys(), false),
    };
    let desired = lsp_network_overlay.unwrap_or_else(crate::toolset::default_lsp_network_mode);
    let network_mode = desired.meet(bash.network_mode);
    CommandConstraints {
        allowed_argv_prefixes: allowed,
        forbidden_argv_prefixes: bash.forbidden_argv_prefixes.iter().cloned().collect(),
        network_mode,
        execution_mode: bash.execution_mode,
        sandbox: crate::toolset::lsp_sandbox_for_effective(bash.execution_mode),
        deny_all_argv: deny_all,
    }
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
    #[serde(default)]
    timeout: Option<u32>,
}

impl Tool for LspTool {
    const NAME: &'static str = LSP_TOOL_NAME;
    type Error = ToolError;
    type Args = LspArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: LSP_TOOL_NAME.to_string(),
            description: "Symbol-aware code intelligence from language servers. Position actions accept file + 1-indexed line + symbol; when line is omitted, Gents searches the file for symbol. symbols with a file lists document symbols; file=\"*\" + query searches the workspace. Prefer lsp over text search for definitions, references, renames, and code actions because it follows shadowing and cross-file callsites. status never starts a server: call a server-backed action first, then status in a later tool turn if you need to verify ready. reload retires the current snapshot's clients without starting replacements. workspace/executeCommand is never sent.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": [
                            "status",
                            "reload",
                            "capabilities",
                            "diagnostics",
                            "definition",
                            "type_definition",
                            "implementation",
                            "references",
                            "hover",
                            "symbols",
                            "rename",
                            "rename_file",
                            "code_actions",
                            "request"
                        ]
                    },
                    "file": {
                        "type": "string",
                        "description": "Path relative to the file-tool root. For diagnostics also a glob; use \"*\" for workspace symbols/diagnostics/reload."
                    },
                    "line": {
                        "type": "integer",
                        "description": "1-indexed line for hover, definition, references, rename, and code_actions. Omit to search the whole file for symbol."
                    },
                    "symbol": {
                        "type": "string",
                        "description": "Substring on that line used to pick the column. Use name#N for the Nth match (1-indexed)."
                    },
                    "query": {
                        "type": "string",
                        "description": "Workspace symbol query; code-action title substring or 0/1-based index when apply=true; or raw LSP method for action=request."
                    },
                    "new_name": {
                        "type": "string",
                        "description": "Required for rename and rename_file."
                    },
                    "apply": {
                        "type": "boolean",
                        "description": "rename/rename_file apply unless false. code_actions lists unless true."
                    },
                    "payload": {
                        "type": "string",
                        "description": "JSON object for action=request. Only allowlisted read methods; every file URI is validated."
                    },
                    "timeout": {
                        "type": "integer",
                        "minimum": 5,
                        "maximum": 300,
                        "description": "Tool timeout in seconds (default 20)."
                    }
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
        if !lsp_action_authorized(self.config.lsp, self.config.file, action) {
            return Err(ToolError::reported_failure(
                FailureClass::PolicyDenied,
                "lsp action is not authorized for this file-tool mode".into(),
            ));
        }
        if matches!(action, LspAction::RequestRead | LspAction::RequestWrite) {
            let method = args.query.as_deref().unwrap_or("");
            actions::validate_raw_request(&self.context, method, args.payload.as_deref())?;
        }
        let detected =
            catalog::detect_admitted_servers(&self.config.workspace, &self.config.servers);
        let lease = if action.may_cold_start() {
            let file = args.file.as_deref().unwrap_or("");
            let path = if file.is_empty() || file == "*" || file.contains(['*', '?', '{', '[']) {
                None
            } else {
                edits::resolve_inbound_path(&self.context, file).ok()
            };
            let server = match path.as_ref() {
                Some(path) => primary_for_file(&detected, path).cloned(),
                None => detected.iter().find(|server| !server.is_linter).cloned(),
            };
            if let Some(server) = server {
                let session_id =
                    crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
                        .and_then(|scope| scope.session_id)
                        .filter(|id| !id.is_empty())
                        .unwrap_or_else(|| self.config.session_id.clone());
                let key = PoolKey {
                    session_id,
                    behavior_id: self.config.behavior_id.clone(),
                    workspace_root: self.config.workspace.clone(),
                    server_name: server.name.clone(),
                    config_digest: self.config.digest.clone(),
                };
                Some(
                    self.pool
                        .get_or_start(key, &server, &self.config)
                        .await
                        .map_err(|err| {
                            ToolError::reported_failure(FailureClass::ServiceUnavailable, err)
                        })?,
                )
            } else {
                None
            }
        } else {
            None
        };
        actions::dispatch(
            &self.context,
            lease.as_ref(),
            &self.pool,
            &self.config,
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
                timeout: args.timeout,
            },
        )
        .await
    }

    fn into_dyn_error(error: Self::Error) -> crate::llm::tool::ToolError {
        error.into_dispatch_error()
    }
}

pub fn merge_catalog(raw_config: Option<&str>) -> Vec<CatalogServer> {
    let doc = LspConfigDocument::parse_operator(raw_config).unwrap_or_default();
    apply_overrides(builtin_catalog(), &doc)
}

pub fn config_digest(
    workspace: &std::path::Path,
    servers: &[CatalogServer],
    constraints: &CommandConstraints,
) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(workspace.to_string_lossy().as_bytes());
    hasher.update(format!("{:?}", constraints.execution_mode).as_bytes());
    hasher.update(format!("{:?}", constraints.sandbox).as_bytes());
    hasher.update(format!("{:?}", constraints.network_mode).as_bytes());
    hasher.update(constraints.deny_all_argv.to_string().as_bytes());
    for prefix in &constraints.allowed_argv_prefixes {
        hasher.update(prefix.join("\0").as_bytes());
    }
    for prefix in &constraints.forbidden_argv_prefixes {
        hasher.update(prefix.join("\0").as_bytes());
    }
    for server in servers {
        hasher.update(server.name.as_bytes());
        if let Ok(canonical) = admit_command(&server.command, workspace) {
            hasher.update(canonical.to_string_lossy().as_bytes());
        } else {
            hasher.update(server.command.as_bytes());
        }
        for arg in &server.args {
            hasher.update(arg.as_bytes());
        }
        if let Some(language_id) = &server.language_id {
            hasher.update(language_id.as_bytes());
        }
        if let Some(init) = &server.init_options {
            hasher.update(init.to_string().as_bytes());
        }
        if let Some(settings) = &server.settings {
            hasher.update(settings.to_string().as_bytes());
        }
        if let Some(caps) = &server.capabilities {
            hasher.update(caps.to_string().as_bytes());
        }
        if let Some(timings) = &server.workspace_ready_timings {
            hasher.update(timings.to_string().as_bytes());
        }
    }
    format!("{:x}", hasher.finalize())
}

pub fn advertised(lsp: bool, file: FileToolMode) -> bool {
    lsp_advertised(lsp, file)
}
