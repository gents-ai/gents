use std::collections::BTreeMap;

use super::lookup::{
    lookup_request_status_by_request_id, lookup_response_status_by_request_id,
    lookup_terminal_response_by_request_id,
};
use super::*;

impl RequestLifecycle {
    pub async fn recover_all(node: &EmbeddedNode, agent_did: &str) -> Result<RecoveryReport> {
        let responses_recovered = recover_stuck_responses(node, agent_did).await?
            + recover_missing_response_documents(node, agent_did).await?;
        let requests_recovered = Self::repair_terminal_requests(node, agent_did)
            .await?
            .repaired;
        let conversations = recover_stuck_conversations(node, agent_did).await?;

        Ok(RecoveryReport {
            responses_recovered,
            requests_recovered,
            conversations_recovered: conversations.recovered,
            conversations_failed: conversations.failed,
            duplicate_conversation_sessions: conversations.duplicate_sessions,
        })
    }

    /// to its `requester_did` peer. Safety already holds (the watcher
    /// it whether a peer caught up. Each successful re-assert atomically advances
    /// at [`TERMINAL_REDRIVE_CAP`] across process restarts. Candidate ordering is
    pub async fn redrive_terminal_convergence(
        node: &EmbeddedNode,
        agent_did: &str,
    ) -> Result<TerminalRedriveReport> {
        let escaped_agent_did = escape_graphql_string(agent_did);
        let terminal_states = crate::lifecycle::terminal_lifecycle_state_graphql_list();
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{
                        agent_did: {{ _eq: "{escaped_agent_did}" }},
                        requester_did: {{ _neq: null }},
                        lifecycle_state: {{ _in: {terminal_states} }},
                        terminal_redrive_attempts: {{ _lt: {cap} }}
                    }},
                    order: [{{ terminalized_at: ASC }}, {{ request_id: ASC }}],
                    limit: {limit}
                ) {{
                    _docID
                    request_id
                    status
                    lifecycle_state
                    terminal_redrive_attempts
                }}
            }}"#,
            limit = TERMINAL_REDRIVE_BATCH_LIMIT,
            cap = TERMINAL_REDRIVE_CAP,
        );

        let resp = node.execute(&query).await;
        if resp.has_errors() {
            anyhow::bail!("querying terminal requests to re-drive: {:?}", resp.errors);
        }

        let rows: Vec<serde_json::Value> = resp
            .data
            .as_ref()
            .and_then(|d| d.get("AgentRequest"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut candidates: Vec<(String, String, String, String, u32)> = Vec::new();
        for row in &rows {
            let doc_id = row.get("_docID").and_then(|v| v.as_str()).unwrap_or("");
            let request_id = row.get("request_id").and_then(|v| v.as_str()).unwrap_or("");
            let status = row.get("status").and_then(|v| v.as_str()).unwrap_or("");
            let lifecycle_state = row
                .get("lifecycle_state")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let attempts = row
                .get("terminal_redrive_attempts")
                .and_then(|value| value.as_u64())
                .unwrap_or(TERMINAL_REDRIVE_CAP as u64) as u32;
            if doc_id.is_empty() || status.is_empty() || lifecycle_state.is_empty() {
                continue;
            }
            candidates.push((
                doc_id.to_string(),
                request_id.to_string(),
                status.to_string(),
                lifecycle_state.to_string(),
                attempts,
            ));
        }

        let scanned = candidates.len();
        let mut reasserted = 0usize;
        let mut failed = 0usize;
        for (doc_id, request_id, status, lifecycle_state, attempts) in &candidates {
            let next_attempts = attempts.saturating_add(1);
            let escaped_doc_id = escape_graphql_string(doc_id);
            let escaped_status = escape_graphql_string(status);
            let escaped_lifecycle_state = escape_graphql_string(lifecycle_state);
            // queue.rs seam guards — a re-drive must never touch a foreign replica.
            let mutation = format!(
                r#"mutation {{
                    update_AgentRequest(
                        filter: {{
                            _docID: {{ _eq: "{escaped_doc_id}" }},
                            agent_did: {{ _eq: "{escaped_agent_did}" }},
                            requester_did: {{ _neq: null }},
                            lifecycle_state: {{ _eq: "{escaped_lifecycle_state}" }},
                            terminal_redrive_attempts: {{ _eq: {attempts} }}
                        }},
                        input: {{
                            status: "{escaped_status}",
                            lifecycle_state: "{escaped_lifecycle_state}",
                            terminal_redrive_attempts: {next_attempts}
                        }}
                    ) {{ _docID }}
                }}"#,
            );

            let resp = node.execute(&mutation).await;
            if resp.has_errors() {
                tracing::warn!(
                    doc_id = %doc_id,
                    request_id = %request_id,
                    status = %status,
                    errors = ?resp.errors,
                    "failed to re-drive terminal request convergence"
                );
                failed += 1;
                continue;
            }

            let updated = resp
                .data
                .as_ref()
                .and_then(|data| data.get("update_AgentRequest"))
                .is_some_and(response_has_documents);
            if !updated {
                continue;
            }
            reasserted += 1;
            tracing::debug!(
                doc_id = %doc_id,
                request_id = %request_id,
                status = %status,
                lifecycle_state = %lifecycle_state,
                terminal_redrive_attempts = next_attempts,
                "re-asserted terminal request state to converge replicas"
            );
        }

        Ok(TerminalRedriveReport {
            reasserted,
            scanned,
            failed,
        })
    }

    pub async fn repair_terminal_requests(
        node: &EmbeddedNode,
        agent_did: &str,
    ) -> Result<TerminalRepairReport> {
        // mirror the Lean `Recovery.requestRecoveryStale` model exactly, rather than
        let stale_states = crate::lifecycle::stuck_request_lifecycle_state_graphql_list();
        let escaped_agent_did = escape_graphql_string(agent_did);
        let query = format!(
            r#"{{
            AgentRequest(
                filter: {{
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    lifecycle_state: {{ _in: {stale_states} }}
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

        let mut report = TerminalRepairReport {
            scanned: rows.len(),
            ..Default::default()
        };
        for row in &rows {
            let doc_id = row.get("_docID").and_then(|v| v.as_str()).unwrap_or("");
            let request_id = row.get("request_id").and_then(|v| v.as_str()).unwrap_or("");
            let session_id = row.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
            let retry_count = row.get("retry_count").and_then(|v| v.as_i64()).unwrap_or(0);
            let terminal_response =
                lookup_terminal_response_by_request_id(node, agent_did, request_id).await?;
            let Some(terminal_response) = terminal_response else {
                report.awaiting_outcome += 1;
                continue;
            };
            let response_status = terminal_response.status;
            let response_reason = terminal_response
                .error_message
                .as_deref()
                .unwrap_or_default();
            // interrupt flow stamps it standalone and again atomically inside
            let response_was_interrupted = terminal_response
                .interrupted_at
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty());
            let (next_status, next_lifecycle_state) =
                if matches!(response_status.as_str(), "complete" | "completed") {
                    ("completed", PersistedLifecycleState::Completed.as_str())
                } else if response_was_interrupted {
                    ("interrupted", PersistedLifecycleState::Interrupted.as_str())
                } else {
                    ("error", PersistedLifecycleState::Failed.as_str())
                };
            let terminalized_at = chrono::Utc::now().to_rfc3339();
            let escaped_terminalized_at = escape_graphql_string(&terminalized_at);
            let escaped_doc_id = escape_graphql_string(doc_id);
            let failure_reason = match next_lifecycle_state {
                state if state == PersistedLifecycleState::Completed.as_str() => "",
                state if state == PersistedLifecycleState::Interrupted.as_str() => "interrupted",
                _ => response_reason,
            };
            let escaped_failure_reason = escape_graphql_string(failure_reason);
            let escaped_agent_did = escape_graphql_string(agent_did);
            let stale_states = crate::lifecycle::stuck_request_lifecycle_state_graphql_list();

            let mutation = format!(
                r#"mutation {{
                update_AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        agent_did: {{ _eq: "{escaped_agent_did}" }},
                        lifecycle_state: {{ _in: {stale_states} }}
                    }},
                    input: {{
                        status: "{next_status}",
                        lifecycle_state: "{next_lifecycle_state}",
                        failure_reason: "{escaped_failure_reason}",
                        terminalized_at: "{escaped_terminalized_at}",
                        terminal_redrive_attempts: 0
                    }}
                ) {{ _docID }}
            }}"#,
            );

            match crate::retry::execute_graphql_with_terminal_persistence_retry(
                node,
                &mutation,
                "repair_terminal_request",
            )
            .await
            {
                Err(error) => {
                    tracing::warn!(
                        doc_id = %doc_id,
                        request_id = %request_id,
                        session_id = %session_id,
                        next_status = %next_status,
                        response_status = %response_status,
                        error = %error,
                        "failed to recover stuck request"
                    );
                    report.failed += 1;
                }
                Ok(resp) => {
                    let updated = resp
                        .data
                        .as_ref()
                        .and_then(|data| data.get("update_AgentRequest"))
                        .is_some_and(response_has_documents);
                    if !updated {
                        continue;
                    }
                    report.repaired += 1;
                    tracing::info!(
                        doc_id = %doc_id,
                        request_id = %request_id,
                        session_id = %session_id,
                        retry_count = retry_count,
                        response_status = %response_status,
                        "recovered stuck request: processing → {next_status}"
                    );
                }
            }
        }

        Ok(report)
    }
}

async fn recover_stuck_responses(node: &EmbeddedNode, agent_did: &str) -> Result<usize> {
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
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

    let mut count = 0;
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
        let escaped_error_message =
            escape_graphql_string("daemon restarted before response could be finalized");
        let escaped_doc_id = escape_graphql_string(doc_id);

        let mutation = format!(
            r#"mutation {{
                update_AgentResponse(
                    filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                    input: {{
                        content: "{escaped_content}",
                        status: "error",
                        error_message: "{escaped_error_message}",
                        completed_at: "{now}"
                    }}
                ) {{ _docID }}
            }}"#
        );

        let resp = crate::retry::execute_graphql_with_terminal_persistence_retry(
            node,
            &mutation,
            "recover_stuck_response",
        )
        .await;
        if let Err(error) = resp {
            tracing::warn!(
                doc_id = %doc_id,
                request_id = %request_id,
                error = %error,
                "failed to finalize stuck response"
            );
        } else {
            count += 1;
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
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    status: {{ _eq: "processing" }}
                }}
            ) {{
                request_id
                requester_did
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
        let requester_did = row.get("requester_did").and_then(|v| v.as_str());
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

        let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let error_reason = "daemon restarted before response could be generated";
        let error_text = escape_graphql_string(&format!("Error: {error_reason}"));
        let escaped_error_reason = escape_graphql_string(error_reason);
        let escaped_request_id = escape_graphql_string(request_id);
        let escaped_agent_did = escape_graphql_string(agent_did);
        let escaped_behavior_id = escape_graphql_string(behavior_id);
        let escaped_session_id = escape_graphql_string(session_id);
        let requester_did_field = crate::session::requester_did_create_field(requester_did);
        let mutation = format!(
            r#"mutation {{
                create_AgentResponse(input: {{
                    response_key: "{escaped_request_id}",
                    request_id: "{escaped_request_id}",
                    agent_did: "{escaped_agent_did}",
                    {requester_did_field}
                    behavior_id: "{escaped_behavior_id}",
                    session_id: "{escaped_session_id}",
                    content: "{error_text}",
                    status: "error",
                    error_message: "{escaped_error_reason}",
                    token_count: 0,
                    progress_seq: 0,
                    created_at: "{now}",
                    completed_at: "{now}"
                }}) {{ _docID }}
            }}"#
        );

        let resp = crate::retry::execute_graphql_with_terminal_persistence_retry(
            node,
            &mutation,
            "recover_missing_response_document",
        )
        .await;
        if let Err(error) = resp {
            tracing::warn!(
                request_id = %request_id,
                session_id = %session_id,
                error = %error,
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

#[derive(Debug, Default)]
struct ConversationRecoveryOutcome {
    recovered: usize,
    failed: usize,
    duplicate_sessions: usize,
}

/// Mirrors the Lean sweep `Recovery.conversationRecoverySweep`
///    permanently (DefraDB cannot add an index to an existing collection), and
async fn recover_stuck_conversations(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<ConversationRecoveryOutcome> {
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
                title
                preview_text
                updated_at
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

    let mut sessions: BTreeMap<String, Vec<StuckConversationRow>> = BTreeMap::new();
    for row in &rows {
        let parsed = StuckConversationRow::from_row(row);
        sessions
            .entry(parsed.session_id.clone())
            .or_default()
            .push(parsed);
    }

    let mut outcome = ConversationRecoveryOutcome::default();
    for (session_id, mut docs) in sessions {
        // `_docID` (mirrors Lean `docRank`).
        docs.sort_by(|left, right| right.rank().cmp(&left.rank()));
        let Some(canonical) = docs.first().cloned() else {
            continue;
        };

        if docs.len() > 1 {
            outcome.duplicate_sessions += 1;
            let duplicate_doc_ids = docs
                .iter()
                .skip(1)
                .map(|doc| doc.doc_id.as_str())
                .collect::<Vec<_>>();
            tracing::warn!(
                session_id = %session_id,
                doc_count = docs.len(),
                canonical_doc_id = %canonical.doc_id,
                duplicate_doc_ids = ?duplicate_doc_ids,
                "duplicate AgentConversation documents share a session_id; recovering the \
                 canonical document and converging the duplicates onto it"
            );
        }

        let latest_request_status =
            lookup_request_status_by_request_id(node, agent_did, &canonical.latest_request_id)
                .await?;
        let next_status = match latest_request_status.as_deref() {
            Some("completed") => "completed",
            Some("error") => "active",
            _ => "active",
        };

        let mut session_failed = false;
        for doc in &docs {
            if let Err(error) =
                update_conversation_status_by_doc_id(node, &doc.doc_id, &canonical, next_status)
                    .await
            {
                session_failed = true;
                tracing::warn!(
                    doc_id = %doc.doc_id,
                    session_id = %session_id,
                    agent_name = %canonical.agent_name,
                    latest_request_id = %canonical.latest_request_id,
                    latest_request_status = latest_request_status.as_deref().unwrap_or("missing"),
                    error = %error,
                    "failed to recover stuck conversation"
                );
            }
        }

        if session_failed {
            outcome.failed += 1;
            continue;
        }

        outcome.recovered += 1;
        tracing::info!(
            doc_id = %canonical.doc_id,
            session_id = %session_id,
            agent_name = %canonical.agent_name,
            old_status = %canonical.status,
            doc_count = docs.len(),
            latest_request_id = %canonical.latest_request_id,
            latest_request_status = latest_request_status.as_deref().unwrap_or("missing"),
            "recovered stuck conversation: {} → {next_status}",
            canonical.status
        );
    }

    Ok(outcome)
}

#[derive(Debug, Clone, Default)]
struct StuckConversationRow {
    doc_id: String,
    session_id: String,
    agent_name: String,
    behavior_id: String,
    latest_request_id: String,
    status: String,
    title: String,
    preview_text: String,
    updated_at: String,
}

impl StuckConversationRow {
    fn from_row(row: &serde_json::Value) -> Self {
        let field = |key: &str| {
            row.get(key)
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string()
        };
        Self {
            doc_id: field("_docID"),
            session_id: field("session_id"),
            agent_name: field("agent_name"),
            behavior_id: field("behavior_id"),
            latest_request_id: field("latest_request_id"),
            status: field("status"),
            title: field("title"),
            preview_text: field("preview_text"),
            updated_at: field("updated_at"),
        }
    }

    /// Ranking key mirroring Lean `Recovery.docRank`: newest, then richest, then
    fn rank(&self) -> (String, usize, String) {
        let richness = [
            self.title.trim(),
            self.preview_text.trim(),
            self.latest_request_id.trim(),
        ]
        .iter()
        .filter(|field| !field.is_empty())
        .count();
        (self.updated_at.clone(), richness, self.doc_id.clone())
    }
}

async fn update_conversation_status_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
    canonical: &StuckConversationRow,
    status: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            update_AgentConversation(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                input: {{
                    agent_name: "{agent_name}",
                    behavior_id: "{behavior_id}",
                    status: "{status}",
                    updated_at: "{now}",
                    latest_request_id: "{latest_request_id}"
                }}
            ) {{ _docID }}
        }}"#,
        doc_id = escape_graphql_string(doc_id),
        agent_name = escape_graphql_string(&canonical.agent_name),
        behavior_id = escape_graphql_string(&canonical.behavior_id),
        status = escape_graphql_string(status),
        latest_request_id = escape_graphql_string(&canonical.latest_request_id),
    );

    let resp =
        crate::retry::execute_graphql_with_conflict_retry(node, &mutation, "recover_conversation")
            .await;
    if resp.has_errors() {
        anyhow::bail!("recovering conversation doc_id={doc_id}: {:?}", resp.errors);
    }
    Ok(())
}
