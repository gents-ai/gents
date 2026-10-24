use super::query::{decode_session_row, session_scope_filter, validate_agent_session};
use super::*;

#[cfg(test)]
pub(crate) async fn create_session_with_id(
    node: &std::sync::Arc<EmbeddedNode>,
    session_id: &str,
    agent_id: &str,
    node_did: &str,
) -> Result<()> {
    create_session_with_agent_id(node, session_id, node_did, agent_id).await
}

#[cfg(test)]
pub(crate) async fn create_session_with_agent_id(
    node: &std::sync::Arc<EmbeddedNode>,
    session_id: &str,
    node_did: &str,
    agent_id: &str,
) -> Result<()> {
    create_session_with_agent_id_and_requester_did(node, session_id, node_did, agent_id, None).await
}

#[cfg(test)]
async fn create_session_with_agent_id_and_requester_did(
    node: &std::sync::Arc<EmbeddedNode>,
    session_id: &str,
    node_did: &str,
    agent_id: &str,
    requester_did: Option<&str>,
) -> Result<()> {
    crate::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "session.create",
        move |txn| {
            Box::pin(async move {
                ensure_session_in_txn(
                    txn,
                    session_id,
                    node_did,
                    agent_id,
                    requester_did,
                    None,
                    None,
                    &chrono::Utc::now().to_rfc3339(),
                )
                .await?;
                Ok::<bool, SessionError>(true)
            })
        },
    )
    .await
    .map(|_| ())
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn ensure_session_with_agent_id_and_requester_did(
    node: &std::sync::Arc<EmbeddedNode>,
    session_id: &str,
    node_did: &str,
    agent_id: &str,
    requester_did: Option<&str>,
) -> Result<()> {
    create_session_with_agent_id_and_requester_did(
        node,
        session_id,
        node_did,
        agent_id,
        requester_did,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn ensure_session_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    session_id: &str,
    node_did: &str,
    agent_id: &str,
    requester_did: Option<&str>,
    title: Option<gents_protocol::session::SessionTitle>,
    provenance: Option<gents_protocol::session::SessionProvenance>,
    now: &str,
) -> Result<bool> {
    if session_id.trim().is_empty() {
        return Err(SessionError::SessionIdBlank);
    }
    if node_did.trim().is_empty() {
        return Err(SessionError::NodeDidBlank);
    }
    if agent_id.trim().is_empty() {
        return Err(SessionError::AgentIdBlank);
    }
    if let Some(requester_did) = requester_did.map(str::trim) {
        if requester_did.is_empty() {
            return Err(SessionError::RequesterDidBlank);
        }
    }
    chrono::DateTime::parse_from_rfc3339(now).map_err(SessionError::InvalidCreatedAt)?;

    let scope = session_scope_filter(node_did, session_id, requester_did);
    let query = format!(
        r#"{{
            AgentSession(filter: {{ {scope} }}) {{
                {SESSION_SCOPE_FIELDS}
            }}
        }}"#
    );
    let response = txn.execute(&query).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("AgentSession"))
        .and_then(serde_json::Value::as_array)
        .ok_or(SessionError::AgentSessionRowsOmitted)?
        .clone();
    let rows: Vec<serde_json::Value> = rows;
    if rows.len() > 1 {
        return Err(SessionError::CreateDuplicateRows {
            session_id: session_id.to_owned(),
        });
    }

    if let Some(row) = rows.first() {
        let session = decode_session_row(row)?.session;
        validate_agent_session(&session)?;
        if session.session_id != session_id {
            return Err(SessionError::SessionIdMismatch {
                existing: session.session_id.clone(),
                requested: session_id.to_owned(),
            });
        }
        if session.node_did != node_did {
            return Err(SessionError::NodeDidMismatch {
                existing: session.node_did.clone(),
                requested: node_did.to_owned(),
            });
        }
        match (&session.requester_did, requester_did) {
            (Some(existing), Some(requested)) => {
                if existing != requested {
                    return Err(SessionError::RequesterMismatch {
                        existing: existing.clone(),
                        requested: requested.to_owned(),
                    });
                }
            }
            (Some(existing), None) => {
                return Err(SessionError::RequesterExistingOnly {
                    existing: existing.clone(),
                })
            }
            (None, Some(requested)) => {
                return Err(SessionError::RequesterRequestedOnly {
                    requested: requested.to_owned(),
                })
            }
            (None, None) => {}
        }
        if session.agent_id != agent_id {
            return Err(SessionError::AgentMismatch {
                existing: session.agent_id.clone(),
                requested: agent_id.to_owned(),
            });
        }
        return Ok(false);
    }

    let created = txn.execute_with_variables(
        "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }",
        &serde_json::json!({"input": {
            "session_id": session_id, "node_did": node_did,
            "requester_did": requester_did, "agent_id": agent_id,
            "created_at": now, "title": title, "provenance": provenance
        }}),
    ).await?;
    let created = defra_node::QueryResponse::success(
        created
            .get("data")
            .ok_or(SessionError::CreateOmittedData)?
            .clone(),
    );
    if crate::graphql::single_mutation_document(&created, "create_AgentSession")?.is_none() {
        // Revalidate scope through the same preserved-document contract so a
        // colliding writer cannot silently win with a different binding.
        let response = txn.execute(&query).await?;
        let rows = response
            .get("data")
            .and_then(|data| data.get("AgentSession"))
            .and_then(serde_json::Value::as_array)
            .ok_or(SessionError::AgentSessionRowsOmitted)?
            .clone();
        if rows.len() == 1 {
            let session = decode_session_row(&rows[0])?.session;
            validate_agent_session(&session)?;
            if !(session.session_id == session_id
                && session.node_did == node_did
                && session.agent_id == agent_id
                && session.requester_did.as_deref() == requester_did)
            {
                return Err(SessionError::CreateRaced);
            }
            return Ok(false);
        }
        return Err(SessionError::CreateNoDocument);
    }
    tracing::info!(
        session_id = %session_id,
        node_did = %node_did,
        agent_id = %agent_id,
        created = true,
        "session created"
    );
    Ok(true)
}

pub(crate) async fn reopen_session_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    now: &str,
) -> Result<bool> {
    let Some(row) = load_agent_session_row_in_txn(txn, node_did, session_id, requester_did).await?
    else {
        return Err(SessionError::ReopenMissing {
            session_id: session_id.to_owned(),
        });
    };
    let session = &row.session;
    if session.node_did != node_did {
        return Err(SessionError::NodeDidMismatch {
            existing: session.node_did.clone(),
            requested: node_did.trim().to_owned(),
        });
    }
    match (
        session.requester_did.as_deref(),
        requester_did.map(str::trim),
    ) {
        (Some(existing), Some(requested)) => {
            if existing != requested {
                return Err(SessionError::RequesterMismatch {
                    existing: existing.to_owned(),
                    requested: requested.to_owned(),
                });
            }
        }
        (Some(existing), None) => {
            return Err(SessionError::RequesterExistingOnly {
                existing: existing.to_owned(),
            })
        }
        (None, Some(_)) => return Err(SessionError::RequesterAbsentMismatch),
        (None, None) => {}
    }
    let was_closed = session.closed_at.is_some();
    if was_closed {
        patch_session_in_txn(
            txn,
            &row.doc_id,
            serde_json::json!({
                "closed_at": null,
                "observation": touched_observation(&row, now),
            }),
        )
        .await?;
    }
    Ok(was_closed)
}

pub(crate) async fn touch_session_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    row: &super::rows::SessionOwnerRow,
    now: &str,
) -> Result<()> {
    patch_session_in_txn(
        txn,
        &row.doc_id,
        serde_json::json!({ "observation": touched_observation(row, now) }),
    )
    .await
}

pub async fn close_session(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    chrono::DateTime::parse_from_rfc3339(&now).map_err(SessionError::InvalidClosedAt)?;
    crate::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "session.close",
        move |txn| {
            let now = now.clone();
            Box::pin(async move {
                let Some(row) =
                    load_agent_session_row_in_txn(txn, node_did, session_id, requester_did).await?
                else {
                    return Err(SessionError::CloseMissing {
                        session_id: session_id.to_owned(),
                    });
                };
                if row.session.closed_at.is_some() {
                    return Ok::<(), SessionError>(());
                }
                patch_session_in_txn(
                    txn,
                    &row.doc_id,
                    serde_json::json!({
                        "closed_at": now,
                        "observation": touched_observation(&row, &now),
                    }),
                )
                .await?;
                tracing::info!(session_id = %session_id, "session closed");
                Ok(())
            })
        },
    )
    .await
}

pub(super) const SESSION_SCOPE_FIELDS: &str = "session_id node_did requester_did agent_id \
created_at closed_at title provenance observation tags _docID";

pub async fn load_agent_session_row_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<super::rows::SessionOwnerRow>> {
    Ok(load_agent_session_owner_in_txn(txn, node_did, session_id)
        .await?
        .filter(|row| row.session.requester_did.as_deref() == requester_did))
}

pub(crate) async fn load_agent_session_owner_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<Option<super::rows::SessionOwnerRow>> {
    let scope = format!(
        r#"node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(session_id),
    );
    let query = format!(
        r#"{{
            AgentSession(filter: {{ {scope} }}) {{
                {SESSION_SCOPE_FIELDS}
            }}
        }}"#
    );
    let response = txn.execute(&query).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("AgentSession"))
        .and_then(serde_json::Value::as_array)
        .ok_or(SessionError::AgentSessionRowsOmitted)?
        .clone();
    if rows.len() > 1 {
        return Err(SessionError::DuplicateRowsForSession {
            session_id: session_id.to_owned(),
        });
    }
    rows.first().map(decode_session_row).transpose()
}

pub(super) fn touch_activity_value(row: &super::rows::SessionOwnerRow, now: &str) -> String {
    let old = row
        .session
        .observation
        .as_ref()
        .map(|observation| observation.last_activity_at.clone())
        .unwrap_or_else(|| row.session.created_at.clone());
    match (
        chrono::DateTime::parse_from_rfc3339(&old),
        chrono::DateTime::parse_from_rfc3339(now),
    ) {
        (Ok(old_time), Ok(now_time)) if old_time > now_time => old,
        _ => now.to_string(),
    }
}

pub(super) fn touched_observation(
    row: &super::rows::SessionOwnerRow,
    now: &str,
) -> gents_protocol::session::SessionObservation {
    gents_protocol::session::SessionObservation {
        last_activity_at: touch_activity_value(row, now),
        preview: row
            .session
            .observation
            .as_ref()
            .and_then(|v| v.preview.clone()),
        latest_request: row
            .session
            .observation
            .as_ref()
            .and_then(|v| v.latest_request.clone()),
    }
}

pub(super) async fn patch_session_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    doc_id: &str,
    input: serde_json::Value,
) -> Result<()> {
    let doc_id = escape_graphql_string(doc_id);
    let mutation = format!(
        r#"mutation($input: AgentSessionMutationInputArg!) {{
        update_AgentSession(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, input: $input) {{ _docID }}
    }}"#
    );
    let response = txn
        .execute_with_variables(&mutation, &serde_json::json!({"input": input}))
        .await?;
    if !response
        .get("data")
        .and_then(|data| data.get("update_AgentSession"))
        .is_some_and(crate::graphql::response_has_documents)
    {
        return Err(SessionError::PatchNoDocument);
    }
    Ok(())
}

pub(crate) async fn max_sequence(
    node: &std::sync::Arc<EmbeddedNode>,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<u32> {
    crate::config_client::ConfigAccess::transact_local_readonly(
        node,
        None,
        "session.max_sequence",
        |txn| {
            Box::pin(
                async move { max_sequence_in_txn(txn, session_id, node_did, requester_did).await },
            )
        },
    )
    .await
}

pub(super) async fn max_sequence_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<u32> {
    let scope = session_scope_filter(node_did, session_id, requester_did);
    let response = txn.execute(&format!(r#"{{ AgentMessage(filter: {{ {scope} }}, order: {{sequence: DESC}}, limit: 1) {{ sequence }} }}"#)).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("AgentMessage"))
        .and_then(serde_json::Value::as_array)
        .ok_or(SessionError::SequenceRowsOmitted)?;
    match rows.first() {
        None => Ok(0),
        Some(row) => {
            let sequence = row
                .get("sequence")
                .and_then(serde_json::Value::as_u64)
                .ok_or(SessionError::SequenceInvalid)?;
            u32::try_from(sequence).map_err(SessionError::SequenceOverflow)
        }
    }
}
