// Soft-cap justified: StreamWriter trait + its only production impl
// (DefraStreamWriter) are tightly coupled through shared DB mutation patterns.
// Splitting the impl from the trait would fragment what is functionally a
// single coherent unit.
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

use queries::{
    extract_mutation_doc_id, load_response_state, load_response_state_by_key,
    PersistedResponseState,
};

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

    fn write_reasoning(
        &self,
        doc_id: &str,
        reasoning: &str,
    ) -> impl std::future::Future<Output = Result<bool>> + Send;

    fn flush_pending(&self, doc_id: &str)
        -> impl std::future::Future<Output = Result<bool>> + Send;

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
    reasoning: String,
    token_count: usize,
    last_flush_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StreamBufferSnapshot {
    content: String,
    reasoning: String,
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

    async fn flush_snapshot(&self, doc_id: &str, snapshot: &StreamBufferSnapshot) -> Result<()> {
        tracing::debug!(
            doc_id = %doc_id,
            token_count = snapshot.token_count,
            content_len = snapshot.content.len(),
            reasoning_len = snapshot.reasoning.len(),
            "flushing streaming response snapshot"
        );
        let mutation = format!(
            r#"mutation {{
                update_AgentResponse(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        status: {{ _eq: "streaming" }}
                    }},
                    input: {{
                        content: "{content}",
                        reasoning: "{reasoning}",
                        token_count: {token_count}
                    }}
                ) {{ _docID }}
            }}"#,
            content = escape_graphql_string(&snapshot.content),
            reasoning = escape_graphql_string(&snapshot.reasoning),
            token_count = snapshot.token_count,
        );

        let resp =
            execute_mutation_with_retry(&self.node, &mutation, "flush_streaming_response_snapshot")
                .await?;

        if !resp
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentResponse"))
            .is_some_and(response_has_documents)
        {
            let current = load_response_state(&self.node, doc_id).await?;
            anyhow::bail!(
                "cannot write streaming state to AgentResponse {} because it is {}",
                doc_id,
                current
                    .as_ref()
                    .map(|response| response.status.as_str())
                    .unwrap_or("missing")
            );
        }

        Ok(())
    }

    async fn pending_snapshot(
        &self,
        doc_id: &str,
        force: bool,
    ) -> Result<Option<StreamBufferSnapshot>> {
        let mut buffers = self.buffers.lock().await;
        let buf = buffers
            .get_mut(doc_id)
            .ok_or_else(|| anyhow::anyhow!("no buffer for doc_id={}", doc_id))?;
        if !force && buf.last_flush_at.elapsed() < self.batch_interval {
            return Ok(None);
        }
        buf.last_flush_at = Instant::now();
        Ok(Some(StreamBufferSnapshot {
            content: buf.content.clone(),
            reasoning: buf.reasoning.clone(),
            token_count: buf.token_count,
        }))
    }

    /// Reset the live-tail buffer at a commit boundary.
    ///
    /// Clears the in-memory content/reasoning, leaves token_count cumulative
    /// (metering field), and persists empty content/reasoning on the
    /// streaming response row. progress_seq is not bumped here — it is
    /// owned by `RequestLifecycle::advance` and bumps at lifecycle
    /// boundaries (which are exactly the call sites that invoke
    /// reset_tail).
    pub async fn reset_tail(&self, doc_id: &str) -> Result<()> {
        tracing::debug!(
            doc_id = %doc_id,
            "resetting streaming response live tail"
        );
        {
            let mut buffers = self.buffers.lock().await;
            let buf = buffers
                .get_mut(doc_id)
                .ok_or_else(|| anyhow::anyhow!("no buffer for doc_id={}", doc_id))?;
            buf.content.clear();
            buf.reasoning.clear();
            buf.last_flush_at = Instant::now();
        }

        let mutation = format!(
            r#"mutation {{
                update_AgentResponse(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        status: {{ _eq: "streaming" }}
                    }},
                    input: {{
                        content: "",
                        reasoning: ""
                    }}
                ) {{ _docID }}
            }}"#
        );

        let resp =
            execute_mutation_with_retry(&self.node, &mutation, "reset_streaming_response_tail")
                .await?;

        if !resp
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentResponse"))
            .is_some_and(response_has_documents)
        {
            let current = load_response_state(&self.node, doc_id).await?;
            anyhow::bail!(
                "cannot reset tail of AgentResponse {} because it is {}",
                doc_id,
                current
                    .as_ref()
                    .map(|response| response.status.as_str())
                    .unwrap_or("missing")
            );
        }

        Ok(())
    }

    pub async fn set_error_message(&self, doc_id: &str, error_message: &str) -> Result<()> {
        let escaped_error_message = escape_graphql_string(error_message);
        let mutation = format!(
            r#"mutation {{
                update_AgentResponse(
                    filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                    input: {{ error_message: "{escaped_error_message}" }}
                ) {{ _docID }}
            }}"#
        );

        execute_mutation_with_retry(&self.node, &mutation, "set_streaming_response_error")
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "updating AgentResponse error_message for doc_id={doc_id}: {error:#}"
                )
            })?;

        Ok(())
    }

    pub async fn finalize_existing_request_error(
        &self,
        request_id: &str,
        error_message: &str,
    ) -> Result<bool> {
        let Some(existing) = load_response_state_by_key(&self.node, request_id).await? else {
            return Ok(false);
        };

        if existing.status == StreamStatus::Error.as_str()
            || existing.status == StreamStatus::Complete.as_str()
        {
            return Ok(true);
        }

        self.set_error_message(&existing.doc_id, error_message)
            .await?;
        self.finalize(&existing.doc_id, StreamStatus::Error).await?;
        Ok(true)
    }

    /// Mark an existing response row as interrupted. Writes `interrupted_at`
    /// to the doc; does NOT change `status`. Called by the daemon's interrupt
    /// flow, sequenced BEFORE the terminal `AgentRequest.lifecycle_state` write.
    pub async fn write_interrupted_at(&self, doc_id: &str, at: &str) -> Result<bool> {
        let escaped_doc_id = escape_graphql_string(doc_id);
        let escaped_at = escape_graphql_string(at);
        let mutation = format!(
            r#"mutation {{
                update_AgentResponse(
                    filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                    input: {{ interrupted_at: "{escaped_at}" }}
                ) {{ _docID }}
            }}"#
        );
        let resp =
            execute_mutation_with_retry(&self.node, &mutation, "write_interrupted_at").await?;
        Ok(resp
            .data
            .as_ref()
            .and_then(|d| d.get("update_AgentResponse"))
            .is_some_and(response_has_documents))
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
                    reasoning: "",
                    status: "streaming",
                    error_message: "",
                    token_count: 0,
                    progress_seq: 0,
                    created_at: "{now}",
                    completed_at: ""
                }}) {{ _docID }}
            }}"#,
            agent_did = self.agent_did,
        );

        let resp = execute_mutation_with_retry(&self.node, &mutation, "begin_streaming_response")
            .await
            .map_err(|error| anyhow::anyhow!("creating AgentResponse failed: {error:#}"))?;

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
                reasoning: String::new(),
                token_count: 0,
                last_flush_at: Instant::now(),
            },
        );

        tracing::info!(
            doc_id = %doc_id,
            request_id = %request_id,
            session_id = %session_id,
            behavior_id = %behavior_id,
            "started streaming response"
        );

        Ok(doc_id)
    }

    async fn write_tokens(&self, doc_id: &str, tokens: &str) -> Result<bool> {
        {
            let mut buffers = self.buffers.lock().await;
            let buf = buffers
                .get_mut(doc_id)
                .ok_or_else(|| anyhow::anyhow!("no buffer for doc_id={}", doc_id))?;
            buf.content.push_str(tokens);
            buf.token_count += tokens.split_whitespace().count();
        }

        let snapshot = self.pending_snapshot(doc_id, false).await?;

        let Some(snapshot) = snapshot else {
            return Ok(false);
        };

        self.flush_snapshot(doc_id, &snapshot).await?;
        Ok(true)
    }

    async fn write_reasoning(&self, doc_id: &str, reasoning: &str) -> Result<bool> {
        {
            let mut buffers = self.buffers.lock().await;
            let buf = buffers
                .get_mut(doc_id)
                .ok_or_else(|| anyhow::anyhow!("no buffer for doc_id={}", doc_id))?;
            buf.reasoning.push_str(reasoning);
        }

        let snapshot = self.pending_snapshot(doc_id, false).await?;

        let Some(snapshot) = snapshot else {
            return Ok(false);
        };

        self.flush_snapshot(doc_id, &snapshot).await?;
        Ok(true)
    }

    async fn flush_pending(&self, doc_id: &str) -> Result<bool> {
        let snapshot = self.pending_snapshot(doc_id, true).await?;
        let Some(snapshot) = snapshot else {
            return Ok(false);
        };
        self.flush_snapshot(doc_id, &snapshot).await?;
        Ok(true)
    }

    async fn finalize(&self, doc_id: &str, status: StreamStatus) -> Result<StreamResult> {
        let existing = load_response_state(&self.node, doc_id).await?;
        let snapshot = {
            let buffers = self.buffers.lock().await;
            buffers.get(doc_id).map(|buf| StreamBufferSnapshot {
                content: buf.content.clone(),
                reasoning: buf.reasoning.clone(),
                token_count: buf.token_count,
            })
        };
        let now = chrono::Utc::now().to_rfc3339();
        let mutation =
            build_finalize_mutation(existing.as_ref(), doc_id, &status, &now, snapshot.as_ref());
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
                        lost_reasoning_len = snapshot.reasoning.len(),
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
    existing: Option<&PersistedResponseState>,
    doc_id: &str,
    status: &StreamStatus,
    now: &str,
    snapshot: Option<&StreamBufferSnapshot>,
) -> String {
    let request_transition = existing
        .map(|existing| build_request_terminal_update(&existing.request_id, status))
        .unwrap_or_default();
    // content / reasoning are always cleared on finalize because they
    // represent the live tail (issue #64). token_count is preserved as a
    // cumulative metering field — only updated when the in-memory buffer
    // is present (the snapshot path); on the crash-recovery path
    // (`snapshot = None`) the previously-flushed token_count is left
    // untouched.
    match snapshot {
        Some(snapshot) => format!(
            r#"mutation {{
                update_AgentResponse(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        status: {{ _eq: "streaming" }}
                    }},
                    input: {{
                        content: "",
                        reasoning: "",
                        status: "{status}",
                        token_count: {token_count},
                        completed_at: "{now}"
                    }}
                ) {{ _docID }}
                {request_transition}
            }}"#,
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
                        content: "",
                        reasoning: "",
                        status: "{status}",
                        completed_at: "{now}"
                    }}
                ) {{ _docID }}
                {request_transition}
            }}"#,
            status = status.as_str(),
        ),
    }
}

fn build_request_terminal_update(request_id: &str, status: &StreamStatus) -> String {
    let (request_status, lifecycle_state) = match status {
        StreamStatus::Complete => ("completed", "completed"),
        StreamStatus::Error => ("error", "failed"),
        StreamStatus::Streaming => return String::new(),
    };
    let escaped_request_id = escape_graphql_string(request_id);
    format!(
        r#"update_AgentRequest(
                    filter: {{
                        request_id: {{ _eq: "{escaped_request_id}" }},
                        status: {{ _eq: "processing" }}
                    }},
                    input: {{
                        status: "{request_status}",
                        lifecycle_state: "{lifecycle_state}"
                    }}
                ) {{ _docID }}"#
    )
}
