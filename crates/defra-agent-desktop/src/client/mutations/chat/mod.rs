mod binding;
mod conversation;
mod request;

pub use conversation::{create_conversation, rename_conversation, CreatedConversation};
// Re-export the shared interrupt helper from `defra-agent` so the desktop
// client and the runtime share a single GraphQL implementation. Keeping this
// behind the `chat::` module path preserves the existing public surface.
// `fetch_interrupt_requested_at` is reachable directly via `defra_agent::` for
// the conformance test; desktop code only uses `interrupt_request`.
pub use defra_agent::interrupt_request;
pub use request::{
    resend_request, retry_request, submit_request, SubmitRequestOptions, SubmittedRequest,
};

#[cfg(test)]
mod tests;
