use crate::call::lifecycle::ToolCallLifecycle;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents_protocol::document_query::{
    fields::agent_output_segment as seg_fields, fields::agent_tool_call as tool_fields, Direction,
    Filter, Query, QuerySet, Selection,
};
use gents_protocol::output::extent::ObservedShape;
use gents_protocol::output::rows::{
    decode_scoped_request_output_segment_shapes, decode_scoped_request_output_segments,
    AGENT_OUTPUT_SEGMENT_FIELDS, AGENT_OUTPUT_SEGMENT_SHAPE_FIELDS,
    CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents_protocol::output::tail_append::{
    plan_tool_append_from_tail, TailAppendKind, TailAppendObservation,
};
use gents_protocol::output::{
    OutputSegment, OutputSource, OutputWriter, SegmentRun, StreamDeclaration, StreamPayload,
};
use gents_protocol::tool_call::ToolCallState;
use gents_store::config_client::session_children::output_segment_bound_variables;
use gents_store::config_client::{ConfigAccess, IdempotentTransactionRetry};
use gents_store::graphql::created_doc_id;

#[derive(Clone)]
pub struct ToolOutputBinding {
    pub(crate) actor: identity::Did,
    pub(crate) agent_signer: Option<gents_store::identity::ManagedAgentSigner>,
    pub(crate) node: std::sync::Arc<defra_node::Node>,
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
    pub fn tool_output_binding(&self) -> Result<ToolOutputBinding> {
        anyhow::ensure!(
            self.state == ToolCallState::Running,
            "raw tool output requires a running admitted lifecycle"
        );
        Ok(ToolOutputBinding {
            actor: self.actor.clone(),
            agent_signer: self.agent_signer.clone(),
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
pub enum ToolOutputAppendRejection {
    #[error("tool output append lost the running tool lifecycle")]
    NotRunning,
    #[error("tool output append exceeded the accepted tool deadline")]
    DeadlineExceeded,
}

#[cfg(any(test, feature = "test-fixtures"))]
impl ToolOutputAppendRejection {
    pub(crate) fn in_failure(error: &anyhow::Error) -> Option<&Self> {
        gents_protocol::error_chain::find_map_source(error.as_ref(), |cause| {
            cause.downcast_ref::<Self>()
        })
    }
}

#[derive(Debug)]
pub struct ToolOutputAppendReceipt {
    pub(crate) range: std::ops::Range<u64>,
    pub segment_doc_id: String,
}

pub async fn append_tool_output(
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

#[cfg(any(test, feature = "test-fixtures"))]
pub async fn append_tool_output_at(
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
    let repair_actor = binding.actor.clone();
    let repair_node_did = binding.node_did.clone();
    let repair_session_id = binding.session_id.clone();
    let access = ConfigAccess::local_with_optional_agent_signer(
        node.clone(),
        binding.actor.clone(),
        binding.agent_signer.clone(),
    )?;
    let result = access
        .transact_idempotent(
            IdempotentTransactionRetry::Standard,
            "tool_call.append_output_segment",
            move |txn| {
                let created_at = created_at.clone();
                let payload = payload.clone();
                let binding = binding.clone();
                Box::pin(async move {
                    let now = fixture_now.unwrap_or_else(Utc::now);
                    let source = OutputSource::ToolCall {
                        tool_call_doc_id: binding.tool_call_doc_id.clone(),
                    };
                    let source_key = source.canonical_key();
                    let writer = OutputWriter::ToolExecution {
                        tool_call_doc_id: binding.tool_call_doc_id.clone(),
                    };
                    let requester_filter = match binding.requester_did.as_deref() {
                        Some(value) => Filter::eq(tool_fields::REQUESTER_DID, value),
                        None => Filter::is_null(tool_fields::REQUESTER_DID),
                    };
                    let tool_query = Query::select(
                        Selection::doc_id()
                            .field(tool_fields::LIFECYCLE_STATE)
                            .field(tool_fields::DEADLINE_AT),
                    )
                    .filter(Filter::eq(tool_fields::DOC_ID, &binding.tool_call_doc_id))
                    .filter(Filter::eq(
                        tool_fields::REQUEST_DOC_ID,
                        &binding.request_doc_id,
                    ))
                    .filter(Filter::eq(tool_fields::NODE_DID, &binding.node_did))
                    .filter(Filter::eq(tool_fields::SESSION_ID, &binding.session_id))
                    .filter(requester_filter)
                    .limit(2);
                    let tail_query =
                        Query::select(Selection::owned(AGENT_OUTPUT_SEGMENT_SHAPE_FIELDS))
                            .filter(Filter::eq(
                                seg_fields::REQUEST_DOC_ID,
                                &binding.request_doc_id,
                            ))
                            .filter(Filter::eq(seg_fields::SOURCE_KEY, &source_key))
                            .order_by(seg_fields::ORDINAL, Direction::Desc)
                            .limit(3);
                    let closures_query =
                        Query::select(Selection::owned(AGENT_OUTPUT_SEGMENT_SHAPE_FIELDS))
                            .filter(Filter::eq(
                                seg_fields::REQUEST_DOC_ID,
                                &binding.request_doc_id,
                            ))
                            .filter(Filter::eq(seg_fields::IS_CLOSE, true))
                            .filter(Filter::eq(seg_fields::SOURCE_KEY, &source_key))
                            .limit(2);
                    let same_attempt_query =
                        Query::select(Selection::owned(AGENT_OUTPUT_SEGMENT_FIELDS))
                            .filter(Filter::eq(
                                seg_fields::REQUEST_DOC_ID,
                                &binding.request_doc_id,
                            ))
                            .filter(Filter::eq(seg_fields::CREATED_AT, &created_at))
                            .filter(Filter::eq(seg_fields::SOURCE_KEY, &source_key))
                            .limit(2);
                    let query = QuerySet::new()
                        .root("tool", &tool_query)
                        .root("tail", &tail_query)
                        .root("closures", &closures_query)
                        .root("same_attempt", &same_attempt_query)
                        .render();
                    let response = txn.run(&query).await?;
                    let data = &response["data"];
                    let tools = data["tool"]
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
                    let tail_raw = data["tail"]
                        .as_array()
                        .context("tool output append query omitted tail rows")?;
                    let closures_raw = data["closures"]
                        .as_array()
                        .context("tool output append query omitted closure rows")?;
                    let same_raw = data["same_attempt"]
                        .as_array()
                        .context("tool output append query omitted retry rows")?;
                    let tail_rows = decode_scoped_request_output_segment_shapes(
                        tail_raw,
                        &binding.node_did,
                        Some(&binding.session_id),
                        binding.requester_did.as_deref(),
                    )?;
                    anyhow::ensure!(
                        tail_rows.len() == tail_raw.len(),
                        "tool output tail crossed exact scope"
                    );
                    let closure_rows = decode_scoped_request_output_segment_shapes(
                        closures_raw,
                        &binding.node_did,
                        Some(&binding.session_id),
                        binding.requester_did.as_deref(),
                    )?;
                    anyhow::ensure!(
                        closure_rows.len() == closures_raw.len(),
                        "tool output closure witness crossed exact scope"
                    );
                    let same_attempt_rows = decode_scoped_request_output_segments(
                        same_raw,
                        &binding.node_did,
                        Some(&binding.session_id),
                        binding.requester_did.as_deref(),
                    )?;
                    anyhow::ensure!(
                        same_attempt_rows.len() == same_raw.len(),
                        "tool output retry witness crossed exact scope"
                    );
                    #[cfg(any(test, feature = "test-fixtures"))]
                    {
                        let decoded_rows =
                            tail_rows.len() + closure_rows.len() + same_attempt_rows.len();
                        let decoded_payload_bytes = same_attempt_rows
                            .iter()
                            .map(|row| row.segment.payload.len())
                            .sum();
                        gents_session::session::record_source_read(
                            decoded_rows,
                            decoded_payload_bytes,
                        );
                    }
                    let tail_shapes = tail_rows
                        .iter()
                        .map(|row| ObservedShape::from_shape(&row.doc_id, &row.shape))
                        .collect::<Vec<_>>();
                    let closure_shapes = closure_rows
                        .iter()
                        .map(|row| ObservedShape::from_shape(&row.doc_id, &row.shape))
                        .collect::<Vec<_>>();
                    let same_shapes = same_attempt_rows
                        .iter()
                        .map(|row| ObservedShape::from_segment(&row.doc_id, &row.segment))
                        .collect::<Vec<_>>();
                    let plan = plan_tool_append_from_tail(
                        TailAppendObservation {
                            tail: &tail_shapes,
                            closure_witness: &closure_shapes,
                            same_attempt: &same_shapes,
                        },
                        &binding.request_doc_id,
                        &source,
                        &writer,
                        &created_at,
                        payload.len() as u64,
                    )
                    .map_err(anyhow::Error::from)?;
                    let segment = OutputSegment {
                        node_did: binding.node_did.clone(),
                        requester_did: binding.requester_did.clone(),
                        session_id: binding.session_id.clone(),
                        request_doc_id: binding.request_doc_id.clone(),
                        source,
                        writer,
                        ordinal: Some(plan.ordinal),
                        runs: vec![SegmentRun {
                            stream: 0,
                            bytes: u32::try_from(payload.len())
                                .context("tool output chunk too large")?,
                            declaration: (plan.ordinal == 0).then_some(StreamDeclaration {
                                block_index: 0,
                                part_index: 0,
                                payload: StreamPayload::ToolOutput,
                            }),
                        }],
                        stream_bytes_after: Some(vec![plan.end_byte]),
                        payload,
                        close: None,
                        created_at,
                    };
                    if plan.kind == TailAppendKind::Replay {
                        let doc_id = plan
                            .doc_id
                            .context("tool output replay omitted physical doc_id")?;
                        let recorded = match same_attempt_rows
                            .into_iter()
                            .find(|row| row.doc_id == doc_id)
                        {
                            Some(row) => row,
                            None => {
                                gents_session::session::load_output_segment_in_txn(
                                    txn,
                                    &doc_id,
                                    &binding.node_did,
                                    &binding.session_id,
                                    binding.requester_did.as_deref(),
                                )
                                .await?
                            }
                        };
                        anyhow::ensure!(
                            recorded.segment == segment,
                            "canonical tool output retry conflicts with the recorded segment"
                        );
                        return Ok(ToolOutputAppendReceipt {
                            range: plan.start_byte..plan.end_byte,
                            segment_doc_id: doc_id,
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
                            &output_segment_bound_variables(txn, &segment).await?,
                        )
                        .await?;
                    Ok(ToolOutputAppendReceipt {
                        range: plan.start_byte..plan.end_byte,
                        segment_doc_id: created_doc_id(&response, "AgentOutputSegment")?,
                    })
                })
            },
        )
        .await?;
    gents_store::config_client::session_children::publish_output_segment_relationships_with_access(
        &access,
        &repair_node_did,
        &repair_actor,
        &repair_session_id,
        &result.segment_doc_id,
    )
    .await?;
    Ok(result)
}
