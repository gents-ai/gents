use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents_protocol::output::reconstruction::ObservedSegment;
use gents_protocol::output::{
    MessageBlock, MessagePublication, OutputOutcome, OutputSegment, OutputSource, OutputWriter,
    PayloadRef, SourceClose, TranscriptMessage,
};

use crate::config_client::{ConfigAccess, ConfigApplyTxn, IdempotentTransactionRetry};
use crate::graphql::{created_doc_id, escape_graphql_string, mutation_documents};
use crate::request::lifecycle::execution_policy::{authorize_output_append, LeaseObservation};
use crate::session::canonical_rows::{
    decode_transcript_message_row, output_segment_create_variables,
    transcript_message_create_variables, AGENT_MESSAGE_FIELDS, CREATE_AGENT_MESSAGE_MUTATION,
    CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};

use super::{
    source::load_request_in_txn, ProviderCloseRejection, ProviderPublicationPlan,
    ProviderReplayRejection, PublishedProviderTurn,
};

fn created_tool_call_doc_ids(
    response: &serde_json::Value,
    expected_keys: &[String],
) -> Result<Vec<String>> {
    let rows = mutation_documents(response, "create_AgentToolCall")?
        .context("create AgentToolCall omitted response data")?;
    anyhow::ensure!(
        !rows.is_empty(),
        "create AgentToolCall omitted its mutation field"
    );
    anyhow::ensure!(
        rows.len() == expected_keys.len(),
        "create AgentToolCall returned {} rows for {} tool calls",
        rows.len(),
        expected_keys.len()
    );
    let expected = expected_keys
        .iter()
        .map(String::as_str)
        .collect::<rapidhash::RapidHashSet<_>>();
    anyhow::ensure!(
        expected.len() == expected_keys.len(),
        "provider publication contains duplicate physical tool-call keys"
    );
    let mut doc_ids =
        rapidhash::RapidHashMap::with_capacity_and_hasher(rows.len(), Default::default());
    for row in rows {
        let key = row
            .get("tool_call_key")
            .and_then(serde_json::Value::as_str)
            .context("create AgentToolCall omitted tool_call_key")?;
        anyhow::ensure!(
            expected.contains(key),
            "create AgentToolCall returned an unexpected tool_call_key"
        );
        let doc_id = row
            .get("_docID")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .context("create AgentToolCall omitted its physical identity")?;
        anyhow::ensure!(
            doc_ids.insert(key.to_owned(), doc_id.to_owned()).is_none(),
            "create AgentToolCall returned a duplicate tool_call_key"
        );
    }
    expected_keys
        .iter()
        .map(|key| {
            doc_ids
                .remove(key)
                .with_context(|| format!("create AgentToolCall omitted tool_call_key {key:?}"))
        })
        .collect()
}

pub(crate) async fn publish_provider_turn(
    node: &std::sync::Arc<EmbeddedNode>,
    generation: &str,
    plan: ProviderPublicationPlan,
) -> Result<PublishedProviderTurn> {
    publish_provider_turn_with_time(node, generation, plan, None).await
}

#[cfg(test)]
pub(crate) async fn publish_provider_turn_at(
    node: &std::sync::Arc<EmbeddedNode>,
    generation: &str,
    plan: ProviderPublicationPlan,
    now: DateTime<Utc>,
) -> Result<PublishedProviderTurn> {
    publish_provider_turn_with_time(node, generation, plan, Some(now)).await
}

pub(in crate::request::streaming) async fn publish_provider_turn_with_time(
    node: &std::sync::Arc<EmbeddedNode>,
    generation: &str,
    plan: ProviderPublicationPlan,
    fixture_now: Option<DateTime<Utc>>,
) -> Result<PublishedProviderTurn> {
    let plan = std::sync::Arc::new(plan);
    ConfigAccess::transact_local_idempotent(
        node,
        None,
        IdempotentTransactionRetry::Standard,
        "streaming.publish_provider_turn",
        move |txn| {
            let plan = std::sync::Arc::clone(&plan);
            let message_key = plan.message_key.clone();
            let encoded = std::sync::Arc::clone(&plan.encoded);
            let expected = std::sync::Arc::clone(&plan.expected);
            let tool_deadline_at = plan.tool_deadline_at.clone();
            let background_calls = plan.background_calls.clone();
            Box::pin(async move {
            let exemplar = plan.final_flush.as_ref().context("provider publication has no output")?;
            anyhow::ensure!(
                matches!(&exemplar.source, OutputSource::ProviderTurn { .. } | OutputSource::Authored { .. })
                    && !exemplar.source.is_auxiliary_audit()
                    && matches!(&exemplar.writer, OutputWriter::RequestExecution { execution_generation }
                        if execution_generation == generation)
                    && matches!(&exemplar.close, None | Some(SourceClose::Closed { outcome: OutputOutcome::Complete, .. })),
                "provider publication does not bind its request generation"
            );
            anyhow::ensure!(matches!(exemplar.source, OutputSource::ProviderTurn { .. }) || encoded.tool_calls.is_empty(), "authored publication cannot create tool intent");
            let request = load_request_in_txn(txn, exemplar).await?;
            let owner = request.execution_generation.as_deref().context("request generation missing")?;
            let state = request.lifecycle_state.context("request lifecycle missing")?;
            let expiry = request.execution_lease_expires_at.as_deref().context("request expiry missing")?;
            let deadline = DateTime::parse_from_rfc3339(expiry)?.timestamp_millis();
            anyhow::ensure!(authorize_output_append(
                LeaseObservation { request: state, generation: owner, deadline_ms: deadline },
                generation, fixture_now.unwrap_or_else(Utc::now).timestamp_millis()), "provider publication lost its live lease");

            let mut reader = crate::session::TxnCanonicalReader::new_single(
                txn, &exemplar.node_did, exemplar.requester_did.as_deref(),
            );
            let records = reader.load_source(
                &exemplar.request_doc_id, &exemplar.session_id, &exemplar.source,
            ).await?;
            if records.iter().any(|row| row.segment.close.is_some()) {
                if let Some(candidate) = exemplar.close.as_ref() {
                    let accepted = records
                        .iter()
                        .filter_map(|row| row.segment.close.as_ref())
                        .collect::<Vec<_>>();
                    if accepted.as_slice() != [candidate] {
                        return Err(ProviderCloseRejection::InvalidExtent.into());
                    }
                }
                let accepted_writer = records
                    .iter()
                    .find(|row| row.segment.close.is_some())
                    .map(|row| row.segment.writer.clone());
                let published = replay_publication_in_txn(txn, &mut reader, ReplayPublication { exemplar, generation, accepted_writer: accepted_writer.as_ref(), message_key: &message_key, expected: expected.as_ref(), background_calls: &background_calls })
                .await?;
                if let Some(consumption) = plan.consumes_folded.as_ref() {
                    crate::request::lifecycle::queue::ensure_folded_consumed_in_txn(txn, consumption).await?;
                }
                return Ok(published);
            }
            anyhow::ensure!(records.iter().all(|row| row.segment.close.is_none()), "provider source is already closed");

            let prepared = exemplar;
            let mut observations = records.iter().map(|row| ObservedSegment { doc_id: &row.doc_id, segment: &row.segment }).collect::<Vec<_>>();
            if prepared.ordinal.is_some() {
                observations.push(ObservedSegment { doc_id: "<pending-close>", segment: prepared });
            }
            let extent = gents_protocol::output::extent::inspect_open_source(
                &observations, &prepared.request_doc_id, &prepared.source, &prepared.writer)?;
            if let Some(last_created_at) = extent.last_created_at.as_deref() {
                let last_created_at = DateTime::parse_from_rfc3339(last_created_at)
                    .context("provider source has an invalid committed timestamp")?;
                let closing_created_at = DateTime::parse_from_rfc3339(&prepared.created_at)
                    .context("provider closure has an invalid created_at")?;
                if closing_created_at < last_created_at {
                    return Err(ProviderCloseRejection::RegressedTimestamp.into());
                }
            }
            let derived_close = SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: extent.segments,
                stream_bytes: extent.stream_bytes,
            };
            if prepared.close.as_ref().is_some_and(|candidate| candidate != &derived_close) {
                return Err(ProviderCloseRejection::InvalidExtent.into());
            }
            let mut variables = output_segment_create_variables(prepared)?;
            variables["input"]["close"] = serde_json::to_value(&derived_close)?;
            let created = txn.execute_with_variables(
                CREATE_AGENT_OUTPUT_SEGMENT_MUTATION, &variables,
            ).await?;
            let close_doc_id = created_doc_id(&created, "AgentOutputSegment")?;
            let sequence = crate::request::lifecycle::queue::next_append_sequence_in_transaction(
                txn, &prepared.node_did, &prepared.session_id,
            ).await?;
            anyhow::ensure!(
                background_calls
                    .iter()
                    .all(|id| encoded.tool_calls.iter().any(|tool| tool.id == *id)),
                "background publication names a tool call outside this turn"
            );
            let mut tool_call_keys = Vec::with_capacity(encoded.tool_calls.len());
            let mut tool_inputs = Vec::with_capacity(encoded.tool_calls.len());
            for tool in &encoded.tool_calls {
                let await_mode = if background_calls.contains(&tool.id) {
                    crate::tool_call_lifecycle::AwaitMode::Background
                } else {
                    crate::tool_call_lifecycle::AwaitMode::Foreground
                };
                let tool_call_key = format!("{}:{}:{}", prepared.request_doc_id, message_key, tool.native_index);
                tool_call_keys.push(tool_call_key.clone());
                tool_inputs.push(serde_json::json!({
                        "tool_call_key": tool_call_key,
                        "request_id": request.request_id,
                        "request_doc_id": prepared.request_doc_id,
                        "session_id": prepared.session_id,
                        "node_did": prepared.node_did,
                        "requester_did": prepared.requester_did,
                        "message_sequence": sequence,
                        "tool_name": tool.name,
                        "tool_call_id": tool.id,
                        "lifecycle_state": "pending",
                        "deadline_at": tool_deadline_at,
                        "await_mode": await_mode.as_str()
                    }));
            }
            let tool_doc_ids = if tool_inputs.is_empty() {
                Vec::new()
            } else {
                let created = txn.execute_with_variables(
                    "mutation($input: [AgentToolCallMutationInputArg!]!) { create_AgentToolCall(input: $input) { _docID tool_call_key } }",
                    &serde_json::json!({"input": tool_inputs}),
                ).await?;
                created_tool_call_doc_ids(&created, &tool_call_keys)?
            };
            let blocks = encoded.build_blocks(&close_doc_id, &tool_doc_ids)?;
            let message = TranscriptMessage {
                message_key,
                session_id: prepared.session_id.clone(),
                node_did: prepared.node_did.clone(),
                requester_did: prepared.requester_did.clone(),
                request_doc_id: Some(prepared.request_doc_id.clone()),
                publication: MessagePublication::RequestExecution { execution_generation: generation.to_owned() },
                outcome: OutputOutcome::Complete,
                sequence,
                role: encoded.role,
                native_id: encoded.native_id.clone(),
                blocks,
                created_at: prepared.created_at.clone(),
            };
            let created = txn.execute_with_variables(
                CREATE_AGENT_MESSAGE_MUTATION, &transcript_message_create_variables(&message)?,
            ).await?;
            let message_doc_id = created_doc_id(&created, "AgentMessage")?;
            let (_, reconstructed) = reader.load_created_message(&message_doc_id, &close_doc_id).await?;
            anyhow::ensure!(
                &reconstructed == expected.as_ref(),
                "first publication native message changed"
            );

            if let Some(consumption) = plan.consumes_folded.as_ref() {
                anyhow::ensure!(
                    crate::request::lifecycle::RequestLifecycle::input_publication_before_deadline(
                        fixture_now.unwrap_or_else(Utc::now),
                        Some(DateTime::parse_from_rfc3339(&tool_deadline_at)?.with_timezone(&Utc)),
                    ),
                    "request deadline expired before steering consumption"
                );
                crate::request::lifecycle::queue::consume_folded_in_txn(txn, consumption, &prepared.created_at, expected.as_ref()).await?;
            }
            let accepted_tools = encoded.tool_calls.iter().zip(tool_doc_ids).map(|(tool, tool_call_doc_id)| {
                crate::request::streaming::AcceptedToolCall {
                    tool_call_doc_id,
                    request_doc_id: prepared.request_doc_id.clone(),
                    session_id: prepared.session_id.clone(),
                    accepted_header_doc_id: message_doc_id.clone(),
                    message_sequence: sequence,
                    id: tool.id.clone(),
                    call_id: tool.call_id.clone(),
                    tool_name: tool.name.clone(),
                    execution_generation: generation.to_owned(),
                    arguments: PayloadRef { close_doc_id: close_doc_id.clone(), stream: tool.arguments_stream },
                    await_mode: if background_calls.contains(&tool.id) {
                        crate::tool_call_lifecycle::AwaitMode::Background
                    } else {
                        crate::tool_call_lifecycle::AwaitMode::Foreground
                    },
                }
            }).collect();
            Ok(PublishedProviderTurn {
                message_doc_id,
                #[cfg(test)]
                sequence,
                accepted_tools,
            })
        })},
    ).await
}

struct ReplayPublication<'a> {
    exemplar: &'a OutputSegment,
    generation: &'a str,
    accepted_writer: Option<&'a OutputWriter>,
    message_key: &'a str,
    expected: &'a gents_protocol::message::Message,
    background_calls: &'a [String],
}

async fn replay_publication_in_txn(
    txn: &ConfigApplyTxn<'_>,
    reader: &mut crate::session::TxnCanonicalReader<'_, '_>,
    replay: ReplayPublication<'_>,
) -> Result<PublishedProviderTurn> {
    let ReplayPublication {
        exemplar,
        generation,
        accepted_writer,
        message_key,
        expected,
        background_calls,
    } = replay;
    let requester = exemplar.requester_did.as_ref().map_or_else(
        || "null".to_owned(),
        |did| format!("\"{}\"", escape_graphql_string(did)),
    );
    let response = txn.execute_local_response(&format!(
        r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{}" }}, request_doc_id: {{ _eq: "{}" }}, node_did: {{ _eq: "{}" }}, requester_did: {{ _eq: {requester} }} }}) {{ {AGENT_MESSAGE_FIELDS} }} }}"#,
        escape_graphql_string(message_key),
        escape_graphql_string(&exemplar.request_doc_id),
        escape_graphql_string(&exemplar.node_did),
    )).await?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(serde_json::Value::as_array)
        .context("publication replay omitted headers")?;
    anyhow::ensure!(
        rows.len() == 1,
        "closed provider source has no unique accepted header"
    );
    let row = decode_transcript_message_row(&rows[0])?;
    let MessagePublication::RequestExecution {
        execution_generation: accepted_generation,
    } = &row.message.publication
    else {
        anyhow::bail!("publication replay generation changed");
    };
    anyhow::ensure!(
        accepted_generation == generation
            || (matches!(exemplar.source, OutputSource::Authored { .. })
                && accepted_writer
                    == Some(&OutputWriter::RequestExecution {
                        execution_generation: accepted_generation.clone(),
                    })),
        "publication replay generation changed"
    );
    reader.observe_headers(std::slice::from_ref(&row));
    let (_, reconstructed) = reader.load_message(&row.doc_id).await?;
    anyhow::ensure!(
        &reconstructed == expected,
        "publication replay native message changed"
    );
    let mut accepted_tools = Vec::new();
    for block in &row.message.blocks {
        let MessageBlock::ToolCall {
            tool_call_doc_id,
            id,
            call_id,
            name,
            arguments,
            ..
        } = block
        else {
            continue;
        };
        let response = txn.execute_local_response(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID request_doc_id session_id node_did requester_did message_sequence tool_call_id tool_name lifecycle_state await_mode }} }}"#,
            escape_graphql_string(tool_call_doc_id),
        )).await?;
        let tools = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentToolCall"))
            .and_then(serde_json::Value::as_array)
            .context("publication replay omitted pending tool row")?;
        anyhow::ensure!(
            tools.len() == 1,
            "publication replay has ambiguous pending tool row"
        );
        let tool = &tools[0];
        anyhow::ensure!(
            tool.get("request_doc_id")
                .and_then(serde_json::Value::as_str)
                == Some(exemplar.request_doc_id.as_str())
                && tool.get("session_id").and_then(serde_json::Value::as_str)
                    == Some(exemplar.session_id.as_str())
                && tool.get("node_did").and_then(serde_json::Value::as_str)
                    == Some(exemplar.node_did.as_str())
                && tool
                    .get("requester_did")
                    .and_then(serde_json::Value::as_str)
                    == exemplar.requester_did.as_deref()
                && tool
                    .get("message_sequence")
                    .and_then(serde_json::Value::as_u64)
                    == Some(u64::from(row.message.sequence))
                && tool.get("tool_call_id").and_then(serde_json::Value::as_str)
                    == Some(id.as_str())
                && tool.get("tool_name").and_then(serde_json::Value::as_str) == Some(name.as_str())
                && tool
                    .get("lifecycle_state")
                    .and_then(serde_json::Value::as_str)
                    == Some("pending"),
            "publication replay pending tool binding changed"
        );
        let await_mode = crate::tool_call_lifecycle::AwaitMode::from_persisted(
            tool["await_mode"]
                .as_str()
                .context("publication replay lacks await mode")?,
        )
        .context("publication replay has invalid await mode")?;
        let planned = if background_calls.contains(id) {
            crate::tool_call_lifecycle::AwaitMode::Background
        } else {
            crate::tool_call_lifecycle::AwaitMode::Foreground
        };
        if await_mode != planned {
            return Err(ProviderReplayRejection::AwaitModeChanged.into());
        }
        accepted_tools.push(crate::request::streaming::AcceptedToolCall {
            tool_call_doc_id: tool_call_doc_id.clone(),
            request_doc_id: exemplar.request_doc_id.clone(),
            session_id: exemplar.session_id.clone(),
            accepted_header_doc_id: row.doc_id.clone(),
            message_sequence: row.message.sequence,
            id: id.clone(),
            call_id: call_id.clone(),
            tool_name: name.clone(),
            execution_generation: generation.to_owned(),
            arguments: arguments.clone(),
            await_mode,
        });
    }
    Ok(PublishedProviderTurn {
        message_doc_id: row.doc_id,
        #[cfg(test)]
        sequence: row.message.sequence,
        accepted_tools,
    })
}

#[cfg(test)]
#[path = "tool_call_batch_tests.rs"]
mod tool_call_batch_tests;
