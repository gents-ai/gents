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
                    "command": { "type": "string" },
                    "args": { "type": "array", "items": { "type": "string" } },
                    "cwd": { "type": "string" },
                    "timeout_secs": { "type": "integer" }
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
            description: "Run a command under the configured writable root.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "args": { "type": "array", "items": { "type": "string" } },
                    "cwd": { "type": "string" },
                    "timeout_secs": { "type": "integer" }
                },
                "required": ["command"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
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
