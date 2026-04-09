use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use defra_node::EmbeddedNode;
use tokio::sync::Mutex;

use crate::graphql::{escape_graphql_string, response_has_documents};
use crate::session::execute_mutation_with_retry;

mod queries;
#[cfg(test)]
mod tests;

use queries::{extract_mutation_doc_id, load_response_state, load_response_state_by_key};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamStatus {
    Streaming,
    Complete,
    Error,
}

impl StreamStatus {
    fn as_str(&self) -> &str {
        match self {
            StreamStatus::Streaming => "streaming",
            StreamStatus::Complete => "complete",
            StreamStatus::Error => "error",
        }
    }
}

#[derive(Debug, Clone)]
pub struct StreamResult {
    pub doc_id: String,
    pub content: String,
    pub status: StreamStatus,
    pub token_count: usize,
}

pub trait StreamWriter: Send + Sync {
    fn begin(
        &self,
        session_id: &str,
        request_id: &str,
        behavior_id: &str,
    ) -> impl std::future::Future<Output = Result<String>> + Send;

    fn write_tokens(
        &self,
        doc_id: &str,
        tokens: &str,
    ) -> impl std::future::Future<Output = Result<bool>> + Send;

    fn finalize(
        &self,
        doc_id: &str,
        status: StreamStatus,
    ) -> impl std::future::Future<Output = Result<StreamResult>> + Send;
}

pub struct DefraStreamWriter {
    node: Arc<EmbeddedNode>,
    agent_did: String,
    batch_interval: Duration,
    buffers: Mutex<HashMap<String, StreamBuffer>>,
}

struct StreamBuffer {
    content: String,
    token_count: usize,
    last_flush_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StreamBufferSnapshot {
    content: String,
    token_count: usize,
}

impl DefraStreamWriter {
    pub fn new(node: Arc<EmbeddedNode>, agent_did: &str, batch_interval: Duration) -> Self {
        Self {
            node,
            agent_did: agent_did.to_string(),
            batch_interval,
            buffers: Mutex::new(HashMap::new()),
        }
    }
}

impl StreamWriter for DefraStreamWriter {
    async fn begin(&self, session_id: &str, request_id: &str, behavior_id: &str) -> Result<String> {
        if let Some(existing) = load_response_state_by_key(&self.node, request_id).await? {
            anyhow::bail!(
                "refusing to begin response for request_id={} because AgentResponse {} already exists with status={}",
                request_id,
                existing.doc_id,
                existing.status
            );
        }

        let now = chrono::Utc::now().to_rfc3339();
        let response_key = request_id.to_string();
        let escaped_behavior_id = escape_graphql_string(behavior_id);
        let mutation = format!(
            r#"mutation {{
                create_AgentResponse(input: {{
                    response_key: "{response_key}",
                    request_id: "{request_id}",
                    agent_did: "{agent_did}",
                    behavior_id: "{escaped_behavior_id}",
                    session_id: "{session_id}",
                    content: "",
                    status: "streaming",
                    token_count: 0,
                    progress_seq: 0,
                    created_at: "{now}",
                    completed_at: ""
                }}) {{ _docID }}
            }}"#,
            agent_did = self.agent_did,
        );

        let resp = self.node.execute(&mutation).await;
        if resp.has_errors() {
            anyhow::bail!("creating AgentResponse failed: {:?}", resp.errors);
        }

        let doc_id = resp
            .data
            .as_ref()
            .and_then(|data| extract_mutation_doc_id(data, "AgentResponse"))
            .ok_or_else(|| anyhow::anyhow!("create_AgentResponse returned no _docID"))?
            .to_string();

        self.buffers.lock().await.insert(
            doc_id.clone(),
            StreamBuffer {
                content: String::new(),
                token_count: 0,
                last_flush_at: Instant::now(),
            },
        );

        tracing::debug!(
            doc_id = %doc_id,
            session_id = %session_id,
            "started streaming response"
        );

        Ok(doc_id)
    }

    async fn write_tokens(&self, doc_id: &str, tokens: &str) -> Result<bool> {
        let snapshot = {
            let mut buffers = self.buffers.lock().await;
            let buf = buffers
                .get_mut(doc_id)
                .ok_or_else(|| anyhow::anyhow!("no buffer for doc_id={}", doc_id))?;
            buf.content.push_str(tokens);
            buf.token_count += tokens.split_whitespace().count();
            if buf.last_flush_at.elapsed() < self.batch_interval {
                None
            } else {
                buf.last_flush_at = Instant::now();
                Some((buf.content.clone(), buf.token_count))
            }
        };

        let Some((content, token_count)) = snapshot else {
            return Ok(false);
        };

        let mutation = format!(
            r#"mutation {{
                update_AgentResponse(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        status: {{ _eq: "streaming" }}
                    }},
                    input: {{
                        content: "{content}",
                        token_count: {token_count}
                    }}
                ) {{ _docID }}
            }}"#,
            content = escape_graphql_string(&content),
        );

        let resp = self.node.execute(&mutation).await;
        if resp.has_errors() {
            anyhow::bail!("updating AgentResponse failed: {:?}", resp.errors);
        }

        if !resp
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentResponse"))
            .is_some_and(response_has_documents)
        {
            let current = load_response_state(&self.node, doc_id).await?;
            anyhow::bail!(
                "cannot write tokens to AgentResponse {} because it is {}",
                doc_id,
                current
                    .as_ref()
                    .map(|response| response.status.as_str())
                    .unwrap_or("missing")
            );
        }

        Ok(true)
    }

    async fn finalize(&self, doc_id: &str, status: StreamStatus) -> Result<StreamResult> {
        let snapshot = {
            let buffers = self.buffers.lock().await;
            buffers.get(doc_id).map(|buf| StreamBufferSnapshot {
                content: buf.content.clone(),
                token_count: buf.token_count,
            })
        };
        let now = chrono::Utc::now().to_rfc3339();
        let mutation = build_finalize_mutation(doc_id, &status, &now, snapshot.as_ref());
        let operation = if snapshot.is_some() {
            "finalize_streaming_response"
        } else {
            "finalize_streaming_response_without_buffer"
        };

        let resp = match execute_mutation_with_retry(&self.node, &mutation, operation).await {
            Ok(resp) => resp,
            Err(error) => {
                if let Some(snapshot) = snapshot.as_ref() {
                    tracing::error!(
                        doc_id = %doc_id,
                        status = %status.as_str(),
                        token_count = snapshot.token_count,
                        lost_content_len = snapshot.content.len(),
                        error = %error,
                        "failed to finalize streaming response after retries; leaving buffer in place for crash-recovery"
                    );
                } else {
                    tracing::error!(
                        doc_id = %doc_id,
                        status = %status.as_str(),
                        error = %error,
                        "failed to finalize streaming response without in-memory buffer"
                    );
                }
                return Err(error);
            }
        };

        let persisted = if resp
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentResponse"))
            .is_some_and(response_has_documents)
        {
            load_response_state(&self.node, doc_id).await?
        } else {
            match load_response_state(&self.node, doc_id).await? {
                Some(existing) if existing.status == status.as_str() => {
                    tracing::warn!(
                        doc_id = %doc_id,
                        status = %status.as_str(),
                        "finalize became an idempotent no-op because response was already terminal"
                    );
                    Some(existing)
                }
                Some(existing) => {
                    anyhow::bail!(
                        "cannot finalize AgentResponse {} as {} because it is already {}",
                        doc_id,
                        status.as_str(),
                        existing.status
                    );
                }
                None => anyhow::bail!(
                    "cannot finalize AgentResponse {} as {} because the response document is missing",
                    doc_id,
                    status.as_str()
                ),
            }
        };

        self.buffers.lock().await.remove(doc_id);

        let content = persisted
            .as_ref()
            .map(|response| response.content.clone())
            .or_else(|| snapshot.as_ref().map(|snapshot| snapshot.content.clone()))
            .unwrap_or_default();
        let token_count = persisted
            .as_ref()
            .map(|response| response.token_count)
            .or_else(|| snapshot.as_ref().map(|snapshot| snapshot.token_count))
            .unwrap_or_default();

        tracing::info!(
            doc_id = %doc_id,
            status = %status.as_str(),
            tokens = token_count,
            "finalized streaming response"
        );

        Ok(StreamResult {
            doc_id: doc_id.to_string(),
            content,
            status,
            token_count,
        })
    }
}

fn build_finalize_mutation(
    doc_id: &str,
    status: &StreamStatus,
    now: &str,
    snapshot: Option<&StreamBufferSnapshot>,
) -> String {
    match snapshot {
        Some(snapshot) => format!(
            r#"mutation {{
                update_AgentResponse(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        status: {{ _eq: "streaming" }}
                    }},
                    input: {{
                        content: "{content}",
                        status: "{status}",
                        token_count: {token_count},
                        completed_at: "{now}"
                    }}
                ) {{ _docID }}
            }}"#,
            content = escape_graphql_string(&snapshot.content),
            status = status.as_str(),
            token_count = snapshot.token_count,
        ),
        None => format!(
            r#"mutation {{
                update_AgentResponse(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        status: {{ _eq: "streaming" }}
                    }},
                    input: {{
                        status: "{status}",
                        completed_at: "{now}"
                    }}
                ) {{ _docID }}
            }}"#,
            status = status.as_str(),
        ),
    }
}
