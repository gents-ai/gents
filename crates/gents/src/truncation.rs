use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;

mod spill;
#[cfg(test)]
mod tests;

// The pure head/tail clamp moved to `gents-loop` (G-1): the loop needs it with
// no storage side effect, and this crate re-exports it so `crate::truncation`
// keeps every symbol callers already use.
pub use gents_loop::truncation::{
    truncate, truncate_text, tool_result_truncation_mode, TextTruncation, TruncationLimits,
    TruncationMode, TruncationTrigger, LIVE_STREAM_CAPACITY_BYTES,
};

#[derive(Debug, Clone)]
pub struct TruncationResult {
    pub text: String,
    pub truncated: bool,
    pub truncated_by: Option<TruncationTrigger>,
    pub original_lines: usize,
    pub original_bytes: usize,
    pub spill_doc_id: Option<String>,
}

pub trait Truncator: Send + Sync {
    fn truncate(
        &self,
        tool_name: &str,
        tool_input: &str,
        output: &str,
        mode: TruncationMode,
        limits: &TruncationLimits,
        tool_call_doc_id: Option<&str>,
    ) -> impl std::future::Future<Output = Result<TruncationResult>> + Send;
}

pub struct DefraSpillTruncator {
    node: Arc<EmbeddedNode>,
    agent_did: String,
    requester_did: Option<String>,
    session_id: String,
}

impl DefraSpillTruncator {
    pub fn new(node: Arc<EmbeddedNode>, agent_did: &str, session_id: &str) -> Self {
        Self {
            node,
            agent_did: agent_did.to_string(),
            requester_did: None,
            session_id: session_id.to_string(),
        }
    }

    pub(crate) fn with_requester_did(mut self, requester_did: Option<String>) -> Self {
        self.requester_did = requester_did.and_then(|did| {
            let did = did.trim();
            (!did.is_empty()).then(|| did.to_string())
        });
        self
    }
}
