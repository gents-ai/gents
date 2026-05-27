use std::collections::BTreeMap;
use std::path::Path;

use codex_app_server_protocol as codex;
use serde_json::Value;

use super::progress::{defra_exec_metadata, defra_tool_call_status, DefraToolCallProgress};

#[derive(Clone, Debug, PartialEq)]
pub(super) enum ToolProjectionStatus {
    Mcp(codex::McpToolCallStatus),
    Command(codex::CommandExecutionStatus),
}

impl ToolProjectionStatus {
    pub(super) fn command_status(&self) -> codex::CommandExecutionStatus {
        match self {
            Self::Mcp(_) => codex::CommandExecutionStatus::InProgress,
            Self::Command(status) => status.clone(),
        }
    }
}

pub(super) fn tool_projection_status(tool: &DefraToolCallProgress) -> ToolProjectionStatus {
    let status = defra_tool_call_status(tool);
    if should_project_as_command_execution(tool) {
        ToolProjectionStatus::Command(command_status_from_mcp_status(status))
    } else {
        ToolProjectionStatus::Mcp(status)
    }
}

fn should_project_as_command_execution(tool: &DefraToolCallProgress) -> bool {
    is_defra_background_tool(tool)
        || matches!(tool.tool_name.as_str(), "bash" | "bash_unrestricted")
        || is_defra_fs_exploration_tool(tool)
}

fn is_defra_background_tool(tool: &DefraToolCallProgress) -> bool {
    tool.await_mode
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| value == "background")
}

fn is_defra_fs_exploration_tool(tool: &DefraToolCallProgress) -> bool {
    matches!(
        tool.tool_name.as_str(),
        "read_file" | "list_files" | "glob" | "grep"
    )
}

fn command_status_from_mcp_status(
    status: codex::McpToolCallStatus,
) -> codex::CommandExecutionStatus {
    match status {
        codex::McpToolCallStatus::InProgress => codex::CommandExecutionStatus::InProgress,
        codex::McpToolCallStatus::Completed => codex::CommandExecutionStatus::Completed,
        codex::McpToolCallStatus::Failed => codex::CommandExecutionStatus::Failed,
    }
}

pub(super) fn update_running_background_tools(
    running: &mut BTreeMap<String, codex::CommandExecutionStatus>,
    tool: &DefraToolCallProgress,
    status: &ToolProjectionStatus,
) {
    match status {
        ToolProjectionStatus::Command(codex::CommandExecutionStatus::InProgress)
            if is_defra_background_tool(tool) =>
        {
            running.insert(
                tool.tool_call_key.clone(),
                codex::CommandExecutionStatus::InProgress,
            );
        }
        _ => {
            running.remove(&tool.tool_call_key);
        }
    }
}

pub(super) fn command_execution_item(
    cwd: &Path,
    tool: &DefraToolCallProgress,
    status: codex::CommandExecutionStatus,
) -> codex::ThreadItem {
    let aggregated_output = command_output_payload(tool);
    let command = command_execution_display(tool);
    let command_actions = command_actions_for_tool(cwd, tool, &command);
    let exit_code = match status {
        codex::CommandExecutionStatus::Completed => defra_exec_exit_code(&tool.result).or(Some(0)),
        codex::CommandExecutionStatus::Failed => defra_exec_exit_code(&tool.result).or(Some(1)),
        codex::CommandExecutionStatus::InProgress | codex::CommandExecutionStatus::Declined => None,
    };
    let is_background = is_defra_background_tool(tool);
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
        duration_ms: None,
    }
}

fn command_execution_display(tool: &DefraToolCallProgress) -> String {
    if let Some(child_request_id) = tool.child_request_id.as_deref() {
        return format!("spawn_subagent {child_request_id}");
    }
    if let Some(command) = shell_command_from_tool_args(&tool.args) {
        return command;
    }
    if tool.args.trim().is_empty() {
        return format!("background_tool {}", tool.tool_name);
    }
    format!("{} {}", tool.tool_name, tool.args.trim())
}

fn command_actions_for_tool(
    cwd: &Path,
    tool: &DefraToolCallProgress,
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

fn absolute_tool_path(cwd: &Path, path: &str) -> codex_utils_absolute_path::AbsolutePathBuf {
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

pub(super) fn command_output_payload(tool: &DefraToolCallProgress) -> Option<String> {
    let trimmed = tool.result.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("defra_exec:") {
        let mut lines = tool.result.lines();
        let _metadata = lines.next();
        let rest = lines.collect::<Vec<_>>().join("\n");
        if !rest.trim().is_empty() {
            return Some(rest);
        }
    }
    Some(tool.result.clone())
}

fn defra_exec_exit_code(result: &str) -> Option<i32> {
    let metadata = defra_exec_metadata(result)?;
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
    fn background_tool_projects_as_codex_unified_exec_startup() {
        let tool =
            test_tool("slow_tool", "running", r#"{"delay_ms":5000}"#).with_await_mode("background");

        assert_eq!(
            tool_projection_status(&tool),
            ToolProjectionStatus::Command(codex::CommandExecutionStatus::InProgress)
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
        let tool = test_tool("slow_tool", "completed", "{}")
            .with_await_mode("background")
            .with_result("done");

        assert_eq!(
            tool_projection_status(&tool),
            ToolProjectionStatus::Command(codex::CommandExecutionStatus::Completed)
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
            ..
        } = item
        else {
            panic!("expected command execution item");
        };
        assert_eq!(status, codex::CommandExecutionStatus::Completed);
        assert_eq!(aggregated_output.as_deref(), Some("done"));
        assert_eq!(exit_code, Some(0));
    }

    #[test]
    fn bash_tool_projects_as_codex_agent_command_execution() {
        let tool = test_tool(
            "bash_unrestricted",
            "running",
            r#"{"command":"cargo test -p defra-agent-cli","timeout_secs":600}"#,
        );

        assert_eq!(
            tool_projection_status(&tool),
            ToolProjectionStatus::Command(codex::CommandExecutionStatus::InProgress)
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
        assert_eq!(command, "cargo test -p defra-agent-cli");
        assert_eq!(process_id, None);
        assert_eq!(source, codex::CommandExecutionSource::Agent);
        assert_eq!(status, codex::CommandExecutionStatus::InProgress);
    }

    #[test]
    fn defra_exec_exit_nonzero_projects_as_failed_command_execution() {
        let tool = test_tool("bash_unrestricted", "completed", r#"{"command":"false"}"#)
            .with_result(r#"defra_exec: {"ok":false,"status":"exit_nonzero","exit_code":42}"#);

        assert_eq!(
            defra_tool_call_status(&tool),
            codex::McpToolCallStatus::Failed
        );

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
    fn read_file_projects_as_native_exploring_command() {
        let tool = test_tool(
            "read_file",
            "completed",
            r#"{"path":"crates/defra-agent/proofs/Proofs/Process.lean"}"#,
        );

        assert_eq!(
            tool_projection_status(&tool),
            ToolProjectionStatus::Command(codex::CommandExecutionStatus::Completed)
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
            Path::new("/repo/crates/defra-agent/proofs/Proofs/Process.lean")
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
            r#"{"pattern":"RequestState","path":"crates/defra-agent/proofs"}"#,
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
                if query == "RequestState" && path == "crates/defra-agent/proofs"
        ));
    }

    fn test_tool(tool_name: &str, status: &str, args: &str) -> DefraToolCallProgress {
        DefraToolCallProgress {
            tool_call_key: "session:call".to_string(),
            tool_name: tool_name.to_string(),
            status: status.to_string(),
            lifecycle_state: Some(status.to_string()),
            await_mode: None,
            child_request_id: None,
            args: args.to_string(),
            result: String::new(),
        }
    }

    trait ToolTestExt {
        fn with_await_mode(self, await_mode: &str) -> Self;
        fn with_result(self, result: &str) -> Self;
    }

    impl ToolTestExt for DefraToolCallProgress {
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
