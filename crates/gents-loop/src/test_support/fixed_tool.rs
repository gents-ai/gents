//! A tool that answers every call with one fixed text.

use crate::tool::{BoxFuture, ToolCallEnvelope, ToolDefinition, ToolDyn, ToolError};

pub struct FixedTool {
    name: &'static str,
    description: &'static str,
    output: &'static str,
}

impl FixedTool {
    pub fn new(name: &'static str, output: &'static str) -> Self {
        Self {
            name,
            description: name,
            output,
        }
    }

    pub fn with_description(mut self, description: &'static str) -> Self {
        self.description = description;
        self
    }
}

impl ToolDyn for FixedTool {
    fn hidden(&self) -> bool {
        false
    }

    fn name(&self) -> String {
        self.name.into()
    }

    fn definition<'a>(&'a self, _: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: self.name.into(),
                description: self.description.into(),
                parameters: serde_json::json!({"type":"object"}),
            }
        })
    }

    fn call<'a>(
        &'a self,
        _: String,
        _context: &'a ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async { Ok(self.output.into()) })
    }
}
