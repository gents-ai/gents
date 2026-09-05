use std::collections::HashSet;

use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::Deserialize;

use super::{validate_agent_request, AgentRequest, DefraWatcher};

mod rows;
use rows::{AgentRequestRow, SessionQueueRow};

pub(crate) const AGENT_REQUEST_FIELDS: &str = r#"
                    _docID
                    request_id
                    agent_did
                    requester_did
                    behavior_id
                    session_id
                    content
                    temperature
                    top_p
                    top_k
                    seed
                    max_tokens
                    max_total_tokens
                    metadata
                    execution_origin
                    created_at
                    deadline
                    subagent_depth
                    caused_by_parent_request_id
                    caused_by_parent_request_doc_id
                    caused_by_parent_tool_call_id
                    caused_by_parent_tool_call_doc_id
                    caused_by_trigger_id
                    caused_by_trigger_kind
                    caused_by_source_doc_id
                    caused_by_correlation
                    caused_by_trigger_context
                    workspace_id
                    workspace_authority
                    workspace_owner_deployment_id
                    workspace_seal_hash
"#;

impl DefraWatcher {
    pub async fn try_fetch_request(&self, doc_id: &str) -> anyhow::Result<Option<AgentRequest>> {
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        agent_did: {{ _eq: "{agent_did}" }},
                        lifecycle_state: {{ _eq: "pending" }}
                    }},
                    limit: 1
                ) {{{fields}
                    lifecycle_state
                    interrupt_requested_at
                    valid_until
                }}
            }}"#,
            doc_id = doc_id,
            agent_did = self.agent_did,
            fields = AGENT_REQUEST_FIELDS,
        );

        let resp = self.node.execute(&query).await;
        if resp.has_errors() {
            anyhow::bail!("watcher query failed: {:?}", resp.errors);
        }

        let (rows, malformed) = parse_active_runtime_rows(resp.data.as_ref())?;
        for row in malformed {
            self.terminalize_malformed_pending_request(&row).await;
        }
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        if !self.row_is_claimable(&row).await? {
            return Ok(None);
        }
        match row.clone().into_agent_request() {
            Ok(request) => {
                if !self.request_is_locally_claimable(&request) {
                    return Ok(None);
                }
                Ok(Some(request))
            }
            Err(error) => {
                self.terminalize_incoherent_pending_request(&row, &error)
                    .await;
                Ok(None)
            }
        }
    }

    pub(super) async fn pending_requests(&self) -> anyhow::Result<Vec<AgentRequest>> {
        let active_runtime_states = RequestLifecycleState::active_runtime_graphql_list();
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{
                        agent_did: {{ _eq: "{agent_did}" }},
                        lifecycle_state: {{ _in: {active_runtime_states} }}
                    }},
                    order: [{{ created_at: ASC }}, {{ request_id: ASC }}]
                ) {{{fields}
                    lifecycle_state
                    interrupt_requested_at
                    valid_until
                }}
            }}"#,
            agent_did = self.agent_did,
            active_runtime_states = active_runtime_states,
            fields = AGENT_REQUEST_FIELDS,
        );

        let resp = self.node.execute(&query).await;
        if resp.has_errors() {
            anyhow::bail!("watcher pending-request query failed: {:?}", resp.errors);
        }

        let (rows, malformed) = parse_active_runtime_rows(resp.data.as_ref())?;
        for row in malformed {
            self.terminalize_malformed_pending_request(&row).await;
        }
        for row in &rows {
            if row.is_pending() {
                if let Err(error) = row.clone().into_agent_request() {
                    self.terminalize_incoherent_pending_request(row, &error)
                        .await;
                }
            }
        }

        prioritize_aged_background_wakes(claimable_pending_rows_from_rows(rows), chrono::Utc::now())
            .into_iter()
            .map(AgentRequestRow::into_agent_request)
            .filter(|request| {
                request
                    .as_ref()
                    .map(|request| self.request_is_locally_claimable(request))
                    .unwrap_or(true)
            })
            .collect()
    }

    async fn terminalize_incoherent_pending_request(
        &self,
        row: &AgentRequestRow,
        error: &anyhow::Error,
    ) {
        let failure_reason =
            format!("request rejected at ingest: incoherent durable lineage ({error})");
        if let Err(persist_error) = crate::request_admission::terminalize_pending_request_rejection(
            self.node.as_ref(),
            &row.doc_id,
            &self.agent_did,
            &failure_reason,
            "terminalize_incoherent_pending_request",
        )
        .await
        {
            tracing::error!(
                doc_id = %row.doc_id,
                request_id = %row.request_id,
                error = %persist_error,
                "failed to terminalize incoherent AgentRequest",
            );
            return;
        }
        tracing::warn!(
            doc_id = %row.doc_id,
            request_id = %row.request_id,
            %error,
            "terminalized incoherent AgentRequest at watcher ingest",
        );
    }

    async fn terminalize_malformed_pending_request(&self, row: &MalformedPendingRow) {
        if row.agent_did != self.agent_did || row.lifecycle_state.as_deref() != Some("pending") {
            return;
        }
        let reason = "request rejected at ingest: malformed durable AgentRequest row";
        if let Err(error) = crate::request_admission::terminalize_pending_request_rejection(
            self.node.as_ref(),
            &row.doc_id,
            &self.agent_did,
            reason,
            "terminalize_malformed_pending_request",
        )
        .await
        {
            tracing::error!(doc_id = %row.doc_id, error = %error,
                "failed to terminalize malformed AgentRequest");
        }
    }

    async fn row_is_claimable(&self, row: &AgentRequestRow) -> anyhow::Result<bool> {
        if !row.is_pending() {
            return Ok(false);
        }
        if row.has_preclaim_terminal_signal() {
            return Ok(true);
        }

        let session_id = crate::graphql::escape_graphql_string(&row.session_id);
        let active_runtime_states = RequestLifecycleState::active_runtime_graphql_list();
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{
                        session_id: {{ _eq: "{session_id}" }},
                        lifecycle_state: {{ _in: {active_runtime_states} }}
                    }},
                    order: [{{ created_at: ASC }}, {{ request_id: ASC }}]
                ) {{
                    _docID
                    request_id
                    lifecycle_state
                    created_at
                }}
            }}"#,
            session_id = session_id,
            active_runtime_states = active_runtime_states,
        );
        let resp = self.node.execute(&query).await;
        if resp.has_errors() {
            anyhow::bail!("watcher session queue query failed: {:?}", resp.errors);
        }

        let rows: Vec<SessionQueueRow> = resp
            .data
            .as_ref()
            .and_then(|d| d.get("AgentRequest"))
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let active_blocker = rows
            .iter()
            .any(|candidate| candidate.doc_id != row.doc_id && candidate.is_active_non_pending());
        if active_blocker {
            return Ok(false);
        }

        Ok(rows
            .iter()
            .find(|candidate| candidate.is_pending())
            .is_some_and(|candidate| candidate.doc_id == row.doc_id))
    }
}

#[cfg(test)]
fn active_runtime_rows(data: Option<&serde_json::Value>) -> anyhow::Result<Vec<AgentRequestRow>> {
    match data.and_then(|d| d.get("AgentRequest")) {
        Some(value) => Ok(serde_json::from_value(value.clone())?),
        None => Ok(Vec::new()),
    }
}

#[derive(Debug)]
struct MalformedPendingRow {
    doc_id: String,
    agent_did: String,
    lifecycle_state: Option<String>,
}

fn parse_active_runtime_rows(
    data: Option<&serde_json::Value>,
) -> anyhow::Result<(Vec<AgentRequestRow>, Vec<MalformedPendingRow>)> {
    let Some(value) = data.and_then(|data| data.get("AgentRequest")) else {
        return Ok((Vec::new(), Vec::new()));
    };
    let values = value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("AgentRequest query result is not an array"))?;
    let mut rows = Vec::with_capacity(values.len());
    let mut malformed = Vec::new();
    for value in values {
        match serde_json::from_value::<AgentRequestRow>(value.clone()) {
            Ok(row) => rows.push(row),
            Err(error) => {
                let string = |name: &str| value.get(name).and_then(serde_json::Value::as_str);
                if let (Some(doc_id), Some(agent_did)) = (string("_docID"), string("agent_did")) {
                    malformed.push(MalformedPendingRow {
                        doc_id: doc_id.to_string(),
                        agent_did: agent_did.to_string(),
                        lifecycle_state: string("lifecycle_state").map(str::to_string),
                    });
                } else {
                    tracing::warn!(%error, "unattributable malformed AgentRequest row quarantined");
                }
            }
        }
    }
    Ok((rows, malformed))
}

pub(crate) fn agent_request_from_mutation_response(
    response: &defra_node::QueryResponse,
    field: &str,
) -> anyhow::Result<Option<AgentRequest>> {
    crate::graphql::single_mutation_document(response, field)?
        .cloned()
        .map(serde_json::from_value::<AgentRequestRow>)
        .transpose()?
        .map(AgentRequestRow::into_agent_request)
        .transpose()
}

fn prioritize_aged_background_wakes(
    mut rows: Vec<AgentRequestRow>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<AgentRequestRow> {
    // The query is already FIFO. Stable partitioning preserves that order
    // within both classes while preventing an old completion wake from being
    // perpetually overtaken by ordinary work. Once selected, the wake has at
    // most the bounded executor queue and active workers ahead of it.
    rows.sort_by_key(|row| !row.is_aged_background_completion_wakeup(now));
    rows
}

fn claimable_pending_rows_from_rows(rows: Vec<AgentRequestRow>) -> Vec<AgentRequestRow> {
    // Quarantine malformed pending work without allowing a second claim next
    // to malformed live work in the same session.
    let blocked_sessions = rows
        .iter()
        .filter(|row| row.is_active_non_pending())
        .map(|row| row.session_id.clone())
        .collect::<HashSet<_>>();
    let rows = rows
        .into_iter()
        .filter_map(|row| match row.clone().into_agent_request() {
            Ok(_) => Some(row),
            Err(error) => {
                tracing::warn!(
                    doc_id = %row.doc_id,
                    request_id = %row.request_id,
                    %error,
                    "watcher quarantined incoherent AgentRequest row during pending scan",
                );
                None
            }
        })
        .collect::<Vec<_>>();
    let mut seen_pending_sessions = HashSet::new();
    let mut claimable = Vec::new();

    for row in rows {
        let is_pending = row.is_pending();
        let is_preclaim_terminal = row.has_preclaim_terminal_signal();
        let pending_session_seen = seen_pending_sessions.contains(&row.session_id);
        let session_blocked = blocked_sessions.contains(&row.session_id);

        if is_pending && (is_preclaim_terminal || (!session_blocked && !pending_session_seen)) {
            claimable.push(row.clone());
        }

        if is_pending {
            seen_pending_sessions.insert(row.session_id.clone());
        }
    }

    claimable
}

#[cfg(test)]
mod tests {
    use super::{
        active_runtime_rows, claimable_pending_rows_from_rows, prioritize_aged_background_wakes,
    };

    fn versioned_wake_metadata(session_id: &str) -> String {
        serde_json::json!({
            "queue": {
                "source": "background_completion",
                "policy": "coalesce",
                "key": format!("background_completion:{session_id}"),
                "queued_after_request_id": "parent"
            },
            "background_completion_wake_version": 1
        })
        .to_string()
    }

    fn pending_row(
        request_id: &str,
        session_id: &str,
        created_at: &str,
        metadata: Option<String>,
        execution_origin: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "_docID": format!("doc-{request_id}"),
            "request_id": request_id,
            "agent_did": "did:agent:1",
            "behavior_id": "default",
            "session_id": session_id,
            "content": "work",
            "metadata": metadata,
            "execution_origin": execution_origin,
            "created_at": created_at,
            "lifecycle_state": "pending"
        })
    }

    #[test]
    fn aged_completion_wake_moves_ahead_of_older_descendant() {
        let witness = crate::lean_vocab_test::lean_r6_backgrounding_case(
            "aged_background_wake_precedes_new_descendant",
        );
        assert!(witness.legal);
        assert_eq!(witness.reason.as_deref(), Some("aged_priority"));
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-12T22:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let data = serde_json::json!({
            "AgentRequest": [
                pending_row(
                    "older-descendant",
                    "descendant-session",
                    "2026-08-12T21:00:00Z",
                    None,
                    "interactive",
                ),
                pending_row(
                    "aged-wake",
                    "parent-session",
                    "2026-08-12T21:59:30Z",
                    Some(versioned_wake_metadata("parent-session")),
                    "scheduled",
                )
            ]
        });
        let rows = claimable_pending_rows_from_rows(active_runtime_rows(Some(&data)).unwrap());
        let ranked = prioritize_aged_background_wakes(rows, now);
        assert_eq!(ranked[0].request_id, "aged-wake");
        assert_eq!(ranked[1].request_id, "older-descendant");
    }

    #[test]
    fn fresh_completion_wake_preserves_fifo() {
        let witness = crate::lean_vocab_test::lean_r6_backgrounding_case(
            "fresh_background_wake_preserves_fifo",
        );
        assert!(!witness.legal);
        assert_eq!(witness.reason.as_deref(), Some("fifo"));
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-12T22:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let data = serde_json::json!({
            "AgentRequest": [
                pending_row(
                    "older-descendant",
                    "descendant-session",
                    "2026-08-12T21:59:00Z",
                    None,
                    "interactive",
                ),
                pending_row(
                    "fresh-wake",
                    "parent-session",
                    "2026-08-12T21:59:31Z",
                    Some(versioned_wake_metadata("parent-session")),
                    "scheduled",
                )
            ]
        });
        let rows = claimable_pending_rows_from_rows(active_runtime_rows(Some(&data)).unwrap());
        let ranked = prioritize_aged_background_wakes(rows, now);
        assert_eq!(ranked[0].request_id, "older-descendant");
        assert_eq!(ranked[1].request_id, "fresh-wake");
    }
}
