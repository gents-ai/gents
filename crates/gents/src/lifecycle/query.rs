use super::lookup::lookup_response_status_by_request_id;
use super::rows::{DedupPlan, DedupRow, RequestViewRow, StatusRow};
use super::*;
use gents_protocol::request_lifecycle::RequestLifecycleState;

impl RequestLifecycle {
    pub(super) async fn check_deduplication(&self) -> Result<DedupPlan> {
        let escaped_session_id = escape_graphql_string(&self.request.session_id);
        let active_runtime_states = RequestLifecycleState::active_runtime_graphql_list();
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{
                        session_id: {{ _eq: "{escaped_session_id}" }},
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

        let rows: Vec<DedupRow> = resp
            .data
            .as_ref()
            .and_then(|d| d.get("AgentRequest"))
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let active_blocker = rows
            .iter()
            .find(|row| row.doc_id != self.request.doc_id && row.is_active_non_pending());
        let first_pending = rows.iter().find(|row| row.is_pending());
        let is_earliest = active_blocker.is_none()
            && first_pending.is_some_and(|row| row.doc_id == self.request.doc_id);
        let blocking_request_id = active_blocker
            .or_else(|| {
                first_pending.and_then(|row| (row.doc_id != self.request.doc_id).then_some(row))
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

    pub(super) async fn request_view(&self) -> Result<Option<RequestViewRow>> {
        let doc_id = &self.request.doc_id;
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                    limit: 1
                ) {{
                    lifecycle_state
                    backend_id
                    execution_origin
                }}
            }}"#,
        );

        let resp = self.node.execute(&query).await;
        if resp.has_errors() {
            anyhow::bail!("request status query failed: {:?}", resp.errors);
        }

        let rows: Vec<RequestViewRow> = resp
            .data
            .as_ref()
            .and_then(|d| d.get("AgentRequest"))
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        Ok(rows.into_iter().next())
    }

    pub(super) async fn response_status(&self) -> Result<Option<String>> {
        if let Some(doc_id) = self.response_doc_id.as_deref() {
            let query = format!(
                r#"{{
                    AgentResponse(
                        filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                        limit: 1
                    ) {{
                        status
                    }}
                }}"#
            );

            let resp = self.node.execute(&query).await;
            if resp.has_errors() {
                anyhow::bail!(
                    "response status query failed for doc_id={doc_id}: {:?}",
                    resp.errors
                );
            }

            let rows: Vec<StatusRow> = resp
                .data
                .as_ref()
                .and_then(|d| d.get("AgentResponse"))
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            if let Some(row) = rows.into_iter().next() {
                return Ok(Some(row.status));
            }
        }

        lookup_response_status_by_request_id(&self.node, &self.agent_did, &self.request.request_id)
            .await
    }

    pub async fn response_exists(&self) -> Result<bool> {
        Ok(self.response_status().await?.is_some())
    }
}
