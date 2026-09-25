use super::*;

/// Standalone queue control when there is no active request to latch. Active
/// interruption must drain inside the latch transaction so replay cannot widen
/// its cutoff to later completions.
pub(crate) async fn drain_automated_wakeups_returning_ids(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    reason: &str,
) -> Result<Vec<String>> {
    drain_pending_session_requests_where(
        node,
        session_id,
        agent_did,
        requester_did,
        reason,
        is_scheduled_automated_wakeup,
    )
    .await
}

pub(crate) async fn drain_automated_wakeups_in_txn(
    txn: &ConfigApplyTxn<'_>,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    reason: &str,
) -> Result<Vec<String>> {
    drain_pending_session_requests_where_in_txn(
        txn,
        session_id,
        agent_did,
        requester_did,
        reason,
        is_scheduled_automated_wakeup,
    )
    .await
}

fn is_scheduled_automated_wakeup(row: &AgentRequestRow) -> bool {
    row.execution_origin.as_deref() == Some("scheduled") && row_is_automated_wakeup(row)
}

pub(crate) async fn drain_subagent_owned_queue(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    reason: &str,
) -> Result<usize> {
    Ok(drain_pending_session_requests_where(
        node,
        session_id,
        agent_did,
        requester_did,
        reason,
        |row| row_is_subagent_owned_queue(row),
    )
    .await?
    .len())
}

async fn drain_pending_session_requests_where(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    reason: &str,
    should_drain: fn(&AgentRequestRow) -> bool,
) -> Result<Vec<String>> {
    crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "lifecycle.drain_pending_session_requests",
        |txn| {
            Box::pin(async move {
                drain_pending_session_requests_where_in_txn(
                    txn,
                    session_id,
                    agent_did,
                    requester_did,
                    reason,
                    should_drain,
                )
                .await
            })
        },
    )
    .await
}

// SAFETY (#664): `agent_did` scopes both the pending-row scan and mutation.
// A foreign-DID replica sharing `session_id` cannot be drained by this owner.
async fn drain_pending_session_requests_where_in_txn(
    txn: &ConfigApplyTxn<'_>,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    reason: &str,
    should_drain: fn(&AgentRequestRow) -> bool,
) -> Result<Vec<String>> {
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

    let response = txn.execute(&query).await?;
    let pending = &response["data"]["AgentRequest"];
    anyhow::ensure!(
        pending.is_array(),
        "pending AgentRequest query omitted rows"
    );
    let rows: Vec<AgentRequestRow> = serde_json::from_value(pending.clone())?;

    let escaped_reason = escape_graphql_string(reason);
    let mut drained = Vec::new();
    for row in rows.into_iter().filter(should_drain) {
        let terminalized_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let doc_id = row
            .doc_id
            .as_deref()
            .context("pending AgentRequest row is missing _docID")?;
        let escaped_doc_id = escape_graphql_string(doc_id);
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
        let response = txn.execute(&mutation).await?;
        let updated = response["data"]
            .get("update_AgentRequest")
            .context("pending AgentRequest drain mutation omitted affected rows")?;
        let affected = match updated {
            Value::Null => None,
            Value::Array(rows) if rows.is_empty() => None,
            Value::Array(rows) if rows.len() == 1 => Some(
                rows[0]["_docID"]
                    .as_str()
                    .context("pending AgentRequest drain receipt omitted _docID")?,
            ),
            Value::Object(_) => Some(
                updated["_docID"]
                    .as_str()
                    .context("pending AgentRequest drain receipt omitted _docID")?,
            ),
            _ => anyhow::bail!("pending AgentRequest drain mutation returned unexpected rows"),
        };
        if let Some(affected) = affected {
            anyhow::ensure!(
                affected == doc_id,
                "pending AgentRequest drain mutation updated another physical request"
            );
            drained.push(row.request_id);
        }
    }

    Ok(drained)
}
