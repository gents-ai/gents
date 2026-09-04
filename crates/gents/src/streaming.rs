use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use anyhow::Result;
use defra_node::EmbeddedNode;
use tokio::sync::Mutex;

use crate::graphql::{escape_graphql_string, response_has_documents};
use crate::lifecycle::{ExecutionWriteFence, ExecutionWriteKind};

mod queries;
#[cfg(test)]
mod tests;

use queries::{extract_mutation_doc_id, load_response_state, load_response_state_by_key};

const MAX_LIVE_REASONING_BYTES: usize = 64 * 1024;

type ResponseWriteGate = Mutex<()>;

/// Response operations include read-before-write checks that must remain
/// ordered across behavior daemons. The actual mutations also pass through
/// the runtime-wide mutation gate in `graphql`.
fn response_write_gate(node: &Arc<EmbeddedNode>) -> Arc<ResponseWriteGate> {
    static GATES: OnceLock<StdMutex<HashMap<usize, Weak<ResponseWriteGate>>>> = OnceLock::new();

    let node_key = Arc::as_ptr(node) as usize;
    let mut gates = GATES
        .get_or_init(|| StdMutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(gate) = gates.get(&node_key).and_then(Weak::upgrade) {
        return gate;
    }

    gates.retain(|_, gate| gate.strong_count() > 0);
    let gate = Arc::new(Mutex::new(()));
    gates.insert(node_key, Arc::downgrade(&gate));
    gate
}

pub trait StreamWriter: Send + Sync {
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
}

pub struct DefraStreamWriter {
    node: Arc<EmbeddedNode>,
    agent_did: String,
    batch_interval: Duration,
    buffers: Mutex<HashMap<String, StreamBuffer>>,
    response_write_gate: Arc<ResponseWriteGate>,
}

struct StreamBuffer {
    content: String,
    reasoning: String,
    token_count: usize,
    reasoning_progress_seq: usize,
    last_flush_at: Instant,
    persisted: StreamBufferSnapshot,
    lease: ExecutionWriteFence,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct StreamBufferSnapshot {
    content: String,
    reasoning: String,
    token_count: usize,
    reasoning_progress_seq: usize,
}

impl DefraStreamWriter {
    pub fn new(node: Arc<EmbeddedNode>, agent_did: &str, batch_interval: Duration) -> Self {
        let response_write_gate = response_write_gate(&node);
        Self {
            node,
            agent_did: agent_did.to_string(),
            batch_interval,
            buffers: Mutex::new(HashMap::new()),
            response_write_gate,
        }
    }

    pub(crate) async fn begin_owned_response(
        &self,
        session_id: &str,
        request_id: &str,
        request_doc_id: &str,
        behavior_id: &str,
        requester_did: Option<&str>,
        execution_generation: &str,
        lease_duration_secs: u64,
    ) -> Result<String> {
        self.begin_inner(
            session_id,
            request_id,
            request_doc_id,
            behavior_id,
            requester_did,
            ExecutionWriteFence {
                request_doc_id: request_doc_id.to_string(),
                execution_generation: execution_generation.to_string(),
                lease_duration_secs,
            },
        )
        .await
    }

    pub(crate) async fn discard_buffer(&self, doc_id: &str) {
        self.buffers.lock().await.remove(doc_id);
    }

    async fn flush_snapshot(&self, doc_id: &str, _snapshot: &StreamBufferSnapshot) -> Result<bool> {
        let _write_guard = self.response_write_gate.lock().await;
        // Re-read under the write gate: a queued flush cannot replay an old
        // snapshot or charge progress for another flush's identical write.
        let Some(snapshot) = self.pending_snapshot(doc_id, true).await? else {
            return Ok(false);
        };
        tracing::debug!(
            doc_id = %doc_id,
            token_count = snapshot.token_count,
            content_len = snapshot.content.len(),
            reasoning_len = snapshot.reasoning.len(),
            "flushing streaming response snapshot"
        );
        let escaped_doc_id = escape_graphql_string(doc_id);
        let mutation = format!(
            r#"mutation {{
                update_AgentResponse(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        status: {{ _eq: "streaming" }}
                    }},
                    input: {{
                        content: "{content}",
                        reasoning: "{reasoning}",
                        token_count: {token_count},
                        reasoning_progress_seq: {reasoning_progress_seq}
                    }}
                ) {{ _docID }}
            }}"#,
            content = escape_graphql_string(&snapshot.content),
            reasoning = escape_graphql_string(&snapshot.reasoning),
            token_count = snapshot.token_count,
            reasoning_progress_seq = snapshot.reasoning_progress_seq,
        );

        let lease = self
            .buffers
            .lock()
            .await
            .get(doc_id)
            .map(|buffer| buffer.lease.clone())
            .ok_or_else(|| anyhow::anyhow!("no buffer for doc_id={doc_id}"))?;
        let resp = lease
            .execute_response_write(&self.node, &mutation, ExecutionWriteKind::Progress)
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

        if let Some(buffer) = self.buffers.lock().await.get_mut(doc_id) {
            buffer.persisted = snapshot.clone();
            buffer.last_flush_at = Instant::now();
        }
        Ok(true)
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
        let snapshot = StreamBufferSnapshot {
            content: buf.content.clone(),
            reasoning: buf.reasoning.clone(),
            token_count: buf.token_count,
            reasoning_progress_seq: buf.reasoning_progress_seq,
        };
        Ok((snapshot != buf.persisted).then_some(snapshot))
    }

    pub async fn reset_tail(&self, doc_id: &str) -> Result<()> {
        let _write_guard = self.response_write_gate.lock().await;
        tracing::debug!(
            doc_id = %doc_id,
            "resetting streaming response live tail"
        );
        let mut buffers = self.buffers.lock().await;
        let buf = buffers
            .get_mut(doc_id)
            .ok_or_else(|| anyhow::anyhow!("no buffer for doc_id={}", doc_id))?;
        if buf.content.is_empty()
            && buf.reasoning.is_empty()
            && buf.persisted.content.is_empty()
            && buf.persisted.reasoning.is_empty()
        {
            return Ok(());
        }
        let lease = buf.lease.clone();

        let escaped_doc_id = escape_graphql_string(doc_id);
        let mutation = format!(
            r#"mutation {{
                update_AgentResponse(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        status: {{ _eq: "streaming" }}
                    }},
                    input: {{
                        content: "",
                        reasoning: "",
                        token_count: {token_count},
                        reasoning_progress_seq: {reasoning_progress_seq}
                    }}
                ) {{ _docID }}
            }}"#,
            token_count = buf.token_count,
            reasoning_progress_seq = buf.reasoning_progress_seq,
        );

        let resp = lease
            .execute_response_write(&self.node, &mutation, ExecutionWriteKind::Observe)
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

        buf.content.clear();
        buf.reasoning.clear();
        buf.persisted = StreamBufferSnapshot {
            content: String::new(),
            reasoning: String::new(),
            token_count: buf.token_count,
            reasoning_progress_seq: buf.reasoning_progress_seq,
        };
        buf.last_flush_at = Instant::now();
        Ok(())
    }

    pub async fn set_error_message(&self, doc_id: &str, error_message: &str) -> Result<()> {
        let _write_guard = self.response_write_gate.lock().await;
        let escaped_doc_id = escape_graphql_string(doc_id);
        let escaped_error_message = escape_graphql_string(error_message);
        let mutation = format!(
            r#"mutation {{
                update_AgentResponse(
                    filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                    input: {{ error_message: "{escaped_error_message}" }}
                ) {{ _docID }}
            }}"#
        );

        let lease = self
            .buffers
            .lock()
            .await
            .get(doc_id)
            .map(|buffer| buffer.lease.clone())
            .ok_or_else(|| anyhow::anyhow!("no buffer for doc_id={doc_id}"))?;
        lease
            .execute_response_write(&self.node, &mutation, ExecutionWriteKind::Observe)
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "updating AgentResponse error_message for doc_id={doc_id}: {error:#}"
                )
            })?;

        Ok(())
    }

    pub async fn write_interrupted_at(&self, doc_id: &str, at: &str) -> Result<bool> {
        let _write_guard = self.response_write_gate.lock().await;
        let Some(current) = load_response_state(&self.node, doc_id).await? else {
            return Ok(false);
        };
        if current.status != "streaming"
            || current
                .interrupted_at
                .as_deref()
                .is_some_and(|value| !value.is_empty())
        {
            return Ok(false);
        }

        let escaped_doc_id = escape_graphql_string(doc_id);
        let escaped_at = escape_graphql_string(at);
        let mutation = format!(
            r#"mutation {{
                update_AgentResponse(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        status: {{ _eq: "streaming" }}
                    }},
                    input: {{ interrupted_at: "{escaped_at}" }}
                ) {{ _docID }}
            }}"#
        );
        let lease = self
            .buffers
            .lock()
            .await
            .get(doc_id)
            .map(|buffer| buffer.lease.clone())
            .ok_or_else(|| anyhow::anyhow!("no buffer for doc_id={doc_id}"))?;
        let resp = lease
            .execute_response_write(&self.node, &mutation, ExecutionWriteKind::Observe)
            .await?;
        Ok(resp
            .data
            .as_ref()
            .and_then(|d| d.get("update_AgentResponse"))
            .is_some_and(response_has_documents))
    }
}

impl StreamWriter for DefraStreamWriter {
    async fn write_tokens(&self, doc_id: &str, tokens: &str) -> Result<bool> {
        DefraStreamWriter::write_tokens(self, doc_id, tokens).await
    }

    async fn write_reasoning(&self, doc_id: &str, reasoning: &str) -> Result<bool> {
        DefraStreamWriter::write_reasoning(self, doc_id, reasoning).await
    }

    async fn flush_pending(&self, doc_id: &str) -> Result<bool> {
        DefraStreamWriter::flush_pending(self, doc_id).await
    }
}

impl DefraStreamWriter {
    async fn begin_inner(
        &self,
        session_id: &str,
        request_id: &str,
        request_doc_id: &str,
        behavior_id: &str,
        requester_did: Option<&str>,
        lease: ExecutionWriteFence,
    ) -> Result<String> {
        let _write_guard = self.response_write_gate.lock().await;
        if let Some(existing) = load_response_state_by_key(&self.node, request_id).await? {
            anyhow::bail!(
                "refusing to begin response for request_id={} because AgentResponse {} already exists with status={}",
                request_id,
                existing.doc_id,
                existing.status
            );
        }

        let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let response_key = escape_graphql_string(request_id);
        let escaped_request_id = escape_graphql_string(request_id);
        let request_doc_id_field = format!(
            "request_doc_id: \"{}\",",
            escape_graphql_string(request_doc_id)
        );
        let escaped_agent_did = escape_graphql_string(&self.agent_did);
        let requester_did_field = crate::session::requester_did_create_field(requester_did);
        let escaped_session_id = escape_graphql_string(session_id);
        let escaped_behavior_id = escape_graphql_string(behavior_id);
        let mutation = format!(
            r#"mutation {{
                create_AgentResponse(input: {{
                    response_key: "{response_key}",
                    request_id: "{escaped_request_id}",
                    {request_doc_id_field}
                    agent_did: "{escaped_agent_did}",
                    {requester_did_field}
                    behavior_id: "{escaped_behavior_id}",
                    session_id: "{escaped_session_id}",
                    content: "",
                    reasoning: "",
                    status: "streaming",
                    error_message: "",
                    token_count: 0,
                    progress_seq: 0,
                    reasoning_progress_seq: 0,
                    created_at: "{now}",
                    completed_at: ""
                }}) {{ _docID }}
            }}"#,
        );

        let resp = lease
            .execute_response_write(&self.node, &mutation, ExecutionWriteKind::Begin)
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
                reasoning_progress_seq: 0,
                last_flush_at: Instant::now(),
                persisted: StreamBufferSnapshot::default(),
                lease,
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
        if tokens.is_empty() {
            return Ok(false);
        }
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

        self.flush_snapshot(doc_id, &snapshot).await
    }

    async fn write_reasoning(&self, doc_id: &str, reasoning: &str) -> Result<bool> {
        if reasoning.is_empty() {
            return Ok(false);
        }
        {
            let mut buffers = self.buffers.lock().await;
            let buf = buffers
                .get_mut(doc_id)
                .ok_or_else(|| anyhow::anyhow!("no buffer for doc_id={}", doc_id))?;
            append_live_reasoning_preview(&mut buf.reasoning, reasoning);
            buf.reasoning_progress_seq = buf.reasoning_progress_seq.saturating_add(1);
        }

        let snapshot = self.pending_snapshot(doc_id, false).await?;

        let Some(snapshot) = snapshot else {
            return Ok(false);
        };

        self.flush_snapshot(doc_id, &snapshot).await
    }

    async fn flush_pending(&self, doc_id: &str) -> Result<bool> {
        let snapshot = self.pending_snapshot(doc_id, true).await?;
        let Some(snapshot) = snapshot else {
            return Ok(false);
        };
        self.flush_snapshot(doc_id, &snapshot).await
    }
}

fn append_live_reasoning_preview(buffer: &mut String, reasoning: &str) {
    if reasoning.len() >= MAX_LIVE_REASONING_BYTES {
        buffer.clear();
        buffer.push_str(tail_window(reasoning, MAX_LIVE_REASONING_BYTES));
        return;
    }

    trim_string_to_tail_bytes(buffer, MAX_LIVE_REASONING_BYTES - reasoning.len());
    buffer.push_str(reasoning);
}

fn trim_string_to_tail_bytes(buffer: &mut String, max_bytes: usize) {
    if buffer.len() <= max_bytes {
        return;
    }

    let mut start = buffer.len() - max_bytes;
    while !buffer.is_char_boundary(start) {
        start += 1;
    }
    buffer.drain(..start);
}

fn tail_window(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut start = value.len() - max_bytes;
    while !value.is_char_boundary(start) {
        start += 1;
    }
    &value[start..]
}
