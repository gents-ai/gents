//! A tool that answers every call with one fixed text.

use crate::tool::{BoxFuture, ToolCallEnvelope, ToolDefinition, ToolDyn, ToolError};

pub struct FixedTool {
    name: &'static str,
    output: &'static str,
}

impl FixedTool {
    pub fn new(name: &'static str, output: &'static str) -> Self {
        Self { name, output }
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
                description: self.name.into(),
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
