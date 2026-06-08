//! Cancellable-tool opt-in trait.
//!
//! Default behavior is non-cancellable: tools run to completion and their
//! results are discarded if the request was interrupted (via
//! `discarded_because_interrupted: true` at the tool-result write site —
//! see Task 3 schema + future tool-result persistence integration).
//!
//! Tools that can observe cancellation (filesystem read, future HTTP
//! fetch, etc.) opt in by overriding BOTH methods. To opt in, override:
//!   - `supports_cancellation` to return `true`
//!   - `call_cancellable` to race the inner work against the cancellation
//!     token
//!
//! Tools that cannot safely observe cancellation (filesystem writes,
//! shell exec, stdio MCP) should NOT override these methods and will
//! accept the default (run-to-completion with results discarded at the
//! persistence layer).
//!
//! # Dispatch integration (deferred)
//!
//! This codebase uses `rig::agent::Agent` for tool dispatch. rig's
//! current API does not expose a hook that allows us to call
//! `call_cancellable` instead of `call` when `supports_cancellation()`
//! is `true`. Until rig gains such a hook — or until we fork/wrap
//! rig's tool-dispatch loop — this trait is infrastructure: it
//! documents which tools COULD observe cancellation, ready to be wired
//! when a dispatch interception point becomes available.
//!
//! Task 7 handles the common case: `request_token` cancels the
//! inference stream directly. Tool invocations in flight when an
//! interrupt fires run to completion; their results are discarded at
//! the persistence layer (see `discarded_because_interrupted` schema
//! field).
//!
//! # Opting in
//!
//! Every `Tool` implementor in this crate should provide an explicit
//! `impl CancellableTool for X` (even if empty — empty uses the
//! defaults). This avoids coherence trouble that would come with a
//! blanket impl and keeps the rule simple: "want cancellation?
//! override both methods. want the default? write an empty
//! `impl CancellableTool for YourTool {}`."

use crate::llm::tool::Tool;
use tokio_util::sync::CancellationToken;

/// Opt-in trait for tools that can observe a cancellation token.
///
/// See the module docs for the opt-in protocol and the current status
/// of dispatch integration.
//
// `dead_code` is allowed until rig exposes a dispatch-interception
// hook — the trait is referenced only by concrete `impl` blocks today
// and not yet called from production code (see module docs).
#[allow(dead_code)]
pub(crate) trait CancellableTool: Tool {
    /// Whether this tool supports cancellation via `call_cancellable`.
    ///
    /// Returning `true` MUST be paired with a meaningful
    /// `call_cancellable` implementation that actually observes the
    /// token (e.g. via `tokio::select!`).
    fn supports_cancellation(&self) -> bool {
        false
    }

    /// Run the tool with an observable cancellation token.
    ///
    /// The default implementation ignores the token and forwards to
    /// `Tool::call`. This default is safe only for tools that return
    /// `supports_cancellation() == false`; the caller is expected to
    /// race the result against the token at a higher layer for those
    /// tools (or simply discard the result if cancellation fired).
    #[allow(async_fn_in_trait)]
    async fn call_cancellable(
        &self,
        args: Self::Args,
        cancel: CancellationToken,
    ) -> Result<Self::Output, Self::Error> {
        let _ = cancel;
        self.call(args).await
    }
}
