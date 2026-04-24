use serde::Deserialize;

use super::{AgentRequest, DefraWatcher};

impl DefraWatcher {
    pub async fn try_fetch_request(&self, doc_id: &str) -> anyhow::Result<Option<AgentRequest>> {
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        agent_did: {{ _eq: "{agent_did}" }},
                        status: {{ _eq: "pending" }}
                    }}
                ) {{
                    request_id
                    agent_did
                    behavior_id
                    session_id
                    content
                    temperature
                    top_p
                    top_k
                    max_tokens
                    metadata
                    execution_origin
                    created_at
                }}
            }}"#,
            doc_id = doc_id,
            agent_did = self.agent_did,
        );

        let resp = self.node.execute(&query).await;
        if resp.has_errors() {
            anyhow::bail!("watcher query failed: {:?}", resp.errors);
        }

        let docs: Vec<AgentRequestRow> =
            match resp.data.as_ref().and_then(|d| d.get("AgentRequest")) {
                Some(value) => serde_json::from_value(value.clone())?,
                None => Vec::new(),
            };

        match docs.into_iter().next() {
            Some(row) => Ok(Some(AgentRequest {
                doc_id: doc_id.to_string(),
                request_id: row.request_id,
                agent_did: row.agent_did,
                behavior_id: normalize_optional_string(row.behavior_id),
                session_id: row.session_id,
                content: row.content,
                temperature: row.temperature,
                top_p: row.top_p,
                top_k: row.top_k,
                max_tokens: row.max_tokens,
                metadata: row.metadata,
                execution_origin: normalize_optional_string(row.execution_origin),
                created_at: row.created_at,
            })),
            None => Ok(None),
        }
    }

    pub(super) async fn pending_requests(&self) -> anyhow::Result<Vec<AgentRequest>> {
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{
                        agent_did: {{ _eq: "{agent_did}" }},
                        status: {{ _eq: "pending" }}
                    }},
                    order: {{ created_at: ASC }}
                ) {{
                    _docID
                    request_id
                    agent_did
                    behavior_id
                    session_id
                    content
                    temperature
                    top_p
                    top_k
                    max_tokens
                    metadata
                    execution_origin
                    created_at
                }}
            }}"#,
            agent_did = self.agent_did,
        );

        let resp = self.node.execute(&query).await;
        if resp.has_errors() {
            anyhow::bail!("watcher pending-request query failed: {:?}", resp.errors);
        }

        let docs: Vec<PendingAgentRequestRow> =
            match resp.data.as_ref().and_then(|d| d.get("AgentRequest")) {
                Some(value) => serde_json::from_value(value.clone())?,
                None => Vec::new(),
            };

        Ok(docs
            .into_iter()
            .map(|row| AgentRequest {
                doc_id: row.doc_id,
                request_id: row.request_id,
                agent_did: row.agent_did,
                behavior_id: normalize_optional_string(row.behavior_id),
                session_id: row.session_id,
                content: row.content,
                temperature: row.temperature,
                top_p: row.top_p,
                top_k: row.top_k,
                max_tokens: row.max_tokens,
                metadata: row.metadata,
                execution_origin: normalize_optional_string(row.execution_origin),
                created_at: row.created_at,
            })
            .collect())
    }
}

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

#[derive(Deserialize)]
struct AgentRequestRow {
    request_id: String,
    agent_did: String,
    behavior_id: Option<String>,
    session_id: String,
    content: String,
    temperature: Option<f64>,
    top_p: Option<f64>,
    top_k: Option<i64>,
    max_tokens: Option<i64>,
    metadata: Option<String>,
    execution_origin: Option<String>,
    created_at: String,
}

#[derive(Deserialize)]
struct PendingAgentRequestRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_id: String,
    agent_did: String,
    behavior_id: Option<String>,
    session_id: String,
    content: String,
    temperature: Option<f64>,
    top_p: Option<f64>,
    top_k: Option<i64>,
    max_tokens: Option<i64>,
    metadata: Option<String>,
    execution_origin: Option<String>,
    created_at: String,
}
