use serde_json::{json, Value};

use crate::tools::native::shared::{ToolContext, ToolError};

use super::super::client::LspClient;
use super::super::edits::{resolve_inbound_path, walk_uris};
use super::{arg_invalid, policy, READ_REQUEST_METHODS};

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
        m if m.starts_with("textDocument/")
            && params
                .pointer("/textDocument/uri")
                .and_then(Value::as_str)
                .is_none() =>
        {
            return Err(arg_invalid("textDocument.uri required"));
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

pub(crate) async fn request_maybe_retry(
    client: &LspClient,
    method: &str,
    params: Value,
    timeout: std::time::Duration,
    max_timeout: std::time::Duration,
    retry_empty: bool,
) -> Result<Value, String> {
    const MAX_EMPTY_ATTEMPTS: usize = 3;
    let deadline = std::time::Instant::now() + timeout;
    let mut attempts = 0;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Ok(Value::Null);
        }
        let result = client
            .request_with_timeout(method, params.clone(), remaining)
            .await?;
        if !retry_empty || !semantic_result_is_empty(&result) {
            return Ok(result);
        }
        if client.wait_while_busy(deadline).await {
            if std::time::Instant::now() >= deadline {
                return Err(format!(
                    "LSP request {method} timed out while the language server is indexing; call lsp again with a larger timeout (maximum {} seconds)",
                    max_timeout.as_secs()
                ));
            }
            continue;
        }
        attempts += 1;
        if attempts == MAX_EMPTY_ATTEMPTS {
            return Ok(result);
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        tokio::time::sleep(remaining.min(std::time::Duration::from_millis(400))).await;
    }
}

fn semantic_result_is_empty(result: &Value) -> bool {
    result.is_null() || result.as_array().is_some_and(Vec::is_empty)
}

pub(super) async fn resolve_code_action_edit(
    client: &LspClient,
    result: &Value,
    query: Option<&str>,
) -> Result<Value, ToolError> {
    let mut action = if let Some(arr) = result.as_array() {
        select_code_action(arr, query)?.clone()
    } else {
        result.clone()
    };
    if action.get("edit").is_none() && action.get("data").is_some() {
        action = client
            .request("codeAction/resolve", action.clone())
            .await
            .map_err(|err| arg_invalid(format!("code action resolve failed: {err}")))?;
    }
    if action.get("command").is_some() && action.get("edit").is_none() {
        return Err(arg_invalid("bare Command code actions are not executed"));
    }
    action
        .get("edit")
        .cloned()
        .ok_or_else(|| arg_invalid("code action has no edit"))
}

pub(crate) fn select_code_action<'a>(
    actions: &'a [Value],
    query: Option<&str>,
) -> Result<&'a Value, ToolError> {
    let selected = match query.map(str::trim).filter(|q| !q.is_empty()) {
        Some(query) => {
            if let Ok(index) = query.parse::<usize>() {
                actions
                    .get(index)
                    .or_else(|| index.checked_sub(1).and_then(|idx| actions.get(idx)))
            } else {
                let needle = query.to_ascii_lowercase();
                actions.iter().find(|item| {
                    item.get("title")
                        .and_then(Value::as_str)
                        .is_some_and(|title| title.to_ascii_lowercase().contains(&needle))
                })
            }
        }
        None => actions
            .iter()
            .find(|item| item.get("edit").is_some())
            .or_else(|| {
                actions
                    .iter()
                    .find(|item| item.get("data").is_some() && item.get("command").is_none())
            }),
    };
    selected.ok_or_else(|| arg_invalid("no matching CodeAction.edit to apply"))
}
