use super::rows::DedupPlan;
use super::*;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

impl RequestLifecycle {
    pub(super) async fn check_deduplication(&self) -> Result<DedupPlan> {
        let escaped_session_id = escape_graphql_string(&self.request.session_id);
        let active_runtime_states = RequestLifecycleState::active_runtime_graphql_list();
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{
                        session_id: {{ _eq: "{escaped_session_id}" }},
                        purpose: {{ _eq: "normal" }},
                        lifecycle_state: {{ _in: {active_runtime_states} }}
                    }},
                    order: [{{ created_at: ASC }}, {{ request_id: ASC }}]
                ) {{
                    _docID
                    request_id
                    lifecycle_state
                    created_at
                }}
            }}"#
        );

        let resp = self.node.execute(&query).await;
        if resp.has_errors() {
            anyhow::bail!("deduplication check failed: {:?}", resp.errors);
        }

        let rows: Vec<AgentRequestRow> = crate::graphql::rows(&resp, "AgentRequest")?;

        let active_blocker = rows.iter().find(|row| {
            row.doc_id.as_deref() != Some(self.request.doc_id.as_str())
                && row.lifecycle_state != Some(RequestLifecycleState::Pending)
        });
        let first_pending = rows
            .iter()
            .find(|row| row.lifecycle_state == Some(RequestLifecycleState::Pending));
        let is_earliest = active_blocker.is_none()
            && first_pending
                .is_some_and(|row| row.doc_id.as_deref() == Some(self.request.doc_id.as_str()));
        let blocking_request_id = active_blocker
            .or_else(|| {
                first_pending.and_then(|row| {
                    (row.doc_id.as_deref() != Some(self.request.doc_id.as_str())).then_some(row)
                })
            })
            .map(|row| row.request_id.clone());

        if rows.len() > 1 {
            tracing::info!(
                request_id = %self.request.request_id,
                session_id = %self.request.session_id,
                is_earliest,
                same_session_runtime_count = rows.len(),
                blocking_request_id = blocking_request_id.as_deref().unwrap_or(""),
                "same-session request queue check found pending or active requests"
            );
        }

        Ok(DedupPlan {
            is_earliest,
            blocking_request_id,
        })
    }

    pub(super) async fn request_view(&self) -> Result<Option<AgentRequestRow>> {
        let doc_id = escape_graphql_string(&self.request.doc_id);
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                    limit: 1
                ) {{
                    request_id
                    lifecycle_state
                    backend_id
                    execution_generation
                    execution_lease_expires_at
                    execution_origin
                    failure_reason
                    terminal_output
                }}
            }}"#,
        );

        let resp = self.node.execute(&query).await;
        if resp.has_errors() {
            anyhow::bail!("request status query failed: {:?}", resp.errors);
        }

        let rows: Vec<AgentRequestRow> = crate::graphql::rows(&resp, "AgentRequest")?;

        Ok(rows.into_iter().next())
    }
}
