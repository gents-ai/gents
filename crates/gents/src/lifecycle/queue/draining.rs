use super::*;

pub(crate) async fn drain_automated_wakeups_in_txn(
    txn: &ConfigApplyTxn<'_>,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    reason: &str,
) -> Result<Vec<String>> {
    drain_pending_session_requests_where_in_txn(
        txn,
        session_id,
        node_did,
        requester_did,
        reason,
        is_scheduled_automated_wakeup,
    )
    .await
}

pub(crate) async fn drain_all_pending_session_requests_in_txn(
    txn: &ConfigApplyTxn<'_>,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    reason: &str,
) -> Result<Vec<String>> {
    drain_pending_session_requests_where_in_txn(
        txn,
        session_id,
        node_did,
        requester_did,
        reason,
        is_any_pending_request,
    )
    .await
}

fn is_any_pending_request(_: &AgentRequestRow) -> bool {
    true
}

fn is_scheduled_automated_wakeup(row: &AgentRequestRow) -> bool {
    row.execution_origin.as_deref() == Some("scheduled") && row_is_automated_wakeup(row)
}

// SAFETY (#664): `node_did` scopes both the pending-row scan and mutation.
async fn drain_pending_session_requests_where_in_txn(
    txn: &ConfigApplyTxn<'_>,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    reason: &str,
    should_drain: fn(&AgentRequestRow) -> bool,
) -> Result<Vec<String>> {
    let scope = crate::session::session_scope_filter(node_did, session_id, requester_did);
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
    if !pending.is_array() {
        return Err(QueueError::PendingQueryOmittedRows);
    }
    let rows: Vec<AgentRequestRow> = serde_json::from_value(pending.clone())?;

    let escaped_reason = escape_graphql_string(reason);
    let mut drained = Vec::new();
    for row in rows.into_iter().filter(should_drain) {
        let terminalized_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let doc_id = row
            .doc_id
            .as_deref()
            .ok_or(QueueError::PendingRequestMissingDocId)?;
        let escaped_doc_id = escape_graphql_string(doc_id);
        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    docID: "{escaped_doc_id}", filter: {{
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
            .ok_or(QueueError::DrainMutationOmittedRows)?;
        let affected = match updated {
            Value::Null => None,
            Value::Array(rows) if rows.is_empty() => None,
            Value::Array(rows) if rows.len() == 1 => Some(
                rows[0]["_docID"]
                    .as_str()
                    .ok_or(QueueError::DrainReceiptMissingDocId)?,
            ),
            Value::Object(_) => Some(
                updated["_docID"]
                    .as_str()
                    .ok_or(QueueError::DrainReceiptMissingDocId)?,
            ),
            _ => return Err(QueueError::DrainMutationUnexpectedRows),
        };
        if let Some(affected) = affected {
            if affected != doc_id {
                return Err(QueueError::DrainMutationUpdatedAnother);
            }
            crate::trigger_engine::durable::publish_request_outcome(
                txn,
                node_did,
                &row.request_id,
                "interrupted",
                reason,
                &terminalized_at,
            )
            .await?;
            drained.push(row.request_id);
        }
    }

    Ok(drained)
}
