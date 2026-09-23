use super::*;

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};

use tokio::sync::Mutex;

type BackgroundCompletionGate = Mutex<()>;

pub(crate) struct ToolNotificationPublication {
    pub(crate) tool_call_doc_id: String,
    pub(crate) presentation: Vec<gents_protocol::output::PresentationPart>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct BackgroundCompletionGateKey {
    node: usize,
    session_id: String,
    agent_did: String,
    queue_key: String,
}

/// Serialize the read/create/reconcile path for one local coalescing domain.
/// The proven queue transition is sequential: one coalescing enqueue must
/// observe the pending entry created by the previous transition. DefraDB
/// transactions conflict only when they touch the same document, so two empty
/// reads could otherwise create disjoint requests and return before duplicate
/// reconciliation converges them. The weak registry does not retain either
/// nodes or idle gates; stale entries are pruned when a new gate is created.
pub(super) fn background_completion_gate(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    queue_key: &str,
) -> Arc<BackgroundCompletionGate> {
    static GATES: OnceLock<
        StdMutex<HashMap<BackgroundCompletionGateKey, Weak<BackgroundCompletionGate>>>,
    > = OnceLock::new();

    let key = BackgroundCompletionGateKey {
        node: node as *const EmbeddedNode as usize,
        session_id: session_id.to_string(),
        agent_did: agent_did.to_string(),
        queue_key: queue_key.to_string(),
    };
    let mut gates = GATES
        .get_or_init(|| StdMutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(gate) = gates.get(&key).and_then(Weak::upgrade) {
        return gate;
    }

    gates.retain(|_, gate| gate.strong_count() > 0);
    let gate = Arc::new(Mutex::new(()));
    gates.insert(key, Arc::downgrade(&gate));
    gate
}

/// Atomically persist background input. Goal-owned sessions bind it to its
/// parent without waking; otherwise reuse or create the coalesced pending wake.
/// A concurrent claim conflicts and retries, so a wake cannot precede its input.
/// The single transaction owner for fresh input and canonical receipt replay.
/// An observed receipt ID is reloaded and validated inside this transaction.
pub(crate) async fn persist_background_completion_with_message_canonical(
    node: &EmbeddedNode,
    parent: &AgentRequest,
    notification_content: &str,
    message_key: &str,
    wake_content: &str,
    queue: RequestQueue,
    existing_notification_doc_id: Option<&str>,
    native: &ToolNotificationPublication,
) -> Result<EnqueuedBackgroundCompletionInput> {
    anyhow::ensure!(
        queue.source == QueueSource::BackgroundCompletion && queue.policy == QueuePolicy::Coalesce,
        "atomic background completion enqueue requires coalescing background queue input"
    );
    let queue_key = queue
        .key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .context("atomic background completion enqueue requires a queue key")?
        .to_string();

    let gate = background_completion_gate(node, &parent.session_id, &parent.agent_did, &queue_key);
    let _guard = gate.lock().await;

    let behavior_id = parent_behavior_id(parent)?;
    let queue_key_ref = &queue_key;
    let behavior_id = &behavior_id;
    let queue = &queue;

    let mut enqueued = crate::config_client::ConfigAccess::transact_local_idempotent(
        node,
        None,
        crate::config_client::IdempotentTransactionRetry::Standard,
        "lifecycle.enqueue_background_completion",
        move |txn| {
            Box::pin(async move {
                background_completion_transaction_attempt(
                    txn,
                    parent,
                    notification_content,
                    message_key,
                    queue_key_ref,
                    behavior_id,
                    wake_content,
                    queue,
                    existing_notification_doc_id,
                    native,
                )
                .await
            })
        },
    )
    .await?;

    if let Some(created) = enqueued
        .request
        .as_ref()
        .filter(|_| enqueued.created_request)
    {
        let created_request_doc_id = created.doc_id.clone();
        let active_request = reconcile_coalesced_pending_request(
            node,
            &parent.session_id,
            &parent.agent_did,
            QueueSource::BackgroundCompletion,
            &queue_key,
        )
        .await?
        .unwrap_or_else(|| created.clone());
        enqueued.created_request = active_request.doc_id == created_request_doc_id;

        enqueued.request = Some(active_request);
    }

    Ok(enqueued)
}

#[cfg(test)]
pub(crate) async fn persist_background_completion_with_message(
    node: &EmbeddedNode,
    parent: &AgentRequest,
    notification_content: &str,
    message_key: &str,
    wake_content: &str,
    queue: RequestQueue,
    existing_notification_doc_id: Option<&str>,
) -> Result<EnqueuedBackgroundCompletionInput> {
    use gents_protocol::output::{
        OutputOutcome, OutputSegment, OutputSource, OutputWriter, SegmentRun, SourceClose,
        StreamDeclaration, StreamPayload,
    };
    let tool_key = format!("fixture:{message_key}");
    let (tool_call_doc_id, close_doc_id) = crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "test.canonical_background_notification_authority",
        |txn| {
            let tool_key = tool_key.clone();
            Box::pin(async move {
                let existing = txn
                    .execute(&format!(
                        r#"{{
                AgentToolCall(filter: {{ tool_call_key: {{ _eq: "{}" }} }}, limit: 2) {{ _docID }}
                AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {} }}
            }}"#,
                        escape_graphql_string(&tool_key),
                        escape_graphql_string(&parent.doc_id),
                        crate::session::canonical_rows::AGENT_OUTPUT_SEGMENT_FIELDS
                    ))
                    .await?;
                let tools = existing["data"]["AgentToolCall"]
                    .as_array()
                    .context("fixture tool lookup")?;
                if tools.len() == 1 {
                    let tool_doc = tools[0]["_docID"]
                        .as_str()
                        .context("fixture tool ID")?
                        .to_owned();
                    let source = OutputSource::ToolCall {
                        tool_call_doc_id: tool_doc.clone(),
                    };
                    let rows = existing["data"]["AgentOutputSegment"]
                        .as_array()
                        .context("fixture segment lookup")?;
                    let close = rows
                        .iter()
                        .map(crate::session::canonical_rows::decode_output_segment_row)
                        .collect::<Result<Vec<_>>>()?
                        .into_iter()
                        .find(|row| row.segment.source == source && row.segment.close.is_some())
                        .context("fixture tool closure")?;
                    return Ok((tool_doc, close.doc_id));
                }
                anyhow::ensure!(tools.is_empty(), "ambiguous fixture tool authority");
                let created = txn
                    .execute(&format!(
                        r#"mutation {{ create_AgentToolCall(input: {{
                tool_call_key: "{}", tool_call_id: "{}", request_id: "{}",
                request_doc_id: "{}", agent_did: "{}", requester_did: {}, session_id: "{}",
                tool_name: "fixture", message_sequence: 1, await_mode: "background",
                status: "completed", lifecycle_state: "completed"
            }}) {{ _docID }} }}"#,
                        escape_graphql_string(&tool_key),
                        escape_graphql_string(&tool_key),
                        escape_graphql_string(&parent.request_id),
                        escape_graphql_string(&parent.doc_id),
                        escape_graphql_string(&parent.agent_did),
                        parent
                            .requester_did
                            .as_deref()
                            .map(|v| format!("\"{}\"", escape_graphql_string(v)))
                            .unwrap_or_else(|| "null".into()),
                        escape_graphql_string(&parent.session_id)
                    ))
                    .await?;
                let tool_doc = crate::graphql::created_doc_id(&created, "AgentToolCall")?;
                let segment = OutputSegment {
                    agent_did: parent.agent_did.clone(),
                    requester_did: parent.requester_did.clone(),
                    session_id: parent.session_id.clone(),
                    request_doc_id: parent.doc_id.clone(),
                    source: OutputSource::ToolCall {
                        tool_call_doc_id: tool_doc.clone(),
                    },
                    writer: OutputWriter::ToolExecution {
                        tool_call_doc_id: tool_doc.clone(),
                    },
                    ordinal: Some(0),
                    runs: vec![SegmentRun {
                        stream: 0,
                        bytes: notification_content.len() as u32,
                        declaration: Some(StreamDeclaration {
                            block_index: 0,
                            part_index: 0,
                            payload: StreamPayload::ToolOutput,
                        }),
                    }],
                    payload: notification_content.to_owned(),
                    close: Some(SourceClose::Closed {
                        outcome: OutputOutcome::Complete,
                        segments: 1,
                        stream_bytes: vec![notification_content.len() as u64],
                    }),
                    created_at: chrono::Utc::now().to_rfc3339(),
                };
                let created = txn
                    .execute_with_variables(
                        crate::session::canonical_rows::CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                        &crate::session::canonical_rows::output_segment_create_variables(&segment)?,
                    )
                    .await?;
                Ok((
                    tool_doc,
                    crate::graphql::created_doc_id(&created, "AgentOutputSegment")?,
                ))
            })
        },
    )
    .await?;
    let _ = close_doc_id;
    persist_background_completion_with_message_canonical(
        node,
        parent,
        notification_content,
        message_key,
        wake_content,
        queue,
        existing_notification_doc_id,
        &ToolNotificationPublication {
            tool_call_doc_id,
            presentation: vec![gents_protocol::output::PresentationPart::OutputRange {
                start_byte: 0,
                end_byte: notification_content.len() as u64,
            }],
        },
    )
    .await
}

async fn background_completion_transaction_attempt(
    txn: &ConfigApplyTxn<'_>,
    parent: &AgentRequest,
    content: &str,
    message_key: &str,
    queue_key: &str,
    behavior_id: &str,
    wake_content: &str,
    queue: &RequestQueue,
    existing_notification_doc_id: Option<&str>,
    native: &ToolNotificationPublication,
) -> Result<EnqueuedBackgroundCompletionInput> {
    use sha2::{Digest, Sha256};

    let goal_owned =
        crate::goal::load_canonical_goal_in_txn(txn, &parent.agent_did, &parent.session_id)
            .await?
            .is_some();
    let escaped_session_id = escape_graphql_string(&parent.session_id);
    let escaped_agent_did = escape_graphql_string(&parent.agent_did);
    let notification_filter = match existing_notification_doc_id {
        Some(doc_id) => format!("_docID: {{ _eq: \"{}\" }}", escape_graphql_string(doc_id)),
        None => format!(
            "message_key: {{ _eq: \"{}\" }}",
            escape_graphql_string(message_key)
        ),
    };
    let mut scope_hasher = Sha256::new();
    for component in [&parent.agent_did, &parent.session_id, queue_key] {
        scope_hasher.update((component.len() as u64).to_be_bytes());
        scope_hasher.update(component.as_bytes());
    }
    let queue_scope = format!("{:x}", scope_hasher.finalize());
    let retry_key_prefix = format!("background-completion:{queue_scope}:");
    let escaped_retry_key_pattern = escape_graphql_string(&format!("{retry_key_prefix}%"));
    let response = txn
        .execute(&format!(
            r#"{{
                notification: AgentMessage(
                    filter: {{ {notification_filter} }},
                    limit: 2
                ) {{
                    {}
                }}
                pending: AgentRequest(
                    filter: {{
                        session_id: {{ _eq: "{escaped_session_id}" }},
                        agent_did: {{ _eq: "{escaped_agent_did}" }},
                        lifecycle_state: {{ _eq: "pending" }}
                    }},
                    order: [{{ created_at: ASC }}, {{ request_id: ASC }}]
                ) {{
                    _docID
                    request_id
                    session_id
                    input
                }}
                generations: AgentRequest(
                    filter: {{
                        session_id: {{ _eq: "{escaped_session_id}" }},
                        agent_did: {{ _eq: "{escaped_agent_did}" }},
                        retry_key: {{ _like: "{escaped_retry_key_pattern}" }}
                    }}
                ) {{
                    retry_key
                }}
            }}"#,
            crate::session::canonical_rows::AGENT_MESSAGE_FIELDS,
        ))
        .await?;
    let notifications = response["data"]["notification"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    anyhow::ensure!(
        notifications.len() <= 1,
        "background completion notification key resolved to multiple rows"
    );
    anyhow::ensure!(
        existing_notification_doc_id.is_none() || notifications.len() == 1,
        "observed background notification disappeared"
    );
    if let Some(raw) = notifications.first() {
        let row = crate::session::canonical_rows::decode_transcript_message_row(raw)?;
        let (_, reconstructed) = crate::session::load_canonical_message_in_txn(
            txn,
            &row.doc_id,
            &parent.agent_did,
            parent.requester_did.as_deref(),
        )
        .await?;
        anyhow::ensure!(
            row.message.message_key == message_key
                && row.message.session_id == parent.session_id
                && row.message.agent_did == parent.agent_did
                && row.message.requester_did == parent.requester_did
                && row.message.role == gents_protocol::output::MessageRole::User
                && reconstructed == gents_protocol::message::Message::user(content)
                && matches!(&row.message.publication,
                    gents_protocol::output::MessagePublication::ToolDelivery { tool_call_doc_id }
                    if tool_call_doc_id == &native.tool_call_doc_id),
            "canonical background notification replay conflicts with authority, scope, or content"
        );
        let doc_id = row
            .message
            .request_doc_id
            .as_deref()
            .context("canonical notification replay has no request binding")?;
        let binding = txn.execute(&format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID request_id session_id agent_did input }} }}"#,
            escape_graphql_string(doc_id))).await?;
        let rows: Vec<AgentRequestRow> =
            serde_json::from_value(binding["data"]["AgentRequest"].clone())?;
        anyhow::ensure!(
            rows.len() == 1
                && rows[0].agent_did.as_deref() == Some(parent.agent_did.as_str())
                && rows[0].session_id.as_deref() == Some(parent.session_id.as_str()),
            "canonical notification replay request binding is invalid"
        );
        let bound = &rows[0];
        let parent_bound = bound.doc_id.as_deref() == Some(parent.doc_id.as_str());
        let wake_bound = row_matches_coalesced_source_and_key(
            bound,
            QueueSource::BackgroundCompletion,
            queue_key,
        );
        anyhow::ensure!(
            (goal_owned && parent_bound) || (!goal_owned && wake_bound),
            "canonical notification replay uses the wrong Goal/wake binding"
        );
        let request = if goal_owned {
            None
        } else {
            Some(
                queue_row_to_enqueued_request(bound)
                    .context("canonical wake binding is incomplete")?,
            )
        };
        return Ok(EnqueuedBackgroundCompletionInput {
            request,
            message_sequence: row.message.sequence,
            created_request: false,
        });
    }

    if goal_owned {
        // GoalSource owns automatic continuation for this session, including when
        // its Goal is terminal or paused. The durable input belongs to the
        // request whose background work actually produced it.
        anyhow::ensure!(
            !parent.doc_id.trim().is_empty() && !parent.request_id.trim().is_empty(),
            "Goal-owned background notification requires a parent request binding"
        );
        let message_sequence =
            next_append_sequence_in_transaction(txn, &parent.agent_did, &parent.session_id).await?;
        publish_native_tool_notification(
            txn,
            parent,
            &parent.doc_id,
            message_sequence,
            message_key,
            content,
            native,
        )
        .await?;
        return Ok(EnqueuedBackgroundCompletionInput {
            request: None,
            message_sequence,
            created_request: false,
        });
    }

    let pending_rows: Vec<AgentRequestRow> =
        serde_json::from_value(response["data"]["pending"].clone())
            .context("decode pending AgentRequest rows")?;
    let pending = pending_rows
        .into_iter()
        .find(|row| {
            row_matches_coalesced_source_and_key(row, QueueSource::BackgroundCompletion, queue_key)
        })
        .and_then(|row| queue_row_to_enqueued_request(&row));
    let message_sequence =
        next_append_sequence_in_transaction(txn, &parent.agent_did, &parent.session_id).await?;
    let mut max_generation = None::<u64>;
    for row in response["data"]["generations"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let retry_key = row["retry_key"]
            .as_str()
            .context("background completion generation row has no retry key")?;
        let generation = retry_key
            .strip_prefix(&retry_key_prefix)
            .context("background completion generation row has the wrong scope")?
            .parse::<u64>()
            .context("background completion generation row is malformed")?;
        max_generation = Some(max_generation.map_or(generation, |current| current.max(generation)));
    }
    let next_generation = match max_generation {
        Some(generation) => generation
            .checked_add(1)
            .context("background completion queue generation overflow")?,
        None => 0,
    };

    let (request, created_request) = match pending {
        Some(request) => (request, false),
        None => {
            let request_id = format!("background-completion-{queue_scope}-{next_generation:020}");
            let retry_key = format!("{retry_key_prefix}{next_generation:020}");
            let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            // The durable wake format marker is stamped by this owner on
            // creation, never taken from caller-supplied input.
            let wake_input = RequestInput {
                queue: Some(background_wake_queue(
                    queue,
                    queue.queued_after_request_id.clone(),
                )),
                ..Default::default()
            };
            let request_mutation = session_request_create_mutation(
                parent,
                behavior_id,
                wake_content,
                ExecutionOrigin::Scheduled,
                wake_input,
                &request_id,
                &now,
                Some(&retry_key),
            )
            .await?;
            let response = txn.execute(&request_mutation).await?;
            let doc_id = transaction_created_doc_id(&response, "AgentRequest")?;
            (
                EnqueuedAgentRequest {
                    doc_id,
                    request_id,
                    session_id: parent.session_id.clone(),
                },
                true,
            )
        }
    };
    publish_native_tool_notification(
        txn,
        parent,
        &request.doc_id,
        message_sequence,
        message_key,
        content,
        native,
    )
    .await?;

    Ok(EnqueuedBackgroundCompletionInput {
        request: Some(request),
        message_sequence,
        created_request,
    })
}

async fn publish_native_tool_notification(
    txn: &ConfigApplyTxn<'_>,
    parent: &AgentRequest,
    binding_request_doc_id: &str,
    sequence: u32,
    message_key: &str,
    expected_content: &str,
    native: &ToolNotificationPublication,
) -> Result<()> {
    use crate::session::canonical_rows::{
        decode_output_segment_row, transcript_message_create_variables,
        AGENT_OUTPUT_SEGMENT_FIELDS, CREATE_AGENT_MESSAGE_MUTATION,
    };
    use gents_protocol::output::{
        MessageBlock, MessagePublication, MessageRole, OutputSource, OutputWriter,
        PayloadPresentation, PayloadRef, PresentedPayload, SourceClose, TranscriptMessage,
    };

    let tool = escape_graphql_string(&native.tool_call_doc_id);
    let request = escape_graphql_string(&parent.doc_id);
    let scope = crate::session::session_scope_filter(
        &parent.agent_did,
        &parent.session_id,
        parent.requester_did.as_deref(),
    );
    let facts = txn.execute(&format!(r#"{{
        AgentToolCall(filter: {{ _docID: {{ _eq: "{tool}" }}, request_doc_id: {{ _eq: "{request}" }}, await_mode: {{ _eq: "background" }} }}, limit: 2) {{ _docID lifecycle_state }}
        AgentOutputSegment(filter: {{ {scope}, request_doc_id: {{ _eq: "{request}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }}
    }}"#)).await?;
    let tools = facts["data"]["AgentToolCall"]
        .as_array()
        .context("tool authority query omitted rows")?;
    anyhow::ensure!(
        tools.len() == 1
            && tools[0]["lifecycle_state"]
                .as_str()
                .is_some_and(|state| matches!(
                    state,
                    "completed" | "failed" | "timedOut" | "cancelled"
                )),
        "background notification requires the exact terminal background tool"
    );
    let source = OutputSource::ToolCall {
        tool_call_doc_id: native.tool_call_doc_id.clone(),
    };
    let writer = OutputWriter::ToolExecution {
        tool_call_doc_id: native.tool_call_doc_id.clone(),
    };
    let source_rows = facts["data"]["AgentOutputSegment"]
        .as_array()
        .context("tool output query omitted rows")?
        .iter()
        .map(decode_output_segment_row)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|row| row.segment.source == source && row.segment.writer == writer)
        .collect::<Vec<_>>();
    let closes = source_rows
        .iter()
        .filter(|row| row.segment.close.is_some())
        .collect::<Vec<_>>();
    anyhow::ensure!(
        closes.len() == 1,
        "background notification requires one exact tool closure"
    );
    let close = closes[0];
    let outcome = match close
        .segment
        .close
        .as_ref()
        .context("tool closure missing outcome")?
    {
        SourceClose::Closed { outcome, .. } => *outcome,
        SourceClose::Retracted => anyhow::bail!("retracted tool output cannot be notified"),
    };
    let observed = source_rows
        .iter()
        .map(
            |row| gents_protocol::output::reconstruction::ObservedSegment {
                doc_id: &row.doc_id,
                segment: &row.segment,
            },
        )
        .collect::<Vec<_>>();
    let payload = PayloadRef {
        close_doc_id: close.doc_id.clone(),
        stream: 0,
    };
    let stream =
        gents_protocol::output::reconstruction::reconstruct_stream(&observed, &[], &[], &payload)?;
    let rendered = crate::tool_call_lifecycle::delivery::render_presentation(
        &stream.text,
        &PayloadPresentation::Composed {
            parts: native.presentation.clone(),
        },
    )?;
    anyhow::ensure!(
        rendered == expected_content,
        "canonical tool notification presentation does not reproduce rendered content"
    );
    let message = TranscriptMessage {
        message_key: message_key.to_owned(),
        session_id: parent.session_id.clone(),
        agent_did: parent.agent_did.clone(),
        requester_did: parent.requester_did.clone(),
        request_doc_id: Some(binding_request_doc_id.to_owned()),
        publication: MessagePublication::ToolDelivery {
            tool_call_doc_id: native.tool_call_doc_id.clone(),
        },
        outcome,
        sequence,
        role: MessageRole::User,
        native_id: None,
        blocks: vec![MessageBlock::Text {
            text: PresentedPayload {
                output: payload,
                presentation: PayloadPresentation::Composed {
                    parts: native.presentation.clone(),
                },
            },
        }],
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
    };
    txn.execute_with_variables(
        CREATE_AGENT_MESSAGE_MUTATION,
        &transcript_message_create_variables(&message)?,
    )
    .await?;
    Ok(())
}

pub(super) async fn steering_transaction_attempt(
    txn: &ConfigApplyTxn<'_>,
    parent: &AgentRequest,
    request_id: &str,
    request_mutation: &str,
) -> Result<EnqueuedAgentRequest> {
    let request_response = txn.execute(request_mutation).await?;
    let request_doc_id = transaction_created_doc_id(&request_response, "AgentRequest")?;

    Ok(EnqueuedAgentRequest {
        doc_id: request_doc_id,
        request_id: request_id.to_string(),
        session_id: parent.session_id.clone(),
    })
}

pub(crate) async fn next_append_sequence_in_transaction(
    txn: &ConfigApplyTxn<'_>,
    agent_did: &str,
    session_id: &str,
) -> Result<u32> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_session_id = escape_graphql_string(session_id);
    let response = txn
        .execute(&format!(
            r#"{{
                AgentMessage(
                    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }}, agent_did: {{ _eq: "{escaped_agent_did}" }} }},
                    order: {{ sequence: DESC }},
                    limit: 1
                ) {{ sequence }}
                AgentToolCall(
                    filter: {{
                        session_id: {{ _eq: "{escaped_session_id}" }},
                        agent_did: {{ _eq: "{escaped_agent_did}" }},
                        await_mode: {{ _eq: "background" }}
                    }}
                ) {{ message_sequence }}
            }}"#
        ))
        .await?;
    let message_max = response["data"]["AgentMessage"]
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| row["sequence"].as_u64())
        .unwrap_or(0) as u32;
    let mut reserved_counts = std::collections::BTreeMap::<u32, u32>::new();
    if let Some(rows) = response["data"]["AgentToolCall"].as_array() {
        for row in rows {
            if let Some(sequence) = row["message_sequence"]
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
            {
                *reserved_counts.entry(sequence).or_default() += 1;
            }
        }
    }
    let reserved_max = reserved_counts
        .into_iter()
        .map(|(sequence, count)| sequence + count)
        .max()
        .unwrap_or(0);
    Ok(message_max.max(reserved_max) + 1)
}

pub(super) fn transaction_created_doc_id(response: &Value, collection: &str) -> Result<String> {
    crate::graphql::created_doc_id(response, collection)
}
