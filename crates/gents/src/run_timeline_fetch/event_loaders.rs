use super::query_helpers::required_rows;
use super::*;
use crate::session::load_canonical_message;

/// One timeline message row assembled from an authorized canonical header
/// resolution. The physical `AgentMessage` header doc ID is the row's identity;
/// the strict protocol header and the reconstructed native message travel
/// together as loaded. No string projection or logical request label is
/// synthesized: request ownership is the header's physical `request_doc_id`.
pub(super) async fn resolve_timeline_messages_for_session(
    access: &ConfigAccess,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Vec<TimelineMessageRow>> {
    let header_doc_ids =
        load_timeline_message_header_ids_for_session(access, agent_did, session_id, requester_did)
            .await?;
    resolve_timeline_messages(access, agent_did, session_id, requester_did, header_doc_ids).await
}

/// The canonical messages at one accepted sequence of an exact session scope.
/// Duplicate headers at a sequence are returned, never silently picked.
pub(super) async fn resolve_timeline_messages_at_sequence(
    access: &ConfigAccess,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    sequence: i64,
) -> Result<Vec<TimelineMessageRow>> {
    let header_doc_ids = load_message_header_ids(
        access,
        agent_did,
        session_id,
        requester_did,
        &format!(", sequence: {{ _eq: {sequence} }}"),
    )
    .await?;
    resolve_timeline_messages(access, agent_did, session_id, requester_did, header_doc_ids).await
}

async fn resolve_timeline_messages(
    access: &ConfigAccess,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    header_doc_ids: Vec<String>,
) -> Result<Vec<TimelineMessageRow>> {
    let mut rows = Vec::with_capacity(header_doc_ids.len());
    for header_doc_id in header_doc_ids {
        let (header, message) =
            load_canonical_message(access, &header_doc_id, agent_did, requester_did)
                .await
                .with_context(|| {
                    format!("resolving canonical AgentMessage {header_doc_id} for timeline")
                })?;
        anyhow::ensure!(
            header.session_id == session_id
                && header.agent_did == agent_did
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

/// The physical `AgentMessage` header doc IDs for one exact session scope,
/// in canonical sequence order. Headers are the only selected surface; each
/// doc ID is then resolved exactly through the authorized canonical reader
/// (`session::load_canonical_message`) — there is no serialized-content
/// fallback, and unresolved or integrity-failing headers propagate as errors.
pub(super) async fn load_timeline_message_header_ids_for_session(
    access: &ConfigAccess,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Vec<String>> {
    load_message_header_ids(access, agent_did, session_id, requester_did, "").await
}

async fn load_message_header_ids(
    access: &ConfigAccess,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    extra_filter: &str,
) -> Result<Vec<String>> {
    let scope = crate::session::session_scope_filter(agent_did, session_id, requester_did);
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
    Ok(required_rows(access, "AgentMessage", &query)
        .await?
        .into_iter()
        .map(|row| {
            row.get("_docID")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
                .context("AgentMessage row omitted _docID")
        })
        .collect::<Result<Vec<_>>>()?)
}

pub(super) async fn load_timeline_tool_observations_for_session(
    access: &ConfigAccess,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Vec<ToolLifecycleObservation>> {
    load_tool_observations(access, agent_did, session_id, requester_did, "").await
}

/// Lifecycle observations for one exact physical tool document in a session
/// scope. A document outside the scope yields no rows.
pub(super) async fn load_timeline_tool_observation(
    access: &ConfigAccess,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    tool_doc_id: &str,
) -> Result<Vec<ToolLifecycleObservation>> {
    let tool_doc_id = escape_graphql_string(tool_doc_id);
    load_tool_observations(
        access,
        agent_did,
        session_id,
        requester_did,
        &format!(r#", _docID: {{ _eq: "{tool_doc_id}" }}"#),
    )
    .await
}

async fn load_tool_observations(
    access: &ConfigAccess,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    extra_filter: &str,
) -> Result<Vec<ToolLifecycleObservation>> {
    let scope = crate::session::session_scope_filter(agent_did, session_id, requester_did);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ {scope}{extra_filter} }},
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
                delegated_input
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
                cancel_policy
                cancel_cause
                child_request_id
            }}
        }}"#,
    );
    load_rows(access, "AgentToolCall", &query).await
}

pub(super) async fn resolve_timeline_tool_observations(
    access: &ConfigAccess,
    agent_did: &str,
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
                    agent_did,
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
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    request_doc_id: &str,
    tool_doc_id: &str,
) -> Result<String> {
    use gents_protocol::output::{
        reconstruction::{reconstruct_stream, ObservedSegment},
        OutputSource, PayloadRef, StreamPayload,
    };
    let scope = crate::session::session_scope_filter(agent_did, session_id, requester_did);
    let request = escape_graphql_string(request_doc_id);
    let query = format!(
        r#"{{ AgentOutputSegment(filter: {{ {scope}, request_doc_id: {{ _eq: "{request}" }} }}) {{ {} }} }}"#,
        crate::session::canonical_rows::AGENT_OUTPUT_SEGMENT_FIELDS
    );
    let values = required_rows(access, "AgentOutputSegment", &query).await?;
    let source = OutputSource::ToolCall {
        tool_call_doc_id: tool_doc_id.to_owned(),
    };
    let rows = values
        .iter()
        .map(crate::session::canonical_rows::decode_output_segment_row)
        .collect::<Result<Vec<_>>>()?
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
    pub(super) delegated_input: Option<gents_protocol::output::DelegatedToolInput>,
}

/// The lifecycle row contributes status, never payload bytes. Payloads come
/// from the already authorized, fully reconstructed session messages (or the
/// host's argument-only delegated admission). An absent delivery stays absent.
pub(super) fn resolve_tool_payloads(
    observation: ToolLifecycleObservation,
    messages: &[TimelineMessageRow],
) -> Result<TimelineToolCallRow> {
    use gents_protocol::message::{AssistantContent, Message};
    use gents_protocol::output::{MessageBlock, MessagePublication, MessageRole, OutputOutcome};
    let ToolLifecycleObservation {
        mut row,
        spawned_by_tool_call_doc_id,
        delegated_input,
    } = observation;
    // `status` is a nullable historical display mirror. The accepted
    // lifecycle row is authoritative even before dispatch has installed a
    // running state, so require that fact and project it into the derived
    // timeline status rather than decoding a null as an invented empty value.
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
    anyhow::ensure!(
        !(spawned_by_tool_call_doc_id.is_some() && delegated_input.is_some()),
        "timeline tool cannot be both spawned and delegated"
    );
    let mut accepted_call_id = None;
    if let Some(input) = delegated_input {
        // The source ref is provenance, not permission to fetch its private
        // coordinator stream. The authorized host row carries exact arguments.
        row.args = input.arguments;
    } else {
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
                let Some(AssistantContent::ToolCall(native)) = content.iter().nth(index) else {
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

pub(super) async fn load_timeline_inference_calls_for_request(
    access: &ConfigAccess,
    request_doc_id: &str,
) -> Result<Vec<TimelineInferenceCallRow>> {
    let query = format!(
        r#"{{
            InferenceCall(
                filter: {{ request_doc_id: {{ _eq: "{}" }} }},
                order: {{ call_seq: ASC }}
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
                behavior_id
                agent_did
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
        escape_graphql_string(request_doc_id)
    );
    load_rows(access, "InferenceCall", &query).await
}

/// The rendered-request capture rows for one session, metadata columns only.
/// `request_json` is deliberately never selected here — see
/// `TimelineRenderedRequestRow`. Pre-#1059 databases have no `RenderedRequest`
/// collection; `load_rows` reports that as an empty section, not a failed
/// timeline.
pub(super) async fn load_timeline_rendered_requests_for_session(
    access: &ConfigAccess,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Vec<TimelineRenderedRequestRow>> {
    // RenderedRequest stores absent requester authority as the required empty
    // string, while canonical session-scoped documents represent it as null.
    let requester_did = requester_did.unwrap_or_default();
    let scope = format!(
        r#"agent_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }}, requester_did: {{ _eq: "{}" }}"#,
        escape_graphql_string(agent_did),
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
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Vec<TimelineCompactionRow>> {
    let scope = crate::session::session_scope_filter(agent_did, session_id, requester_did);
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

pub(super) async fn load_timeline_provider_context_reductions_for_request(
    access: &ConfigAccess,
    request_doc_id: &str,
) -> Result<Vec<TimelineProviderContextReductionRow>> {
    let query = format!(
        r#"{{
            ProviderContextReduction(
                filter: {{ request_doc_id: {{ _eq: "{}" }} }},
                order: {{ reduction_index: ASC }}
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
        escape_graphql_string(request_doc_id)
    );
    load_rows(access, "ProviderContextReduction", &query).await
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
