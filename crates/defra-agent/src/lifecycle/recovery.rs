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

    /// Owner-scoped terminal-convergence re-drive (#664).
    ///
    /// Under `subagent-host` replication an `AgentRequest` is replicated onto
    /// non-owning peers. Safety already holds (the watcher `agent_did` filter
    /// never lets a peer claim a foreign replica), but liveness does not: when
    /// the owner terminalizes, the terminal delta reaches replicas via a single
    /// one-shot PushLog that can drop, and there is no per-doc anti-entropy on a
    /// running peer (defradb.rs#1074) to re-request it. This re-drive is the
    /// owner side of the fix — periodically re-asserting the current terminal
    /// value of recently-terminalized own-requests. A same-value re-write is a
    /// genuine higher-priority CRDT delta (it does not no-op), so it flows
    /// through the normal PushLog path and a lagging replica accepts it (LWW,
    /// higher priority ⇒ applied). Idempotent and bounded: each row is
    /// re-asserted at most [`TERMINAL_REDRIVE_CAP`] times before it self-drops,
    /// and each pass scans at most [`TERMINAL_REDRIVE_BATCH_LIMIT`] rows.
    ///
    /// `agent_did` MUST be the runtime's own DID: only the owner re-asserts its
    /// own documents; peers stay passive (a peer-authored delta to a foreign doc
    /// would fork the CRDT, not converge it). `agent_did` itself is never
    /// written (it is `@immutable`); only the mutable terminal `status` and
    /// `lifecycle_state` columns are re-asserted, to their current values.
    ///
    /// `budget` is the caller-owned, in-memory per-doc re-emit counter, carried
    /// across ticks so a converged row stops being re-driven. It is pruned to the
    /// current candidate window each pass, so it stays bounded; losing it on
    /// restart is harmless because startup recovery re-drives from scratch.
    pub async fn redrive_terminal_convergence(
        node: &EmbeddedNode,
        agent_did: &str,
        budget: &mut std::collections::HashMap<String, u32>,
    ) -> Result<TerminalRedriveReport> {
        let escaped_agent_did = escape_graphql_string(agent_did);
        let terminal_states = crate::lifecycle::terminal_lifecycle_state_graphql_list();
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{
                        agent_did: {{ _eq: "{escaped_agent_did}" }},
                        lifecycle_state: {{ _in: {terminal_states} }}
                    }},
                    order: [{{ created_at: DESC }}, {{ request_id: DESC }}],
                    limit: {limit}
                ) {{
                    _docID
                    request_id
                    status
                    lifecycle_state
                }}
            }}"#,
            limit = TERMINAL_REDRIVE_BATCH_LIMIT,
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

        // Collect the bounded candidate window and prune the carried-over budget
        // to it, so the in-memory map never outgrows the query window.
        let mut candidates: Vec<(String, String, String, String)> = Vec::new();
        for row in &rows {
            let doc_id = row.get("_docID").and_then(|v| v.as_str()).unwrap_or("");
            let request_id = row.get("request_id").and_then(|v| v.as_str()).unwrap_or("");
            let status = row.get("status").and_then(|v| v.as_str()).unwrap_or("");
            let lifecycle_state = row
                .get("lifecycle_state")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if doc_id.is_empty() || status.is_empty() || lifecycle_state.is_empty() {
                continue;
            }
            candidates.push((
                doc_id.to_string(),
                request_id.to_string(),
                status.to_string(),
                lifecycle_state.to_string(),
            ));
        }
        let candidate_ids: std::collections::HashSet<&str> = candidates
            .iter()
            .map(|(doc_id, ..)| doc_id.as_str())
            .collect();
        budget.retain(|doc_id, _| candidate_ids.contains(doc_id.as_str()));

        let scanned = candidates.len();
        let mut reasserted = 0usize;
        for (doc_id, request_id, status, lifecycle_state) in &candidates {
            let remaining = budget.entry(doc_id.clone()).or_insert(TERMINAL_REDRIVE_CAP);
            if *remaining == 0 {
                continue;
            }

            let escaped_doc_id = escape_graphql_string(doc_id);
            let escaped_status = escape_graphql_string(status);
            let escaped_lifecycle_state = escape_graphql_string(lifecycle_state);
            let mutation = format!(
                r#"mutation {{
                    update_AgentRequest(
                        filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                        input: {{
                            status: "{escaped_status}",
                            lifecycle_state: "{escaped_lifecycle_state}"
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
                continue;
            }

            *remaining -= 1;
            reasserted += 1;
            tracing::debug!(
                doc_id = %doc_id,
                request_id = %request_id,
                status = %status,
                lifecycle_state = %lifecycle_state,
                remaining_reemits = *remaining,
                "re-asserted terminal request state to converge replicas"
            );
        }

        Ok(TerminalRedriveReport {
            reasserted,
            scanned,
        })
    }
}

async fn recover_stuck_requests(node: &EmbeddedNode, agent_did: &str) -> Result<usize> {
    // Key the stale predicate on `lifecycle_state ∈ {claimed, processing}` to
    // mirror the Lean `Recovery.requestRecoveryStale` model exactly, rather than
    // on the coarser `status = "processing"`. A stuck `claimed` own-request is
    // now recovered even if its `status` is not `"processing"`.
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

    let count = rows.len();
    for row in &rows {
        let doc_id = row.get("_docID").and_then(|v| v.as_str()).unwrap_or("");
        let request_id = row.get("request_id").and_then(|v| v.as_str()).unwrap_or("");
        let session_id = row.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
        let retry_count = row.get("retry_count").and_then(|v| v.as_i64()).unwrap_or(0);
        let response_status =
            lookup_response_status_by_request_id(node, agent_did, request_id).await?;
        let next_status = if matches!(response_status.as_deref(), Some("complete" | "completed")) {
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
