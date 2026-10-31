use crate::session::canonical_rows::{
    output_segment_create_variables, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use crate::store::config_client::{ConfigAccess, IdempotentTransactionRetry};
use crate::store::graphql::{created_doc_id, escape_graphql_string};
use crate::tools::call::lifecycle::{ToolCallLifecycle, ToolCallState};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents_protocol::output::reconstruction::ObservedSegment;
use gents_protocol::output::{
    OutputSegment, OutputSource, OutputWriter, SegmentRun, StreamDeclaration, StreamPayload,
};

#[derive(Clone)]
pub(crate) struct ToolOutputBinding {
    pub(crate) node: std::sync::Arc<defra_node::EmbeddedNode>,
    pub(crate) tool_call_doc_id: String,
    pub(crate) request_doc_id: String,
    pub(crate) session_id: String,
    pub(crate) node_did: String,
    pub(crate) requester_did: Option<String>,
}

impl gents_loop::live_output::CanonicalOutputAppender for ToolOutputBinding {
    fn append<'a>(
        &'a self,
        text: &'a str,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<std::ops::Range<u64>, gents_loop::live_output::LiveOutputError>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            append_tool_output(self, text).await.map_err(|error| {
                gents_loop::live_output::LiveOutputError::Appender {
                    source: error.into(),
                }
            })
        })
    }
}

impl std::fmt::Debug for ToolOutputBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolOutputBinding")
            .field("tool_call_doc_id", &self.tool_call_doc_id)
            .field("request_doc_id", &self.request_doc_id)
            .field("session_id", &self.session_id)
            .field("node_did", &self.node_did)
            .field("requester_did", &self.requester_did)
            .finish()
    }
}

impl ToolCallLifecycle {
    pub(crate) fn tool_output_binding(&self) -> Result<ToolOutputBinding> {
        anyhow::ensure!(
            self.state == ToolCallState::Running,
            "raw tool output requires a running admitted lifecycle"
        );
        Ok(ToolOutputBinding {
            node: self.node.clone(),
            tool_call_doc_id: self
                .doc_id
                .clone()
                .context("raw tool output requires a physical tool row")?,
            request_doc_id: self
                .request_doc_id
                .clone()
                .context("raw tool output requires a request binding")?,
            session_id: self.session_id.clone(),
            node_did: self.node_did.clone(),
            requester_did: self.requester_did.clone(),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ToolOutputAppendRejection {
    #[error("tool output append lost the running tool lifecycle")]
    NotRunning,
    #[error("tool output append exceeded the accepted tool deadline")]
    DeadlineExceeded,
}

#[derive(Debug)]
pub(crate) struct ToolOutputAppendReceipt {
    pub(crate) range: std::ops::Range<u64>,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Conformance binds the returned physical segment ID to the modeled write."
        )
    )]
    pub(crate) segment_doc_id: String,
}

pub(crate) async fn append_tool_output(
    binding: &ToolOutputBinding,
    bytes: &str,
) -> Result<std::ops::Range<u64>> {
    if bytes.is_empty() {
        return Ok(0..0);
    }
    Ok(
        append_tool_output_with_time(binding, bytes, Utc::now(), None)
            .await?
            .range,
    )
}

#[cfg(test)]
pub(crate) async fn append_tool_output_at(
    binding: &ToolOutputBinding,
    bytes: &str,
    operation_created_at: DateTime<Utc>,
    observed_now: DateTime<Utc>,
) -> Result<ToolOutputAppendReceipt> {
    append_tool_output_with_time(binding, bytes, operation_created_at, Some(observed_now)).await
}

async fn append_tool_output_with_time(
    binding: &ToolOutputBinding,
    bytes: &str,
    operation_created_at: DateTime<Utc>,
    fixture_now: Option<DateTime<Utc>>,
) -> Result<ToolOutputAppendReceipt> {
    anyhow::ensure!(
        !bytes.is_empty(),
        "empty tool output has no physical append receipt"
    );
    let created_at = operation_created_at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
    let payload = bytes.to_owned();
    let binding = binding.clone();
    let node = binding.node.clone();
    ConfigAccess::transact_local_idempotent(
        &node,
        None,
        IdempotentTransactionRetry::Standard,
        "tool_call.append_output_segment",
        move |txn| {
            let created_at = created_at.clone();
            let payload = payload.clone();
            let binding = binding.clone();
            Box::pin(async move {
                let now = fixture_now.unwrap_or_else(Utc::now);
                let tool = escape_graphql_string(&binding.tool_call_doc_id);
                let request = escape_graphql_string(&binding.request_doc_id);
                let agent = escape_graphql_string(&binding.node_did);
                let session = escape_graphql_string(&binding.session_id);
                let requester_filter = binding
                    .requester_did
                    .as_deref()
                    .map(|value| {
                        format!(
                            r#"requester_did: {{ _eq: "{}" }}"#,
                            escape_graphql_string(value)
                        )
                    })
                    .unwrap_or_else(|| "requester_did: { _eq: null }".to_owned());
                let row = txn
                    .execute(&format!(
                        r#"{{ AgentToolCall(filter: {{
                _docID: {{ _eq: "{tool}" }}, request_doc_id: {{ _eq: "{request}" }},
                node_did: {{ _eq: "{agent}" }}, session_id: {{ _eq: "{session}" }},
                {requester_filter}
            }}, limit: 2) {{ _docID lifecycle_state deadline_at }} }}"#
                    ))
                    .await?;
                let tools = row["data"]["AgentToolCall"]
                    .as_array()
                    .context("tool output append query omitted physical rows")?;
                anyhow::ensure!(
                    tools.len() == 1,
                    "tool output append has no unique physical binding"
                );
                let state = tools[0]["lifecycle_state"]
                    .as_str()
                    .and_then(ToolCallState::from_persisted)
                    .context("tool output append has malformed lifecycle state")?;
                if state != ToolCallState::Running {
                    return Err(ToolOutputAppendRejection::NotRunning.into());
                }
                let source = OutputSource::ToolCall {
                    tool_call_doc_id: binding.tool_call_doc_id.clone(),
                };
                let writer = OutputWriter::ToolExecution {
                    tool_call_doc_id: binding.tool_call_doc_id.clone(),
                };
                let rows = crate::session::load_source_rows_in_txn(
                    txn,
                    &binding.request_doc_id,
                    &source,
                    &binding.node_did,
                    &binding.session_id,
                    binding.requester_did.as_deref(),
                )
                .await?;
                let same_attempt = rows
                    .iter()
                    .filter(|row| row.segment.created_at == created_at)
                    .collect::<Vec<_>>();
                anyhow::ensure!(
                    same_attempt.len() <= 1,
                    "canonical tool output append has ambiguous retry identity"
                );
                anyhow::ensure!(
                    rows.iter().all(|row| row.segment.close.is_none()),
                    "cannot append raw output after tool source closure"
                );
                let prior = rows
                    .iter()
                    .filter(|row| row.segment.created_at != created_at)
                    .map(|row| ObservedSegment {
                        doc_id: &row.doc_id,
                        segment: &row.segment,
                    })
                    .collect::<Vec<_>>();
                let extent = gents_protocol::output::extent::inspect_open_source(
                    &prior,
                    &binding.request_doc_id,
                    &source,
                    &writer,
                )
                .map_err(anyhow::Error::from)?;
                anyhow::ensure!(
                    extent.streams.len() <= 1,
                    "tool raw output source has unexpected multiple streams"
                );
                let ordinal = extent.segments;
                let start = extent.stream_bytes.first().copied().unwrap_or(0);
                let segment = OutputSegment {
                    node_did: binding.node_did.clone(),
                    requester_did: binding.requester_did.clone(),
                    session_id: binding.session_id.clone(),
                    request_doc_id: binding.request_doc_id.clone(),
                    source,
                    writer,
                    ordinal: Some(ordinal),
                    runs: vec![SegmentRun {
                        stream: 0,
                        bytes: u32::try_from(payload.len())
                            .context("tool output chunk too large")?,
                        declaration: (ordinal == 0).then_some(StreamDeclaration {
                            block_index: 0,
                            part_index: 0,
                            payload: StreamPayload::ToolOutput,
                        }),
                    }],
                    payload,
                    close: None,
                    created_at,
                };
                if let Some(existing) = same_attempt.first() {
                    anyhow::ensure!(
                        existing.segment == segment,
                        "canonical tool output retry conflicts with the recorded segment"
                    );
                    return Ok(ToolOutputAppendReceipt {
                        range: start..start + segment.payload.len() as u64,
                        segment_doc_id: existing.doc_id.clone(),
                    });
                }
                let deadline = tools[0]["deadline_at"]
                    .as_str()
                    .context("tool output append has no accepted deadline")?;
                let deadline = DateTime::parse_from_rfc3339(deadline)
                    .context("tool output append has malformed accepted deadline")?
                    .with_timezone(&Utc);
                if now > deadline {
                    return Err(ToolOutputAppendRejection::DeadlineExceeded.into());
                }
                let response = txn
                    .execute_with_variables(
                        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                        &output_segment_create_variables(&segment)?,
                    )
                    .await?;
                Ok(ToolOutputAppendReceipt {
                    range: start..start + segment.payload.len() as u64,
                    segment_doc_id: created_doc_id(&response, "AgentOutputSegment")?,
                })
            })
        },
    )
    .await
}
