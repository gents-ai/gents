use std::sync::Arc;

use gents_loop::tool::{BoxFuture, ToolCallEnvelope};

/// Whether a committed Tools write reaches the request whose config call made
/// it, from that request's next turn (Lean `RuntimeReconcile.ToolRebinding`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolsRebinding {
    /// The request's own Tools document did not change.
    NotOwn,
    /// The next turn is sent the toolset a fresh request would get.
    Rebound {
        added: Vec<String>,
        removed: Vec<String>,
    },
    /// The agent cannot run with the committed Tools; the request keeps its
    /// tools.
    Unrunnable { reason: String },
    /// No generation holding the change was activated within the wait; the
    /// request keeps its tools.
    Unpublished,
}

/// The running request's own toolset, rebound after its config call commits
/// a Tools write. Implemented by the runtime that runs the request.
pub trait OwnToolsRebinding: Send + Sync {
    fn after_tools_commit(&self) -> BoxFuture<'_, ToolsRebinding>;
}

/// The envelope extension carrying the request's [`OwnToolsRebinding`].
#[derive(Clone)]
pub struct RequestToolsRebinding(pub Arc<dyn OwnToolsRebinding>);

/// What a committed Tools write by the call `envelope` serves does to its own
/// request: [`ToolsRebinding::NotOwn`] outside a running request.
pub async fn after_tools_commit(envelope: &ToolCallEnvelope) -> ToolsRebinding {
    match envelope.extensions().get::<RequestToolsRebinding>() {
        Some(rebinding) => rebinding.0.after_tools_commit().await,
        None => ToolsRebinding::NotOwn,
    }
}
