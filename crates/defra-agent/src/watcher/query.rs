use serde::Deserialize;

use super::{validate_agent_request_subagent_coherence, AgentRequest, DefraWatcher};

// Keep this projection aligned with `AgentRequest`; downstream intake needs
// metadata so it can inspect lifecycle queue hints.
const AGENT_REQUEST_FIELDS: &str = r#"
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
                    deadline
                    subagent_depth
                    caused_by_parent_request_id
                    caused_by_parent_tool_call_id
"#;

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
                ) {{{fields}
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

        agent_request_rows(resp.data.as_ref())?
            .into_iter()
            .next()
            .map(AgentRequestRow::into_agent_request)
            .transpose()
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
                ) {{{fields}
                }}
            }}"#,
            agent_did = self.agent_did,
            fields = AGENT_REQUEST_FIELDS,
        );

        let resp = self.node.execute(&query).await;
        if resp.has_errors() {
            anyhow::bail!("watcher pending-request query failed: {:?}", resp.errors);
        }

        agent_request_rows(resp.data.as_ref())?
            .into_iter()
            .map(AgentRequestRow::into_agent_request)
            .collect()
    }
}

fn agent_request_rows(data: Option<&serde_json::Value>) -> anyhow::Result<Vec<AgentRequestRow>> {
    match data.and_then(|d| d.get("AgentRequest")) {
        Some(value) => Ok(serde_json::from_value(value.clone())?),
        None => Ok(Vec::new()),
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
    deadline: Option<String>,
    subagent_depth: Option<u32>,
    caused_by_parent_request_id: Option<String>,
    caused_by_parent_tool_call_id: Option<String>,
}

impl AgentRequestRow {
    fn into_agent_request(self) -> anyhow::Result<AgentRequest> {
        let req = AgentRequest {
            doc_id: self.doc_id,
            request_id: self.request_id,
            agent_did: self.agent_did,
            behavior_id: normalize_optional_string(self.behavior_id),
            session_id: self.session_id,
            content: self.content,
            temperature: self.temperature,
            top_p: self.top_p,
            top_k: self.top_k,
            max_tokens: self.max_tokens,
            metadata: self.metadata,
            execution_origin: normalize_optional_string(self.execution_origin),
            created_at: self.created_at,
            deadline: normalize_optional_string(self.deadline),
            subagent_depth: self.subagent_depth.unwrap_or(0),
            caused_by_parent_request_id: self.caused_by_parent_request_id,
            caused_by_parent_tool_call_id: self.caused_by_parent_tool_call_id,
        };
        validate_agent_request_subagent_coherence(&req)?;
        Ok(req)
    }
}
