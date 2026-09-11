use super::*;

pub async fn drain_automated_wakeups(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    reason: &str,
) -> Result<usize> {
    drain_pending_session_requests_where(
        node,
        session_id,
        agent_did,
        requester_did,
        reason,
        |row| row.execution_origin.as_deref() == Some("scheduled") && row_is_automated_wakeup(row),
    )
    .await
}

pub(crate) async fn drain_subagent_owned_queue(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    reason: &str,
) -> Result<usize> {
    drain_pending_session_requests_where(
        node,
        session_id,
        agent_did,
        requester_did,
        reason,
        |row| row_is_subagent_owned_queue(row),
    )
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
    requester_did: Option<&str>,
    reason: &str,
    should_drain: impl Fn(&AgentRequestRow) -> bool,
) -> Result<usize> {
    let scope = crate::session::session_scope_filter(agent_did, session_id, requester_did);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    {scope},
                    lifecycle_state: {{ _eq: "pending" }}
                }}
            ) {{
                _docID
                request_id
                execution_origin
                input
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

    let rows: Vec<AgentRequestRow> = crate::graphql::rows(&response, "AgentRequest")?;

    let escaped_reason = escape_graphql_string(reason);
    let mut drained = 0;
    for row in rows.into_iter().filter(should_drain) {
        let terminalized_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let escaped_doc_id = escape_graphql_string(
            row.doc_id
                .as_deref()
                .context("pending AgentRequest row is missing _docID")?,
        );
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        {scope},
                        lifecycle_state: {{ _eq: "pending" }}
                    }},
                    input: {{
                        lifecycle_state: "interrupted",
                        failure_reason: "{escaped_reason}",
                        terminalized_at: "{terminalized_at}",
                        terminal_redrive_attempts: 0
                    }}
                ) {{ _docID }}
            }}"#
        );
        let response = crate::config_client::ConfigAccess::write_local_idempotent_update_response(
            node,
            "drain_automated_wakeup",
            &mutation,
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
