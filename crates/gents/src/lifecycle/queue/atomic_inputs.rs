use super::*;

/// Atomically bind a background-completion notification to the pending wake
/// request that will consume it. The transaction either reuses the coalesced
/// pending wake it read or creates a new one; a concurrent claim conflicts and
/// retries, so no visible wake can race ahead of its input message.
pub(crate) async fn enqueue_background_completion_with_message(
    node: &EmbeddedNode,
    parent: &AgentRequest,
    notification_content: &str,
    message_key: &str,
    wake_content: &str,
    queue_hints: QueueHints,
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
    let parent = normalize_request_only_control_parent(node, parent).await?;
    let behavior_id = parent_behavior_id(node, &parent).await?;
    let request_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let metadata = queue_metadata_json(&queue_hints);
    let request_mutation = session_request_create_mutation(
        &parent,
        &behavior_id,
        wake_content,
        ExecutionOrigin::Scheduled,
        &metadata,
        &request_id,
        &now,
        true,
    )?;

    let mut retry_index = 0;
    let mut enqueued = loop {
        let txn = ConfigApplyTxn::begin_local(node, None).await?;
        let attempt = background_completion_transaction_attempt(
            &txn,
            &parent,
            notification_content,
            message_key,
            &queue_key,
            &request_id,
            &request_mutation,
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
                    request_id,
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

    if enqueued.created_request {
        let created_request_doc_id = enqueued.request.doc_id.clone();
        let active_request = reconcile_coalesced_pending_request(
            node,
            &parent.session_id,
            &parent.agent_did,
            QueueSource::BackgroundCompletion,
            &queue_key,
        )
        .await?
        .unwrap_or_else(|| enqueued.request.clone());
        enqueued.created_request = active_request.doc_id == created_request_doc_id;
        enqueued.request = active_request;
    }

    Ok(enqueued)
}

async fn background_completion_transaction_attempt(
    txn: &ConfigApplyTxn<'_>,
    parent: &AgentRequest,
    content: &str,
    message_key: &str,
    queue_key: &str,
    request_id: &str,
    request_mutation: &str,
) -> Result<EnqueuedBackgroundCompletionInput> {
    let escaped_session_id = escape_graphql_string(&parent.session_id);
    let escaped_agent_did = escape_graphql_string(&parent.agent_did);
    let response = txn
        .execute(&format!(
            r#"{{
                pending_requests: AgentRequest(
                    filter: {{
                        session_id: {{ _eq: "{escaped_session_id}" }},
                        agent_did: {{ _eq: "{escaped_agent_did}" }},
                        status: {{ _eq: "pending" }},
                        lifecycle_state: {{ _eq: "pending" }}
                    }},
                    order: [{{ created_at: ASC }}, {{ request_id: ASC }}]
                ) {{
                    _docID
                    request_id
                    session_id
                    metadata
                }}
                AgentMessage(
                    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                    order: {{ sequence: ASC }}
                ) {{
                    _docID sequence request_id request_doc_id message_key content timestamp
                }}
                background_wakes: AgentRequest(
                    filter: {{
                        session_id: {{ _eq: "{escaped_session_id}" }},
                        agent_did: {{ _eq: "{escaped_agent_did}" }}
                    }},
                    order: [{{ created_at: ASC }}, {{ request_id: ASC }}]
                ) {{
                    _docID request_id session_id metadata status lifecycle_state created_at
                }}
            }}"#
        ))
        .await?;
    let message_rows = response["data"]["AgentMessage"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let existing_message = select_existing_background_input(message_rows, message_key)?;
    let wake_rows = response["data"]["background_wakes"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    if let Some(existing) = existing_message.as_ref() {
        if let Some(request) =
            bound_background_request_from_rows(wake_rows, existing, &parent.session_id, queue_key)?
        {
            let message_sequence = existing["sequence"]
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .context("bound background completion input has invalid sequence")?;
            return Ok(EnqueuedBackgroundCompletionInput {
                request,
                message_sequence,
                created_request: false,
                created_message: false,
            });
        }
        if let Some(request) =
            background_wakeup_after_message(wake_rows, existing, &parent.session_id, queue_key)?
        {
            if existing["request_doc_id"]
                .as_str()
                .is_none_or(|value| value.trim().is_empty())
            {
                let message_doc_id = existing["_docID"]
                    .as_str()
                    .context("existing background completion input has no _docID")?;
                txn.execute(&bind_legacy_background_input_mutation(
                    message_doc_id,
                    &parent.session_id,
                    &request,
                )?)
                .await?;
            }
            let message_sequence = existing["sequence"]
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .context("existing background completion input has invalid sequence")?;
            return Ok(EnqueuedBackgroundCompletionInput {
                request,
                message_sequence,
                created_request: false,
                created_message: false,
            });
        }
    }
    let pending = response["data"]["pending_requests"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| serde_json::from_value::<PendingQueueRow>(row).ok())
        .find(|row| {
            queue_source_and_key_match(
                row.metadata.as_deref(),
                QueueSource::BackgroundCompletion,
                queue_key,
            )
        })
        .and_then(|row| queue_row_to_enqueued_request(&row));

    let (request, created_request) = match pending {
        Some(request) => (request, false),
        None => {
            let response = txn.execute(request_mutation).await?;
            let doc_id = transaction_created_doc_id(&response, "AgentRequest")?;
            (
                EnqueuedAgentRequest {
                    doc_id,
                    request_id: request_id.to_string(),
                    session_id: parent.session_id.clone(),
                },
                true,
            )
        }
    };
    let (message_sequence, created_message, message_mutation) = if let Some(existing) =
        existing_message
    {
        let message_sequence = existing["sequence"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .context("existing background completion input has invalid sequence")?;
        let message_doc_id = existing["_docID"]
            .as_str()
            .context("existing background completion input has no _docID")?;
        // request_doc_id is immutable. Only logical-only legacy rows may be
        // associated with a request; a physically bound row is never rebound.
        let mutation = existing["request_doc_id"]
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .map(|_| String::new())
            .map_or_else(
                || {
                    bind_legacy_background_input_mutation(
                        message_doc_id,
                        &parent.session_id,
                        &request,
                    )
                },
                Ok,
            )?;
        (message_sequence, false, mutation)
    } else {
        let message_sequence = next_append_sequence_in_transaction(txn, &parent.session_id).await?;
        (
            message_sequence,
            true,
            session::create_message_mutation(
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
            ),
        )
    };
    if !message_mutation.is_empty() {
        txn.execute(&message_mutation).await?;
    }

    Ok(EnqueuedBackgroundCompletionInput {
        request,
        message_sequence,
        created_request,
        created_message,
    })
}

fn select_existing_background_input(rows: &[Value], message_key: &str) -> Result<Option<Value>> {
    let exact = rows
        .iter()
        .filter(|row| row["message_key"].as_str() == Some(message_key))
        .collect::<Vec<_>>();
    if exact.len() > 1 {
        anyhow::bail!(
            "background completion input key {message_key} resolved to multiple AgentMessage rows"
        );
    }
    if let Some(row) = exact.first() {
        return Ok(Some((*row).clone()));
    }

    let Some((stable_id, kind)) =
        crate::background_completion::background_completion_notification_identity(message_key)
    else {
        return Ok(None);
    };
    Ok(rows
        .iter()
        .find(|row| {
            crate::background_completion::is_legacy_background_completion_notification(
                row["message_key"].as_str().unwrap_or_default(),
                row["content"].as_str().unwrap_or_default(),
                stable_id,
                kind,
            )
        })
        .cloned())
}

fn background_wakeup_after_message(
    rows: &[Value],
    message: &Value,
    session_id: &str,
    queue_key: &str,
) -> Result<Option<EnqueuedAgentRequest>> {
    let timestamp = message["timestamp"]
        .as_str()
        .context("existing background completion input has no timestamp")?;
    let timestamp = chrono::DateTime::parse_from_rfc3339(timestamp)
        .context("existing background completion input timestamp is not RFC3339")?
        .with_timezone(&chrono::Utc);
    for row in rows {
        if row["session_id"].as_str() != Some(session_id)
            || !queue_source_and_key_match(
                row["metadata"].as_str(),
                QueueSource::BackgroundCompletion,
                queue_key,
            )
        {
            continue;
        }
        let Some(created_at) = row["created_at"].as_str() else {
            continue;
        };
        let created_at = chrono::DateTime::parse_from_rfc3339(created_at)
            .context("background completion wake created_at is not RFC3339")?
            .with_timezone(&chrono::Utc);
        if created_at < timestamp {
            continue;
        }
        let (Some(doc_id), Some(request_id)) = (row["_docID"].as_str(), row["request_id"].as_str())
        else {
            continue;
        };
        return Ok(Some(EnqueuedAgentRequest {
            doc_id: doc_id.to_string(),
            request_id: request_id.to_string(),
            session_id: session_id.to_string(),
        }));
    }
    Ok(None)
}

fn bound_background_request_from_rows(
    rows: &[Value],
    message: &Value,
    session_id: &str,
    queue_key: &str,
) -> Result<Option<EnqueuedAgentRequest>> {
    let Some(request_id) = message["request_id"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let request_doc_id = message["request_doc_id"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let row = if let Some(request_doc_id) = request_doc_id {
        rows.iter()
            .find(|row| row["_docID"].as_str() == Some(request_doc_id))
    } else {
        let matches = rows
            .iter()
            .filter(|row| {
                row["request_id"].as_str() == Some(request_id)
                    && row["session_id"].as_str() == Some(session_id)
            })
            .collect::<Vec<_>>();
        if matches.len() > 1 {
            anyhow::bail!(
                "background completion input request {request_id} resolved to multiple AgentRequest rows"
            );
        }
        matches.first().copied()
    };
    let Some(row) = row else {
        return Ok(None);
    };
    let Some(resolved_request_doc_id) = row["_docID"].as_str() else {
        return Ok(None);
    };
    let Some(resolved_request_id) = row["request_id"].as_str() else {
        return Ok(None);
    };
    let matches = row["session_id"].as_str() == Some(session_id)
        && queue_source_and_key_match(
            row["metadata"].as_str(),
            QueueSource::BackgroundCompletion,
            queue_key,
        );
    Ok(matches.then(|| EnqueuedAgentRequest {
        doc_id: resolved_request_doc_id.to_string(),
        request_id: resolved_request_id.to_string(),
        session_id: session_id.to_string(),
    }))
}

fn bind_legacy_background_input_mutation(
    message_doc_id: &str,
    session_id: &str,
    request: &EnqueuedAgentRequest,
) -> Result<String> {
    let message_doc_id = message_doc_id.trim();
    anyhow::ensure!(
        !message_doc_id.is_empty(),
        "cannot bind a legacy background completion input without an AgentMessage _docID"
    );
    Ok(format!(
        r#"mutation {{
            update_AgentMessage(
                filter: {{
                    _docID: {{ _eq: "{}" }},
                    session_id: {{ _eq: "{}" }}
                }},
                input: {{
                    request_id: "{}"
                }}
            ) {{ _docID }}
        }}"#,
        escape_graphql_string(message_doc_id),
        escape_graphql_string(session_id),
        escape_graphql_string(&request.request_id),
    ))
}

pub(super) async fn normalize_request_only_control_parent(
    node: &EmbeddedNode,
    parent: &AgentRequest,
) -> Result<AgentRequest> {
    let mut normalized = parent.clone();
    match (
        normalized.caused_by_parent_request_id.as_deref(),
        normalized.caused_by_parent_request_doc_id.as_deref(),
    ) {
        (Some(request_id), None) if !request_id.trim().is_empty() => {
            normalized.caused_by_parent_request_doc_id =
                Some(crate::request_binding::require_request_doc_id(node, request_id).await?);
            tracing::warn!(
                request_id = %normalized.request_id,
                caused_by_parent_request_id = %request_id,
                "recovered legacy logical-only control-continuation parent binding",
            );
        }
        (Some(request_id), Some(request_doc_id))
            if !request_id.trim().is_empty() && !request_doc_id.trim().is_empty() => {}
        (None, None) => {}
        _ => anyhow::bail!("cannot enqueue control continuation from incoherent parent linkage"),
    }
    normalized.caused_by_parent_tool_call_id = None;
    normalized.caused_by_parent_tool_call_doc_id = None;
    Ok(normalized)
}

pub(super) async fn steering_transaction_attempt(
    txn: &ConfigApplyTxn<'_>,
    parent: &AgentRequest,
    content: &str,
    request_id: &str,
    request_mutation: &str,
) -> Result<EnqueuedSteeringInput> {
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

    Ok(EnqueuedSteeringInput {
        request: EnqueuedAgentRequest {
            doc_id: request_doc_id,
            request_id: request_id.to_string(),
            session_id: parent.session_id.clone(),
        },
        message_sequence: sequence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_background_binding_requires_a_physical_message_id() {
        let request = EnqueuedAgentRequest {
            doc_id: "request-doc".to_string(),
            request_id: "request-id".to_string(),
            session_id: "session-id".to_string(),
        };
        let error = bind_legacy_background_input_mutation("  ", "session-id", &request)
            .expect_err("blank message IDs must not produce a session-wide mutation");
        assert!(error.to_string().contains("AgentMessage _docID"));

        let mutation =
            bind_legacy_background_input_mutation("message-doc", "session-id", &request).unwrap();
        assert!(mutation.contains(r#"_docID: { _eq: "message-doc" }"#));
    }
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
