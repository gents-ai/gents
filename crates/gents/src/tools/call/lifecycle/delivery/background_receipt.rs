use crate::request::lifecycle::queue::next_append_sequence_in_transaction;
use crate::session::canonical_rows::{
    decode_output_segment_row, decode_transcript_message_row, output_segment_create_variables,
    transcript_message_create_variables, AGENT_MESSAGE_FIELDS, AGENT_OUTPUT_SEGMENT_FIELDS,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use crate::store::config_client::ConfigApplyTxn;
#[cfg(test)]
use crate::store::config_client::{ConfigAccess, IdempotentTransactionRetry};
use crate::store::graphql::{created_doc_id, escape_graphql_string};
use crate::tools::call::lifecycle::ToolCallLifecycle;
use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadPresentation, PresentedPayload, SegmentRun, SourceClose,
    StreamDeclaration, StreamPayload, TranscriptMessage,
};

impl ToolCallLifecycle {
    #[cfg(test)]
    pub(crate) async fn publish_background_receipt(&mut self, text: &str) -> Result<bool> {
        let binding = self.background_receipt_binding()?;
        let receipt = text.to_owned();
        ConfigAccess::transact_local_idempotent(
            &self.node,
            None,
            IdempotentTransactionRetry::Standard,
            "tool_call.publish_background_receipt",
            move |txn| {
                let binding = binding.clone();
                let receipt = receipt.clone();
                Box::pin(
                    async move { publish_background_receipt_in_txn(txn, &binding, &receipt).await },
                )
            },
        )
        .await
    }

    pub(crate) fn background_receipt_binding(&self) -> Result<BackgroundReceiptBinding> {
        anyhow::ensure!(
            self.await_mode == crate::tools::call::lifecycle::AwaitMode::Background
                && !self.is_spawned_background(),
            "background receipt requires an accepted direct background invocation"
        );
        Ok(BackgroundReceiptBinding {
            tool_doc_id: self
                .doc_id
                .clone()
                .context("background receipt requires tool document")?,
            request_doc_id: self
                .request_doc_id
                .clone()
                .context("background receipt requires request document")?,
            accepted_header_doc_id: self
                .accepted_header_doc_id
                .clone()
                .context("background receipt requires accepted header")?,
            arguments: self
                .arguments
                .clone()
                .context("background receipt requires accepted arguments")?,
            generation: self
                .execution_generation
                .clone()
                .context("background receipt requires execution generation")?,
            node_did: self.node_did.clone(),
            requester_did: self.requester_did.clone(),
            session_id: self.session_id.clone(),
            tool_call_id: self.tool_call_id.clone(),
            call_id: self.call_id.clone(),
            tool_name: self.tool_name.clone(),
            message_sequence: self.message_sequence,
        })
    }
}

pub(super) struct BackgroundReceiptKey<'a> {
    pub(super) tool_call_doc_id: &'a str,
    pub(super) request_doc_id: &'a str,
    pub(super) node_did: &'a str,
    pub(super) requester_did: Option<&'a str>,
    pub(super) session_id: &'a str,
    pub(super) tool_call_id: &'a str,
    pub(super) call_id: Option<&'a str>,
}

pub(super) async fn ensure_background_receipt_before_bridge_close(
    txn: &ConfigApplyTxn<'_>,
    key: BackgroundReceiptKey<'_>,
) -> Result<()> {
    let BackgroundReceiptKey {
        tool_call_doc_id,
        request_doc_id,
        node_did,
        requester_did,
        session_id,
        tool_call_id,
        call_id,
    } = key;
    let key = escape_graphql_string(&format!(
        "{session_id}:background-receipt:{tool_call_doc_id}"
    ));
    let scope = crate::session::session_scope_filter(node_did, session_id, requester_did);
    let response = txn
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ {scope}, message_key: {{ _eq: "{key}" }} }}, limit: 2) {{ {AGENT_MESSAGE_FIELDS} }} }}"#
        ))
        .await?;
    let rows = response["data"]["AgentMessage"]
        .as_array()
        .context("background receipt lookup omitted AgentMessage rows")?;
    anyhow::ensure!(
        rows.len() == 1,
        "background bridge terminalization requires one immutable receipt"
    );
    let receipt = decode_transcript_message_row(&rows[0])?;
    anyhow::ensure!(
        receipt.message.request_doc_id.as_deref() == Some(request_doc_id)
            && receipt.message.session_id == session_id
            && receipt.message.node_did == node_did
            && receipt.message.requester_did.as_deref() == requester_did
            && receipt.message.role == MessageRole::User
            && receipt.message.outcome == OutputOutcome::Complete
            && matches!(&receipt.message.publication,
                MessagePublication::ToolDelivery { tool_call_doc_id: id } if id == tool_call_doc_id)
            && matches!(receipt.message.blocks.as_slice(),
                [MessageBlock::ToolResult { tool_call_doc_id: id, id: result_id,
                    call_id: result_call_id, .. }]
                if id == tool_call_doc_id && result_id == tool_call_id
                    && result_call_id.as_deref() == call_id),
        "background receipt conflicts with the accepted bridge invocation"
    );
    Ok(())
}

#[derive(Clone)]
pub(crate) struct BackgroundReceiptBinding {
    pub(crate) tool_doc_id: String,
    pub(crate) request_doc_id: String,
    pub(crate) accepted_header_doc_id: String,
    pub(crate) arguments: gents_protocol::output::PayloadRef,
    pub(crate) generation: String,
    pub(crate) node_did: String,
    pub(crate) requester_did: Option<String>,
    pub(crate) session_id: String,
    pub(crate) tool_call_id: String,
    pub(crate) call_id: Option<String>,
    pub(crate) tool_name: String,
    pub(crate) message_sequence: u32,
}

pub(crate) async fn publish_background_receipt_in_txn(
    txn: &ConfigApplyTxn<'_>,
    binding: &BackgroundReceiptBinding,
    receipt: &str,
) -> Result<bool> {
    let BackgroundReceiptBinding {
        tool_doc_id,
        request_doc_id,
        accepted_header_doc_id,
        arguments,
        generation,
        node_did,
        requester_did,
        session_id,
        tool_call_id,
        call_id,
        tool_name,
        message_sequence,
    } = binding.clone();
    let receipt = receipt.to_owned();
    let key = format!("{session_id}:background-receipt:{tool_doc_id}");
    let tool = escape_graphql_string(&tool_doc_id);
    let request = escape_graphql_string(&request_doc_id);
    let agent = escape_graphql_string(&node_did);
    let session = escape_graphql_string(&session_id);
    let requester_filter = requester_did
        .as_deref()
        .map(|value| {
            format!(
                r#", requester_did: {{ _eq: "{}" }}"#,
                escape_graphql_string(value)
            )
        })
        .unwrap_or_else(|| ", requester_did: { _eq: null }".to_owned());
    let existing = txn.execute(&format!(r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{}" }}, session_id: {{ _eq: "{session}" }}, node_did: {{ _eq: "{agent}" }}{requester_filter} }}, limit: 2) {{ {AGENT_MESSAGE_FIELDS} }} }}"#, escape_graphql_string(&key))).await?;
    let existing_rows = existing["data"]["AgentMessage"]
        .as_array()
        .context("background receipt lookup omitted messages")?;
    anyhow::ensure!(existing_rows.len() <= 1, "background receipt is ambiguous");
    if let Some(row) = existing_rows.first() {
        let existing = decode_transcript_message_row(row)?;
        anyhow::ensure!(
            existing.message.request_doc_id.as_deref() == Some(request_doc_id.as_str())
                && existing.message.session_id == session_id
                && existing.message.node_did == node_did
                && existing.message.requester_did.as_deref() == requester_did.as_deref()
                && existing.message.role == MessageRole::User
                && existing.message.outcome == OutputOutcome::Complete
                && matches!(existing.message.publication, MessagePublication::ToolDelivery { ref tool_call_doc_id } if tool_call_doc_id == &tool_doc_id)
                && matches!(existing.message.blocks.as_slice(), [MessageBlock::ToolResult { tool_call_doc_id, id, call_id: existing_call_id, .. }]
                    if tool_call_doc_id == &tool_doc_id && id == &tool_call_id && existing_call_id.as_deref() == call_id.as_deref()),
            "background receipt replay conflicts with physical tool binding"
        );
        let close_doc_id = match existing.message.blocks.as_slice() {
            [MessageBlock::ToolResult { parts, .. }] => match parts.as_slice() {
                [gents_protocol::output::ToolResultPart::Text { text }]
                    if text.presentation == PayloadPresentation::Full
                        && text.output.stream == 0 =>
                {
                    &text.output.close_doc_id
                }
                _ => anyhow::bail!("background receipt replay has a non-native payload reference"),
            },
            _ => unreachable!("physical binding guard established one ToolResult"),
        };
        let close = txn.execute(&format!(r#"{{ AgentOutputSegment(filter: {{ _docID: {{ _eq: "{}" }}, request_doc_id: {{ _eq: "{request}" }}, session_id: {{ _eq: "{session}" }}, node_did: {{ _eq: "{agent}" }}{requester_filter} }}, limit: 2) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#, escape_graphql_string(close_doc_id))).await?;
        let close_rows = close["data"]["AgentOutputSegment"]
            .as_array()
            .context("background receipt replay omitted authored close")?;
        anyhow::ensure!(
            close_rows.len() == 1,
            "background receipt replay close is missing or ambiguous"
        );
        let close = decode_output_segment_row(&close_rows[0])?;
        anyhow::ensure!(
            close.segment.source == OutputSource::Authored { key: key.clone() }
                && close.segment.writer
                    == OutputWriter::ToolExecution {
                        tool_call_doc_id: tool_doc_id.clone()
                    }
                && close.segment.payload == receipt
                && matches!(close.segment.close, Some(SourceClose::Closed {
                    outcome: OutputOutcome::Complete, segments: 1, ref stream_bytes
                }) if stream_bytes.as_slice() == [receipt.len() as u64]),
            "background receipt replay conflicts with immutable authored delivery"
        );
        return Ok(false);
    }
    let (accepted, _) = crate::session::load_canonical_message_in_txn(
        txn,
        &accepted_header_doc_id,
        &node_did,
        requester_did.as_deref(),
    )
    .await?;
    anyhow::ensure!(
        accepted.session_id == session_id
            && accepted.request_doc_id.as_deref() == Some(request_doc_id.as_str())
            && accepted.role == MessageRole::Assistant
            && accepted.outcome == OutputOutcome::Complete
            && accepted.sequence == message_sequence
            && matches!(accepted.publication, MessagePublication::RequestExecution { ref execution_generation } if execution_generation == &generation)
            && accepted.blocks.iter().any(|block| matches!(block,
                MessageBlock::ToolCall { tool_call_doc_id, id, call_id: accepted_call_id, name, arguments: accepted_arguments, .. }
                if tool_call_doc_id == &tool_doc_id && id == &tool_call_id && accepted_call_id.as_deref() == call_id.as_deref()
                    && name == &tool_name && accepted_arguments == &arguments)),
        "background receipt lacks exact accepted invocation"
    );
    let bridge = txn.execute(&format!(r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{tool}" }}, request_doc_id: {{ _eq: "{request}" }}, session_id: {{ _eq: "{session}" }}, node_did: {{ _eq: "{agent}" }}, tool_call_id: {{ _eq: "{}" }}, tool_name: {{ _eq: "{}" }}, message_sequence: {{ _eq: {message_sequence} }}, lifecycle_state: {{ _eq: "running" }}, await_mode: {{ _eq: "background" }}{requester_filter} }}, limit: 2) {{ _docID }} }}"#, escape_graphql_string(&tool_call_id), escape_graphql_string(&tool_name))).await?;
    anyhow::ensure!(
        bridge["data"]["AgentToolCall"]
            .as_array()
            .is_some_and(|rows| rows.len() == 1),
        "background receipt bridge is no longer running"
    );
    let now = Utc::now();
    let source = OutputSource::Authored { key: key.clone() };
    let writer = OutputWriter::ToolExecution {
        tool_call_doc_id: tool_doc_id.clone(),
    };
    let segment = OutputSegment {
        node_did: node_did.clone(),
        requester_did: requester_did.clone(),
        session_id: session_id.clone(),
        request_doc_id: request_doc_id.clone(),
        source,
        writer,
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: u32::try_from(receipt.len())
                .context("background receipt exceeds canonical segment size")?,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::ToolOutput,
            }),
        }],
        payload: receipt.clone(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![receipt.len() as u64],
        }),
        created_at: now.to_rfc3339_opts(SecondsFormat::Nanos, true),
    };
    let segment_response = txn
        .execute_with_variables(
            CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
            &output_segment_create_variables(&segment)?,
        )
        .await?;
    let close_doc_id = created_doc_id(&segment_response, "AgentOutputSegment")?;
    let receipt_fence = txn.execute(&format!(r#"mutation {{ update_AgentToolCall(docID: "{tool}", filter: {{ _docID: {{ _eq: "{tool}" }}, request_doc_id: {{ _eq: "{request}" }}, session_id: {{ _eq: "{session}" }}, node_did: {{ _eq: "{agent}" }}, lifecycle_state: {{ _eq: "running" }}, await_mode: {{ _eq: "background" }}{requester_filter} }}, input: {{ status: "running" }}) {{ _docID }} }}"#)).await?;
    anyhow::ensure!(
        receipt_fence["data"]["update_AgentToolCall"]
            .as_array()
            .is_some_and(|rows| rows.len() == 1),
        "background receipt lost running bridge fence"
    );
    let sequence = next_append_sequence_in_transaction(txn, &node_did, &session_id).await?;
    let message = TranscriptMessage {
        message_key: key,
        session_id: session_id.clone(),
        node_did: node_did.clone(),
        requester_did: requester_did.clone(),
        request_doc_id: Some(request_doc_id.clone()),
        publication: MessagePublication::ToolDelivery {
            tool_call_doc_id: tool_doc_id.clone(),
        },
        outcome: OutputOutcome::Complete,
        sequence,
        role: MessageRole::User,
        native_id: None,
        blocks: vec![MessageBlock::ToolResult {
            tool_call_doc_id: tool_doc_id.clone(),
            id: tool_call_id,
            call_id,
            parts: vec![gents_protocol::output::ToolResultPart::Text {
                text: PresentedPayload {
                    output: gents_protocol::output::PayloadRef {
                        close_doc_id,
                        stream: 0,
                    },
                    presentation: PayloadPresentation::Full,
                },
            }],
        }],
        created_at: now.to_rfc3339_opts(SecondsFormat::Nanos, true),
    };
    txn.execute_with_variables(
        CREATE_AGENT_MESSAGE_MUTATION,
        &transcript_message_create_variables(&message)?,
    )
    .await?;
    Ok(true)
}
