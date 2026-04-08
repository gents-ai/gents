//! Dual-limit tool output truncation with automatic DefraDB spill.

use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;

use crate::graphql::escape_graphql_string;
use crate::session::execute_mutation_with_retry;

/// Which end to preserve when truncating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncationMode {
    /// Keep the first N lines (file reads, structured output).
    Head,
    /// Keep the last N lines (command output, logs).
    Tail,
}

/// Result of a truncation operation.
#[derive(Debug, Clone)]
pub struct TruncationResult {
    /// The (possibly truncated) output text.
    pub text: String,
    /// Whether truncation was applied.
    pub truncated: bool,
    /// Which limit triggered truncation (if any).
    pub truncated_by: Option<TruncationTrigger>,
    /// Original line count before truncation.
    pub original_lines: usize,
    /// Original byte count before truncation.
    pub original_bytes: usize,
    /// DefraDB doc ID where full output was spilled (if truncated).
    pub spill_doc_id: Option<String>,
}

/// Which limit triggered truncation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncationTrigger {
    Lines,
    Bytes,
}

/// Limits controlling when truncation triggers.
#[derive(Debug, Clone)]
pub struct TruncationLimits {
    /// Maximum line count (default: 2000).
    pub max_lines: usize,
    /// Maximum byte count (default: 50KB).
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

/// Truncates tool output and spills full content to DefraDB when over limits.
pub trait Truncator: Send + Sync {
    /// Truncate the given tool output according to the mode and limits.
    /// If truncated, persist the full output to DefraDB and include
    /// a pointer in the returned text.
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

/// Pure truncation logic — no DefraDB dependency.
/// Returns (truncated_text, trigger, was_truncated).
pub fn truncate_text(
    text: &str,
    mode: TruncationMode,
    limits: &TruncationLimits,
) -> (String, Option<TruncationTrigger>, bool) {
    let original_bytes = text.len();
    let lines: Vec<&str> = text.lines().collect();
    let original_lines = lines.len();

    let exceeds_lines = original_lines > limits.max_lines;
    let exceeds_bytes = original_bytes > limits.max_bytes;

    if !exceeds_lines && !exceeds_bytes {
        return (text.to_string(), None, false);
    }

    // Determine which limit triggers (prefer the stricter one).
    let trigger = if exceeds_bytes && exceeds_lines {
        // Both exceeded — apply the one that cuts more.
        let line_ratio = original_lines as f64 / limits.max_lines as f64;
        let byte_ratio = original_bytes as f64 / limits.max_bytes as f64;
        if byte_ratio > line_ratio {
            TruncationTrigger::Bytes
        } else {
            TruncationTrigger::Lines
        }
    } else if exceeds_bytes {
        TruncationTrigger::Bytes
    } else {
        TruncationTrigger::Lines
    };

    let truncated = match mode {
        TruncationMode::Head => {
            // Keep the first N lines, up to byte limit.
            let mut result = String::new();
            let mut line_count = 0;

            for line in &lines {
                if line_count >= limits.max_lines {
                    break;
                }
                if result.len() + line.len() + 1 > limits.max_bytes {
                    break;
                }
                if !result.is_empty() {
                    result.push('\n');
                }
                result.push_str(line);
                line_count += 1;
            }

            format!(
                "{}\n\n[Showing lines 1-{} of {} ({} bytes total)]",
                result, line_count, original_lines, original_bytes,
            )
        }
        TruncationMode::Tail => {
            // Keep the last N lines, up to byte limit.
            let start_line = if exceeds_lines {
                original_lines.saturating_sub(limits.max_lines)
            } else {
                0
            };

            let mut result = String::new();
            let mut included = 0;

            for line in lines[start_line..].iter().rev() {
                if result.len() + line.len() + 1 > limits.max_bytes {
                    break;
                }
                included += 1;
                // Prepend (we're iterating in reverse).
                if result.is_empty() {
                    result = line.to_string();
                } else {
                    result = format!("{}\n{}", line, result);
                }
            }

            let shown_start = original_lines - included + 1;
            format!(
                "[Showing lines {}-{} of {} ({} bytes total)]\n\n{}",
                shown_start, original_lines, original_lines, original_bytes, result,
            )
        }
    };

    (truncated, Some(trigger), true)
}

/// DefraDB-backed truncator that spills full output to AgentToolResult documents.
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

    /// Persist full tool output to DefraDB. Returns the doc ID.
    async fn spill(
        &self,
        tool_name: &str,
        tool_input: &str,
        output: &str,
        metadata: &str,
        conversation_doc_id: Option<&str>,
    ) -> Result<String> {
        let now = chrono::Utc::now().to_rfc3339();
        let escaped_output = escape_graphql_string(output);
        let escaped_input = escape_graphql_string(tool_input);
        let escaped_metadata = escape_graphql_string(metadata);
        let escaped_conversation_doc_id =
            escape_graphql_string(conversation_doc_id.unwrap_or_default());
        let mutation = format!(
            r#"mutation {{
                create_AgentToolResult(input: {{
                    agent_did: "{agent_did}",
                    session_id: "{session_id}",
                    tool_name: "{tool_name}",
                    tool_input: "{escaped_input}",
                    output_text: "{escaped_output}",
                    truncated: true,
                    truncation_metadata: "{escaped_metadata}",
                    conversation_doc_id: "{escaped_conversation_doc_id}",
                    created_at: "{now}"
                }}) {{ _docID }}
            }}"#,
            agent_did = self.agent_did,
            session_id = self.session_id,
        );

        let resp = execute_mutation_with_retry(&self.node, &mutation, "spill_tool_output").await?;

        // Extract _docID from response.
        let doc_id = resp
            .data
            .as_ref()
            .and_then(|data| extract_mutation_doc_id(data, "AgentToolResult"))
            .ok_or_else(|| anyhow::anyhow!("spill mutation returned no _docID"))?
            .to_string();

        tracing::debug!(
            tool = %tool_name,
            doc_id = %doc_id,
            bytes = output.len(),
            "spilled full tool output to DefraDB"
        );

        Ok(doc_id)
    }
}

impl Truncator for DefraSpillTruncator {
    async fn truncate(
        &self,
        tool_name: &str,
        tool_input: &str,
        output: &str,
        mode: TruncationMode,
        limits: &TruncationLimits,
        conversation_doc_id: Option<&str>,
    ) -> Result<TruncationResult> {
        let original_lines = output.lines().count();
        let original_bytes = output.len();

        let (text, trigger, truncated) = truncate_text(output, mode, limits);

        let spill_doc_id = if truncated {
            let metadata = serde_json::json!({
                "truncated": truncated,
                "truncated_by": trigger.map(|value| match value {
                    TruncationTrigger::Lines => "lines",
                    TruncationTrigger::Bytes => "bytes",
                }),
                "mode": match mode {
                    TruncationMode::Head => "head",
                    TruncationMode::Tail => "tail",
                },
                "original_lines": original_lines,
                "original_bytes": original_bytes,
                "max_lines": limits.max_lines,
                "max_bytes": limits.max_bytes,
            })
            .to_string();
            match self
                .spill(
                    tool_name,
                    tool_input,
                    output,
                    &metadata,
                    conversation_doc_id,
                )
                .await
            {
                Ok(id) => Some(id),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to spill tool output — continuing without spill");
                    None
                }
            }
        } else {
            None
        };

        // Append spill pointer to truncated output.
        let text = if let Some(ref doc_id) = spill_doc_id {
            format!("{}\n[Full output: DefraDB doc {}]", text, doc_id)
        } else {
            text
        };

        Ok(TruncationResult {
            text,
            truncated,
            truncated_by: trigger,
            original_lines,
            original_bytes,
            spill_doc_id,
        })
    }
}

fn extract_mutation_doc_id<'a>(
    data: &'a serde_json::Value,
    collection_name: &str,
) -> Option<&'a str> {
    for field_name in [
        format!("create_{collection_name}"),
        format!("add_{collection_name}"),
    ] {
        if let Some(value) = data.get(&field_name) {
            if let Some(doc_id) = value.get("_docID").and_then(|value| value.as_str()) {
                return Some(doc_id);
            }

            if let Some(doc_id) = value
                .as_array()
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("_docID"))
                .and_then(|value| value.as_str())
            {
                return Some(doc_id);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn no_truncation_under_limits() {
        let text = "line 1\nline 2\nline 3";
        let (result, trigger, truncated) =
            truncate_text(text, TruncationMode::Head, &TruncationLimits::default());
        assert!(!truncated);
        assert!(trigger.is_none());
        assert_eq!(result, text);
    }

    #[test]
    fn head_truncation_by_lines() {
        let lines: Vec<String> = (0..100).map(|i| format!("line {}", i)).collect();
        let text = lines.join("\n");
        let limits = TruncationLimits {
            max_lines: 10,
            max_bytes: 1024 * 1024,
        };

        let (result, trigger, truncated) = truncate_text(&text, TruncationMode::Head, &limits);
        assert!(truncated);
        assert_eq!(trigger, Some(TruncationTrigger::Lines));
        assert!(result.starts_with("line 0\n"));
        assert!(result.contains("[Showing lines 1-10 of 100"));
    }

    #[test]
    fn tail_truncation_by_lines() {
        let lines: Vec<String> = (0..100).map(|i| format!("line {}", i)).collect();
        let text = lines.join("\n");
        let limits = TruncationLimits {
            max_lines: 10,
            max_bytes: 1024 * 1024,
        };

        let (result, trigger, truncated) = truncate_text(&text, TruncationMode::Tail, &limits);
        assert!(truncated);
        assert_eq!(trigger, Some(TruncationTrigger::Lines));
        assert!(result.contains("line 99"));
        assert!(result.contains("[Showing lines 91-100 of 100"));
    }

    #[test]
    fn head_truncation_by_bytes() {
        let text = "x".repeat(100_000);
        let limits = TruncationLimits {
            max_lines: 1_000_000,
            max_bytes: 1024,
        };

        let (result, trigger, truncated) = truncate_text(&text, TruncationMode::Head, &limits);
        assert!(truncated);
        assert_eq!(trigger, Some(TruncationTrigger::Bytes));
        assert!(result.len() < 100_000);
    }

    #[test]
    fn tail_truncation_by_bytes() {
        let text = "x".repeat(100_000);
        let limits = TruncationLimits {
            max_lines: 1_000_000,
            max_bytes: 1024,
        };

        let (result, trigger, truncated) = truncate_text(&text, TruncationMode::Tail, &limits);
        assert!(truncated);
        assert_eq!(trigger, Some(TruncationTrigger::Bytes));
        assert!(result.len() < 100_000);
    }

    #[test]
    fn both_limits_exceeded() {
        let lines: Vec<String> = (0..5000).map(|i| format!("line {:04}", i)).collect();
        let text = lines.join("\n");
        let limits = TruncationLimits {
            max_lines: 100,
            max_bytes: 1024,
        };

        let (_, trigger, truncated) = truncate_text(&text, TruncationMode::Head, &limits);
        assert!(truncated);
        assert!(trigger.is_some());
    }

    #[test]
    fn extract_mutation_doc_id_accepts_create_and_add_shapes() {
        let create_data = json!({
            "create_AgentToolResult": { "_docID": "doc-create" }
        });
        assert_eq!(
            extract_mutation_doc_id(&create_data, "AgentToolResult"),
            Some("doc-create")
        );

        let add_data = json!({
            "add_AgentToolResult": [{ "_docID": "doc-add" }]
        });
        assert_eq!(
            extract_mutation_doc_id(&add_data, "AgentToolResult"),
            Some("doc-add")
        );
    }
}
