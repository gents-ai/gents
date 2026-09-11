use super::*;

pub(super) fn queue_source_and_key_match(
    input: Option<&RequestInput>,
    source: QueueSource,
    key: &str,
) -> bool {
    input
        .and_then(|input| input.queue.as_ref())
        .is_some_and(|queue| {
            queue.source == source
                && queue.policy == QueuePolicy::Coalesce
                && queue
                    .key
                    .as_deref()
                    .is_some_and(|candidate| candidate.trim() == key)
        })
}

pub(super) fn row_matches_coalesced_source_and_key(
    row: &AgentRequestRow,
    source: QueueSource,
    key: &str,
) -> bool {
    queue_source_and_key_match(row.input.as_ref(), source, key)
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
        let duplicate_doc_id = escape_graphql_string(
            duplicate
                .doc_id
                .as_deref()
                .context("pending AgentRequest row is missing _docID")?,
        );
        let survivor_request_id = escape_graphql_string(&survivor.request_id);
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{duplicate_doc_id}" }},
                        agent_did: {{ _eq: "{escaped_agent_did}" }},
                        lifecycle_state: {{ _eq: "pending" }}
                    }},
                    input: {{
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
        crate::config_client::ConfigAccess::write_local_idempotent_update_response(
            node,
            "reconcile_coalesced_pending_request",
            &mutation,
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
) -> Result<Vec<AgentRequestRow>> {
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentRequest(
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
        }}"#
    );

    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query pending queue entries for session {session_id} failed: {:?}",
            response.errors
        );
    }

    let rows: Vec<AgentRequestRow> = crate::graphql::rows(&response, "AgentRequest")?;

    Ok(rows
        .into_iter()
        .filter(|row| row_matches_coalesced_source_and_key(row, source, key))
        .collect())
}

pub(super) fn queue_row_to_enqueued_request(row: &AgentRequestRow) -> Option<EnqueuedAgentRequest> {
    Some(EnqueuedAgentRequest {
        doc_id: row.doc_id.clone()?,
        request_id: row.request_id.clone(),
        session_id: row.session_id.clone()?,
    })
}

pub(super) fn parent_behavior_id(parent: &AgentRequest) -> Result<String> {
    anyhow::ensure!(
        !parent.behavior_id.trim().is_empty(),
        "cannot enqueue same-session request: parent {} has no behavior_id",
        parent.request_id
    );
    Ok(parent.behavior_id.clone())
}
