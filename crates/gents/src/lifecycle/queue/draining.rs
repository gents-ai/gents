use super::*;

pub async fn drain_automated_wakeups(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    reason: &str,
) -> Result<usize> {
    drain_pending_session_requests_where(node, session_id, agent_did, reason, |row| {
        row.execution_origin.as_deref() == Some("scheduled")
            && is_automated_wakeup(row.metadata.as_deref())
    })
    .await
}

pub(crate) async fn drain_subagent_owned_queue(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    reason: &str,
) -> Result<usize> {
    drain_pending_session_requests_where(node, session_id, agent_did, reason, |row| {
        is_subagent_owned_queue(row.metadata.as_deref())
    })
    .await
}

// SAFETY (#664): `agent_did` scopes both the pending-row scan AND the interrupt
// mutation to the owning principal. A foreign-DID replica sharing this
// `session_id` (P2P replication) is neither surfaced as a drain candidate nor
// interrupted by this owner's drain. Defense in depth on the query and the write.
async fn drain_pending_session_requests_where(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    reason: &str,
    should_drain: impl Fn(&PendingQueueRow) -> bool,
) -> Result<usize> {
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
                }}
            ) {{
                _docID
                execution_origin
                metadata
            }}
        }}"#
    );

    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query pending automated wake-ups for session {session_id} failed: {:?}",
            response.errors
        );
    }

    let rows: Vec<PendingQueueRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();

    let escaped_reason = escape_graphql_string(reason);
    let mut drained = 0;
    for row in rows.into_iter().filter(should_drain) {
        let terminalized_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let escaped_doc_id = escape_graphql_string(&row.doc_id);
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        agent_did: {{ _eq: "{escaped_agent_did}" }},
                        status: {{ _eq: "pending" }},
                        lifecycle_state: {{ _eq: "pending" }}
                    }},
                    input: {{
                        status: "interrupted",
                        lifecycle_state: "interrupted",
                        failure_reason: "{escaped_reason}",
                        terminalized_at: "{terminalized_at}",
                        terminal_redrive_attempts: 0
                    }}
                ) {{ _docID }}
            }}"#
        );
        let response = crate::retry::execute_graphql_with_terminal_persistence_retry(
            node,
            &mutation,
            "drain_automated_wakeup",
        )
        .await?;
        if response
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentRequest"))
            .is_some_and(response_has_documents)
        {
            drained += 1;
        }
    }

    Ok(drained)
}
