use anyhow::Result;
use serde::Deserialize;

use crate::graphql::escape_graphql_string;
use crate::session::execute_mutation_with_retry;
use crate::truncation::{
    truncate_text, DefraSpillTruncator, TruncationLimits, TruncationMode, TruncationResult,
    TruncationTrigger, Truncator,
};

#[derive(Deserialize)]
struct ExistingToolResultFact {
    #[serde(rename = "_docID")]
    doc_id: String,
    tool_call_key: String,
    tool_call_doc_id: String,
    tool_call_composite_commit_cid: String,
    tool_call_signer_did: String,
    agent_did: String,
    requester_did: Option<String>,
    session_id: String,
    tool_name: String,
    tool_input: String,
    output_text: String,
    model_output_truncated: bool,
    truncation_metadata: String,
    conversation_doc_id: String,
}

#[derive(Deserialize)]
struct ExactToolResultPayload {
    tool_call_doc_id: String,
    agent_did: String,
    requester_did: Option<String>,
    session_id: String,
    tool_name: String,
    tool_input: String,
    output_text: String,
    model_output_truncated: bool,
    truncation_metadata: String,
    conversation_doc_id: String,
}

async fn verify_existing_result_fact(
    node: &defra_node::EmbeddedNode,
    row: ExistingToolResultFact,
) -> Result<crate::SignedDocumentVersionRef> {
    let signer = node
        .verified_block_signer_did(&row.tool_call_composite_commit_cid)
        .await?;
    if signer != row.tool_call_signer_did {
        anyhow::bail!("stored AgentToolResult parent signer does not verify");
    }
    let query = format!(
        r#"{{ AgentToolCall(cid: ["{}"]) {{ _docID tool_call_key }} }}"#,
        escape_graphql_string(&row.tool_call_composite_commit_cid)
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "reconstructing stored AgentToolResult parent failed: {:?}",
            response.errors
        );
    }
    let parents = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("stored AgentToolResult parent returned no rows"))?;
    match parents.as_slice() {
        [parent]
            if parent.get("_docID").and_then(serde_json::Value::as_str)
                == Some(row.tool_call_doc_id.as_str())
                && parent
                    .get("tool_call_key")
                    .and_then(serde_json::Value::as_str)
                    == Some(row.tool_call_key.as_str()) => {}
        rows => anyhow::bail!(
            "stored AgentToolResult parent reconstructed {} rows or a different call",
            rows.len()
        ),
    }
    let exact = crate::document_version::verified_current_signed_document_version(
        node,
        "AgentToolResult",
        &row.doc_id,
    )
    .await?;
    if exact.signer_did != row.tool_call_signer_did {
        anyhow::bail!("stored AgentToolResult signer does not match its execution parent");
    }
    Ok(exact)
}

impl DefraSpillTruncator {
    /// Publish the canonical immutable full-output fact for a tool execution.
    ///
    /// Callers that already chose their model-facing projection use this
    /// entry point so every successful or failed execution retains the full
    /// observed bytes before its lifecycle row becomes terminal.
    pub(crate) async fn retain_full_output_fact(
        &self,
        tool_name: &str,
        tool_input: &str,
        output: &str,
        metadata: &str,
        conversation_doc_id: Option<&str>,
        model_output_truncated: bool,
    ) -> Result<crate::SignedDocumentVersionRef> {
        self.spill(
            tool_name,
            tool_input,
            output,
            metadata,
            conversation_doc_id,
            model_output_truncated,
        )
        .await
    }

    /// Re-publish a full-output proposal against the execution's new running
    /// head without reconstructing it from the bounded model projection.
    /// Stale retries must preserve the original bytes and truncation metadata.
    pub(crate) async fn retain_full_output_fact_from_exact(
        &self,
        source: &crate::SignedDocumentVersionRef,
    ) -> Result<crate::SignedDocumentVersionRef> {
        let snapshot =
            crate::document_version::verified_exact_document_snapshot_with_identity(
                &self.node,
                "AgentToolResult",
                &source.version,
                "tool_call_doc_id agent_did requester_did session_id tool_name tool_input output_text model_output_truncated truncation_metadata conversation_doc_id",
                None,
            )
            .await?;
        if snapshot.source.signer_did != source.signer_did {
            anyhow::bail!("AgentToolResult signer changed during stale output rebind");
        }
        let payload: ExactToolResultPayload = snapshot.decode()?;
        let (current_key, current) = self.exact_tool_call().await?;
        if payload.tool_call_doc_id != current.version.doc_id
            || payload.agent_did != self.agent_did
            || payload.requester_did != self.requester_did
            || payload.session_id != self.session_id
            || payload.tool_name.trim().is_empty()
            || current_key.trim().is_empty()
        {
            anyhow::bail!("AgentToolResult stale rebind changed immutable execution identity");
        }
        self.spill(
            &payload.tool_name,
            &payload.tool_input,
            &payload.output_text,
            &payload.truncation_metadata,
            (!payload.conversation_doc_id.is_empty())
                .then_some(payload.conversation_doc_id.as_str()),
            payload.model_output_truncated,
        )
        .await
    }

    async fn exact_tool_call(&self) -> Result<(String, crate::SignedDocumentVersionRef)> {
        #[derive(Deserialize)]
        struct Row {
            #[serde(rename = "_docID")]
            doc_id: String,
            tool_call_key: String,
        }
        #[derive(Deserialize)]
        struct ExactRow {
            #[serde(rename = "_docID")]
            doc_id: String,
            tool_call_key: String,
            session_id: String,
            tool_call_id: String,
            agent_did: String,
            requester_did: Option<String>,
            lifecycle_state: String,
        }

        let tool_call_id = self
            .tool_call_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("full tool-output retention requires a tool_call_id"))?;
        let query = format!(
            r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{}" }}, tool_call_id: {{ _eq: "{}" }} }}) {{ _docID tool_call_key }} }}"#,
            escape_graphql_string(&self.session_id),
            escape_graphql_string(tool_call_id),
        );
        let response = self.node.execute(&query).await;
        if response.has_errors() {
            anyhow::bail!(
                "enumerating exact AgentToolCall parent failed: {:?}",
                response.errors
            );
        }
        let rows: Vec<Row> = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentToolCall"))
            .cloned()
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default();
        let [row] = rows.as_slice() else {
            anyhow::bail!(
                "tool-call logical key resolved to {} physical rows; refusing ambiguous output fact",
                rows.len()
            );
        };
        let signed = crate::document_version::verified_current_signed_document_version(
            &self.node,
            "AgentToolCall",
            &row.doc_id,
        )
        .await?;
        let snapshot = crate::document_version::verified_exact_document_snapshot_with_identity(
            &self.node,
            "AgentToolCall",
            &signed.version,
            "tool_call_key session_id tool_call_id agent_did requester_did lifecycle_state",
            None,
        )
        .await?;
        if snapshot.source.signer_did != signed.signer_did {
            anyhow::bail!("AgentToolCall signer changed during full-output retention");
        }
        let exact: ExactRow = snapshot.decode()?;
        if exact.doc_id != row.doc_id
            || exact.tool_call_key != row.tool_call_key
            || exact.session_id != self.session_id
            || exact.tool_call_id != tool_call_id
            || exact.agent_did != self.agent_did
            || exact.requester_did != self.requester_did
            || exact.lifecycle_state != "running"
        {
            anyhow::bail!("full-output retention requires the exact signed running execution");
        }
        Ok((row.tool_call_key.clone(), signed))
    }

    async fn spill(
        &self,
        tool_name: &str,
        tool_input: &str,
        output: &str,
        metadata: &str,
        conversation_doc_id: Option<&str>,
        model_output_truncated: bool,
    ) -> Result<crate::SignedDocumentVersionRef> {
        let (tool_call_key, call) = self.exact_tool_call().await?;
        // The exact accepted execution version is the idempotency scope. A
        // mutable execution document may gain another live commit (for
        // example a partial-output checkpoint) before terminalization; that
        // newer head must be able to publish its own exact output fact rather
        // than conflicting with evidence for the older head.
        let result_key = call.version.composite_commit_cid.clone();
        let now = chrono::Utc::now().to_rfc3339();
        let escaped_result_key = escape_graphql_string(&result_key);
        let escaped_tool_call_key = escape_graphql_string(&tool_call_key);
        let escaped_call_doc_id = escape_graphql_string(&call.version.doc_id);
        let escaped_call_cid = escape_graphql_string(&call.version.composite_commit_cid);
        let escaped_call_signer = escape_graphql_string(&call.signer_did);
        let escaped_agent_did = escape_graphql_string(&self.agent_did);
        let escaped_session_id = escape_graphql_string(&self.session_id);
        let escaped_tool_name = escape_graphql_string(tool_name);
        let escaped_output = escape_graphql_string(output);
        let escaped_input = escape_graphql_string(tool_input);
        let escaped_metadata = escape_graphql_string(metadata);
        let escaped_conversation_doc_id =
            escape_graphql_string(conversation_doc_id.unwrap_or_default());
        let requester_did_field =
            crate::session::requester_did_create_field(self.requester_did.as_deref());
        let lookup = format!(
            r#"{{ AgentToolResult(filter: {{ result_key: {{ _eq: "{}" }} }}) {{ _docID tool_call_key tool_call_doc_id tool_call_composite_commit_cid tool_call_signer_did agent_did requester_did session_id tool_name tool_input output_text model_output_truncated truncation_metadata conversation_doc_id }} }}"#,
            escape_graphql_string(&result_key)
        );
        let matches = |row: &ExistingToolResultFact| {
            row.tool_call_key == tool_call_key
                && row.tool_call_doc_id == call.version.doc_id
                && row.tool_call_composite_commit_cid == call.version.composite_commit_cid
                && row.tool_call_signer_did == call.signer_did
                && row.agent_did == self.agent_did
                && row.requester_did == self.requester_did
                && row.session_id == self.session_id
                && row.tool_name == tool_name
                && row.tool_input == tool_input
                && row.output_text == output
                && row.model_output_truncated == model_output_truncated
                && row.truncation_metadata == metadata
                && row.conversation_doc_id == conversation_doc_id.unwrap_or_default()
        };
        let matching = |rows: Vec<ExistingToolResultFact>| -> Option<ExistingToolResultFact> {
            let mut rows = rows.into_iter().filter(&matches).collect::<Vec<_>>();
            rows.sort_by(|left, right| left.doc_id.cmp(&right.doc_id));
            rows.into_iter().next()
        };
        let load_existing = || async {
            let response = self.node.execute(&lookup).await;
            if response.has_errors() {
                anyhow::bail!(
                    "enumerating AgentToolResult twins failed: {:?}",
                    response.errors
                );
            }
            Ok::<Vec<ExistingToolResultFact>, anyhow::Error>(
                response
                    .data
                    .as_ref()
                    .and_then(|data| data.get("AgentToolResult"))
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()?
                    .unwrap_or_default(),
            )
        };
        if let Some(existing) = matching(load_existing().await?) {
            return verify_existing_result_fact(&self.node, existing).await;
        }
        let mutation = format!(
            r#"mutation {{
                create_AgentToolResult(input: {{
                    result_key: "{escaped_result_key}",
                    tool_call_key: "{escaped_tool_call_key}",
                    tool_call_doc_id: "{escaped_call_doc_id}",
                    tool_call_composite_commit_cid: "{escaped_call_cid}",
                    tool_call_signer_did: "{escaped_call_signer}",
                    agent_did: "{escaped_agent_did}",
                    {requester_did_field}
                    session_id: "{escaped_session_id}",
                    tool_name: "{escaped_tool_name}",
                    tool_input: "{escaped_input}",
                    output_text: "{escaped_output}",
                    model_output_truncated: {model_output_truncated},
                    truncation_metadata: "{escaped_metadata}",
                    conversation_doc_id: "{escaped_conversation_doc_id}",
                    created_at: "{now}"
                }}) {{ _docID }}
            }}"#,
        );

        let resp =
            match execute_mutation_with_retry(&self.node, &mutation, "spill_tool_output").await {
                Ok(response) => response,
                Err(create_error) => {
                    if let Some(existing) = matching(load_existing().await?) {
                        return verify_existing_result_fact(&self.node, existing).await;
                    }
                    return Err(create_error);
                }
            };

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

        let rows = load_existing().await?;
        let existing = rows
            .into_iter()
            .find(|row| row.doc_id == doc_id)
            .ok_or_else(|| {
                anyhow::anyhow!("created AgentToolResult disappeared during verification")
            })?;
        if !matches(&existing) {
            anyhow::bail!("created AgentToolResult payload changed during verification");
        }
        let exact = verify_existing_result_fact(&self.node, existing).await?;
        if exact.version.doc_id != doc_id {
            anyhow::bail!("verified AgentToolResult changed physical identity");
        }
        Ok(exact)
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
        let spill_ref = self
            .spill(
                tool_name,
                tool_input,
                output,
                &metadata,
                conversation_doc_id,
                truncated,
            )
            .await?;
        let spill_doc_id = Some(spill_ref.version.doc_id.clone());

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
            spill_ref: Some(spill_ref),
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
