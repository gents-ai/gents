//! An installed plugin offered to the model as a tool.
//!
//! The model sees the plugin's own name, description and input schema, and
//! supplies only the arguments. Which artifact runs, and with what authority,
//! is the installed record's, resolved when the tool surface is built.

use std::sync::Arc;

use anyhow::Result;

use super::executor::PluginExecutor;
use super::store::InstalledPlugin;
use super::PluginVerdict;
use crate::document_config::PluginToolRef;
use crate::llm::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct PluginToolError(String);

fn tool_error(message: impl Into<String>) -> ToolError {
    ToolError::ToolCallError(Box::new(PluginToolError(message.into())))
}

pub struct PluginTool {
    executor: Arc<PluginExecutor>,
    record: InstalledPlugin,
    definition: ToolDefinition,
}

impl PluginTool {
    /// The tool `plugin` names, refused when it is not installed or no
    /// longer the pinned artifact.
    pub fn resolve(executor: Arc<PluginExecutor>, plugin: &PluginToolRef) -> Result<Self> {
        let record = executor.resolve(&plugin.plugin, plugin.digest.as_deref())?;
        let definition = ToolDefinition {
            name: plugin.tool_name().to_string(),
            description: record.declaration.description.clone(),
            parameters: record.declaration.input_schema.clone(),
        };
        Ok(Self {
            executor,
            record,
            definition,
        })
    }
}

impl ToolDyn for PluginTool {
    fn name(&self) -> String {
        self.definition.name.clone()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async { self.definition.clone() })
    }

    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let input: serde_json::Value = crate::llm::tool::parse_tool_args(&args)?;
            let call = self
                .executor
                .call(&self.record, input)
                .await
                .map_err(|error| tool_error(format!("{error:#}")))?;
            match call.outcome.verdict {
                PluginVerdict::Success => {
                    serde_json::to_string(&call.outcome.output).map_err(ToolError::JsonError)
                }
                _ => Err(tool_error(format!(
                    "plugin {} did not return a result: {}",
                    call.coordinate, call.outcome.diagnostics
                ))),
            }
        })
    }
}
