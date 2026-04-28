use super::lookup::{lookup_request_status_by_request_id, lookup_response_status_by_request_id};
use super::*;

impl RequestLifecycle {
    pub async fn recover_all(node: &EmbeddedNode, agent_did: &str) -> Result<RecoveryReport> {
        Ok(RecoveryReport {
            responses_recovered: recover_stuck_responses(node, agent_did).await?
                + recover_missing_response_documents(node, agent_did).await?,
            requests_recovered: recover_stuck_requests(node, agent_did).await?,
            conversations_recovered: recover_stuck_conversations(node, agent_did).await?,
        })
    }
}

async fn recover_stuck_requests(node: &EmbeddedNode, agent_did: &str) -> Result<usize> {
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    agent_did: {{ _eq: "{agent_did}" }},
                    status: {{ _eq: "processing" }}
                }}
            ) {{
                _docID
                request_id
                behavior_id
                session_id
                retry_count
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("querying stuck requests: {:?}", resp.errors);
    }

    let rows: Vec<serde_json::Value> = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let count = rows.len();
    for row in &rows {
        let doc_id = row.get("_docID").and_then(|v| v.as_str()).unwrap_or("");
        let request_id = row.get("request_id").and_then(|v| v.as_str()).unwrap_or("");
        let session_id = row.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
        let retry_count = row.get("retry_count").and_then(|v| v.as_i64()).unwrap_or(0);
        let response_status =
            lookup_response_status_by_request_id(node, agent_did, request_id).await?;
        let next_status = if response_status.as_deref() == Some("complete") {
            "completed"
        } else {
            "error"
        };
        let next_lifecycle_state = if next_status == "completed" {
            PersistedLifecycleState::Completed.as_str()
        } else {
            PersistedLifecycleState::Failed.as_str()
        };

        let mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                    input: {{
                        status: "{next_status}",
                        lifecycle_state: "{next_lifecycle_state}"
                    }}
                ) {{ _docID }}
            }}"#,
        );

        let resp = node.execute(&mutation).await;
        if resp.has_errors() {
            tracing::warn!(
                doc_id = %doc_id,
                request_id = %request_id,
                session_id = %session_id,
                next_status = %next_status,
                response_status = response_status.as_deref().unwrap_or("missing"),
                errors = ?resp.errors,
                "failed to recover stuck request"
            );
        } else {
            tracing::info!(
                doc_id = %doc_id,
                request_id = %request_id,
                session_id = %session_id,
                retry_count = retry_count,
                response_status = response_status.as_deref().unwrap_or("missing"),
                "recovered stuck request: processing → {next_status}"
            );
        }
    }

    Ok(count)
}

async fn recover_stuck_responses(node: &EmbeddedNode, agent_did: &str) -> Result<usize> {
    let now = chrono::Utc::now().to_rfc3339();
    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{
                    agent_did: {{ _eq: "{agent_did}" }},
                    status: {{ _eq: "streaming" }}
                }}
            ) {{
                _docID
                request_id
                content
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("querying stuck responses: {:?}", resp.errors);
    }

    let rows: Vec<serde_json::Value> = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentResponse"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let count = rows.len();
    for row in &rows {
        let doc_id = row.get("_docID").and_then(|v| v.as_str()).unwrap_or("");
        let request_id = row.get("request_id").and_then(|v| v.as_str()).unwrap_or("");
        let existing_content = row.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let error_suffix = if existing_content.trim().is_empty() {
            "Error: daemon restarted before response could be generated"
        } else {
            "\n\n[Response interrupted — daemon restarted]"
        };
        let final_content = format!("{existing_content}{error_suffix}");
        let escaped_content = escape_graphql_string(&final_content);

        let mutation = format!(
            r#"mutation {{
                update_AgentResponse(
                    filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                    input: {{
                        content: "{escaped_content}",
                        status: "error",
                        completed_at: "{now}"
                    }}
                ) {{ _docID }}
            }}"#
        );

        let resp = node.execute(&mutation).await;
        if resp.has_errors() {
            tracing::warn!(
                doc_id = %doc_id,
                request_id = %request_id,
                errors = ?resp.errors,
                "failed to finalize stuck response"
            );
        } else {
            tracing::info!(
                doc_id = %doc_id,
                request_id = %request_id,
                "recovered stuck response: streaming → error"
            );
        }
    }

    Ok(count)
}

async fn recover_missing_response_documents(node: &EmbeddedNode, agent_did: &str) -> Result<usize> {
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    agent_did: {{ _eq: "{agent_did}" }},
                    status: {{ _eq: "processing" }}
                }}
            ) {{
                request_id
                behavior_id
                session_id
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!(
            "querying processing requests for missing responses: {:?}",
            resp.errors
        );
    }

    let rows: Vec<serde_json::Value> = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut recovered = 0;
    for row in rows {
        let request_id = row.get("request_id").and_then(|v| v.as_str()).unwrap_or("");
        let behavior_id = row
            .get("behavior_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let session_id = row.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
        if request_id.is_empty() || session_id.is_empty() {
            continue;
        }

        if lookup_response_status_by_request_id(node, agent_did, request_id)
            .await?
            .is_some()
        {
            continue;
        }

        let now = chrono::Utc::now().to_rfc3339();
        let error_text =
            escape_graphql_string("Error: daemon restarted before response could be generated");
        let escaped_request_id = escape_graphql_string(request_id);
        let escaped_agent_did = escape_graphql_string(agent_did);
        let escaped_behavior_id = escape_graphql_string(behavior_id);
        let escaped_session_id = escape_graphql_string(session_id);
        let mutation = format!(
            r#"mutation {{
                create_AgentResponse(input: {{
                    response_key: "{escaped_request_id}",
                    request_id: "{escaped_request_id}",
                    agent_did: "{escaped_agent_did}",
                    behavior_id: "{escaped_behavior_id}",
                    session_id: "{escaped_session_id}",
                    content: "{error_text}",
                    status: "error",
                    token_count: 0,
                    progress_seq: 0,
                    created_at: "{now}",
                    completed_at: "{now}"
                }}) {{ _docID }}
            }}"#
        );

        let resp = node.execute(&mutation).await;
        if resp.has_errors() {
            tracing::warn!(
                request_id = %request_id,
                session_id = %session_id,
                errors = ?resp.errors,
                "failed to create recovery error response for missing AgentResponse"
            );
            continue;
        }

        recovered += 1;
        tracing::info!(
            request_id = %request_id,
            session_id = %session_id,
            "created recovery error response for missing AgentResponse"
        );
    }

    Ok(recovered)
}

async fn recover_stuck_conversations(node: &EmbeddedNode, agent_did: &str) -> Result<usize> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentConversation(
                filter: {{
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    status: {{ _in: ["processing", "error"] }}
                }}
            ) {{
                _docID
                agent_name
                behavior_id
                session_id
                latest_request_id
                status
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("querying stuck conversations: {:?}", resp.errors);
    }

    let rows: Vec<serde_json::Value> = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentConversation"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let count = rows.len();
    for row in &rows {
        let doc_id = row.get("_docID").and_then(|v| v.as_str()).unwrap_or("");
        let agent_name = row.get("agent_name").and_then(|v| v.as_str()).unwrap_or("");
        let behavior_id = row
            .get("behavior_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let session_id = row.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
        let latest_request_id = row
            .get("latest_request_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let old_status = row.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let latest_request_status =
            lookup_request_status_by_request_id(node, agent_did, latest_request_id).await?;
        let next_status = match latest_request_status.as_deref() {
            Some("completed") => "completed",
            Some("error") => "active",
            _ => "active",
        };

        if let Err(error) = session::update_conversation_status_with_identity(
            node,
            session_id,
            agent_name,
            agent_did,
            behavior_id,
            next_status,
        )
        .await
        {
            tracing::warn!(
                doc_id = %doc_id,
                agent_name = %agent_name,
                session_id = %session_id,
                latest_request_id = %latest_request_id,
                latest_request_status = latest_request_status.as_deref().unwrap_or("missing"),
                error = %error,
                "failed to recover stuck conversation"
            );
        } else {
            tracing::info!(
                doc_id = %doc_id,
                agent_name = %agent_name,
                session_id = %session_id,
                old_status = %old_status,
                latest_request_id = %latest_request_id,
                latest_request_status = latest_request_status.as_deref().unwrap_or("missing"),
                "recovered stuck conversation: {old_status} → {next_status}"
            );
        }
    }

    Ok(count)
}
