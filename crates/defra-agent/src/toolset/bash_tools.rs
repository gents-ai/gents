use std::time::Duration;

use anyhow::Result;
use rig::completion::ToolDefinition;
use rig::tool::Tool;

use super::args::BashArgs;
use super::shared::{run_command, validate_read_only_command, ToolContext, ToolError};

#[derive(Clone)]
pub(super) struct ReadOnlyBashTool {
    context: ToolContext,
    default_timeout: Duration,
    allowlist: Vec<String>,
}

impl ReadOnlyBashTool {
    pub(super) fn new(
        context: ToolContext,
        default_timeout: Duration,
        allowlist: Vec<String>,
    ) -> Self {
        Self {
            context,
            default_timeout,
            allowlist,
        }
    }
}

#[derive(Clone)]
pub(super) struct UnrestrictedBashTool {
    context: ToolContext,
    default_timeout: Duration,
}

impl UnrestrictedBashTool {
    pub(super) fn new(context: ToolContext, default_timeout: Duration) -> Self {
        Self {
            context,
            default_timeout,
        }
    }
}

impl Tool for ReadOnlyBashTool {
    const NAME: &'static str = "bash";

    type Error = ToolError;
    type Args = BashArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Run a single read-only command under the allowed root.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Executable name or path from the read-only command allowlist."
                    },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "default": [],
                        "description": "Exec-style arguments. Do not include the executable name here."
                    },
                    "cwd": {
                        "type": "string",
                        "default": ".",
                        "description": "Working directory under the allowed root. Omit for the root."
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "default": self.default_timeout.as_secs(),
                        "minimum": 1,
                        "maximum": self.default_timeout.as_secs(),
                        "description": "Timeout in seconds; higher values are capped by the tool."
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        validate_read_only_command(&args.command, &args.args, &self.allowlist)?;
        run_command(
            &self.context,
            &args.command,
            &args.args,
            args.cwd.as_deref(),
            Duration::from_secs(args.timeout_secs.max(1)).min(self.default_timeout),
        )
        .await
        .map_err(Into::into)
    }
}

impl Tool for UnrestrictedBashTool {
    const NAME: &'static str = "bash_unrestricted";

    type Error = ToolError;
    type Args = BashArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Run a command under the configured writable root. If args is empty, command may be a shell command string; if args is present, command is treated as an executable name or path."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Executable name/path, or a shell command string when args is empty."
                    },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "default": [],
                        "description": "Arguments for exec-style invocation. Leave empty to run command through /bin/sh -lc."
                    },
                    "cwd": {
                        "type": "string",
                        "default": ".",
                        "description": "Working directory under the configured writable root. Omit for the root."
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "default": self.default_timeout.as_secs(),
                        "minimum": 1,
                        "maximum": self.default_timeout.as_secs(),
                        "description": "Timeout in seconds; higher values are capped by the tool."
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let (command, command_args) = if args.args.is_empty() {
            ("/bin/sh", vec!["-lc".to_string(), args.command.clone()])
        } else {
            (args.command.as_str(), args.args.clone())
        };

        run_command(
            &self.context,
            command,
            &command_args,
            args.cwd.as_deref(),
            Duration::from_secs(args.timeout_secs.max(1)).min(self.default_timeout),
        )
        .await
        .map_err(Into::into)
    }
}
