use super::*;

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};

use tokio::sync::Mutex;

type BackgroundCompletionGate = Mutex<()>;

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
/// The single transaction owner for fresh input and repair of a legacy receipt.
/// An observed receipt ID is reloaded and validated inside this transaction.
pub(crate) async fn persist_background_completion_with_message(
    node: &EmbeddedNode,
    parent: &AgentRequest,
    notification_content: &str,
    message_key: &str,
    wake_content: &str,
    queue_hints: QueueHints,
    existing_notification_doc_id: Option<&str>,
) -> Result<EnqueuedBackgroundCompletionInput> {
    anyhow::ensure!(
        queue_hints.source == QueueSource::BackgroundCompletion
            && queue_hints.policy == QueuePolicy::Coalesce,
        "atomic background completion enqueue requires coalescing background metadata"
    );
    let queue_key = queue_hints
        .key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .context("atomic background completion enqueue requires a queue key")?
        .to_string();

    let gate = background_completion_gate(node, &parent.session_id, &parent.agent_did, &queue_key);
    let _guard = gate.lock().await;

    let behavior_id = parent_behavior_id(node, parent).await?;
    let metadata = queue_metadata_json(&queue_hints);

    let mut retry_index = 0;
    let mut enqueued = loop {
        let txn = ConfigApplyTxn::begin_local(node, None).await?;
        let attempt = background_completion_transaction_attempt(
            &txn,
            parent,
            notification_content,
            message_key,
            &queue_key,
            &behavior_id,
            wake_content,
            &metadata,
            existing_notification_doc_id,
        )
        .await;
        let result = match attempt {
            Ok(enqueued) => txn.commit().await.map(|()| enqueued),
            Err(error) => {
                if let Err(discard_error) = txn.discard().await {
                    tracing::warn!(
                        error = %discard_error,
                        "discarding failed background-completion transaction also failed"
                    );
                }
                Err(error)
            }
        };
        match result {
            Ok(enqueued) => break enqueued,
            Err(error)
                if retry_index < DEFRA_DB_CONFLICT_MAX_RETRIES
                    && steering_transaction_error_is_retryable(&error) =>
            {
                let backoff = defradb_conflict_retry_backoff(retry_index);
                retry_index += 1;
                tracing::warn!(
                    queue_key,
                    attempt = retry_index,
                    backoff_ms = backoff.as_millis() as u64,
                    error = %error,
                    "retrying atomic background-completion persistence"
                );
                tokio::time::sleep(backoff).await;
            }
            Err(error) => return Err(error),
        }
    };

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

async fn background_completion_transaction_attempt(
    txn: &ConfigApplyTxn<'_>,
    parent: &AgentRequest,
    content: &str,
    message_key: &str,
    queue_key: &str,
    behavior_id: &str,
    wake_content: &str,
    metadata: &str,
    existing_notification_doc_id: Option<&str>,
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
                    _docID
                    message_key
                    timestamp
                    session_id
                    agent_did
                    request_id
                    request_doc_id
                    sequence
                    role
                    content
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
                    metadata
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
            }}"#
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
    let mut existing_sequence = None;
    if let Some(notification) = notifications.first() {
        let sequence = notification["sequence"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .context("background notification sequence is invalid")?;
        let canonical = notification["message_key"].as_str() == Some(message_key);
        anyhow::ensure!(
            notification["session_id"].as_str() == Some(parent.session_id.as_str())
                && notification["agent_did"].as_str() == Some(parent.agent_did.as_str())
                && notification["role"].as_str() == Some("user")
                && (!canonical || notification["content"].as_str() == Some(content)),
            "background notification conflicts with its persisted scope or content"
        );
        let request_id = notification["request_id"].as_str().unwrap_or_default();
        let doc_id = notification["request_doc_id"].as_str().unwrap_or_default();
        let parent_bound = request_id == parent.request_id && doc_id == parent.doc_id;
        let binding = txn
            .execute(&format!(
                r#"{{ AgentRequest(
            filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2
        ) {{ _docID request_id agent_did session_id metadata }} }}"#,
                escape_graphql_string(doc_id)
            ))
            .await?;
        let bindings = binding["data"]["AgentRequest"]
            .as_array()
            .context("request binding rows missing")?;
        let scoped_binding = bindings.len() == 1
            && bindings[0]["request_id"].as_str() == Some(request_id)
            && bindings[0]["agent_did"].as_str() == Some(parent.agent_did.as_str())
            && bindings[0]["session_id"].as_str() == Some(parent.session_id.as_str());
        let wake_bound = scoped_binding
            && queue_source_and_key_match(
                bindings[0]["metadata"].as_str(),
                QueueSource::BackgroundCompletion,
                queue_key,
            );
        anyhow::ensure!(
            !canonical || (scoped_binding && (parent_bound || wake_bound)),
            "canonical background notification references an invalid request binding"
        );
        // Canonical parent-bound receipts record input-only delivery permanently.
        // Legacy rows may lack a binding; preserve them, never rewrite them.
        if goal_owned || (canonical && parent_bound) || wake_bound {
            return Ok(EnqueuedBackgroundCompletionInput {
                request: (!goal_owned && !(canonical && parent_bound) && wake_bound).then(|| {
                    EnqueuedAgentRequest {
                        doc_id: doc_id.to_owned(),
                        request_id: request_id.to_owned(),
                        session_id: parent.session_id.clone(),
                    }
                }),
                message_sequence: sequence,
                created_request: false,
            });
        }
        let timestamp = chrono::DateTime::parse_from_rfc3339(
            notification["timestamp"]
                .as_str()
                .context("legacy notification timestamp missing")?,
        )
        .context("legacy notification timestamp is invalid")?;
        // Preserve the old acknowledgement rule, but observe it with Goal
        // presence and publication inside the same transaction.
        let wakes = txn.execute(&format!(r#"{{ AgentRequest(filter: {{
            session_id: {{ _eq: "{escaped_session_id}" }},
            agent_did: {{ _eq: "{escaped_agent_did}" }},
            execution_origin: {{ _eq: "scheduled" }}
        }}, order: {{ created_at: ASC }}) {{ _docID request_id session_id metadata created_at }} }}"#)).await?;
        let rows: Vec<AgentRequestRow> =
            serde_json::from_value(wakes["data"]["AgentRequest"].clone())?;
        for row in rows {
            if !queue_source_and_key_match(
                row.metadata.as_deref(),
                QueueSource::BackgroundCompletion,
                queue_key,
            ) {
                continue;
            }
            let created_at = chrono::DateTime::parse_from_rfc3339(
                row.created_at
                    .as_deref()
                    .context("background wake created_at missing")?,
            )?;
            if created_at >= timestamp {
                return Ok(EnqueuedBackgroundCompletionInput {
                    request: Some(
                        queue_row_to_enqueued_request(&row)
                            .context("background wake binding missing")?,
                    ),
                    message_sequence: sequence,
                    created_request: false,
                });
            }
        }
        existing_sequence = Some(sequence);
    }

    if goal_owned {
        // GoalSource owns automatic continuation for this session, including when
        // its Goal is terminal or paused. The durable input belongs to the
        // request whose background work actually produced it.
        anyhow::ensure!(
            !parent.doc_id.trim().is_empty() && !parent.request_id.trim().is_empty(),
            "Goal-owned background notification requires a parent request binding"
        );
        let message_sequence = next_append_sequence_in_transaction(txn, &parent.session_id).await?;
        let message_mutation = session::create_message_mutation(
            &parent.session_id,
            &parent.agent_did,
            parent.requester_did.as_deref(),
            message_sequence,
            "user",
            content,
            None,
            Some(&parent.request_id),
            Some(&parent.doc_id),
            Some(message_key),
        );
        txn.execute(&message_mutation).await?;
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
            queue_source_and_key_match(
                row.metadata.as_deref(),
                QueueSource::BackgroundCompletion,
                queue_key,
            )
        })
        .and_then(|row| queue_row_to_enqueued_request(&row));
    let message_sequence = match existing_sequence {
        Some(sequence) => sequence,
        None => next_append_sequence_in_transaction(txn, &parent.session_id).await?,
    };
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
            let request_mutation = session_request_create_mutation(
                parent,
                behavior_id,
                wake_content,
                ExecutionOrigin::Scheduled,
                metadata,
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
    if existing_sequence.is_none() {
        let message_mutation = session::create_message_mutation(
            &parent.session_id,
            &parent.agent_did,
            parent.requester_did.as_deref(),
            message_sequence,
            "user",
            content,
            None,
            Some(&request.request_id),
            Some(&request.doc_id),
            Some(message_key),
        );
        txn.execute(&message_mutation).await?;
    }

    Ok(EnqueuedBackgroundCompletionInput {
        request: Some(request),
        message_sequence,
        created_request,
    })
}

pub(super) async fn steering_transaction_attempt(
    txn: &ConfigApplyTxn<'_>,
    parent: &AgentRequest,
    content: &str,
    request_id: &str,
    request_mutation: &str,
) -> Result<EnqueuedAgentRequest> {
    let request_response = txn.execute(request_mutation).await?;
    let request_doc_id = transaction_created_doc_id(&request_response, "AgentRequest")?;
    let sequence = next_append_sequence_in_transaction(txn, &parent.session_id).await?;
    let message_key = steering_input_message_key(request_id);
    let message_mutation = session::create_message_mutation(
        &parent.session_id,
        &parent.agent_did,
        parent.requester_did.as_deref(),
        sequence,
        "user",
        content,
        None,
        Some(request_id),
        Some(&request_doc_id),
        Some(&message_key),
    );
    txn.execute(&message_mutation).await?;

    Ok(EnqueuedAgentRequest {
        doc_id: request_doc_id,
        request_id: request_id.to_string(),
        session_id: parent.session_id.clone(),
    })
}

async fn next_append_sequence_in_transaction(
    txn: &ConfigApplyTxn<'_>,
    session_id: &str,
) -> Result<u32> {
    let escaped_session_id = escape_graphql_string(session_id);
    let response = txn
        .execute(&format!(
            r#"{{
                AgentMessage(
                    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                    order: {{ sequence: DESC }},
                    limit: 1
                ) {{ sequence }}
                AgentToolCall(
                    filter: {{
                        session_id: {{ _eq: "{escaped_session_id}" }},
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
    let create_field = format!("create_{collection}");
    let add_field = format!("add_{collection}");
    let value = response
        .get("data")
        .and_then(|data| data.get(&create_field).or_else(|| data.get(&add_field)))
        .with_context(|| {
            format!("transaction create returned neither {create_field} nor {add_field}")
        })?;
    value
        .get("_docID")
        .or_else(|| {
            value
                .as_array()
                .and_then(|rows| rows.first())?
                .get("_docID")
        })
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .with_context(|| format!("transaction create {collection} returned no _docID"))
}

pub(super) fn steering_transaction_error_is_retryable(error: &anyhow::Error) -> bool {
    let text = error.to_string();
    let lower = text.to_ascii_lowercase();
    is_defradb_transaction_conflict_text(&text)
        || lower.contains("unique")
        || lower.contains("duplicate")
}
