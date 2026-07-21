use anyhow::Result;

use crate::graphql::escape_graphql_string;
use crate::session::execute_mutation_with_retry;
use crate::truncation::{
    truncate_text, DefraSpillTruncator, TruncationLimits, TruncationMode, TruncationResult,
    TruncationTrigger, Truncator,
};

impl DefraSpillTruncator {
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
        let requester_did_field =
            crate::session::requester_did_create_field(self.requester_did.as_deref());
        let mutation = format!(
            r#"mutation {{
                create_AgentToolResult(input: {{
                    agent_did: "{agent_did}",
                    {requester_did_field}
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

pub(super) fn extract_mutation_doc_id<'a>(
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
