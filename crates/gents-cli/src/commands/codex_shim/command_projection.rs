use std::collections::BTreeSet;
use std::path::Path;

use gents_codex_protocol as codex;
use serde_json::Value;

use super::progress::{
    gents_exec_metadata, observed_tool_status, tool_duration_ms, GentsToolCallProgress,
};
use super::projection_state::ProjectionStatus;
pub(super) use super::projection_state::ToolProjectionStatus;
use super::subagent_projection::{collab_projection, is_subagent_control_tool};

#[cfg(test)]
pub(super) fn tool_projection_status(tool: &GentsToolCallProgress) -> ToolProjectionStatus {
    tool_projection_status_with_settled(tool, false, false)
}

pub(super) fn tool_projection_status_with_settled(
    tool: &GentsToolCallProgress,
    projection_settled: bool,
    link_settle_expired: bool,
) -> ToolProjectionStatus {
    let status = observed_tool_status(tool);
    if is_subagent_control_tool(&tool.tool_name) {
        if let Some(projection) = collab_projection(tool) {
            ToolProjectionStatus::Collab(projection)
        } else if status == ProjectionStatus::Failed
            || (projection_settled && link_settle_expired && status == ProjectionStatus::Completed)
        {
            ToolProjectionStatus::Mcp(status)
        } else {
            ToolProjectionStatus::DeferredCollab
        }
    } else if is_gents_file_change_tool(tool) {
        if file_update_change(tool).is_none() {
            ToolProjectionStatus::DeferredFileChange
        } else {
            ToolProjectionStatus::FileChange(status)
        }
    } else if should_project_as_command_execution(tool) {
        ToolProjectionStatus::Command(status)
    } else {
        ToolProjectionStatus::Mcp(status)
    }
}

pub(super) fn codex_patch_status(status: ProjectionStatus) -> codex::PatchApplyStatus {
    match status {
        ProjectionStatus::InProgress => codex::PatchApplyStatus::InProgress,
        ProjectionStatus::Completed => codex::PatchApplyStatus::Completed,
        ProjectionStatus::Failed => codex::PatchApplyStatus::Failed,
    }
}

pub(super) fn codex_mcp_status(status: ProjectionStatus) -> codex::McpToolCallStatus {
    match status {
        ProjectionStatus::InProgress => codex::McpToolCallStatus::InProgress,
        ProjectionStatus::Completed => codex::McpToolCallStatus::Completed,
        ProjectionStatus::Failed => codex::McpToolCallStatus::Failed,
    }
}

pub(super) fn codex_command_status(status: ProjectionStatus) -> codex::CommandExecutionStatus {
    match status {
        ProjectionStatus::InProgress => codex::CommandExecutionStatus::InProgress,
        ProjectionStatus::Completed => codex::CommandExecutionStatus::Completed,
        ProjectionStatus::Failed => codex::CommandExecutionStatus::Failed,
    }
}

pub(super) fn observed_mcp_status(status: &codex::McpToolCallStatus) -> ProjectionStatus {
    match status {
        codex::McpToolCallStatus::InProgress => ProjectionStatus::InProgress,
        codex::McpToolCallStatus::Completed => ProjectionStatus::Completed,
        codex::McpToolCallStatus::Failed => ProjectionStatus::Failed,
    }
}

pub(super) fn observed_command_status(status: &codex::CommandExecutionStatus) -> ProjectionStatus {
    match status {
        codex::CommandExecutionStatus::InProgress => ProjectionStatus::InProgress,
        codex::CommandExecutionStatus::Completed => ProjectionStatus::Completed,
        codex::CommandExecutionStatus::Failed | codex::CommandExecutionStatus::Declined => {
            ProjectionStatus::Failed
        }
    }
}

pub(super) fn observed_patch_status(status: &codex::PatchApplyStatus) -> ProjectionStatus {
    match status {
        codex::PatchApplyStatus::InProgress => ProjectionStatus::InProgress,
        codex::PatchApplyStatus::Completed => ProjectionStatus::Completed,
        codex::PatchApplyStatus::Failed | codex::PatchApplyStatus::Declined => {
            ProjectionStatus::Failed
        }
    }
}

fn is_gents_file_change_tool(tool: &GentsToolCallProgress) -> bool {
    matches!(tool.tool_name.as_str(), "write_file" | "edit_file")
}

fn should_project_as_command_execution(tool: &GentsToolCallProgress) -> bool {
    is_gents_background_tool(tool)
        || matches!(tool.tool_name.as_str(), "bash" | "bash_unrestricted")
        || is_gents_fs_command_tool(tool)
}

fn is_gents_background_tool(tool: &GentsToolCallProgress) -> bool {
    tool.await_mode
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| value == "background")
}

fn is_gents_fs_command_tool(tool: &GentsToolCallProgress) -> bool {
    matches!(
        tool.tool_name.as_str(),
        "read_file" | "list_files" | "glob" | "grep"
    )
}

pub(super) fn update_running_background_tools(
    running: &mut BTreeSet<String>,
    tool: &GentsToolCallProgress,
    status: &ToolProjectionStatus,
) {
    match status {
        ToolProjectionStatus::Command(ProjectionStatus::InProgress)
            if is_gents_background_tool(tool) =>
        {
            running.insert(tool.tool_call_key.clone());
        }
        ToolProjectionStatus::Mcp(_)
        | ToolProjectionStatus::Command(_)
        | ToolProjectionStatus::Collab(_)
        | ToolProjectionStatus::DeferredCollab
        | ToolProjectionStatus::DeferredFileChange
        | ToolProjectionStatus::FileChange(_) => {
            running.remove(&tool.tool_call_key);
        }
    }
}

pub(super) fn file_change_item(
    tool: &GentsToolCallProgress,
    status: codex::PatchApplyStatus,
) -> Option<codex::ThreadItem> {
    Some(codex::ThreadItem::FileChange {
        id: tool.tool_call_key.clone(),
        changes: vec![file_update_change(tool)?],
        status,
    })
}

fn file_update_change(tool: &GentsToolCallProgress) -> Option<codex::FileUpdateChange> {
    let args = serde_json::from_str::<Value>(tool.args.trim()).ok()?;
    let path = args.get("path")?.as_str()?.trim();
    if path.is_empty() {
        return None;
    }

    match tool.tool_name.as_str() {
        "write_file" => {
            let content = args.get("content")?.as_str()?.to_string();
            let metadata = gents_fs_metadata(&tool.result);
            if observed_tool_status(tool) == ProjectionStatus::InProgress && metadata.is_none() {
                return None;
            }
            let created = metadata
                .and_then(|metadata| metadata.get("created").and_then(Value::as_bool))
                .unwrap_or(true);
            let (kind, diff) = if created {
                (codex::PatchChangeKind::Add, content)
            } else {
                (
                    codex::PatchChangeKind::Update { move_path: None },
                    additive_unified_diff(&content),
                )
            };
            Some(codex::FileUpdateChange {
                path: path.to_string(),
                kind,
                diff,
            })
        }
        "edit_file" => {
            let old_text = args.get("old_text")?.as_str()?;
            let new_text = args.get("new_text")?.as_str()?;
            Some(codex::FileUpdateChange {
                path: path.to_string(),
                kind: codex::PatchChangeKind::Update { move_path: None },
                diff: replacement_unified_diff(old_text, new_text),
            })
        }
        _ => None,
    }
}

fn gents_fs_metadata(result: &str) -> Option<Value> {
    let trimmed = result.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(raw) = result
        .lines()
        .next()
        .and_then(|line| line.trim().strip_prefix("gents_fs:"))
    {
        return serde_json::from_str(raw.trim()).ok();
    }
    serde_json::from_str(trimmed).ok()
}

fn additive_unified_diff(content: &str) -> String {
    let new_lines = diff_lines(content);
    let mut diff = format!("@@ -0,0 +{} @@\n", unified_range(1, new_lines.len()));
    for line in new_lines {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    diff
}

fn replacement_unified_diff(old_text: &str, new_text: &str) -> String {
    let old_lines = diff_lines(old_text);
    let new_lines = diff_lines(new_text);
    let mut diff = format!(
        "@@ -{} +{} @@\n",
        unified_range(1, old_lines.len()),
        unified_range(1, new_lines.len())
    );
    for line in old_lines {
        diff.push('-');
        diff.push_str(line);
        diff.push('\n');
    }
    for line in new_lines {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    diff
}

fn diff_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.lines().collect()
    }
}

fn unified_range(start: usize, count: usize) -> String {
    match count {
        0 => format!("{start},0"),
        1 => start.to_string(),
        _ => format!("{start},{count}"),
    }
}

pub(super) fn command_execution_item(
    cwd: &Path,
    tool: &GentsToolCallProgress,
    status: codex::CommandExecutionStatus,
) -> codex::ThreadItem {
    let aggregated_output = command_output_payload(tool);
    let command = command_execution_display(tool);
    let command_actions = command_actions_for_tool(cwd, tool, &command);
    let exit_code = match status {
        codex::CommandExecutionStatus::Completed => Some(0),
        codex::CommandExecutionStatus::Failed => gents_exec_exit_code(&tool.result)
            .filter(|exit_code| *exit_code != 0)
            .or(Some(1)),
        codex::CommandExecutionStatus::InProgress | codex::CommandExecutionStatus::Declined => None,
    };
    let is_background = is_gents_background_tool(tool);
    codex::ThreadItem::CommandExecution {
        id: tool.tool_call_key.clone(),
        command,
        cwd: cwd
            .to_path_buf()
            .try_into()
            .expect("Codex shim cwd must be an absolute path"),
        process_id: is_background.then(|| tool.tool_call_key.clone()),
        source: if is_background {
            codex::CommandExecutionSource::UnifiedExecStartup
        } else {
            codex::CommandExecutionSource::Agent
        },
        status,
        command_actions,
        aggregated_output,
        exit_code,
        duration_ms: tool_duration_ms(tool),
    }
}

fn command_execution_display(tool: &GentsToolCallProgress) -> String {
    if let Some(child_request_id) = tool.child_request_id.as_deref() {
        return format!("spawn_subagent {child_request_id}");
    }
    if let Some(command) = shell_command_from_tool_args(&tool.args) {
        return command;
    }
    if tool.args.trim().is_empty() {
        return format!("spawn_process {}", tool.tool_name);
    }
    format!("{} {}", tool.tool_name, tool.args.trim())
}

fn command_actions_for_tool(
    cwd: &Path,
    tool: &GentsToolCallProgress,
    command: &str,
) -> Vec<codex::CommandAction> {
    let Some(args) = serde_json::from_str::<Value>(tool.args.trim()).ok() else {
        return Vec::new();
    };
    match tool.tool_name.as_str() {
        "read_file" => {
            let Some(path) = args.get("path").and_then(Value::as_str) else {
                return Vec::new();
            };
            vec![codex::CommandAction::Read {
                command: command.to_string(),
                name: display_file_name(path),
                path: absolute_tool_path(cwd, path),
            }]
        }
        "list_files" => vec![codex::CommandAction::ListFiles {
            command: command.to_string(),
            path: optional_path_arg(&args),
        }],
        "glob" | "grep" => {
            let Some(pattern) = args.get("pattern").and_then(Value::as_str) else {
                return Vec::new();
            };
            vec![codex::CommandAction::Search {
                command: command.to_string(),
                query: Some(pattern.to_string()),
                path: optional_path_arg(&args),
            }]
        }
        _ => Vec::new(),
    }
}

fn optional_path_arg(args: &Value) -> Option<String> {
    args.get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
}

fn absolute_tool_path(cwd: &Path, path: &str) -> gents_codex_protocol::AbsolutePathBuf {
    let path = Path::new(path);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    path.try_into()
        .expect("Codex shim filesystem tool path must resolve to an absolute path")
}

fn display_file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
        .to_string()
}

fn shell_command_from_tool_args(raw_args: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(raw_args.trim()).ok()?;
    let command = value.get("command")?.as_str()?.trim();
    if command.is_empty() {
        return None;
    }
    let Some(args) = value
        .get("args")
        .and_then(Value::as_array)
        .filter(|args| !args.is_empty())
    else {
        return Some(command.to_string());
    };
    let argv = std::iter::once(command.to_string())
        .chain(args.iter().filter_map(Value::as_str).map(ToOwned::to_owned))
        .collect::<Vec<_>>();
    Some(shell_join(&argv))
}

pub(super) fn command_output_payload(tool: &GentsToolCallProgress) -> Option<String> {
    let trimmed = tool.result.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("gents_exec:") {
        let mut lines = tool.result.lines();
        let _metadata = lines.next();
        let rest = lines.collect::<Vec<_>>().join("\n");
        if !rest.trim().is_empty() {
            return Some(rest);
        }
    }
    Some(tool.result.clone())
}

fn gents_exec_exit_code(result: &str) -> Option<i32> {
    let metadata = gents_exec_metadata(result)?;
    metadata
        .get("exit_code")
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok())
}

fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| {
            if arg.is_empty()
                || arg
                    .chars()
                    .any(|ch| ch.is_whitespace() || matches!(ch, '"' | '\'' | '\\' | '$' | '`'))
            {
                format!("'{}'", arg.replace('\'', "'\\''"))
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settled_unresolved_subagent_control_falls_back_to_visible_mcp() {
        let tool = test_tool(
            "spawn_subagent",
            "completed",
            r#"{"name":"reviewer","prompt":"inspect"}"#,
        )
        .with_result(r#"{"child_request_id":"child-request"}"#);

        assert_eq!(
            tool_projection_status(&tool),
            ToolProjectionStatus::DeferredCollab
        );
        assert_eq!(
            tool_projection_status_with_settled(&tool, true, false),
            ToolProjectionStatus::DeferredCollab
        );
        assert_eq!(
            tool_projection_status_with_settled(&tool, true, true),
            ToolProjectionStatus::Mcp(ProjectionStatus::Completed)
        );
    }

    #[test]
    fn background_tool_projects_as_codex_unified_exec_startup() {
        let tool =
            test_tool("slow_tool", "running", r#"{"delay_ms":5000}"#).with_await_mode("background");

        assert_eq!(
            tool_projection_status(&tool),
            ToolProjectionStatus::Command(ProjectionStatus::InProgress)
        );

        let item = command_execution_item(
            Path::new("/tmp"),
            &tool,
            codex::CommandExecutionStatus::InProgress,
        );
        let codex::ThreadItem::CommandExecution {
            id,
            command,
            process_id,
            source,
            status,
            aggregated_output,
            ..
        } = item
        else {
            panic!("expected command execution item");
        };
        assert_eq!(id, "session:call");
        assert_eq!(command, r#"slow_tool {"delay_ms":5000}"#);
        assert_eq!(process_id.as_deref(), Some("session:call"));
        assert_eq!(source, codex::CommandExecutionSource::UnifiedExecStartup);
        assert_eq!(status, codex::CommandExecutionStatus::InProgress);
        assert_eq!(aggregated_output, None);
    }

    #[test]
    fn completed_background_tool_carries_output_delta_payload() {
        let mut tool = test_tool("slow_tool", "completed", "{}")
            .with_await_mode("background")
            .with_result("done");
        tool.latency_ms = Some(41);

        assert_eq!(
            tool_projection_status(&tool),
            ToolProjectionStatus::Command(ProjectionStatus::Completed)
        );

        let item = command_execution_item(
            Path::new("/tmp"),
            &tool,
            codex::CommandExecutionStatus::Completed,
        );
        let codex::ThreadItem::CommandExecution {
            status,
            aggregated_output,
            exit_code,
            duration_ms,
            ..
        } = item
        else {
            panic!("expected command execution item");
        };
        assert_eq!(status, codex::CommandExecutionStatus::Completed);
        assert_eq!(aggregated_output.as_deref(), Some("done"));
        assert_eq!(exit_code, Some(0));
        assert_eq!(duration_ms, Some(41));
    }

    #[test]
    fn bash_tool_projects_as_codex_agent_command_execution() {
        let tool = test_tool(
            "bash_unrestricted",
            "running",
            r#"{"command":"cargo test -p gents-cli","timeout_secs":600}"#,
        );

        assert_eq!(
            tool_projection_status(&tool),
            ToolProjectionStatus::Command(ProjectionStatus::InProgress)
        );

        let item = command_execution_item(
            Path::new("/tmp"),
            &tool,
            codex::CommandExecutionStatus::InProgress,
        );
        let codex::ThreadItem::CommandExecution {
            command,
            process_id,
            source,
            status,
            ..
        } = item
        else {
            panic!("expected command execution item");
        };
        assert_eq!(command, "cargo test -p gents-cli");
        assert_eq!(process_id, None);
        assert_eq!(source, codex::CommandExecutionSource::Agent);
        assert_eq!(status, codex::CommandExecutionStatus::InProgress);
    }

    #[test]
    fn gents_exec_exit_nonzero_projects_as_failed_command_execution() {
        let mut tool = test_tool("bash_unrestricted", "failed", r#"{"command":"false"}"#)
            .with_result(r#"gents_exec: {"ok":false,"status":"exit_nonzero","exit_code":42}"#);
        tool.tool_failure_class = Some("toolReturnedError".to_string());

        assert_eq!(observed_tool_status(&tool), ProjectionStatus::Failed);

        let item = command_execution_item(
            Path::new("/tmp"),
            &tool,
            codex::CommandExecutionStatus::Failed,
        );
        let codex::ThreadItem::CommandExecution {
            status, exit_code, ..
        } = item
        else {
            panic!("expected command execution item");
        };
        assert_eq!(status, codex::CommandExecutionStatus::Failed);
        assert_eq!(exit_code, Some(42));
    }

    #[test]
    fn durable_completed_command_does_not_leak_failure_shaped_exit_code() {
        let tool = test_tool("bash_unrestricted", "completed", r#"{"command":"true"}"#)
            .with_result(r#"gents_exec: {"ok":false,"status":"exit_nonzero","exit_code":42}"#);

        assert_eq!(observed_tool_status(&tool), ProjectionStatus::Completed);
        let item = command_execution_item(
            Path::new("/tmp"),
            &tool,
            codex::CommandExecutionStatus::Completed,
        );
        let codex::ThreadItem::CommandExecution { exit_code, .. } = item else {
            panic!("expected command execution item");
        };
        assert_eq!(exit_code, Some(0));
    }

    #[test]
    fn read_file_projects_as_native_exploring_command() {
        let tool = test_tool(
            "read_file",
            "completed",
            r#"{"path":"crates/gents/proofs/Proofs/Process.lean"}"#,
        );

        assert_eq!(
            tool_projection_status(&tool),
            ToolProjectionStatus::Command(ProjectionStatus::Completed)
        );

        let item = command_execution_item(
            Path::new("/repo"),
            &tool,
            codex::CommandExecutionStatus::Completed,
        );
        let codex::ThreadItem::CommandExecution {
            source,
            command_actions,
            ..
        } = item
        else {
            panic!("expected command execution item");
        };
        assert_eq!(source, codex::CommandExecutionSource::Agent);
        let [codex::CommandAction::Read { name, path, .. }] = command_actions.as_slice() else {
            panic!("expected read command action: {command_actions:?}");
        };
        assert_eq!(name, "Process.lean");
        assert_eq!(
            path.as_path(),
            Path::new("/repo/crates/gents/proofs/Proofs/Process.lean")
        );
    }

    #[test]
    fn list_and_search_tools_project_as_native_exploring_commands() {
        let list = test_tool("list_files", "running", r#"{"path":"crates"}"#);
        let list_item = command_execution_item(
            Path::new("/repo"),
            &list,
            codex::CommandExecutionStatus::InProgress,
        );
        let codex::ThreadItem::CommandExecution {
            command_actions, ..
        } = list_item
        else {
            panic!("expected command execution item");
        };
        assert!(matches!(
            command_actions.as_slice(),
            [codex::CommandAction::ListFiles { path: Some(path), .. }] if path == "crates"
        ));

        let grep = test_tool(
            "grep",
            "completed",
            r#"{"pattern":"RequestState","path":"crates/gents/proofs"}"#,
        );
        let grep_item = command_execution_item(
            Path::new("/repo"),
            &grep,
            codex::CommandExecutionStatus::Completed,
        );
        let codex::ThreadItem::CommandExecution {
            command_actions, ..
        } = grep_item
        else {
            panic!("expected command execution item");
        };
        assert!(matches!(
            command_actions.as_slice(),
            [codex::CommandAction::Search { query: Some(query), path: Some(path), .. }]
                if query == "RequestState" && path == "crates/gents/proofs"
        ));
    }

    #[test]
    fn write_and_edit_tools_project_as_native_file_changes() {
        let write = test_tool(
            "write_file",
            "completed",
            r###"{"path":"README.md","content":"## Summary\n\nThe CodexShim projection"}"###,
        )
        .with_result(
            r#"gents_fs: {"ok":true,"status":"success","tool":"write_file","path":"README.md","created":true}"#,
        );

        assert_eq!(
            tool_projection_status(&write),
            ToolProjectionStatus::FileChange(ProjectionStatus::Completed)
        );

        let write_item =
            file_change_item(&write, codex::PatchApplyStatus::Completed).expect("file change item");
        let codex::ThreadItem::FileChange {
            changes, status, ..
        } = write_item
        else {
            panic!("expected file change item");
        };
        assert_eq!(status, codex::PatchApplyStatus::Completed);
        assert!(matches!(
            changes.as_slice(),
            [codex::FileUpdateChange { path, kind: codex::PatchChangeKind::Add, diff }]
                if path == "README.md" && diff == "## Summary\n\nThe CodexShim projection"
        ));

        let overwrite = test_tool(
            "write_file",
            "completed",
            r#"{"path":"README.md","content":"replacement\n"}"#,
        )
        .with_result(
            r#"gents_fs: {"ok":true,"status":"success","tool":"write_file","path":"README.md","created":false}"#,
        );
        let overwrite_item = file_change_item(&overwrite, codex::PatchApplyStatus::Completed)
            .expect("file change item");
        let codex::ThreadItem::FileChange { changes, .. } = overwrite_item else {
            panic!("expected file change item");
        };
        assert!(matches!(
            changes.as_slice(),
            [codex::FileUpdateChange {
                path,
                kind: codex::PatchChangeKind::Update { move_path: None },
                diff,
            }] if path == "README.md" && diff.contains("+replacement")
        ));

        let running_write = test_tool(
            "write_file",
            "running",
            r#"{"path":"README.md","content":"replacement\n"}"#,
        );
        assert_eq!(
            tool_projection_status(&running_write),
            ToolProjectionStatus::DeferredFileChange
        );

        let edit = test_tool(
            "edit_file",
            "running",
            r#"{"path":"src/lib.rs","old_text":"old","new_text":"new"}"#,
        );

        assert_eq!(
            tool_projection_status(&edit),
            ToolProjectionStatus::FileChange(ProjectionStatus::InProgress)
        );

        let edit_item =
            file_change_item(&edit, codex::PatchApplyStatus::Completed).expect("file change item");
        let codex::ThreadItem::FileChange { changes, .. } = edit_item else {
            panic!("expected file change item");
        };
        assert!(matches!(
            changes.as_slice(),
            [codex::FileUpdateChange {
                path,
                kind: codex::PatchChangeKind::Update { move_path: None },
                diff,
            }] if path == "src/lib.rs" && diff.contains("-old") && diff.contains("+new")
        ));
    }

    fn test_tool(tool_name: &str, status: &str, args: &str) -> GentsToolCallProgress {
        GentsToolCallProgress {
            tool_call_key: "session:call".to_string(),
            tool_name: tool_name.to_string(),
            lifecycle_state: Some(status.to_string()),
            await_mode: None,
            child_request_id: None,
            args: args.to_string(),
            result: String::new(),
            subagent_link: None,
            ..Default::default()
        }
    }

    trait ToolTestExt {
        fn with_await_mode(self, await_mode: &str) -> Self;
        fn with_result(self, result: &str) -> Self;
    }

    impl ToolTestExt for GentsToolCallProgress {
        fn with_await_mode(mut self, await_mode: &str) -> Self {
            self.await_mode = Some(await_mode.to_string());
            self
        }

        fn with_result(mut self, result: &str) -> Self {
            self.result = result.to_string();
            self
        }
    }
}
