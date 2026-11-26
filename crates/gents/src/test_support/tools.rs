use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use gents_loop::tool::{BoxFuture, ToolCallEnvelope, ToolDefinition, ToolDyn, ToolError};
use tokio::sync::{watch, Notify};

#[derive(Clone)]
pub struct FixedOutputTool {
    name: String,
    description: String,
    parameters: serde_json::Value,
    responder: Arc<dyn Fn(&str) -> String + Send + Sync>,
}

impl FixedOutputTool {
    pub fn new(name: impl Into<String>, output: impl Into<String>) -> Self {
        let output = output.into();
        Self {
            name: name.into(),
            description: "Test probe that returns a fixed string".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "note": { "type": "string" } },
                "required": ["note"]
            }),
            responder: Arc::new(move |_| output.clone()),
        }
    }

    pub fn with_call<F>(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
        responder: F,
    ) -> Self
    where
        F: Fn(&str) -> String + Send + Sync + 'static,
    {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            responder: Arc::new(responder),
        }
    }
}

impl ToolDyn for FixedOutputTool {
    fn hidden(&self) -> bool {
        false
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: self.name.clone(),
                description: self.description.clone(),
                parameters: self.parameters.clone(),
            }
        })
    }

    fn call<'a>(
        &'a self,
        args: String,
        _context: &'a ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move { Ok((self.responder)(&args)) })
    }
}

#[derive(Clone)]
enum ReleaseSignal {
    Notify(Arc<Notify>),
    Watch(watch::Sender<bool>),
}

#[derive(Clone)]
pub struct HeldTool {
    name: String,
    description: String,
    parameters: serde_json::Value,
    output: String,
    entered: Arc<Notify>,
    release: ReleaseSignal,
}

impl HeldTool {
    pub fn notified(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
        output: impl Into<String>,
        entered: Arc<Notify>,
        release: Arc<Notify>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            output: output.into(),
            entered,
            release: ReleaseSignal::Notify(release),
        }
    }

    pub fn watched(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
        output: impl Into<String>,
        entered: Arc<Notify>,
        release: watch::Sender<bool>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            output: output.into(),
            entered,
            release: ReleaseSignal::Watch(release),
        }
    }
}

impl ToolDyn for HeldTool {
    fn hidden(&self) -> bool {
        false
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn definition<'a>(&'a self, _: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: self.name.clone(),
                description: self.description.clone(),
                parameters: self.parameters.clone(),
            }
        })
    }

    fn call<'a>(
        &'a self,
        _: String,
        _context: &'a ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            self.entered.notify_one();
            match &self.release {
                ReleaseSignal::Notify(release) => release.notified().await,
                ReleaseSignal::Watch(release) => {
                    release
                        .subscribe()
                        .wait_for(|open| *open)
                        .await
                        .expect("queue gate owner");
                }
            }
            Ok(self.output.clone())
        })
    }
}

#[derive(Clone)]
pub struct PendingTool {
    name: String,
    description: String,
    entered: Option<Arc<AtomicBool>>,
}

impl PendingTool {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            entered: None,
        }
    }

    pub fn with_entered(mut self, entered: Arc<AtomicBool>) -> Self {
        self.entered = Some(entered);
        self
    }
}

impl ToolDyn for PendingTool {
    fn hidden(&self) -> bool {
        false
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: self.name.clone(),
                description: self.description.clone(),
                parameters: serde_json::json!({"type": "object"}),
            }
        })
    }

    fn call<'a>(
        &'a self,
        _args: String,
        _context: &'a ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
        let entered = self.entered.clone();
        Box::pin(async move {
            if let Some(entered) = entered {
                entered.store(true, Ordering::SeqCst);
            }
            std::future::pending().await
        })
    }
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct PendingDefinitionTool {
    name: String,
    entered: Arc<Notify>,
}

#[cfg(test)]
impl PendingDefinitionTool {
    pub(crate) fn new(name: impl Into<String>, entered: Arc<Notify>) -> Self {
        Self {
            name: name.into(),
            entered,
        }
    }
}

#[cfg(test)]
impl ToolDyn for PendingDefinitionTool {
    fn hidden(&self) -> bool {
        false
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn definition<'a>(&'a self, _: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async move {
            self.entered.notify_one();
            std::future::pending().await
        })
    }

    fn call<'a>(
        &'a self,
        _: String,
        _context: &'a ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async { unreachable!("the pre-inference window never dispatches a tool") })
    }
}

#[cfg(test)]
#[derive(Debug, serde::Deserialize)]
pub(crate) struct EchoArgs {
    pub(crate) value: String,
}

#[cfg(test)]
#[derive(Debug, thiserror::Error)]
#[error("echo tool error")]
pub(crate) struct EchoToolError;

#[cfg(test)]
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub(crate) struct EchoTool;

#[cfg(test)]
impl gents_loop::tool::Tool for EchoTool {
    const NAME: &'static str = "echo_value";

    type Error = EchoToolError;
    type Args = EchoArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Echo a value back".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "value": {
                        "type": "string",
                        "description": "Value to echo"
                    }
                },
                "required": ["value"]
            }),
        }
    }

    async fn call(
        &self,
        args: Self::Args,
        _context: &ToolCallEnvelope,
    ) -> Result<Self::Output, Self::Error> {
        Ok(args.value)
    }
}
