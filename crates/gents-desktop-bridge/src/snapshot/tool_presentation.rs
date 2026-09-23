use serde_json::{Map, Value};

use super::super::types::{ToolCallView, ToolDiffLineKind, ToolDiffLineView, ToolPresentationView};

const COMMAND_TOOLS: &[&str] = &["bash", "bash_unrestricted", "gents_exec", "exec_command"];
const FILE_READ_TOOLS: &[&str] = &["read_file", "grep", "glob", "list_files"];
const FILE_EDIT_TOOLS: &[&str] = &["write_file", "edit_file"];
const SUBAGENT_TOOLS: &[&str] = &[
    "spawn_subagent",
    "wait_subagent",
    "list_subagents",
    "read_subagent",
    "steer_subagent",
    "cancel_subagent",
];
const PROCESS_TOOLS: &[&str] = &[
    "spawn_process",
    "wait_process",
    "list_processes",
    "read_process",
    "cancel_process",
];

pub(super) fn project_tool_presentation(tool: &ToolCallView) -> ToolPresentationView {
    let name = normalized_tool_name(tool.tool_name.as_deref().unwrap_or("tool"));
    if COMMAND_TOOLS.contains(&name) {
        return project_command(tool);
    }
    if FILE_READ_TOOLS.contains(&name) {
        return project_file_read(tool, name);
    }
    if FILE_EDIT_TOOLS.contains(&name) {
        return project_file_edit(tool, name);
    }
    if SUBAGENT_TOOLS.contains(&name) {
        return project_subagent(tool, name);
    }
    if PROCESS_TOOLS.contains(&name) {
        return project_process(tool, name);
    }
    if name == "call_tool" {
        return project_mcp(tool);
    }
    project_generic(tool)
}

fn normalized_tool_name(name: &str) -> &str {
    name.trim()
        .rsplit_once('.')
        .map(|(_, tail)| tail)
        .unwrap_or(name.trim())
}

fn json_object(value: Option<&str>) -> Option<Map<String, Value>> {
    serde_json::from_str::<Value>(value?)
        .ok()?
        .as_object()
        .cloned()
}

fn string_field(map: Option<&Map<String, Value>>, key: &str) -> Option<String> {
    map?.get(key)?.as_str().map(str::to_string)
}

fn i64_field(map: Option<&Map<String, Value>>, key: &str) -> Option<i64> {
    map?.get(key)?.as_i64()
}

fn bool_field(map: Option<&Map<String, Value>>, key: &str) -> Option<bool> {
    map?.get(key)?.as_bool()
}

fn json_field(map: Option<&Map<String, Value>>, key: &str) -> Option<String> {
    let value = map?.get(key)?;
    match value {
        Value::Null => None,
        Value::String(value) => Some(value.clone()),
        _ => serde_json::to_string_pretty(value).ok(),
    }
}

fn clean_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn redact_sensitive(value: String) -> String {
    if looks_sensitive(&value) {
        "[redacted sensitive input]".to_string()
    } else {
        value
    }
}

fn redact_sensitive_optional(value: Option<String>) -> Option<String> {
    value.map(redact_sensitive)
}

fn redact_sensitive_output(value: String) -> String {
    if looks_sensitive(&value) {
        "[redacted sensitive output]".to_string()
    } else {
        value
    }
}

fn redact_sensitive_output_optional(value: Option<String>) -> Option<String> {
    value.map(redact_sensitive_output)
}

fn split_envelope<'a>(value: &'a str, prefix: &str) -> (Option<Map<String, Value>>, &'a str) {
    let Some(head) = value.strip_prefix(prefix) else {
        return (None, value);
    };
    let Some(newline) = head.find('\n') else {
        return (json_object(Some(head)), "");
    };
    (json_object(Some(&head[..newline])), &head[newline + 1..])
}

fn bare_result_object(value: &str) -> Option<Map<String, Value>> {
    let map = json_object(Some(value))?;
    (map.get("status").is_some() || map.get("ok").is_some()).then_some(map)
}

#[derive(Default)]
struct Streams {
    stdout: String,
    stderr: String,
}

fn returned_bytes(meta: Option<&Map<String, Value>>, key: &str) -> Option<(usize, bool)> {
    let object = meta?.get(key)?.as_object()?;
    Some((
        object.get("returned_bytes")?.as_u64()? as usize,
        object
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    ))
}

fn split_command_streams(body: &str, meta: Option<&Map<String, Value>>) -> Streams {
    const STDOUT_PREFIX: &str = "stdout:\n";
    const STDERR_MARKER: &str = "\nstderr:\n";
    if !body.starts_with(STDOUT_PREFIX) {
        return Streams {
            stdout: normalize_stream(body),
            stderr: String::new(),
        };
    }

    let bytes = body.as_bytes();
    if let Some((returned, false)) = returned_bytes(meta, "stdout_truncation") {
        let length = if returned == 0 {
            "(empty)".len()
        } else {
            returned
        };
        if let Some(streams) = split_at_marker(bytes, STDOUT_PREFIX.len() + length) {
            return streams;
        }
    }
    if let Some((returned, false)) = returned_bytes(meta, "stderr_truncation") {
        let length = if returned == 0 {
            "(empty)".len()
        } else {
            returned
        };
        if bytes.len() >= length + STDERR_MARKER.len() {
            if let Some(streams) =
                split_at_marker(bytes, bytes.len() - length - STDERR_MARKER.len())
            {
                return streams;
            }
        }
    }

    match body.find(STDERR_MARKER) {
        Some(at) => Streams {
            stdout: normalize_stream(&body[STDOUT_PREFIX.len()..at]),
            stderr: normalize_stream(&body[at + STDERR_MARKER.len()..]),
        },
        None => Streams {
            stdout: normalize_stream(&body[STDOUT_PREFIX.len()..]),
            stderr: String::new(),
        },
    }
}

fn split_at_marker(bytes: &[u8], marker_start: usize) -> Option<Streams> {
    const STDOUT_PREFIX: &str = "stdout:\n";
    const STDERR_MARKER: &str = "\nstderr:\n";
    let marker_end = marker_start.checked_add(STDERR_MARKER.len())?;
    if marker_start < STDOUT_PREFIX.len()
        || marker_end > bytes.len()
        || &bytes[marker_start..marker_end] != STDERR_MARKER.as_bytes()
    {
        return None;
    }
    Some(Streams {
        stdout: normalize_stream(
            std::str::from_utf8(&bytes[STDOUT_PREFIX.len()..marker_start]).ok()?,
        ),
        stderr: normalize_stream(std::str::from_utf8(&bytes[marker_end..]).ok()?),
    })
}

fn normalize_stream(value: &str) -> String {
    let stripped = strip_ansi(value).trim_end().to_string();
    if stripped == "(empty)" {
        String::new()
    } else {
        stripped
    }
}

fn strip_ansi(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            output.push(character);
            continue;
        }
        match chars.peek().copied() {
            Some('[') => {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                let mut previous_escape = false;
                for next in chars.by_ref() {
                    if next == '\u{7}' || (previous_escape && next == '\\') {
                        break;
                    }
                    previous_escape = next == '\u{1b}';
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    output
}

fn project_command(tool: &ToolCallView) -> ToolPresentationView {
    let args = json_object(tool.args.as_deref());
    let raw_result = tool.result.as_deref().unwrap_or_default();
    let (envelope_meta, body) = split_envelope(raw_result, "gents_exec: ");
    let meta = envelope_meta.or_else(|| bare_result_object(raw_result));
    let streams = if raw_result.starts_with("gents_exec: ") {
        split_command_streams(body, meta.as_ref())
    } else if meta.is_some() {
        Streams {
            stdout: normalize_stream(&string_field(meta.as_ref(), "stdout").unwrap_or_default()),
            stderr: normalize_stream(&string_field(meta.as_ref(), "stderr").unwrap_or_default()),
        }
    } else {
        Streams::default()
    };
    let status = string_field(meta.as_ref(), "status");
    let exit_code = i64_field(meta.as_ref(), "exit_code");
    let timed_out = bool_field(meta.as_ref(), "timed_out") == Some(true)
        || status.as_deref() == Some("timeout");
    let failed = tool_status_is_error(tool)
        || bool_field(meta.as_ref(), "ok") == Some(false)
        || timed_out
        || status.as_deref() == Some("exit_nonzero")
        || exit_code.is_some_and(|code| code != 0);
    let parsed_output = raw_result.is_empty() || meta.is_some();
    ToolPresentationView::Command {
        command: redact_sensitive(
            string_field(meta.as_ref(), "command")
                .or_else(|| string_field(args.as_ref(), "command"))
                .unwrap_or_else(|| {
                    tool.tool_name
                        .clone()
                        .unwrap_or_else(|| "command".to_string())
                }),
        ),
        exit_code,
        timed_out,
        failed,
        duration_ms: i64_field(meta.as_ref(), "duration_ms"),
        cwd: string_field(meta.as_ref(), "cwd").or_else(|| string_field(args.as_ref(), "cwd")),
        execution_mode: string_field(meta.as_ref(), "execution_mode"),
        network_mode: string_field(meta.as_ref(), "network_mode"),
        stdout: redact_sensitive_output(streams.stdout),
        stderr: redact_sensitive_output(streams.stderr),
        fallback_output: redact_sensitive_output_optional(
            (!parsed_output).then(|| raw_result.to_string()),
        ),
    }
}

fn project_file_read(tool: &ToolCallView, operation: &str) -> ToolPresentationView {
    let args = json_object(tool.args.as_deref());
    let raw_result = tool.result.as_deref().unwrap_or_default();
    let (meta, body) = split_envelope(raw_result, "gents_fs: ");
    ToolPresentationView::FileRead {
        operation: operation.to_string(),
        target: redact_sensitive_optional(
            string_field(meta.as_ref(), "path")
                .or_else(|| string_field(args.as_ref(), "path"))
                .or_else(|| string_field(args.as_ref(), "pattern")),
        ),
        returned_count: i64_field(meta.as_ref(), "returned_count"),
        total_count: i64_field(meta.as_ref(), "total_count"),
        truncated: bool_field(meta.as_ref(), "truncated") == Some(true),
        body: redact_sensitive_output(
            meta.as_ref()
                .map(|_| body.trim_end().to_string())
                .unwrap_or_default(),
        ),
        fallback_output: redact_sensitive_output_optional(
            (meta.is_none())
                .then(|| raw_result.to_string())
                .filter(|v| !v.is_empty()),
        ),
    }
}

fn diff_line(kind: ToolDiffLineKind, text: &str) -> ToolDiffLineView {
    ToolDiffLineView {
        kind,
        text: redact_sensitive(text.trim_end_matches('\r').to_string()),
    }
}

fn text_lines(value: &str) -> Vec<&str> {
    let value = value.trim_end_matches(['\r', '\n']);
    if value.is_empty() {
        return Vec::new();
    }
    value.split('\n').collect()
}

fn applied_diff(body: &str) -> Vec<ToolDiffLineView> {
    body.lines()
        .filter_map(|line| {
            let kind = match line.as_bytes().first()? {
                b'+' => ToolDiffLineKind::Added,
                b'-' => ToolDiffLineKind::Removed,
                b' ' => ToolDiffLineKind::Context,
                _ => return None,
            };
            let (number, text) = line[1..].split_once(" |")?;
            let number = number.trim_start();
            if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            Some(diff_line(kind, text.strip_prefix(' ').unwrap_or(text)))
        })
        .collect()
}

fn requested_diff(old: &str, new: &str, operation: Option<&str>) -> Vec<ToolDiffLineView> {
    let old = text_lines(old);
    let new = text_lines(new);
    let lines = |kind, lines: &[&str]| {
        lines
            .iter()
            .map(|line| diff_line(kind, line))
            .collect::<Vec<_>>()
    };
    match operation {
        Some("insert_after") => [
            lines(ToolDiffLineKind::Context, &old),
            lines(ToolDiffLineKind::Added, &new),
        ]
        .concat(),
        Some("insert_before") => [
            lines(ToolDiffLineKind::Added, &new),
            lines(ToolDiffLineKind::Context, &old),
        ]
        .concat(),
        Some("delete") => lines(ToolDiffLineKind::Removed, &old),
        _ => {
            let prefix = old
                .iter()
                .zip(&new)
                .take_while(|(old, new)| old == new)
                .count();
            let suffix = old[prefix..]
                .iter()
                .rev()
                .zip(new[prefix..].iter().rev())
                .take_while(|(old, new)| old == new)
                .count();
            [
                lines(ToolDiffLineKind::Context, &old[..prefix]),
                lines(ToolDiffLineKind::Removed, &old[prefix..old.len() - suffix]),
                lines(ToolDiffLineKind::Added, &new[prefix..new.len() - suffix]),
                lines(ToolDiffLineKind::Context, &old[old.len() - suffix..]),
            ]
            .concat()
        }
    }
}

fn project_file_edit(tool: &ToolCallView, operation: &str) -> ToolPresentationView {
    let args = json_object(tool.args.as_deref());
    let raw_result = tool.result.as_deref().unwrap_or_default();
    let (envelope_meta, body) = split_envelope(raw_result, "gents_fs: ");
    let applied_body = envelope_meta.as_ref().map(|_| body.to_string());
    let meta = envelope_meta.or_else(|| bare_result_object(raw_result));
    let applied = applied_body
        .or_else(|| string_field(meta.as_ref(), "diff"))
        .map(|body| applied_diff(&body))
        .unwrap_or_default();
    let diff = if operation == "write_file" {
        string_field(args.as_ref(), "content")
            .map(|content| {
                text_lines(&content)
                    .into_iter()
                    .map(|line| diff_line(ToolDiffLineKind::Added, line))
                    .collect()
            })
            .unwrap_or_default()
    } else if !applied.is_empty() {
        applied
    } else {
        requested_diff(
            &string_field(args.as_ref(), "old_text").unwrap_or_default(),
            &string_field(args.as_ref(), "new_text").unwrap_or_default(),
            string_field(args.as_ref(), "operation").as_deref(),
        )
    };
    ToolPresentationView::FileEdit {
        operation: operation.to_string(),
        path: redact_sensitive_optional(
            string_field(args.as_ref(), "path").or_else(|| string_field(meta.as_ref(), "path")),
        ),
        created: bool_field(meta.as_ref(), "created"),
        replacements_applied: i64_field(meta.as_ref(), "replacements_applied"),
        diff,
        fallback_output: redact_sensitive_output_optional(
            (meta.is_none())
                .then(|| raw_result.to_string())
                .filter(|v| !v.is_empty()),
        ),
    }
}

fn action_label(name: &str, suffix: &str) -> String {
    match name {
        "list_subagents" | "list_processes" => "list".to_string(),
        _ => name.strip_suffix(suffix).unwrap_or(name).replace('_', " "),
    }
}

fn project_subagent(tool: &ToolCallView, name: &str) -> ToolPresentationView {
    let args = json_object(tool.args.as_deref());
    let result = json_object(tool.result.as_deref());
    let child_request_id = tool
        .child_request_id
        .clone()
        .or_else(|| string_field(args.as_ref(), "child_request_id"))
        .or_else(|| string_field(result.as_ref(), "child_request_id"));
    let description = redact_sensitive_optional(
        string_field(args.as_ref(), "prompt")
            .or_else(|| string_field(args.as_ref(), "message"))
            .or_else(|| string_field(args.as_ref(), "reason")),
    );
    ToolPresentationView::Subagent {
        action: action_label(name, "_subagent"),
        name: string_field(args.as_ref(), "name"),
        child_request_id,
        description,
        output: redact_sensitive_output_optional(clean_text(tool.result.as_deref())),
    }
}

fn project_process(tool: &ToolCallView, name: &str) -> ToolPresentationView {
    let args = json_object(tool.args.as_deref());
    let result = json_object(tool.result.as_deref());
    let target = redact_sensitive_optional(
        string_field(args.as_ref(), "tool_name")
            .or_else(|| string_field(args.as_ref(), "tool_call_id"))
            .or_else(|| string_field(result.as_ref(), "tool_call_id")),
    );
    let description = redact_sensitive_optional(
        json_field(args.as_ref(), "args").or_else(|| string_field(args.as_ref(), "reason")),
    );
    ToolPresentationView::Process {
        action: action_label(name, "_process"),
        target,
        description,
        output: redact_sensitive_output_optional(clean_text(tool.result.as_deref())),
    }
}

fn project_mcp(tool: &ToolCallView) -> ToolPresentationView {
    let args = json_object(tool.args.as_deref());
    ToolPresentationView::Mcp {
        service_id: string_field(args.as_ref(), "service_id"),
        selected_tool_name: string_field(args.as_ref(), "tool_name"),
        arguments: redact_sensitive_optional(json_field(args.as_ref(), "arguments")),
        output: redact_sensitive_output_optional(clean_text(tool.result.as_deref())),
    }
}

fn project_generic(tool: &ToolCallView) -> ToolPresentationView {
    let args = json_object(tool.args.as_deref());
    let summary = [
        "path",
        "file_path",
        "directory",
        "cwd",
        "pattern",
        "query",
        "command",
    ]
    .into_iter()
    .find_map(|key| string_field(args.as_ref(), key))
    .filter(|value| !looks_sensitive(value));
    ToolPresentationView::Generic {
        summary,
        input: redact_sensitive_optional(clean_text(tool.args.as_deref())),
        output: redact_sensitive_output_optional(clean_text(tool.result.as_deref())),
    }
}

fn looks_sensitive(value: &str) -> bool {
    let normalized = value.to_ascii_lowercase();
    [
        "api_key",
        "api-key",
        "access_token",
        "access-token",
        "refresh_token",
        "refresh-token",
        "password",
        "passwd",
        "secret",
        "authorization",
        "bearer ",
        "cookie",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

fn tool_status_is_error(tool: &ToolCallView) -> bool {
    matches!(
        tool.lifecycle_state
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "failed" | "error" | "cancelled" | "timedout"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, args: &str, result: &str, state: &str) -> ToolCallView {
        ToolCallView {
            tool_call_key: "tool-1".into(),
            request_id: Some("request-1".into()),
            message_sequence: Some(2),
            tool_name: Some(name.into()),
            tool_call_id: Some("tool-1".into()),
            args: Some(args.into()),
            partial_output_tail: None,
            result: Some(result.into()),
            reconstruction: crate::types::MessageReconstructionView {
                state: crate::types::ReconstructionState::Ready,
                error: None,
                denied_dependency_doc_id: None,
            },
            status: Some(state.into()),
            lifecycle_state: Some(state.into()),
            child_request_id: None,
            await_mode: None,
            cancel_policy: None,
            started_at: None,
            deadline_at: None,
            completed_at: None,
            denial: None,
            cancel_cause: None,
        }
    }

    fn diff_of(projected: ToolPresentationView) -> Vec<(ToolDiffLineKind, String)> {
        let ToolPresentationView::FileEdit { diff, .. } = projected else {
            panic!("expected a file edit presentation");
        };
        diff.into_iter()
            .map(|line| (line.kind, line.text))
            .collect()
    }

    fn edit_file_result(content: &str, old_text: &str, new_text: &str) -> String {
        use gents::toolset::edit_match::{decide, EditOutcome, EditRequest, MatchMode, Operation};
        let EditOutcome::Applied {
            diff, replacements, ..
        } = decide(
            content,
            &EditRequest {
                old_text,
                new_text,
                replace_all: false,
                operation: Operation::Replace,
                match_mode: MatchMode::Ladder,
            },
        )
        else {
            panic!("edit did not apply");
        };
        format!(
            "gents_fs: {{\"ok\":true,\"status\":\"success\",\"tool\":\"edit_file\",\"path\":\"src/lib.rs\",\"replacements_applied\":{replacements}}}\nedit_file: edited src/lib.rs ({replacements} replacement, strategy exact)\n{diff}"
        )
    }

    #[test]
    fn write_file_lines_are_added() {
        let projected = project_tool_presentation(&tool(
            "write_file",
            r##"{"path":"notes.md","content":"# Notes\nfirst\n"}"##,
            "gents_fs: {\"ok\":true,\"status\":\"success\",\"tool\":\"write_file\",\"path\":\"notes.md\",\"created\":true}\nwrite_file: wrote 15 bytes to notes.md",
            "completed",
        ));
        assert_eq!(
            diff_of(projected),
            vec![
                (ToolDiffLineKind::Added, "# Notes".to_string()),
                (ToolDiffLineKind::Added, "first".to_string()),
            ]
        );
    }

    #[test]
    fn edit_file_classifies_added_changed_and_removed_lines() {
        let content = "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n";
        let old_text = "fn b() {}\nfn c() {}";
        let new_text = "fn b() {}\nfn c2() {}\nfn e() {}";
        let projected = project_tool_presentation(&tool(
            "edit_file",
            &serde_json::json!({ "path": "src/lib.rs", "old_text": old_text, "new_text": new_text })
                .to_string(),
            &edit_file_result(content, old_text, new_text),
            "completed",
        ));
        assert_eq!(
            diff_of(projected),
            vec![
                (ToolDiffLineKind::Context, "fn a() {}".to_string()),
                (ToolDiffLineKind::Context, "fn b() {}".to_string()),
                (ToolDiffLineKind::Removed, "fn c() {}".to_string()),
                (ToolDiffLineKind::Added, "fn c2() {}".to_string()),
                (ToolDiffLineKind::Added, "fn e() {}".to_string()),
                (ToolDiffLineKind::Context, "fn d() {}".to_string()),
                (ToolDiffLineKind::Context, "".to_string()),
            ]
        );
    }

    #[test]
    fn edit_file_removal_is_only_removed_lines() {
        let content = "keep\ndrop me\nkeep too\n";
        let projected = project_tool_presentation(&tool(
            "edit_file",
            r#"{"path":"src/lib.rs","old_text":"drop me\n","new_text":""}"#,
            &edit_file_result(content, "drop me\n", ""),
            "completed",
        ));
        let diff = diff_of(projected);
        assert_eq!(
            diff.iter()
                .filter(|(kind, _)| *kind != ToolDiffLineKind::Context)
                .cloned()
                .collect::<Vec<_>>(),
            vec![(ToolDiffLineKind::Removed, "drop me".to_string())]
        );
    }

    #[test]
    fn edit_file_without_an_applied_diff_keeps_shared_lines_as_context() {
        let projected = project_tool_presentation(&tool(
            "edit_file",
            r#"{"path":"src/lib.rs","old_text":"anchor\nold\ntail","new_text":"anchor\nnew\ntail"}"#,
            "",
            "running",
        ));
        assert_eq!(
            diff_of(projected),
            vec![
                (ToolDiffLineKind::Context, "anchor".to_string()),
                (ToolDiffLineKind::Removed, "old".to_string()),
                (ToolDiffLineKind::Added, "new".to_string()),
                (ToolDiffLineKind::Context, "tail".to_string()),
            ]
        );
    }

    #[test]
    fn diff_line_kinds_serialize_in_the_design_vocabulary() {
        assert_eq!(
            serde_json::to_value([
                ToolDiffLineKind::Added,
                ToolDiffLineKind::Removed,
                ToolDiffLineKind::Context
            ])
            .unwrap(),
            serde_json::json!(["added", "removed", "context"])
        );
    }

    #[test]
    fn command_projection_is_stable_across_terminal_states() {
        let projected = project_tool_presentation(&tool(
            "bash",
            r#"{"command":"cargo test"}"#,
            "gents_exec: {\"ok\":false,\"status\":\"exit_nonzero\",\"command\":\"cargo test\",\"exit_code\":1}\nstdout:\nran tests\nstderr:\nfailed",
            "failed",
        ));
        assert!(matches!(
            projected,
            ToolPresentationView::Command {
                failed: true,
                exit_code: Some(1),
                ref stdout,
                ref stderr,
                ..
            } if stdout == "ran tests" && stderr == "failed"
        ));
    }

    #[test]
    fn mcp_wrapper_projects_selected_identity() {
        let projected = project_tool_presentation(&tool(
            "call_tool",
            r#"{"service_id":"github","tool_name":"search_issues","arguments":{"query":"sync"}}"#,
            "two results",
            "completed",
        ));
        assert!(matches!(
            projected,
            ToolPresentationView::Mcp {
                service_id: Some(ref service),
                selected_tool_name: Some(ref name),
                ..
            } if service == "github" && name == "search_issues"
        ));
    }

    #[test]
    fn generic_summary_does_not_surface_credentials() {
        let projected = project_tool_presentation(&tool(
            "web_request",
            r#"{"command":"curl -H 'Authorization: Bearer secret' example.com"}"#,
            "done",
            "completed",
        ));
        assert!(matches!(
            projected,
            ToolPresentationView::Generic {
                summary: None,
                input: Some(ref input),
                ..
            } if input == "[redacted sensitive input]"
        ));
    }

    #[test]
    fn command_summary_redacts_credentials() {
        let projected = project_tool_presentation(&tool(
            "bash",
            r#"{"command":"curl -H 'Authorization: Bearer secret' example.com"}"#,
            "",
            "completed",
        ));
        assert!(matches!(
            projected,
            ToolPresentationView::Command { ref command, .. }
                if command == "[redacted sensitive input]"
        ));
    }

    #[test]
    fn command_output_redaction_names_the_output() {
        let projected = project_tool_presentation(&tool(
            "bash",
            r#"{"command":"env"}"#,
            "gents_exec: {\"ok\":true,\"status\":\"success\",\"command\":\"env\",\"exit_code\":0}\nstdout:\nAPI_KEY=abc\nstderr:\n",
            "completed",
        ));
        assert!(matches!(
            projected,
            ToolPresentationView::Command { ref command, ref stdout, .. }
                if command == "env" && stdout == "[redacted sensitive output]"
        ));
    }

    #[test]
    fn subagent_summary_redacts_credentials() {
        let projected = project_tool_presentation(&tool(
            "spawn_subagent",
            r#"{"prompt":"Use Authorization: Bearer secret"}"#,
            "",
            "completed",
        ));
        assert!(matches!(
            projected,
            ToolPresentationView::Subagent { description: Some(ref value), .. }
                if value == "[redacted sensitive input]"
        ));
    }
}
