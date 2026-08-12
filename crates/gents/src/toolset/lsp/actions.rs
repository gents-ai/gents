use serde_json::{json, Value};

use crate::tool_call_lifecycle::FailureClass;
use crate::toolset::shared::{ToolContext, ToolError};

use super::auth::LspAction;
use super::catalog::{primary_for_file, CatalogServer};
use super::client::LspClient;
use super::edits::{
    apply_workspace_edit, redact_outside_root, resolve_inbound_path, walk_uris,
};
use super::encoding::{offset_to_position, PositionEncoding};
use super::pool::LspLease;

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
}

pub async fn dispatch(
    context: &ToolContext,
    lease: Option<&LspLease>,
    pool: &super::pool::LspPool,
    config: &super::LspToolConfig,
    servers: &[CatalogServer],
    req: ActionRequest,
) -> Result<String, ToolError> {
    match req.action {
        LspAction::Status => {
            let session_id = crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
                .and_then(|scope| scope.session_id)
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| config.session_id.clone());
            let ready = pool
                .inspect_session(
                    &session_id,
                    &config.behavior_id,
                    &config.workspace,
                    &config.digest,
                )
                .await;
            Ok(status_text(ready, servers))
        }
        LspAction::Reload => Ok("reload requested for current snapshot".into()),
        LspAction::Capabilities => {
            let lease = lease.ok_or_else(|| unavailable("no language server"))?;
            Ok(lease.client().capabilities().await.to_string())
        }
        action => {
            let lease = lease.ok_or_else(|| unavailable("no language server"))?;
            run_file_action(context, lease.client(), servers, action, &req).await
        }
    }
}

async fn run_file_action(
    context: &ToolContext,
    client: &LspClient,
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
    let path = resolve_inbound_path(context, file).map_err(|err| policy(err))?;
    let text = std::fs::read_to_string(&path)
        .map_err(|err| ToolError::reported_failure(FailureClass::ArgumentInvalid, err.to_string()))?;
    let uri = format!("file://{}", path.display());
    let _ = client
        .notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id(servers, &path),
                    "version": 1,
                    "text": text
                }
            }),
        )
        .await;
    let encoding = client.position_encoding().await;
    let _ = client.track_open(&uri, 1).await;
    let pos = offset_to_position(
        &text,
        encoding,
        req.line.unwrap_or(1),
        req.symbol.as_deref().unwrap_or(""),
    )
    .or_else(|| {
        req.symbol.as_ref().map(|_| {
            super::encoding::LspPosition {
                line: req.line.unwrap_or(1).saturating_sub(1),
                character: 0,
            }
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
    let result = client
        .request(method, params)
        .await
        .map_err(|err| ToolError::reported_failure(FailureClass::ToolReturnedError, err))?;
    if matches!(
        action,
        LspAction::Rename | LspAction::RenameFile | LspAction::CodeActionsApply
    ) && req.apply != Some(false)
    {
        if !super::lsp_apply_authorized(
            true,
            crate::tool_surface::FileToolMode::ReadWrite,
            super::LspMutationSource::ForegroundReturnedEdit,
        ) {
            return Err(policy("lsp apply is not authorized"));
        }
        let edit = if matches!(action, LspAction::CodeActionsApply) {
            extract_code_action_edit(&result)?
        } else {
            result.clone()
        };
        let applied = apply_workspace_edit(context, client, &edit).await?;
        return Ok(format!("Applied edit to {applied} file(s)"));
    }
    Ok(truncate_model_output(format_result(context, action, result)))
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
                .request("workspace/symbol", json!({ "query": query }))
                .await
                .map_err(|err| ToolError::reported_failure(FailureClass::ToolReturnedError, err))?;
            Ok(truncate_model_output(format_result(
                context,
                action,
                result,
            )))
        }
        LspAction::Diagnostics => {
            match client
                .request("workspace/diagnostic", json!({ "identifier": "gents" }))
                .await
            {
                Ok(result) => Ok(truncate_model_output(format_result(context, action, result))),
                Err(_) => Ok(
                    "workspace diagnostics require workspace/diagnostic; pass a file or glob"
                        .into(),
                ),
            }
        }
        _ => Err(arg_invalid("file: * is only valid for diagnostics, symbols, or reload")),
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
    if method == "workspace/executeCommand" {
        return Err(arg_invalid(
            "workspace/executeCommand is not supported",
        ));
    }
    if !READ_REQUEST_METHODS.contains(&method) {
        return Err(arg_invalid(format!("unknown request method {method}")));
    }
    let params = if let Some(payload) = &req.payload {
        let parsed: Value =
            serde_json::from_str(payload).map_err(|err| arg_invalid(err.to_string()))?;
        validate_payload_uris(context, &parsed)?;
        parsed
    } else {
        json!({})
    };
    let result = client
        .request(method, params)
        .await
        .map_err(|err| ToolError::reported_failure(FailureClass::ToolReturnedError, err))?;
    Ok(truncate_model_output(format_result(context, req.action, result)))
}

fn validate_payload_uris(context: &ToolContext, params: &Value) -> Result<(), ToolError> {
    for uri in walk_uris(params) {
        resolve_inbound_path(context, &uri).map_err(policy)?;
    }
    Ok(())
}

fn extract_code_action_edit(result: &Value) -> Result<Value, ToolError> {
    let action = if let Some(arr) = result.as_array() {
        arr.iter()
            .find(|item| item.get("edit").is_some())
            .ok_or_else(|| arg_invalid("no CodeAction.edit to apply"))?
    } else {
        result
    };
    if action.get("command").is_some() && action.get("edit").is_none() {
        return Err(arg_invalid(
            "bare Command code actions are not executed",
        ));
    }
    action
        .get("edit")
        .cloned()
        .ok_or_else(|| arg_invalid("code action has no edit"))
}

fn truncate_model_output(text: String) -> String {
    crate::truncation::truncate_text(
        &text,
        crate::truncation::TruncationMode::Tail,
        &crate::truncation::TruncationLimits::default(),
    )
    .0
}

fn format_result(context: &ToolContext, action: LspAction, result: Value) -> String {
    if result.is_null() {
        return match action {
            LspAction::Hover => "No hover information".into(),
            LspAction::Definition => "No definition found".into(),
            _ => "No result".into(),
        };
    }
    if let Some(contents) = result.pointer("/contents") {
        return flatten_hover(contents);
    }
    if let Some(arr) = result.as_array() {
        let mut lines = Vec::new();
        let mut omitted = 0usize;
        for item in arr {
            if let Some(uri) = item
                .pointer("/uri")
                .or_else(|| item.pointer("/targetUri"))
                .and_then(Value::as_str)
            {
                match redact_outside_root(context, uri) {
                    Some(display) => {
                        let line = item
                            .pointer("/range/start/line")
                            .or_else(|| item.pointer("/targetSelectionRange/start/line"))
                            .and_then(Value::as_u64)
                            .unwrap_or(0)
                            + 1;
                        lines.push(format!("{display}:{line}"));
                    }
                    None => omitted += 1,
                }
            } else {
                lines.push(item.to_string());
            }
        }
        if lines.is_empty() && omitted > 0 {
            return format!("omitted {omitted} location(s) outside the allowed workspace");
        }
        if omitted > 0 {
            lines.push(format!("omitted {omitted} location(s) outside the allowed workspace"));
        }
        return if lines.is_empty() {
            "No result".into()
        } else {
            format!("Found {} result(s):\n{}", lines.len(), lines.join("\n"))
        };
    }
    result.to_string()
}

fn flatten_hover(contents: &Value) -> String {
    if let Some(s) = contents.as_str() {
        return s.to_string();
    }
    if let Some(s) = contents.get("value").and_then(Value::as_str) {
        return s.to_string();
    }
    contents.to_string()
}

fn language_id(servers: &[CatalogServer], path: &std::path::Path) -> String {
    primary_for_file(servers, path)
        .and_then(|s| s.language_id.clone())
        .unwrap_or_else(|| {
            path.extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_else(|| "plaintext".into())
        })
}

fn status_text(ready: Vec<String>, servers: &[CatalogServer]) -> String {
    let names: Vec<String> = servers
        .iter()
        .map(|s| {
            if ready.iter().any(|name| name == &s.name) {
                format!("{} (ready)", s.name)
            } else {
                format!("{} (configured, not started)", s.name)
            }
        })
        .collect();
    if names.is_empty() {
        "No language servers configured for this project".into()
    } else {
        format!("Language servers: {}", names.join(", "))
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
