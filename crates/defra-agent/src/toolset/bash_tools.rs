use std::time::Duration;

use anyhow::Result;
use rig::completion::ToolDefinition;
use rig::tool::Tool;

use super::args::BashArgs;
use super::shared::{
    run_command, validate_command_policy, CommandExecutionPolicy, ToolContext, ToolError,
};

#[derive(Clone)]
pub(super) struct ReadOnlyBashTool {
    context: ToolContext,
    default_timeout: Duration,
    policy: CommandExecutionPolicy,
}

impl ReadOnlyBashTool {
    #[cfg(test)]
    pub(super) fn new(
        context: ToolContext,
        default_timeout: Duration,
        allowlist: Vec<String>,
    ) -> Self {
        Self {
            context,
            default_timeout,
            policy: CommandExecutionPolicy::read_only(allowlist),
        }
    }

    pub(super) fn with_policy(
        context: ToolContext,
        default_timeout: Duration,
        policy: CommandExecutionPolicy,
    ) -> Self {
        Self {
            context,
            default_timeout,
            policy,
        }
    }
}

#[derive(Clone)]
pub(super) struct UnrestrictedBashTool {
    context: ToolContext,
    default_timeout: Duration,
    policy: CommandExecutionPolicy,
}

impl UnrestrictedBashTool {
    #[cfg(test)]
    pub(super) fn new(context: ToolContext, default_timeout: Duration) -> Self {
        Self {
            context,
            default_timeout,
            policy: CommandExecutionPolicy::write_capable(),
        }
    }

    pub(super) fn with_policy(
        context: ToolContext,
        default_timeout: Duration,
        policy: CommandExecutionPolicy,
    ) -> Self {
        Self {
            context,
            default_timeout,
            policy,
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
            description: "Run a single read-only command under the allowed root. Relative cwd values resolve from the active request workspace when one is provided, otherwise from the root. Returns compact text with first-line defra_exec metadata. Set raw_json=true for structured JSON."
                .to_string(),
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
                        "description": "Working directory under the allowed root. Omit for the active workspace/root."
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "default": self.default_timeout.as_secs(),
                        "minimum": 1,
                        "maximum": self.default_timeout.as_secs(),
                        "description": "Timeout in seconds; higher values are capped by the tool."
                    },
                    "raw_json": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, return structured JSON instead of the compact default text."
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        validate_command_policy(&args.command, &args.args, &self.policy)?;
        run_command(
            &self.context,
            &args.command,
            &args.args,
            args.cwd.as_deref(),
            Duration::from_secs(args.timeout_secs.max(1)).min(self.default_timeout),
            &self.policy,
            args.raw_json,
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
            description: "Run a write-capable command under the configured writable root. Relative cwd values resolve from the active request workspace when one is provided, otherwise from the root. On macOS the default policy uses sandbox-exec to contain writes to that root; an explicit unrestricted command policy is unsandboxed. If args is empty, command may be a shell command string; if args is present, command is treated as an executable name or path. Returns compact text with first-line defra_exec metadata. Set raw_json=true for structured JSON."
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
                        "description": "Working directory under the configured writable root. Omit for the active workspace/root."
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "default": self.default_timeout.as_secs(),
                        "minimum": 1,
                        "maximum": self.default_timeout.as_secs(),
                        "description": "Timeout in seconds; higher values are capped by the tool."
                    },
                    "raw_json": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, return structured JSON instead of the compact default text."
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

        validate_command_policy(command, &command_args, &self.policy)?;
        run_command(
            &self.context,
            command,
            &command_args,
            args.cwd.as_deref(),
            Duration::from_secs(args.timeout_secs.max(1)).min(self.default_timeout),
            &self.policy,
            args.raw_json,
        )
        .await
        .map_err(Into::into)
    }
}
