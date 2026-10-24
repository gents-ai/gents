use super::query_helpers::required_rows;
use super::*;
use crate::session::load_canonical_message;
use crate::session::{
    canonical_rows::{decode_transcript_message_row, AGENT_MESSAGE_FIELDS},
    TxnCanonicalReader,
};

pub(super) async fn resolve_timeline_messages_for_session(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Vec<TimelineMessageRow>> {
    let node_did = node_did.to_owned();
    let session_id = session_id.to_owned();
    let requester_did = requester_did.map(str::to_owned);
    access
        .transact_readonly("timeline.canonical_session_messages", move |txn| {
            let node_did = node_did.clone();
            let session_id = session_id.clone();
            let requester_did = requester_did.clone();
            Box::pin(async move {
                let scope = crate::session::session_scope_filter(
                    &node_did,
                    &session_id,
                    requester_did.as_deref(),
                );
                let query = format!(
                    r#"{{ AgentMessage(filter: {{ {scope} }}, order: {{ sequence: ASC }}) {{ {AGENT_MESSAGE_FIELDS} }} }}"#
                );
                let response = txn.execute(&query).await?;
                anyhow::ensure!(
                    response.get("errors").is_none_or(|errors| {
                        errors.is_null() || errors.as_array().is_some_and(Vec::is_empty)
                    }),
                    "timeline AgentMessage query failed: {}",
                    response["errors"]
                );
                let raw_headers = response
                    .get("data")
                    .and_then(|data| data.get("AgentMessage"))
                    .and_then(serde_json::Value::as_array)
                    .context("timeline query omitted required AgentMessage row array")?;
                anyhow::ensure!(
                    raw_headers.iter().all(serde_json::Value::is_object),
                    "timeline AgentMessage query returned a non-document row"
                );
                let mut headers = raw_headers
                    .iter()
                    .map(decode_transcript_message_row)
                    .collect::<Result<Vec<_>, _>>()?;
                for header in &headers {
                    anyhow::ensure!(
                        header.message.session_id == session_id
                            && header.message.node_did == node_did
                            && header.message.requester_did.as_deref() == requester_did.as_deref(),
                        "canonical AgentMessage {} crossed the requested session scope",
                        header.doc_id
                    );
                }
                headers.sort_by_key(|header| header.message.sequence);
                let header_order = headers
                    .iter()
                    .map(|header| header.doc_id.clone())
                    .collect::<Vec<_>>();
                let mut reader = TxnCanonicalReader::new(
                    txn,
                    &node_did,
                    requester_did.as_deref(),
                );
                reader.observe_headers(&headers);
                let mut groups = BTreeMap::<String, Vec<_>>::new();
                let mut unbound_headers = Vec::new();
                for header in headers {
                    if let Some(request_doc_id) = &header.message.request_doc_id {
                        groups
                            .entry(request_doc_id.clone())
                            .or_default()
                            .push(header);
                    } else {
                        unbound_headers.push(header);
                    }
                }
                let mut resolved = BTreeMap::new();
                let grouped_headers = groups
                    .into_values()
                    .chain(unbound_headers.into_iter().map(|header| vec![header]));
                for group in grouped_headers {
                    for header in group {
                        let physical_id = header.doc_id.clone();
                        let (canonical_header, message) = reader
                            .load_message(&physical_id)
                            .await
                            .with_context(|| {
                                format!("resolving canonical AgentMessage {physical_id} for timeline")
                            })?;
                        anyhow::ensure!(
                            canonical_header == header.message,
                            "canonical AgentMessage {physical_id} changed during timeline reconstruction"
                        );
                        resolved.insert(
                            physical_id.clone(),
                            TimelineMessageRow::from_canonical(
                                physical_id,
                                canonical_header,
                                message,
                            ),
                        );
                    }
                    reader.clear_output_cache();
                }
                let mut rows = Vec::with_capacity(resolved.len());
                for header_doc_id in header_order {
                    let row = resolved.remove(&header_doc_id).with_context(|| {
                        format!("canonical AgentMessage {header_doc_id} was not resolved")
                    })?;
                    rows.push(row);
                }
                Ok(rows)
            })
        })
        .await
}

pub(super) async fn resolve_timeline_messages_at_sequences(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    sequences: &[i64],
) -> Result<Vec<TimelineMessageRow>> {
    if sequences.is_empty() {
        return Ok(Vec::new());
    }
    let sequence_filter = sequences
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let header_doc_ids = load_message_header_ids(
        access,
        node_did,
        session_id,
        requester_did,
        &format!(", sequence: {{ _in: [{sequence_filter}] }}"),
    )
    .await?;
    resolve_timeline_messages(access, node_did, session_id, requester_did, header_doc_ids).await
}

async fn resolve_timeline_messages(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    header_doc_ids: Vec<String>,
) -> Result<Vec<TimelineMessageRow>> {
    let mut rows = Vec::with_capacity(header_doc_ids.len());
    for header_doc_id in header_doc_ids {
        let (header, message) =
            load_canonical_message(access, &header_doc_id, node_did, requester_did)
                .await
                .with_context(|| {
                    format!("resolving canonical AgentMessage {header_doc_id} for timeline")
                })?;
        anyhow::ensure!(
            header.session_id == session_id
                && header.node_did == node_did
                && header.requester_did.as_deref() == requester_did,
            "canonical AgentMessage {header_doc_id} crossed the requested session scope"
        );
        rows.push(TimelineMessageRow::from_canonical(
            header_doc_id,
            header,
            message,
        ));
    }
    Ok(rows)
}

async fn load_message_header_ids(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    extra_filter: &str,
) -> Result<Vec<String>> {
    let scope = crate::session::session_scope_filter(node_did, session_id, requester_did);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ {scope}{extra_filter} }},
                order: {{ sequence: ASC }}
            ) {{
                _docID
            }}
        }}"#,
    );
    required_rows(access, "AgentMessage", &query)
        .await?
        .into_iter()
        .map(|row| {
            row.get("_docID")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
                .context("AgentMessage row omitted _docID")
        })
        .collect::<Result<Vec<_>>>()
}

pub(super) async fn load_timeline_tool_observations_for_session(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Vec<ToolLifecycleObservation>> {
    load_tool_observations(access, node_did, session_id, requester_did, "").await
}

pub(super) async fn load_timeline_tool_observations(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    tool_doc_ids: &[String],
) -> Result<Vec<ToolLifecycleObservation>> {
    if tool_doc_ids.is_empty() {
        return Ok(Vec::new());
    }
    let doc_ids = gents_protocol::graphql::graphql_string_list_literal(tool_doc_ids);
    load_tool_observations(
        access,
        node_did,
        session_id,
        requester_did,
        &format!(r#"docIDs: {doc_ids},"#),
    )
    .await
}

async fn load_tool_observations(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    extra_args: &str,
) -> Result<Vec<ToolLifecycleObservation>> {
    let scope = crate::session::session_scope_filter(node_did, session_id, requester_did);
    let query = format!(
        r#"{{
            AgentToolCall(
                {extra_args}
                filter: {{ {scope} }},
                order: {{ started_at: ASC }}
            ) {{
                _docID
                request_id
                request_doc_id
                session_id
                message_sequence
                tool_name
                tool_call_id
                spawned_by_tool_call_doc_id
                status
                lifecycle_state
                started_at
                deadline_at
                completed_at
                selected_service_id
                selected_tool_name
                tool_failure_class
                denial_reason
                denied_argv
                denied_command
                denied_argument
                denied_subcommand
                denied_prefix
                policy_mode
                policy_network
                latency_ms
                await_mode
                cancel_cause
            }}
        }}"#,
    );
    load_rows(access, "AgentToolCall", &query).await
}

pub(super) async fn resolve_timeline_tool_observations(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    observations: Vec<ToolLifecycleObservation>,
    messages: &[TimelineMessageRow],
) -> Result<Vec<TimelineToolCallRow>> {
    let mut rows = Vec::with_capacity(observations.len());
    for observation in observations {
        let spawned = observation.spawned_by_tool_call_doc_id.is_some();
        let mut row = resolve_tool_payloads(observation, messages)?;
        if spawned
            && matches!(
                row.lifecycle_state.as_deref(),
                Some("completed" | "failed" | "timedOut" | "cancelled")
            )
        {
            let tool_doc_id = row
                .doc_id
                .as_deref()
                .context("spawned timeline tool lacks physical identity")?;
            let request_doc_id = row
                .request_doc_id
                .as_deref()
                .context("spawned timeline tool lacks request binding")?;
            row.result = Some(
                load_spawned_tool_output(
                    access,
                    node_did,
                    session_id,
                    requester_did,
                    request_doc_id,
                    tool_doc_id,
                )
                .await?,
            );
        }
        rows.push(row);
    }
    Ok(rows)
}

async fn load_spawned_tool_output(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    request_doc_id: &str,
    tool_doc_id: &str,
) -> Result<String> {
    use gents_protocol::output::{
        reconstruction::{reconstruct_stream, ObservedSegment},
        OutputSource, PayloadRef, StreamPayload,
    };
    let query = crate::session::canonical_rows::request_output_segments_query(request_doc_id);
    let values = required_rows(access, "AgentOutputSegment", &query).await?;
    let source = OutputSource::ToolCall {
        tool_call_doc_id: tool_doc_id.to_owned(),
    };
    let rows = crate::session::canonical_rows::decode_scoped_request_output_segments(
        &values,
        node_did,
        Some(session_id),
        requester_did,
    )?
    .into_iter()
    .filter(|row| row.segment.source == source)
    .collect::<Vec<_>>();
    let closes = rows
        .iter()
        .filter(|row| row.segment.close.is_some())
        .collect::<Vec<_>>();
    anyhow::ensure!(
        closes.len() == 1,
        "spawned timeline tool output lacks one exact closure"
    );
    let observed = rows
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    let stream = reconstruct_stream(
        &observed,
        &[],
        &[],
        &PayloadRef {
            close_doc_id: closes[0].doc_id.clone(),
            stream: 0,
        },
    )
    .map_err(anyhow::Error::from)?;
    anyhow::ensure!(
        matches!(stream.declaration.payload, StreamPayload::ToolOutput),
        "spawned timeline source is not tool output"
    );
    Ok(stream.text)
}

#[derive(serde::Deserialize)]
pub(super) struct ToolLifecycleObservation {
    #[serde(flatten)]
    pub(super) row: TimelineToolCallRow,
    pub(super) spawned_by_tool_call_doc_id: Option<String>,
}

pub(super) fn resolve_tool_payloads(
    observation: ToolLifecycleObservation,
    messages: &[TimelineMessageRow],
) -> Result<TimelineToolCallRow> {
    use gents_protocol::message::{AssistantContent, Message};
    use gents_protocol::output::{MessageBlock, MessagePublication, MessageRole, OutputOutcome};
    let ToolLifecycleObservation {
        mut row,
        spawned_by_tool_call_doc_id,
    } = observation;
    let lifecycle_state = row
        .lifecycle_state
        .as_deref()
        .filter(|state| !state.trim().is_empty())
        .context("timeline AgentToolCall omits authoritative lifecycle_state")?;
    row.status = lifecycle_state.to_owned();
    let tool_doc = row
        .doc_id
        .as_deref()
        .context("timeline tool lacks physical identity")?;
    let request_doc = row
        .request_doc_id
        .as_deref()
        .context("timeline tool lacks physical request binding")?;
    let accepted_call_id;
    {
        let binding_doc = spawned_by_tool_call_doc_id.as_deref().unwrap_or(tool_doc);
        let mut bindings = Vec::new();
        for message in messages {
            let header = &message.header;
            if header.session_id != row.session_id
                || header.request_doc_id.as_deref() != Some(request_doc)
                || Some(i64::from(header.sequence)) != row.message_sequence
                || header.role != MessageRole::Assistant
                || header.outcome != OutputOutcome::Complete
                || !matches!(
                    header.publication,
                    MessagePublication::RequestExecution { .. }
                )
            {
                continue;
            }
            for (index, block) in header.blocks.iter().enumerate() {
                let MessageBlock::ToolCall {
                    tool_call_doc_id,
                    id,
                    call_id,
                    name,
                    ..
                } = block
                else {
                    continue;
                };
                if tool_call_doc_id != binding_doc {
                    continue;
                }
                let Message::Assistant { content, .. } = &message.message else {
                    anyhow::bail!("accepted tool header reconstructed as a non-assistant message")
                };
                let Some(AssistantContent::ToolCall(native)) = content.get(index) else {
                    anyhow::bail!("accepted physical tool does not match its native block position")
                };
                anyhow::ensure!(
                    native.id == *id && native.call_id == *call_id && native.function.name == *name,
                    "accepted tool native identity disagrees with its header"
                );
                if spawned_by_tool_call_doc_id.is_some() {
                    anyhow::ensure!(
                        name == crate::toolset::SPAWN_PROCESS_TOOL_NAME,
                        "spawned tool parent is not an accepted spawn_process invocation"
                    );
                    let input: crate::background_tools::BackgroundToolArgs =
                        serde_json::from_value(native.function.arguments.clone())?;
                    anyhow::ensure!(
                        input.tool_name == row.tool_name,
                        "spawned tool name differs from parent input"
                    );
                    bindings.push((serde_json::to_string(&input.args)?, None));
                } else {
                    anyhow::ensure!(
                        id == &row.tool_call_id && name == &row.tool_name,
                        "timeline tool row disagrees with accepted native identity"
                    );
                    bindings.push((
                        serde_json::to_string(&native.function.arguments)?,
                        call_id.clone(),
                    ));
                }
            }
        }
        anyhow::ensure!(
            bindings.len() == 1,
            "timeline tool lacks one exact accepted physical binding"
        );
        let (arguments, call_id) = bindings.pop().expect("one binding");
        row.args = arguments;
        accepted_call_id = call_id;
    }

    let mut deliveries = Vec::new();
    for message in messages {
        let header = &message.header;
        if header.session_id != row.session_id {
            continue;
        }
        if spawned_by_tool_call_doc_id.is_some() {
            if matches!(&header.publication, MessagePublication::ToolDelivery { tool_call_doc_id } if tool_call_doc_id == tool_doc)
                && header.role == MessageRole::User
            {
                if header
                    .blocks
                    .iter()
                    .any(|block| matches!(block, MessageBlock::ToolResult { .. }))
                {
                    deliveries.push(crate::tool_call_lifecycle::query::render_tool_result(
                        &message.message,
                    )?);
                } else {
                    deliveries.push(
                        gents_protocol::transcript::present_message(&message.message).body_markdown,
                    );
                }
            }
        } else if crate::lifecycle::is_exact_invocation_reply(
            header,
            request_doc,
            &row.session_id,
            tool_doc,
            &row.tool_call_id,
            &accepted_call_id,
        ) {
            deliveries.push(crate::tool_call_lifecycle::query::render_tool_result(
                &message.message,
            )?);
        }
    }
    anyhow::ensure!(
        deliveries.len() <= 1,
        "timeline tool has conflicting canonical deliveries"
    );
    row.result = deliveries.pop();
    Ok(row)
}

pub(super) async fn load_timeline_inference_calls_for_requests(
    access: &ConfigAccess,
    request_doc_ids: &[String],
) -> Result<Vec<TimelineInferenceCallRow>> {
    const BATCH_SIZE: usize = 128;
    let mut rows = Vec::new();
    for batch in request_doc_ids.chunks(BATCH_SIZE) {
        if batch.is_empty() {
            continue;
        }
        let request_ids = batch
            .iter()
            .map(|id| format!("\"{}\"", escape_graphql_string(id)))
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            r#"{{
            InferenceCall(
                filter: {{ request_doc_id: {{ _in: [{request_ids}] }} }}
            ) {{
                _docID
                call_id
                runtime_instance_id
                request_id
                request_doc_id
                call_seq
                attempt
                call_state
                failure_reason
                queued_at
                started_at
                ended_at
                backend_id
                agent_id
                node_did
                call_kind
                priority
                queue_depth_at_enqueue
                controller_generation
                backend_config_fingerprint
                prompt_tokens
                completion_tokens
                cached_input_tokens
                context_accounting_json
            }}
        }}"#,
        );
        let batch_rows =
            load_rows::<TimelineInferenceCallRow>(access, "InferenceCall", &query).await?;
        let requested = batch.iter().cloned().collect::<BTreeSet<_>>();
        for row in &batch_rows {
            anyhow::ensure!(
                row.request_doc_id
                    .as_ref()
                    .is_some_and(|request_doc_id| requested.contains(request_doc_id)),
                "timeline InferenceCall returned a row outside its request batch"
            );
        }
        rows.extend(batch_rows);
    }
    let requested = request_doc_ids.iter().cloned().collect::<BTreeSet<_>>();
    let mut by_request = BTreeMap::<String, Vec<TimelineInferenceCallRow>>::new();
    for row in rows {
        let request_doc_id = row
            .request_doc_id
            .as_deref()
            .context("timeline InferenceCall omitted request_doc_id")?;
        anyhow::ensure!(
            requested.contains(request_doc_id),
            "timeline InferenceCall returned an unrequested request document"
        );
        by_request
            .entry(request_doc_id.to_owned())
            .or_default()
            .push(row);
    }
    let mut ordered = Vec::new();
    for request_doc_id in request_doc_ids {
        let mut request_rows = by_request.remove(request_doc_id).unwrap_or_default();
        request_rows.sort_by_key(|row| row.call_seq);
        ordered.extend(request_rows);
    }
    Ok(ordered)
}

pub(super) async fn load_timeline_rendered_requests_for_session(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Vec<TimelineRenderedRequestRow>> {
    let requester_did = requester_did.unwrap_or_default();
    let scope = format!(
        r#"node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }}, requester_did: {{ _eq: "{}" }}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(session_id),
        escape_graphql_string(requester_did),
    );
    let query = format!(
        r#"{{
            RenderedRequest(
                filter: {{ {scope} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                capture_key
                request_doc_id
                request_id
                session_id
                capture_scope
                turn_index
                attempt
                capture_version
                model_name
                source
                provenance_json
                created_at
            }}
        }}"#,
    );
    load_rows(access, "RenderedRequest", &query).await
}

pub(super) async fn load_timeline_rendered_requests_for_request(
    access: &ConfigAccess,
    request_id: &str,
) -> Result<Vec<TimelineRenderedRequestRow>> {
    let query = format!(
        r#"{{
            RenderedRequest(
                filter: {{ request_id: {{ _eq: "{}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                capture_key
                request_doc_id
                request_id
                session_id
                capture_scope
                turn_index
                attempt
                capture_version
                model_name
                source
                provenance_json
                created_at
            }}
        }}"#,
        escape_graphql_string(request_id)
    );
    load_rows(access, "RenderedRequest", &query).await
}

pub(super) async fn load_timeline_compactions_for_session(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Vec<TimelineCompactionRow>> {
    let scope = crate::session::session_scope_filter(node_did, session_id, requester_did);
    let query = format!(
        r#"{{
            CompactionEntry(
                filter: {{ {scope} }},
                order: {{ sequence: ASC }}
            ) {{
                _docID
                compaction_key
                request_id
                request_doc_id
                session_id
                sequence
                summary
                messages_compacted
                original_tokens
                compacted_tokens
                created_at
            }}
        }}"#,
    );
    load_rows(access, "CompactionEntry", &query).await
}

pub(super) async fn load_timeline_provider_context_reductions_for_requests(
    access: &ConfigAccess,
    request_doc_ids: &[String],
) -> Result<Vec<TimelineProviderContextReductionRow>> {
    const BATCH_SIZE: usize = 128;
    let mut rows = Vec::new();
    for batch in request_doc_ids.chunks(BATCH_SIZE) {
        if batch.is_empty() {
            continue;
        }
        let request_ids = batch
            .iter()
            .map(|id| format!("\"{}\"", escape_graphql_string(id)))
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            r#"{{
            ProviderContextReduction(
                filter: {{ request_doc_id: {{ _in: [{request_ids}] }} }}
            ) {{
                _docID
                reduction_key
                request_id
                request_doc_id
                session_id
                reduction_index
                turn_index
                parent_reduction_key
                producer_call_id
                producer_call_seq
                messages_compacted
                original_tokens
                compacted_tokens
                created_at
            }}
        }}"#,
        );
        let batch_rows = load_rows::<TimelineProviderContextReductionRow>(
            access,
            "ProviderContextReduction",
            &query,
        )
        .await?;
        let requested = batch.iter().cloned().collect::<BTreeSet<_>>();
        anyhow::ensure!(
            batch_rows
                .iter()
                .all(|row| requested.contains(&row.request_doc_id)),
            "timeline ProviderContextReduction returned a row outside its request batch"
        );
        rows.extend(batch_rows);
    }
    let requested = request_doc_ids.iter().cloned().collect::<BTreeSet<_>>();
    let mut by_request = BTreeMap::<String, Vec<TimelineProviderContextReductionRow>>::new();
    for row in rows {
        anyhow::ensure!(
            requested.contains(&row.request_doc_id),
            "timeline ProviderContextReduction returned an unrequested request document"
        );
        by_request
            .entry(row.request_doc_id.clone())
            .or_default()
            .push(row);
    }
    let mut ordered = Vec::new();
    for request_doc_id in request_doc_ids {
        let mut request_rows = by_request.remove(request_doc_id).unwrap_or_default();
        request_rows.sort_by_key(|row| row.reduction_index);
        ordered.extend(request_rows);
    }
    Ok(ordered)
}

pub(super) async fn load_timeline_rendered_request_refs(
    access: &ConfigAccess,
    request_doc_id: &str,
) -> Result<Vec<TimelineRenderedRequestRef>> {
    let query = format!(
        r#"{{
            RenderedRequest(
                filter: {{ request_doc_id: {{ _eq: "{}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                request_doc_id
                request_commit_cid
            }}
        }}"#,
        escape_graphql_string(request_doc_id)
    );
    load_rows(access, "RenderedRequest", &query).await
}
