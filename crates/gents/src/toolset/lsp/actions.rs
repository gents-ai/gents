use serde_json::{json, Value};

use crate::tool_call_lifecycle::FailureClass;
use crate::toolset::shared::{ToolContext, ToolError};

use super::auth::LspAction;
use super::catalog::{family_eligible, marker_matches, primary_for_file, CatalogServer};
use super::client::LspClient;
use super::edits::{
    acquire_mutation_locks, apply_prepared_with_held_locks, apply_workspace_edit,
    redact_structured_uris, resolve_inbound_path, walk_uris, PreparedEdit,
};
use super::encoding::offset_to_position;
use super::pool::LspLease;

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
        return glob_action(context, client, config, servers, action, file).await;
    }
    let path = resolve_inbound_path(context, file).map_err(|err| policy(err))?;
    let text = std::fs::read_to_string(&path).map_err(|err| {
        ToolError::reported_failure(FailureClass::ArgumentInvalid, err.to_string())
    })?;
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
        let dest_path = resolve_inbound_path(context, dest).map_err(policy)?;
        params = json!({
            "files": [{
                "oldUri": uri,
                "newUri": format!("file://{}", dest_path.display())
            }]
        });
    }
    let result = client
        .request_with_timeout(method, params, request_timeout(req))
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
        let mut applied = apply_workspace_edit(context, client, &edit).await?;
        if matches!(action, LspAction::RenameFile) {
            let dest = req
                .new_name
                .as_deref()
                .ok_or_else(|| arg_invalid("new_name required for rename_file"))?;
            let dest_path = resolve_inbound_path(context, dest).map_err(policy)?;
            if path.exists() && path != dest_path {
                let prepared = vec![PreparedEdit {
                    path: dest_path,
                    new_bytes: Vec::new(),
                    expected_hash: None,
                    version: None,
                    rename_from: Some(path.clone()),
                }];
                let _guards = acquire_mutation_locks(&prepared).await;
                applied += apply_prepared_with_held_locks(context, client, &prepared).await?;
            }
        }
        return Ok(format!("Applied edit to {applied} file(s)"));
    }
    let mut output = truncate_model_output(format_result(context, action, result));
    if matches!(action, LspAction::Diagnostics) {
        if let Some(linter) = run_linter_diagnostics(config, servers, &path).await {
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
            Ok(truncate_model_output(format_result(
                context, action, result,
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
                Ok(result) => Ok(truncate_model_output(format_result(
                    context, action, result,
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
    Ok(truncate_model_output(format_result(
        context, req.action, result,
    )))
}

fn request_timeout(req: &ActionRequest) -> std::time::Duration {
    std::time::Duration::from_secs(req.timeout.unwrap_or(20).clamp(5, 300) as u64)
}

pub fn validate_raw_request(
    context: &ToolContext,
    method: &str,
    payload: Option<&str>,
) -> Result<Value, ToolError> {
    if method == "workspace/executeCommand" {
        return Err(arg_invalid("workspace/executeCommand is not supported"));
    }
    if !READ_REQUEST_METHODS.contains(&method) {
        return Err(arg_invalid(format!("unknown request method {method}")));
    }
    let params = match payload {
        Some(raw) => serde_json::from_str(raw).map_err(|err| arg_invalid(err.to_string()))?,
        None => json!({}),
    };
    if !params.is_object() {
        return Err(arg_invalid("request payload must be a JSON object"));
    }
    validate_known_request_shape(method, &params)?;
    validate_payload_uris(context, &params)?;
    Ok(params)
}

fn validate_known_request_shape(method: &str, params: &Value) -> Result<(), ToolError> {
    let allowed = match method {
        "textDocument/hover"
        | "textDocument/definition"
        | "textDocument/typeDefinition"
        | "textDocument/implementation"
        | "textDocument/documentSymbol"
        | "textDocument/diagnostic" => &["textDocument", "position", "workDoneToken"][..],
        "textDocument/references" => &["textDocument", "position", "context", "workDoneToken"][..],
        "workspace/symbol" => &["query", "workDoneToken"][..],
        "workspace/diagnostic" => &["identifier", "previousResultId", "workDoneToken"][..],
        _ => return Err(arg_invalid(format!("unknown request method {method}"))),
    };
    let obj = params
        .as_object()
        .ok_or_else(|| arg_invalid("request payload must be a JSON object"))?;
    for key in obj.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(arg_invalid(format!("unknown field {key} for {method}")));
        }
    }
    match method {
        "workspace/symbol" => {
            if params.get("query").and_then(Value::as_str).is_none() {
                return Err(arg_invalid("query required for workspace/symbol"));
            }
        }
        m if m.starts_with("textDocument/") => {
            if params
                .pointer("/textDocument/uri")
                .and_then(Value::as_str)
                .is_none()
            {
                return Err(arg_invalid("textDocument.uri required"));
            }
        }
        _ => {}
    }
    Ok(())
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
        return Err(arg_invalid("bare Command code actions are not executed"));
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
    let (redacted, omitted) = redact_structured_uris(context, &result);
    if let Some(arr) = redacted.as_array() {
        let cap = match action {
            LspAction::Diagnostics => MAX_DIAGNOSTICS,
            LspAction::Symbols => MAX_WORKSPACE_SYMBOLS,
            LspAction::References => MAX_REFERENCES,
            LspAction::Rename | LspAction::RenameFile => MAX_RENAME_PAIRS,
            _ => usize::MAX,
        };
        let mut lines = Vec::new();
        for item in arr.iter().take(cap) {
            if let Some(uri) = item
                .pointer("/uri")
                .or_else(|| item.pointer("/targetUri"))
                .or_else(|| item.pointer("/location/uri"))
                .and_then(Value::as_str)
            {
                let line = item
                    .pointer("/range/start/line")
                    .or_else(|| item.pointer("/targetSelectionRange/start/line"))
                    .or_else(|| item.pointer("/location/range/start/line"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    + 1;
                lines.push(format!("{uri}:{line}"));
            } else if !item.is_null() {
                lines.push(item.to_string());
            }
        }
        return finish_location_lines(lines, omitted);
    }
    if let Some(uri) = redacted
        .pointer("/uri")
        .or_else(|| redacted.pointer("/targetUri"))
        .or_else(|| redacted.pointer("/location/uri"))
        .and_then(Value::as_str)
    {
        let line = redacted
            .pointer("/range/start/line")
            .or_else(|| redacted.pointer("/location/range/start/line"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
            + 1;
        return finish_location_lines(vec![format!("{uri}:{line}")], omitted);
    }
    let mut text = redacted.to_string();
    if omitted > 0 {
        text.push_str(&format!(
            "\nomitted {omitted} location(s) outside the allowed workspace"
        ));
    }
    text
}

fn finish_location_lines(lines: Vec<String>, omitted: usize) -> String {
    if lines.is_empty() && omitted > 0 {
        return format!("omitted {omitted} location(s) outside the allowed workspace");
    }
    let mut lines = lines;
    if omitted > 0 {
        lines.push(format!(
            "omitted {omitted} location(s) outside the allowed workspace"
        ));
    }
    if lines.is_empty() {
        "No result".into()
    } else {
        format!("Found {} result(s):\n{}", lines.len(), lines.join("\n"))
    }
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

fn looks_like_glob(file: &str) -> bool {
    file.contains('*') || file.contains('?') || file.contains('{') || file.contains('[')
}

async fn glob_action(
    context: &ToolContext,
    client: &LspClient,
    config: &super::LspToolConfig,
    servers: &[CatalogServer],
    action: LspAction,
    pattern: &str,
) -> Result<String, ToolError> {
    if !matches!(action, LspAction::Diagnostics) {
        return Err(arg_invalid("globs are only valid for diagnostics"));
    }
    let runner = crate::toolset::native_runner::NativeFsRunner::new(context);
    let paths = runner
        .glob_paths(pattern, MAX_GLOB_TARGETS)
        .await
        .unwrap_or_default();
    if paths.is_empty() {
        return Ok("no files matched the diagnostic glob".into());
    }
    let mut sections = Vec::new();
    for path in paths.into_iter().take(MAX_GLOB_TARGETS) {
        let display = context.display_path(&path);
        let uri = format!("file://{}", path.display());
        let text = std::fs::read_to_string(&path).unwrap_or_default();
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
        let result = client
            .request(
                "textDocument/diagnostic",
                json!({ "textDocument": { "uri": uri } }),
            )
            .await
            .unwrap_or(Value::Null);
        let mut section = format!("{display}:\n{}", format_result(context, action, result));
        if let Some(linter) = run_linter_diagnostics(config, servers, &path).await {
            section = format!("{section}\n{linter}");
        }
        sections.push(section);
    }
    Ok(truncate_model_output(sections.join("\n")))
}

async fn run_linter_diagnostics(
    config: &super::LspToolConfig,
    servers: &[CatalogServer],
    path: &std::path::Path,
) -> Option<String> {
    let linter = servers.iter().find(|server| {
        server.is_linter
            && marker_matches(&config.workspace, &server.root_markers)
            && family_eligible(server, &config.workspace)
            && super::catalog::file_type_matches(server, path)
    })?;
    let argv = match linter.name.as_str() {
        "biome" => vec![
            linter.command.clone(),
            "check".into(),
            "--reporter=json".into(),
            path.to_string_lossy().into_owned(),
        ],
        "swiftlint" => vec![
            linter.command.clone(),
            "lint".into(),
            "--quiet".into(),
            path.to_string_lossy().into_owned(),
        ],
        _ => return None,
    };
    let (program, rest, env, _sandbox) = crate::toolset::prepare_managed_command(
        &config.workspace,
        &argv[0],
        &argv[1..],
        &config.constraints,
    )
    .ok()?;
    let mut full = vec![program.to_string_lossy().into_owned()];
    full.extend(rest);
    let outcome = crate::managed_exec::run_managed_exec(crate::managed_exec::ManagedExecRequest {
        argv: full,
        cwd: config.workspace.clone(),
        deadline_at: None,
        cancellation_token: tokio_util::sync::CancellationToken::new(),
        max_output_bytes: 64 * 1024,
        stdin: Vec::new(),
        environment: Some(env),
        tool_name: Some("lsp".into()),
        live_output: None,
    })
    .await;
    match outcome {
        crate::managed_exec::ManagedExecOutcome::Exited { stdout, .. } => {
            let text = String::from_utf8_lossy(&stdout);
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(format!("{}: {trimmed}", linter.name))
            }
        }
        _ => None,
    }
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
