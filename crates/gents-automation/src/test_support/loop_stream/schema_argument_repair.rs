use super::*;
pub struct SchemaArgumentProbe {
    pub name: String,
    pub schema: serde_json::Value,
    pub expected: serde_json::Value,
    pub calls: Arc<AtomicUsize>,
}

impl ToolDyn for SchemaArgumentProbe {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: self.name.clone(),
                description: "schema argument fixture".into(),
                parameters: self.schema.clone(),
            }
        })
    }

    fn admit(&self, args: &str) -> Result<(), ToolError> {
        let args: serde_json::Value = serde_json::from_str(args).unwrap();
        assert_eq!(args, self.expected, "{}: admission", self.name);
        Ok(())
    }

    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let args: serde_json::Value = serde_json::from_str(&args).unwrap();
            assert_eq!(args, self.expected, "{}: dispatch", self.name);
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok("accepted".into())
        })
    }
}
