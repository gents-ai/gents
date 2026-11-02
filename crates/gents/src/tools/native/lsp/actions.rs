use std::path::Path;

use serde_json::json;

use crate::tools::call::lifecycle::FailureClass;
use crate::tools::native::shared::{ToolContext, ToolError};

use super::auth::LspAction;
use super::catalog::{primary_for_file, CatalogServer};
use super::client::LspClient;
use super::edits::{
    apply_prepared_edits, prepare_rename, prepare_workspace_edit, resolve_inbound_path,
    resolve_inbound_path_allow_create,
};
use super::encoding::position_for_symbol;
use super::pool::LspLease;

pub(super) mod diagnostics;
pub(super) mod format;
pub(super) mod request;

use diagnostics::{glob_action, run_linter_diagnostics};
use format::{format_result_with_limit, truncate_model_output};
use request::{request_maybe_retry, resolve_code_action_edit, validate_raw_request};

/// Keep one result bounded before applying the shared output byte limit.
pub(crate) const MAX_REQUESTED_RESULTS: u32 = 1000;
pub(crate) const MAX_DIAGNOSTICS: usize = 50;
pub(crate) const MAX_WORKSPACE_SYMBOLS: usize = 200;
pub(crate) const MAX_REFERENCES: usize = 50;
pub(crate) const MAX_RENAME_PAIRS: usize = 1_000;
pub(crate) const MAX_GLOB_TARGETS: usize = 20;

pub const READ_REQUEST_METHODS: &[&str] = &[
    "textDocument/hover",
    "textDocument/definition",
    "textDocument/typeDefinition",
    "textDocument/implementation",
    "textDocument/references",
    "textDocument/documentSymbol",
    "textDocument/diagnostic",
    "workspace/symbol",
    "workspace/diagnostic",
];

pub struct ActionRequest {
    pub action: LspAction,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub symbol: Option<String>,
    pub query: Option<String>,
    pub new_name: Option<String>,
    pub apply: Option<bool>,
    pub payload: Option<String>,
    pub timeout: Option<u32>,
    pub limit: Option<u32>,
}

pub async fn dispatch(
    context: &ToolContext,
    lease: Option<&LspLease>,
    pool: &super::pool::LspPool,
    config: &super::LspToolConfig,
    servers: &[CatalogServer],
    workspace: &Path,
    req: ActionRequest,
) -> Result<String, ToolError> {
    match req.action {
        LspAction::Status => {
            let session_id = crate::tools::call::lifecycle::runtime::current_tool_runtime_context()
                .and_then(|scope| scope.session_id)
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| config.session_id.clone());
            let states = pool
                .inspect_session(&session_id, &config.agent_id, workspace, &config.digest)
                .await
                .map_err(|error| ToolError::reported_failure(FailureClass::PolicyDenied, error))?;
            Ok(status_text(states, &config.servers, workspace))
        }
        LspAction::Reload => {
            let session_id = crate::tools::call::lifecycle::runtime::current_tool_runtime_context()
                .and_then(|scope| scope.session_id)
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| config.session_id.clone());
            let retired = pool
                .reload_snapshot(&session_id, &config.agent_id, workspace, &config.digest)
                .await;
            Ok(format!(
                "retired {retired} language-server client(s) for the current snapshot"
            ))
        }
        LspAction::Capabilities => {
            let lease = lease.ok_or_else(|| unavailable_for_workspace(workspace, config))?;
            Ok(lease.client().capabilities().await.to_string())
        }
        action => {
            let lease = lease.ok_or_else(|| unavailable_for_workspace(workspace, config))?;
            run_file_action(context, lease.client(), config, servers, action, &req).await
        }
    }
}

async fn run_file_action(
    context: &ToolContext,
    client: &LspClient,
    config: &super::LspToolConfig,
    servers: &[CatalogServer],
    action: LspAction,
    req: &ActionRequest,
) -> Result<String, ToolError> {
    if matches!(action, LspAction::RequestRead | LspAction::RequestWrite) {
        return raw_request(context, client, req).await;
    }
    let file = req
        .file
        .as_deref()
        .ok_or_else(|| arg_invalid("file parameter required"))?;
    if file == "*" {
        return workspace_action(context, client, action, req).await;
    }
    if looks_like_glob(file) {
        return glob_action(
            context,
            client,
            config,
            servers,
            req,
            file,
            request_timeout(req),
        )
        .await;
    }
    let path = resolve_inbound_path(context, file).map_err(policy)?;
    let text = std::fs::read_to_string(&path).map_err(|err| {
        ToolError::reported_failure(FailureClass::ArgumentInvalid, err.to_string())
    })?;
    let uri = super::uri::path_to_file_uri(&path);
    let lang = language_id(servers, &path);
    client
        .ensure_open(&uri, &lang, &text)
        .await
        .map_err(|err| ToolError::reported_failure(FailureClass::ToolReturnedError, err))?;
    let encoding = client.position_encoding().await;
    let pos = position_for_symbol(
        &text,
        encoding,
        req.line,
        req.symbol.as_deref().unwrap_or(""),
    )
    .or_else(|| {
        req.symbol.as_ref().map(|_| super::encoding::LspPosition {
            line: req.line.unwrap_or(1).saturating_sub(1),
            character: 0,
        })
    });
    let method = match action {
        LspAction::Hover => "textDocument/hover",
        LspAction::Definition => "textDocument/definition",
        LspAction::TypeDefinition => "textDocument/typeDefinition",
        LspAction::Implementation => "textDocument/implementation",
        LspAction::References => "textDocument/references",
        LspAction::Symbols => "textDocument/documentSymbol",
        LspAction::Diagnostics => "textDocument/diagnostic",
        LspAction::Rename => "textDocument/rename",
        LspAction::RenameFile => "workspace/willRenameFiles",
        LspAction::CodeActionsList | LspAction::CodeActionsApply => "textDocument/codeAction",
        _ => return Err(arg_invalid("unsupported action")),
    };
    let mut params = json!({
        "textDocument": { "uri": uri }
    });
    if let Some(pos) = pos {
        params["position"] = json!({ "line": pos.line, "character": pos.character });
    }
    if matches!(
        action,
        LspAction::CodeActionsList | LspAction::CodeActionsApply
    ) {
        let start = pos.unwrap_or(super::encoding::LspPosition {
            line: req.line.unwrap_or(1).saturating_sub(1),
            character: 0,
        });
        params["range"] = json!({
            "start": { "line": start.line, "character": start.character },
            "end": { "line": start.line, "character": start.character }
        });
        params["context"] = json!({
            "diagnostics": [],
            "triggerKind": 1
        });
    }
    if matches!(action, LspAction::References) {
        params["context"] = json!({ "includeDeclaration": true });
    }
    if matches!(action, LspAction::Rename) {
        let name = req
            .new_name
            .as_deref()
            .ok_or_else(|| arg_invalid("new_name required"))?;
        params["newName"] = json!(name);
    }
    if matches!(action, LspAction::RenameFile) {
        let dest = req
            .new_name
            .as_deref()
            .ok_or_else(|| arg_invalid("new_name required for rename_file"))?;
        let dest_path = resolve_inbound_path_allow_create(context, dest).map_err(policy)?;
        params = json!({
            "files": [{
                "oldUri": uri,
                "newUri": super::uri::path_to_file_uri(&dest_path)
            }]
        });
    }
    // Cold-started project servers can acknowledge initialize before their
    // first document index is queryable; a null response there is transient.
    let retry_empty = matches!(
        action,
        LspAction::Hover
            | LspAction::Definition
            | LspAction::TypeDefinition
            | LspAction::Implementation
            | LspAction::References
            | LspAction::Symbols
    );
    let result = match request_maybe_retry(
        client,
        method,
        params,
        request_timeout(req),
        config.action_timeout.maximum,
        retry_empty,
    )
    .await
    {
        Ok(result) => result,
        Err(err) if matches!(action, LspAction::Diagnostics) => client
            .cached_diagnostics(&uri)
            .await
            .ok_or_else(|| ToolError::reported_failure(FailureClass::ToolReturnedError, err))?,
        Err(err) => {
            return Err(ToolError::reported_failure(
                FailureClass::ToolReturnedError,
                err,
            ));
        }
    };
    if matches!(
        action,
        LspAction::Rename | LspAction::RenameFile | LspAction::CodeActionsApply
    ) && req.apply != Some(false)
    {
        if !super::lsp_apply_authorized(
            config.lsp,
            config.file,
            super::LspMutationSource::ForegroundReturnedEdit,
        ) {
            return Err(policy("lsp apply is not authorized"));
        }
        let edit = if matches!(action, LspAction::CodeActionsApply) {
            resolve_code_action_edit(client, &result, req.query.as_deref()).await?
        } else {
            result.clone()
        };
        let mut prepared = prepare_workspace_edit(context, &edit, encoding).map_err(policy)?;
        if matches!(action, LspAction::RenameFile) {
            let dest = req
                .new_name
                .as_deref()
                .ok_or_else(|| arg_invalid("new_name required for rename_file"))?;
            let dest_path = resolve_inbound_path_allow_create(context, dest).map_err(policy)?;
            if path != dest_path
                && !prepared
                    .iter()
                    .any(|edit| edit.rename_from.as_ref() == Some(&path))
            {
                let mut rename = prepare_rename(path.clone(), dest_path).map_err(policy)?;
                rename.expected_hash =
                    Some(super::super::file_tools::content_hash(text.as_bytes()));
                prepared.push(rename);
            }
        }
        let applied = apply_prepared_edits(context, client, &prepared).await?;
        return Ok(format!("Applied edit to {applied} file(s)"));
    }
    let mut output =
        truncate_model_output(format_result_with_limit(context, action, result, req.limit));
    if matches!(action, LspAction::Diagnostics) {
        if let Some(linter) =
            run_linter_diagnostics(config, servers, &path, request_timeout(req)).await
        {
            output = format!("{output}\n{linter}");
        }
    }
    Ok(output)
}

async fn workspace_action(
    context: &ToolContext,
    client: &LspClient,
    action: LspAction,
    req: &ActionRequest,
) -> Result<String, ToolError> {
    match action {
        LspAction::Symbols => {
            let query = req
                .query
                .as_deref()
                .ok_or_else(|| arg_invalid("query required for workspace symbols"))?;
            let result = client
                .request_with_timeout(
                    "workspace/symbol",
                    json!({ "query": query }),
                    request_timeout(req),
                )
                .await
                .map_err(|err| ToolError::reported_failure(FailureClass::ToolReturnedError, err))?;
            Ok(truncate_model_output(format_result_with_limit(
                context, action, result, req.limit,
            )))
        }
        LspAction::Diagnostics => {
            match client
                .request_with_timeout(
                    "workspace/diagnostic",
                    json!({ "identifier": "gents" }),
                    request_timeout(req),
                )
                .await
            {
                Ok(result) => Ok(truncate_model_output(format_result_with_limit(
                    context, action, result, req.limit,
                ))),
                Err(_) => Ok(
                    "workspace diagnostics require workspace/diagnostic; pass a file or glob"
                        .into(),
                ),
            }
        }
        _ => Err(arg_invalid(
            "file: * is only valid for diagnostics, symbols, or reload",
        )),
    }
}

async fn raw_request(
    context: &ToolContext,
    client: &LspClient,
    req: &ActionRequest,
) -> Result<String, ToolError> {
    let method = req
        .query
        .as_deref()
        .ok_or_else(|| arg_invalid("query (method) required for request"))?;
    let params = validate_raw_request(context, method, req.payload.as_deref())?;
    let result = client
        .request_with_timeout(method, params, request_timeout(req))
        .await
        .map_err(|err| ToolError::reported_failure(FailureClass::ToolReturnedError, err))?;
    Ok(truncate_model_output(format_result_with_limit(
        context, req.action, result, None,
    )))
}

fn request_timeout(req: &ActionRequest) -> std::time::Duration {
    std::time::Duration::from_secs(
        req.timeout
            .map(u64::from)
            .unwrap_or(super::DEFAULT_ACTION_TIMEOUT_SECS),
    )
}

fn language_id(servers: &[CatalogServer], path: &std::path::Path) -> String {
    primary_for_file(servers, path)
        .and_then(|s| s.language_id.clone())
        .unwrap_or_else(|| super::catalog::language_id_for_path(path))
}

fn looks_like_glob(file: &str) -> bool {
    file.contains('*') || file.contains('?') || file.contains('{') || file.contains('[')
}

fn status_text(
    states: rapidhash::RapidHashMap<String, super::pool::PoolServerState>,
    servers: &[CatalogServer],
    workspace: &std::path::Path,
) -> String {
    let names: Vec<String> = servers
        .iter()
        .map(|s| {
            use super::pool::PoolServerState;
            match states.get(&s.name) {
                Some(PoolServerState::Starting) => format!("{} (starting/indexing)", s.name),
                Some(PoolServerState::Ready) => format!("{} (ready)", s.name),
                Some(PoolServerState::Retiring) => format!("{} (retiring)", s.name),
                Some(PoolServerState::Failed(error)) => {
                    format!("{} (start failed; retry backoff active: {error})", s.name)
                }
                None => format!(
                    "{} ({})",
                    s.name,
                    super::catalog::server_unavailable_reason(workspace, s).unwrap_or_else(|| {
                        if s.is_linter {
                            "available on demand for diagnostics".into()
                        } else {
                            "not started — run a server-backed action such as hover or symbols"
                                .into()
                        }
                    })
                ),
            }
        })
        .collect();
    if names.is_empty() {
        "No language servers configured for this project".into()
    } else {
        format!("Language servers:\n- {}", names.join("\n- "))
    }
}

fn arg_invalid(text: impl Into<String>) -> ToolError {
    ToolError::reported_failure(FailureClass::ArgumentInvalid, text.into())
}

fn policy(text: impl Into<String>) -> ToolError {
    ToolError::reported_failure(FailureClass::PolicyDenied, text.into())
}

fn unavailable(text: impl Into<String>) -> ToolError {
    ToolError::reported_failure(FailureClass::ServiceUnavailable, text.into())
}

fn unavailable_for_workspace(workspace: &Path, config: &super::LspToolConfig) -> ToolError {
    unavailable(super::catalog::unavailable_servers_message(
        workspace,
        &config.servers,
        None,
    ))
}
