use std::sync::Arc;

use crate::tool::BoxFuture;
use crate::tool_call_lifecycle::ToolOutcome;

#[derive(Debug)]
pub enum ToolEffectError {
    Unavailable(String),
    /// A persistence hook terminated the owning execution. This never becomes
    /// a guest result: the plugin invocation must stop before its next round.
    Fatal(String),
}

/// Implemented by the loop, which retains the selected tools and persistence
/// hook. The guest never receives either object or a way to replace them.
/// Each child runs under the parent tool call's own deadline and cancellation
/// envelope; a child's managed terminal is that child's outcome only.
pub trait ToolEffectDispatcher: Send + Sync {
    /// `parent` is the envelope of the invocation whose child this is.
    fn call<'a>(
        &'a self,
        parent: &'a crate::tool::ToolCallEnvelope,
        ordinal: u32,
        name: &'a str,
        arguments: &'a str,
    ) -> BoxFuture<'a, Result<ToolOutcome, ToolEffectError>>;

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
