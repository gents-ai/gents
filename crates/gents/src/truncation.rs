use std::sync::Arc;

use anyhow::{Context as _, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;

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

pub(crate) fn tool_result_truncation_mode(tool_name: &str) -> TruncationMode {
    match tool_name {
        "bash" | "shell" | "command" => TruncationMode::Tail,
        _ => TruncationMode::Head,
    }
}

#[derive(Debug, Clone)]
pub struct TruncationResult {
    pub text: String,
    pub truncated: bool,
    pub truncated_by: Option<TruncationTrigger>,
    pub original_lines: usize,
    pub original_bytes: usize,
    pub spill_doc_id: Option<String>,
    pub spill_ref: Option<crate::SignedDocumentVersionRef>,
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

const MODEL_PROJECTION_VERSION: u32 = 1;
const MODEL_PROJECTION_V1_MAX_LINES: usize = 2000;
const MODEL_PROJECTION_V1_MAX_BYTES: usize = 50 * 1024;

impl Default for TruncationLimits {
    fn default() -> Self {
        Self {
            max_lines: MODEL_PROJECTION_V1_MAX_LINES,
            max_bytes: MODEL_PROJECTION_V1_MAX_BYTES,
        }
    }
}

#[derive(Debug, Deserialize)]
struct PersistedProjectionContract {
    projection_version: u32,
    truncated: bool,
    truncated_by: Option<String>,
    mode: String,
    original_lines: usize,
    original_bytes: usize,
    max_lines: usize,
    max_bytes: usize,
    spill_reference: bool,
}

/// Reconstruct the only model-facing projection authorized by an immutable
/// `AgentToolResult`. The full bytes remain in `output`; the signed metadata
/// commits the truncation algorithm inputs, and the exact result document ID
/// commits the available paging authority. The authority is rendered into the
/// model projection only when that projection omits full output bytes.
pub(crate) fn canonical_model_projection(
    output: &str,
    result_doc_id: &str,
    model_output_truncated: bool,
    metadata: &str,
) -> Result<String> {
    let (projection, truncated) =
        validated_model_projection(output, model_output_truncated, metadata)?;
    if result_doc_id.trim().is_empty() {
        anyhow::bail!("AgentToolResult projection contract requires an exact result document");
    }
    if truncated {
        Ok(format!(
            "{projection}\n[Full output: DefraDB doc {result_doc_id}]"
        ))
    } else {
        Ok(projection)
    }
}

fn validated_model_projection(
    output: &str,
    model_output_truncated: bool,
    metadata: &str,
) -> Result<(String, bool)> {
    let contract: PersistedProjectionContract = serde_json::from_str(metadata)
        .context("AgentToolResult has no valid model-projection contract")?;
    if contract.projection_version != MODEL_PROJECTION_VERSION {
        anyhow::bail!("AgentToolResult projection contract has an unsupported version");
    }
    if contract.original_lines != output.lines().count()
        || contract.original_bytes != output.len()
        || contract.truncated != model_output_truncated
    {
        anyhow::bail!("AgentToolResult projection contract does not describe its exact output");
    }
    let mode = match contract.mode.as_str() {
        "head" => TruncationMode::Head,
        "tail" => TruncationMode::Tail,
        _ => anyhow::bail!("AgentToolResult projection contract has an unknown truncation mode"),
    };
    let limits = TruncationLimits {
        max_lines: contract.max_lines,
        max_bytes: contract.max_bytes,
    };
    validate_model_projection_limits(&limits)?;
    let (projection, trigger, truncated) = truncate_text(output, mode, &limits);
    let trigger = trigger.map(|value| match value {
        TruncationTrigger::Lines => "lines",
        TruncationTrigger::Bytes => "bytes",
    });
    if truncated != contract.truncated || trigger != contract.truncated_by.as_deref() {
        anyhow::bail!("AgentToolResult projection contract does not reproduce its truncation");
    }
    if !contract.spill_reference {
        anyhow::bail!("AgentToolResult projection contract omits its exact result document");
    }
    Ok((projection, truncated))
}

/// Reject projection settings that a v1 reader cannot reproduce before an
/// immutable result fact is published. Writers must version a larger or
/// otherwise different provider projection rather than persisting an
/// unreadable v1 contract.
pub(crate) fn validate_model_projection_limits(limits: &TruncationLimits) -> Result<()> {
    // Version-one ceilings are protocol constants, not mutable runtime
    // defaults. Future default changes must not make immutable v1 facts
    // unreadable; a new algorithm/ceiling receives a new contract version.
    if limits.max_lines == 0
        || limits.max_bytes == 0
        || limits.max_lines > MODEL_PROJECTION_V1_MAX_LINES
        || limits.max_bytes > MODEL_PROJECTION_V1_MAX_BYTES
    {
        anyhow::bail!("AgentToolResult projection contract exceeds provider input limits");
    }
    Ok(())
}

/// Validate a caller-supplied projection contract at the immutable result
/// publication boundary. This covers direct retention and stale-result
/// rebinding in addition to the ordinary truncation path.
pub(crate) fn validate_model_projection_metadata(
    output: &str,
    model_output_truncated: bool,
    metadata: &str,
) -> Result<()> {
    validated_model_projection(output, model_output_truncated, metadata).map(|_| ())
}

/// Serialize the deterministic inputs and observed outcome of the canonical
/// model projection. Writers persist this string inside the exact signed result
/// fact; readers independently rerun the same function.
pub(crate) fn model_projection_metadata(
    output: &str,
    mode: TruncationMode,
    limits: &TruncationLimits,
) -> String {
    let (_, trigger, truncated) = truncate_text(output, mode, limits);
    serde_json::json!({
        "projection_version": MODEL_PROJECTION_VERSION,
        "truncated": truncated,
        "truncated_by": trigger.map(|value| match value {
            TruncationTrigger::Lines => "lines",
            TruncationTrigger::Bytes => "bytes",
        }),
        "mode": match mode {
            TruncationMode::Head => "head",
            TruncationMode::Tail => "tail",
        },
        "original_lines": output.lines().count(),
        "original_bytes": output.len(),
        "max_lines": limits.max_lines,
        "max_bytes": limits.max_bytes,
        // Every exact ToolOutputFact is itself the durable full-output spill.
        // The exact pointer remains mandatory even when a lossless projection
        // does not need to render it into the provider transcript.
        "spill_reference": true,
    })
    .to_string()
}

pub(crate) const LIVE_STREAM_CAPACITY_BYTES: usize = 256 * 1024;

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
    requester_did: Option<String>,
    session_id: String,
    tool_call_id: Option<String>,
}

impl DefraSpillTruncator {
    pub fn new(node: Arc<EmbeddedNode>, agent_did: &str, session_id: &str) -> Self {
        Self {
            node,
            agent_did: agent_did.to_string(),
            requester_did: None,
            session_id: session_id.to_string(),
            tool_call_id: None,
        }
    }

    pub(crate) fn with_requester_did(mut self, requester_did: Option<String>) -> Self {
        self.requester_did = requester_did.and_then(|did| {
            let did = did.trim();
            (!did.is_empty()).then(|| did.to_string())
        });
        self
    }

    pub(crate) fn with_tool_call_id(mut self, tool_call_id: &str) -> Self {
        self.tool_call_id = Some(tool_call_id.to_string());
        self
    }
}
