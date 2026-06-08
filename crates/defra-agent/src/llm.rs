//! Defra-native LLM type vocabulary.
//!
//! These types mirror the rig types defra-agent used to depend on, so that the
//! runtime, hook, persistence, and tool surfaces speak Defra-owned types rather
//! than rig's. rig is being confined to the provider/streaming-parsing layer
//! ("Layer A"); the [`rig_compat`] module converts between these native types
//! and rig's at that single boundary and will be removed once Layer A is owned.
//!
//! Shapes intentionally mirror rig 1:1 for now (mechanical conversion); they can
//! be simplified once rig is gone. See
//! `docs/design/native-llm-types-shed-rig.md`.

/// Outcome of a hook callback for a completion/tool-result event: continue the
/// loop, or terminate it early with a reason. Mirrors rig's `HookAction`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookAction {
    /// Continue loop execution as normal.
    Continue,
    /// Terminate the loop early.
    Terminate { reason: String },
}

impl HookAction {
    /// Continue the loop.
    pub fn cont() -> Self {
        Self::Continue
    }

    /// Terminate the loop early with `reason`.
    pub fn terminate(reason: impl Into<String>) -> Self {
        Self::Terminate {
            reason: reason.into(),
        }
    }
}

/// Outcome of the pre-execution tool-call hook: run the tool, skip it (returning
/// the reason as the tool result), or terminate the loop. Mirrors rig's
/// `ToolCallHookAction`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCallHookAction {
    /// Run the tool as normal.
    Continue,
    /// Skip execution; `reason` becomes the tool result.
    Skip { reason: String },
    /// Terminate the loop early.
    Terminate { reason: String },
}

impl ToolCallHookAction {
    /// Run the tool as normal.
    pub fn cont() -> Self {
        Self::Continue
    }

    /// Skip execution; `reason` becomes the tool result.
    pub fn skip(reason: impl Into<String>) -> Self {
        Self::Skip {
            reason: reason.into(),
        }
    }

    /// Terminate the loop early with `reason`.
    pub fn terminate(reason: impl Into<String>) -> Self {
        Self::Terminate {
            reason: reason.into(),
        }
    }
}
