use super::*;
pub struct SchemaArgumentProbe {
    pub name: String,
    pub schema: serde_json::Value,
    pub admitted: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    pub dispatched: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
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
        self.admitted
            .lock()
            .unwrap()
            .push(serde_json::from_str(args).unwrap());
        Ok(())
    }

    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            self.dispatched
                .lock()
                .unwrap()
                .push(serde_json::from_str(&args).unwrap());
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok("accepted".into())
        })
    }
}
