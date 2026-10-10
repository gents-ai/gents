//! Invocation-scoped access to the owned tool dispatch boundary.

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
pub trait ToolEffectDispatcher: Send + Sync {
    fn call<'a>(
        &'a self,
        ordinal: u32,
        name: &'a str,
        arguments: &'a str,
        budget: std::time::Duration,
    ) -> BoxFuture<'a, Result<ToolOutcome, ToolEffectError>>;
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
