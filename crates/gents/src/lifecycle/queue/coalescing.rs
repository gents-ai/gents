use super::*;

#[derive(Debug, Deserialize)]
pub(super) struct PendingQueueRow {
    #[serde(rename = "_docID")]
    pub(super) doc_id: String,
    pub(super) request_id: Option<String>,
    pub(super) session_id: Option<String>,
    pub(super) execution_origin: Option<String>,
    pub(super) metadata: Option<String>,
}

pub(super) fn queue_source_and_key_match(
    metadata: Option<&str>,
    source: QueueSource,
    key: &str,
) -> bool {
    parse_queue_hints(metadata).is_some_and(|hints| {
        hints.source == source
            && hints.policy == QueuePolicy::Coalesce
            && hints
                .key
                .as_deref()
                .is_some_and(|candidate| candidate.trim() == key)
    })
}

pub(super) fn coalesce_key(hints: &QueueHints) -> Option<&str> {
    if hints.policy != QueuePolicy::Coalesce {
        return None;
    }
    hints
        .key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
}

pub async fn reconcile_coalesced_pending_request(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    source: QueueSource,
    key: &str,
) -> Result<Option<EnqueuedAgentRequest>> {
    let matching =
        matching_coalesced_pending_requests(node, session_id, agent_did, source, key).await?;
    let Some(survivor) = matching.first().and_then(queue_row_to_enqueued_request) else {
        return Ok(None);
    };

    let escaped_agent_did = escape_graphql_string(agent_did);
    for duplicate in matching.iter().skip(1) {
        let terminalized_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let duplicate_doc_id = escape_graphql_string(&duplicate.doc_id);
        let survivor_request_id = escape_graphql_string(&survivor.request_id);
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{duplicate_doc_id}" }},
                        agent_did: {{ _eq: "{escaped_agent_did}" }},
                        status: {{ _eq: "pending" }},
                        lifecycle_state: {{ _eq: "pending" }}
                    }},
                    input: {{
                        status: "superseded",
                        lifecycle_state: "superseded",
                        superseded_by_request: "{survivor_request_id}",
                        superseded_by_request_doc_id: "{survivor_doc_id}",
                        failure_reason: "coalesced into earlier queued request",
                        terminalized_at: "{terminalized_at}",
                        terminal_redrive_attempts: 0
                    }}
                ) {{ _docID }}
            }}"#,
            survivor_doc_id = escape_graphql_string(&survivor.doc_id),
        );
        crate::retry::execute_graphql_with_terminal_persistence_retry(
            node,
            &mutation,
            "reconcile_coalesced_pending_request",
        )
        .await?;
    }

    Ok(Some(survivor))
}

async fn matching_coalesced_pending_requests(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    source: QueueSource,
    key: &str,
) -> Result<Vec<PendingQueueRow>> {
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentRequest(
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
        }}"#
    );

    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query pending queue entries for session {session_id} failed: {:?}",
            response.errors
        );
    }

    let rows: Vec<PendingQueueRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();

    Ok(rows
        .into_iter()
        .filter(|row| queue_source_and_key_match(row.metadata.as_deref(), source, key))
        .collect())
}

pub(super) fn queue_row_to_enqueued_request(row: &PendingQueueRow) -> Option<EnqueuedAgentRequest> {
    Some(EnqueuedAgentRequest {
        doc_id: row.doc_id.clone(),
        request_id: row.request_id.clone()?,
        session_id: row.session_id.clone()?,
    })
}

pub(super) async fn parent_behavior_id(
    node: &EmbeddedNode,
    parent: &AgentRequest,
) -> Result<String> {
    if let Some(behavior_id) = parent
        .behavior_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(behavior_id.to_string());
    }

    let escaped_session_id = escape_graphql_string(&parent.session_id);
    let escaped_agent_did = escape_graphql_string(&parent.agent_did);
    let query = format!(
        r#"{{
            AgentConversation(
                filter: {{
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    session_id: {{ _eq: "{escaped_session_id}" }}
                }},
                limit: 2
            ) {{
                behavior_id
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query parent conversation for queued request failed: {:?}",
            response.errors
        );
    }

    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentConversation"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    anyhow::ensure!(
        rows.len() <= 1,
        "parent conversation scope resolved to multiple rows"
    );
    rows.first()
        .and_then(|row| row.get("behavior_id"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "cannot enqueue same-session request: parent request {} has no behavior_id",
                parent.request_id
            )
        })
}

pub(super) fn parent_linkage_graphql_fields(parent: &AgentRequest) -> Result<String> {
    match (
        parent.caused_by_parent_request_id.as_deref(),
        parent.caused_by_parent_request_doc_id.as_deref(),
        parent.caused_by_parent_tool_call_id.as_deref(),
        parent.caused_by_parent_tool_call_doc_id.as_deref(),
    ) {
        (
            Some(parent_request_id),
            Some(parent_request_doc_id),
            Some(parent_tool_call_id),
            Some(parent_tool_call_doc_id),
        ) if !parent_request_id.trim().is_empty()
            && !parent_request_doc_id.trim().is_empty()
            && !parent_tool_call_id.trim().is_empty()
            && !parent_tool_call_doc_id.trim().is_empty() =>
        {
            Ok(format!(
                r#",
                caused_by_parent_request_id: "{}",
                caused_by_parent_request_doc_id: "{}",
                caused_by_parent_tool_call_id: "{}",
                caused_by_parent_tool_call_doc_id: "{}""#,
                escape_graphql_string(parent_request_id),
                escape_graphql_string(parent_request_doc_id),
                escape_graphql_string(parent_tool_call_id),
                escape_graphql_string(parent_tool_call_doc_id),
            ))
        }
        (Some(parent_request_id), Some(parent_request_doc_id), None, None)
            if !parent_request_id.trim().is_empty() && !parent_request_doc_id.trim().is_empty() =>
        {
            Ok(format!(
                r#",
                caused_by_parent_request_id: "{}",
                caused_by_parent_request_doc_id: "{}""#,
                escape_graphql_string(parent_request_id),
                escape_graphql_string(parent_request_doc_id),
            ))
        }
        (None, None, None, None) => Ok(String::new()),
        _ => anyhow::bail!("cannot enqueue request from incoherent parent linkage"),
    }
}

pub(super) fn request_only_parent_linkage_graphql_fields(parent: &AgentRequest) -> Result<String> {
    match (
        parent.caused_by_parent_request_id.as_deref(),
        parent.caused_by_parent_request_doc_id.as_deref(),
        parent.caused_by_parent_tool_call_id.as_deref(),
        parent.caused_by_parent_tool_call_doc_id.as_deref(),
    ) {
        (
            Some(parent_request_id),
            Some(parent_request_doc_id),
            Some(parent_tool_call_id),
            Some(parent_tool_call_doc_id),
        ) if !parent_request_id.trim().is_empty()
            && !parent_request_doc_id.trim().is_empty()
            && !parent_tool_call_id.trim().is_empty()
            && !parent_tool_call_doc_id.trim().is_empty() =>
        {
            Ok(format!(
                r#",
                caused_by_parent_request_id: "{}",
                caused_by_parent_request_doc_id: "{}""#,
                escape_graphql_string(parent_request_id),
                escape_graphql_string(parent_request_doc_id),
            ))
        }
        (Some(parent_request_id), Some(parent_request_doc_id), None, None)
            if !parent_request_id.trim().is_empty() && !parent_request_doc_id.trim().is_empty() =>
        {
            Ok(format!(
                r#",
                caused_by_parent_request_id: "{}",
                caused_by_parent_request_doc_id: "{}""#,
                escape_graphql_string(parent_request_id),
                escape_graphql_string(parent_request_doc_id),
            ))
        }
        (None, None, None, None)
            if !parent.request_id.trim().is_empty() && !parent.doc_id.trim().is_empty() =>
        {
            Ok(format!(
                r#",
                caused_by_parent_request_id: "{}",
                caused_by_parent_request_doc_id: "{}""#,
                escape_graphql_string(&parent.request_id),
                escape_graphql_string(&parent.doc_id),
            ))
        }
        (None, None, None, None) => {
            anyhow::bail!("cannot enqueue control continuation from an unbound parent request")
        }
        _ => anyhow::bail!("cannot enqueue control continuation from incoherent parent linkage"),
    }
}

pub(super) async fn lookup_request_doc_id(node: &EmbeddedNode, request_id: &str) -> Result<String> {
    crate::request_binding::require_request_doc_id(node, request_id).await
}

pub(super) async fn lookup_request_doc_id_optional(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<Option<String>> {
    crate::request_binding::resolve_request_doc_id(node, request_id).await
}
