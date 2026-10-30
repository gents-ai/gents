use serde_json::Value;

use crate::toolset::shared::ToolContext;

use super::super::auth::LspAction;
use super::super::edits::redact_structured_uris;
use super::{
    MAX_DIAGNOSTICS, MAX_REFERENCES, MAX_RENAME_PAIRS, MAX_REQUESTED_RESULTS, MAX_WORKSPACE_SYMBOLS,
};

pub(crate) fn truncate_model_output(text: String) -> String {
    crate::truncation::truncate_text(
        &text,
        crate::truncation::TruncationMode::Head,
        &crate::truncation::TruncationLimits::default(),
    )
    .0
}

#[cfg(test)]
pub(crate) fn format_result(context: &ToolContext, action: LspAction, result: Value) -> String {
    format_result_with_limit(context, action, result, None)
}

pub(crate) fn format_result_with_limit(
    context: &ToolContext,
    action: LspAction,
    result: Value,
    limit: Option<u32>,
) -> String {
    if result.is_null() {
        return match action {
            LspAction::Hover => "No hover information".into(),
            LspAction::Definition => "No definition found".into(),
            LspAction::Symbols => "No symbols found".into(),
            _ => "No result".into(),
        };
    }
    if let Some(contents) = result.pointer("/contents") {
        return flatten_hover(contents);
    }
    let (redacted, omitted) = redact_structured_uris(context, &result);
    if matches!(action, LspAction::Diagnostics) {
        return format_diagnostics(
            &redacted,
            omitted,
            limit.map_or(MAX_DIAGNOSTICS, |v| v.min(MAX_REQUESTED_RESULTS) as usize),
        );
    }
    if let Some(arr) = redacted.as_array() {
        let cap = match action {
            LspAction::Diagnostics => MAX_DIAGNOSTICS,
            LspAction::Symbols => limit.map_or(MAX_WORKSPACE_SYMBOLS, |v| {
                v.min(MAX_REQUESTED_RESULTS) as usize
            }),
            LspAction::References => {
                limit.map_or(MAX_REFERENCES, |v| v.min(MAX_REQUESTED_RESULTS) as usize)
            }
            LspAction::Rename | LspAction::RenameFile => MAX_RENAME_PAIRS,
            _ => usize::MAX,
        };
        let mut lines = Vec::new();
        for item in arr {
            if lines.len() >= cap {
                break;
            }
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
                let name = item.get("name").and_then(Value::as_str);
                let kind = item
                    .get("kind")
                    .and_then(Value::as_u64)
                    .map(symbol_kind_name);
                let container = item.get("containerName").and_then(Value::as_str);
                lines.push(match name {
                    Some(name) => format_symbol_location(name, kind, container, uri, line),
                    None => format!("{uri}:{line}"),
                });
            } else if item.get("name").is_some() {
                collect_document_symbol_lines(item, None, &mut lines, cap);
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

fn collect_document_symbol_lines(
    item: &Value,
    container: Option<&str>,
    lines: &mut Vec<String>,
    cap: usize,
) {
    if lines.len() >= cap {
        return;
    }
    if let Some(name) = item.get("name").and_then(Value::as_str) {
        let line = item
            .pointer("/selectionRange/start/line")
            .or_else(|| item.pointer("/range/start/line"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
            + 1;
        let kind = item
            .get("kind")
            .and_then(Value::as_u64)
            .map(symbol_kind_name);
        let qualified = container
            .map(clean_symbol_container)
            .filter(|parent| !parent.is_empty())
            .map(|parent| format!("{parent}::{name}"))
            .unwrap_or_else(|| name.to_string());
        lines.push(match kind {
            Some(kind) => format!("{qualified} ({kind}):{line}"),
            None => format!("{qualified}:{line}"),
        });
    }
    if let Some(children) = item.get("children").and_then(Value::as_array) {
        for child in children {
            if lines.len() >= cap {
                break;
            }
            let next_container = item.get("name").and_then(Value::as_str).or(container);
            collect_document_symbol_lines(child, next_container, lines, cap);
        }
    }
}

fn clean_symbol_container(container: &str) -> &str {
    container.strip_prefix("impl ").unwrap_or(container).trim()
}

fn format_symbol_location(
    name: &str,
    kind: Option<&str>,
    container: Option<&str>,
    uri: &str,
    line: u64,
) -> String {
    let qualified = container
        .map(clean_symbol_container)
        .filter(|parent| !parent.is_empty())
        .map(|parent| format!("{parent}::{name}"))
        .unwrap_or_else(|| name.to_string());
    match kind {
        Some(kind) => format!("{qualified} ({kind}) {uri}:{line}"),
        None => format!("{qualified} {uri}:{line}"),
    }
}

fn symbol_kind_name(kind: u64) -> &'static str {
    const KINDS: &[&str] = &[
        "unknown",
        "file",
        "module",
        "namespace",
        "package",
        "class",
        "method",
        "property",
        "field",
        "constructor",
        "enum",
        "interface",
        "function",
        "variable",
        "constant",
        "string",
        "number",
        "boolean",
        "array",
        "object",
        "key",
        "null",
        "enum member",
        "struct",
        "event",
        "operator",
        "type parameter",
    ];
    KINDS.get(kind as usize).copied().unwrap_or("unknown")
}

fn format_diagnostics(value: &Value, omitted: usize, limit: usize) -> String {
    let mut diagnostics = Vec::new();
    collect_diagnostics(value, None, &mut diagnostics, limit);
    if diagnostics.is_empty() {
        return if omitted > 0 {
            format!(
                "No diagnostics in the allowed workspace; omitted {omitted} outside-root location(s)"
            )
        } else {
            "No diagnostics".into()
        };
    }
    let count = diagnostics.len();
    let mut text = format!("Found {count} diagnostic(s):\n{}", diagnostics.join("\n"));
    if omitted > 0 {
        text.push_str(&format!(
            "\nomitted {omitted} diagnostic location(s) outside the allowed workspace"
        ));
    }
    text
}

fn collect_diagnostics(
    value: &Value,
    inherited_uri: Option<&str>,
    out: &mut Vec<String>,
    cap: usize,
) {
    if out.len() >= cap {
        return;
    }
    match value {
        Value::Array(items) => {
            for item in items {
                if out.len() >= cap {
                    break;
                }
                collect_diagnostics(item, inherited_uri, out, cap);
            }
        }
        Value::Object(map) => {
            let uri = map.get("uri").and_then(Value::as_str).or(inherited_uri);
            if map.get("message").and_then(Value::as_str).is_some() {
                out.push(format_diagnostic(value, uri));
                return;
            }
            if let Some(items) = map
                .get("diagnostics")
                .or_else(|| map.get("items"))
                .and_then(Value::as_array)
            {
                for item in items {
                    if out.len() >= cap {
                        break;
                    }
                    collect_diagnostics(item, uri, out, cap);
                }
            }
        }
        _ => {}
    }
}

fn format_diagnostic(item: &Value, uri: Option<&str>) -> String {
    let line = item
        .pointer("/range/start/line")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        + 1;
    let location = uri
        .map(|uri| format!("{uri}:{line}"))
        .unwrap_or_else(|| format!("line {line}"));
    let severity = match item.get("severity").and_then(Value::as_u64) {
        Some(1) => "error",
        Some(2) => "warning",
        Some(3) => "information",
        Some(4) => "hint",
        _ => "diagnostic",
    };
    let source = item
        .get("source")
        .and_then(Value::as_str)
        .map(|source| format!(" [{source}]"))
        .unwrap_or_default();
    let code = item
        .get("code")
        .and_then(|code| {
            code.as_str()
                .map(str::to_string)
                .or_else(|| code.as_i64().map(|n| n.to_string()))
        })
        .map(|code| format!(" ({code})"))
        .unwrap_or_default();
    let message = item
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("missing diagnostic message");
    format!("{location}: {severity}{source}{code}: {message}")
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
