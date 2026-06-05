use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;

mod logic;
mod spill;
#[cfg(test)]
mod tests;

pub use logic::{truncate, truncate_text, TextTruncation};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncationMode {
    Head,
    Tail,
}

#[derive(Debug, Clone)]
pub struct TruncationResult {
    pub text: String,
    pub truncated: bool,
    pub truncated_by: Option<TruncationTrigger>,
    pub original_lines: usize,
    pub original_bytes: usize,
    pub spill_doc_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncationTrigger {
    Lines,
    Bytes,
}

#[derive(Debug, Clone)]
pub struct TruncationLimits {
    pub max_lines: usize,
    pub max_bytes: usize,
}

impl Default for TruncationLimits {
    fn default() -> Self {
        Self {
            max_lines: 2000,
            max_bytes: 50 * 1024,
        }
    }
}

pub trait Truncator: Send + Sync {
    fn truncate(
        &self,
        tool_name: &str,
        tool_input: &str,
        output: &str,
        mode: TruncationMode,
        limits: &TruncationLimits,
        conversation_doc_id: Option<&str>,
    ) -> impl std::future::Future<Output = Result<TruncationResult>> + Send;
}

pub struct DefraSpillTruncator {
    node: Arc<EmbeddedNode>,
    agent_did: String,
    session_id: String,
}

impl DefraSpillTruncator {
    pub fn new(node: Arc<EmbeddedNode>, agent_did: &str, session_id: &str) -> Self {
        Self {
            node,
            agent_did: agent_did.to_string(),
            session_id: session_id.to_string(),
        }
    }
}
