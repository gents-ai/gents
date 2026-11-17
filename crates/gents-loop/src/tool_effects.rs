pub mod parallel;

use std::sync::Arc;

use crate::tool::{BoxFuture, ToolDefinition};
use crate::tool_call_lifecycle::ToolOutcome;

#[derive(Debug)]
pub enum ToolEffectError {
    Unavailable(String),
    /// A persistence hook terminated the owning execution. This never becomes
    /// a guest result: the plugin invocation must stop before its next round.
    Fatal(String),
}

#[derive(Debug, Clone)]
pub struct ToolEffectCall {
    pub ordinal: u32,
    pub name: String,
    pub arguments: String,
}

pub type EffectResult = Result<ToolOutcome, ToolEffectError>;

/// Implemented by the loop, which retains the selected tools and persistence
/// hook. The guest never receives either object or a way to replace them.
/// Each child runs under the parent tool call's own deadline and cancellation
/// envelope; a child's managed terminal is that child's outcome only.
pub trait ToolEffectDispatcher: Send + Sync {
    fn call<'a>(
        &'a self,
        ordinal: u32,
        name: &'a str,
        arguments: &'a str,
    ) -> BoxFuture<'a, Result<ToolOutcome, ToolEffectError>>;

    /// Dispatchers without a trusted footprint owner retain serial execution.
    fn batch<'a>(&'a self, calls: Vec<ToolEffectCall>) -> BoxFuture<'a, Vec<EffectResult>> {
        Box::pin(async move {
            let mut results = Vec::with_capacity(calls.len());
            for call in calls {
                let result = self.call(call.ordinal, &call.name, &call.arguments).await;
                let fatal = matches!(result, Err(ToolEffectError::Fatal(_)));
                results.push(result);
                if fatal {
                    break;
                }
            }
            results
        })
    }

    /// The definitions of the tools `call` reaches: the request's selected
    /// surface, in its order.
    fn definitions<'a>(&'a self) -> BoxFuture<'a, Vec<ToolDefinition>>;

    /// Settle the children the invocation left unfinished once it has stopped
    /// with `stop`. A stopped invocation drops its in-flight child call, so no
    /// child reaches its own terminal write; its owner calls this before the
    /// invocation's own terminal.
    fn settle_stopped<'a>(&'a self, _stop: &'a ToolOutcome) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

tokio::task_local! {
    static DISPATCHER: Option<Arc<dyn ToolEffectDispatcher>>;
}

pub fn current() -> Option<Arc<dyn ToolEffectDispatcher>> {
    DISPATCHER.try_with(Clone::clone).ok().flatten()
}

pub async fn scope<T>(
    dispatcher: Option<Arc<dyn ToolEffectDispatcher>>,
    future: impl std::future::Future<Output = T>,
) -> T {
    DISPATCHER.scope(dispatcher, future).await
}
