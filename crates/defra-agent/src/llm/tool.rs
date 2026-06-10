//! Native tool trait + definition, mirroring rig's `tool::{Tool, ToolDyn,
//! ToolError}` and `completion::ToolDefinition`. defra-agent is not a wasm
//! target, so the wasm-compat bounds reduce to `Send`/`Sync` and the boxed
//! future is a plain [`BoxFuture`].
//!
//! Tools implement [`Tool`] (typed args/output); the blanket impl gives every
//! `Tool` a dyn-safe [`ToolDyn`] (string-in / string-out) that the owned loop
//! dispatches. See `docs/design/native-llm-types-shed-rig.md` (removed from the tree; see git history).

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

/// Boxed, `Send` future — the off-wasm form of rig's `BoxFuture`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A tool's name, description, and JSON-schema parameters, sent to the provider.
/// Mirrors rig's `completion::ToolDefinition`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Error from dyn tool dispatch: the tool itself failed, or args/output failed
/// to de/serialize. Mirrors rig's `tool::ToolError`.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// Error returned by the tool's own `call`.
    #[error("tool call error: {0}")]
    ToolCallError(#[from] Box<dyn std::error::Error + Send + Sync>),
    /// Arguments or output failed to de/serialize.
    #[error("tool json error: {0}")]
    JsonError(#[from] serde_json::Error),
}

/// A typed tool: deserializes `Args`, runs, serializes `Output`. Mirrors rig's
/// `tool::Tool`.
pub trait Tool: Sized + Send + Sync {
    /// Unique tool name.
    const NAME: &'static str;
    /// The tool's error type.
    type Error: std::error::Error + Send + Sync + 'static;
    /// The tool's argument type (deserialized from the model's JSON).
    type Args: for<'a> Deserialize<'a> + Send + Sync;
    /// The tool's output type (serialized back to the model).
    type Output: Serialize;

    /// The tool's name (defaults to [`Tool::NAME`]).
    fn name(&self) -> String {
        Self::NAME.to_string()
    }

    /// The tool's definition; `prompt` may tailor it.
    fn definition(&self, prompt: String) -> impl Future<Output = ToolDefinition> + Send + Sync;

    /// Execute the tool.
    fn call(
        &self,
        args: Self::Args,
    ) -> impl Future<Output = Result<Self::Output, Self::Error>> + Send;
}

/// Dyn-safe, string-in/string-out tool the loop dispatches. Mirrors rig's
/// `tool::ToolDyn`.
pub trait ToolDyn: Send + Sync {
    fn name(&self) -> String;
    fn definition<'a>(&'a self, prompt: String) -> BoxFuture<'a, ToolDefinition>;
    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>>;
}

fn serialize_tool_output(output: impl Serialize) -> serde_json::Result<String> {
    match serde_json::to_value(output)? {
        serde_json::Value::String(text) => Ok(text),
        value => Ok(value.to_string()),
    }
}

impl<T: Tool> ToolDyn for T {
    fn name(&self) -> String {
        Tool::name(self)
    }

    fn definition<'a>(&'a self, prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(<Self as Tool>::definition(self, prompt))
    }

    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            match serde_json::from_str(&args) {
                Ok(args) => <Self as Tool>::call(self, args)
                    .await
                    .map_err(|error| ToolError::ToolCallError(Box::new(error)))
                    .and_then(|output| serialize_tool_output(output).map_err(ToolError::JsonError)),
                Err(error) => Err(ToolError::JsonError(error)),
            }
        })
    }
}
