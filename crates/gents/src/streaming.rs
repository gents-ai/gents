use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use anyhow::Result;
use defra_node::EmbeddedNode;
use tokio::sync::Mutex;
use tracing::Instrument;

use crate::graphql::{escape_graphql_string, response_has_documents};
use crate::session::execute_mutation_with_retry;

mod queries;
#[cfg(test)]
mod tests;

use queries::{
    extract_mutation_doc_id, load_response_state, load_response_state_by_key,
    PersistedResponseState,
};

const MAX_LIVE_REASONING_BYTES: usize = 64 * 1024;

type ResponseWriteGate = Mutex<()>;

/// DefraDB commits mutations at a database-wide revision boundary, so
/// independent behavior daemons streaming into different AgentResponse rows
/// can still collide. Writers are constructed per behavior; this node-scoped
/// gate keeps their short response mutations ordered without coupling daemon
/// ownership or holding the gate while waiting on the provider.
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
    response_write_gate: Arc<ResponseWriteGate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestFinalizeMode {
    UpdateRequest,
    ResponseOnly,
}

impl RequestFinalizeMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::UpdateRequest => "update_request",
            Self::ResponseOnly => "response_only",
        }
    }
}

struct StreamBuffer {
    content: String,
    reasoning: String,
    token_count: usize,
    reasoning_progress_seq: usize,
    last_flush_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

    pub async fn begin_with_requester_did(
        &self,
        session_id: &str,
        request_id: &str,
        request_doc_id: Option<&str>,
        behavior_id: &str,
        requester_did: Option<&str>,
    ) -> Result<String> {
        let resolved_request_doc_id;
        let request_doc_id = match request_doc_id.map(str::trim).filter(|id| !id.is_empty()) {
            Some(request_doc_id) => Some(request_doc_id),
            None => {
                resolved_request_doc_id =
                    crate::request_binding::require_request_doc_id(&self.node, request_id).await?;
                Some(resolved_request_doc_id.as_str())
            }
        };
        self.begin_inner(
            session_id,
            request_id,
            request_doc_id,
            behavior_id,
            requester_did,
        )
        .await
    }

    async fn flush_snapshot(&self, doc_id: &str, snapshot: &StreamBufferSnapshot) -> Result<()> {
        let _write_guard = self.response_write_gate.lock().await;
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
            reasoning_progress_seq: buf.reasoning_progress_seq,
        }))
    }

    pub async fn reset_tail(&self, doc_id: &str) -> Result<()> {
        let _write_guard = self.response_write_gate.lock().await;
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

        self.finalize_error(&existing.doc_id, error_message).await?;
        Ok(true)
    }

    pub async fn finalize_error(&self, doc_id: &str, error_message: &str) -> Result<StreamResult> {
        self.finalize_inner(
            doc_id,
            StreamStatus::Error,
            Some(error_message),
            RequestFinalizeMode::UpdateRequest,
            false,
        )
        .await
    }

    /// Complete the response-side interrupt edge without rewriting
    /// `AgentRequest`, which is terminalized separately as `interrupted`.
    ///
    /// `interrupted_at` — not the human-readable error text — is the durable
    /// marker request repair classifies on, so this finalize stamps it
    /// atomically whenever the earlier standalone `write_interrupted_at` did
    /// not survive.
    pub async fn finalize_interrupted_response(&self, doc_id: &str) -> Result<StreamResult> {
        self.finalize_inner(
            doc_id,
            StreamStatus::Error,
            Some("interrupted"),
            RequestFinalizeMode::ResponseOnly,
            true,
        )
        .await
    }

    pub async fn write_interrupted_at(&self, doc_id: &str, at: &str) -> Result<bool> {
        let _write_guard = self.response_write_gate.lock().await;
        let Some(current) = load_response_state(&self.node, doc_id).await? else {
            return Ok(false);
        };
        if current.status != StreamStatus::Streaming.as_str()
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
        let resp =
            execute_mutation_with_retry(&self.node, &mutation, "write_interrupted_at").await?;
        Ok(resp
            .data
            .as_ref()
            .and_then(|d| d.get("update_AgentResponse"))
            .is_some_and(response_has_documents))
    }

    async fn finalize_inner(
        &self,
        doc_id: &str,
        status: StreamStatus,
        error_message: Option<&str>,
        request_mode: RequestFinalizeMode,
        mark_interrupted: bool,
    ) -> Result<StreamResult> {
        let _write_guard = self.response_write_gate.lock().await;
        let existing = load_response_state(&self.node, doc_id).await?;
        let snapshot = {
            let buffers = self.buffers.lock().await;
            buffers.get(doc_id).map(|buf| StreamBufferSnapshot {
                content: buf.content.clone(),
                reasoning: buf.reasoning.clone(),
                token_count: buf.token_count,
                reasoning_progress_seq: buf.reasoning_progress_seq,
            })
        };
        let request_id = existing
            .as_ref()
            .map(|response| response.request_id.as_str())
            .unwrap_or("");
        let session_id = existing
            .as_ref()
            .and_then(|response| response.session_id.as_deref())
            .unwrap_or("");
        let agent_did = existing
            .as_ref()
            .and_then(|response| response.agent_did.as_deref())
            .unwrap_or("");
        let behavior_id = existing
            .as_ref()
            .and_then(|response| response.behavior_id.as_deref())
            .unwrap_or("");
        let span = tracing::info_span!(
            "stream.finalize",
            doc_id = %doc_id,
            request_id = %request_id,
            session_id = %session_id,
            agent_did = %agent_did,
            behavior_id = %behavior_id,
            status = %status.as_str(),
            has_buffer = snapshot.is_some(),
            has_error_message = error_message.is_some(),
            request_finalize_mode = %request_mode.as_str(),
            token_count = tracing::field::Empty,
            final_content_len = tracing::field::Empty,
            update_matched_response = tracing::field::Empty,
            finalize_outcome = tracing::field::Empty,
        );

        async {
            let now = chrono::Utc::now().to_rfc3339();
            let mutation = build_finalize_mutation(
                existing.as_ref(),
                doc_id,
                &status,
                &now,
                snapshot.as_ref(),
                error_message,
                request_mode,
                &self.agent_did,
                mark_interrupted,
            );
            let operation = if snapshot.is_some() {
                "finalize_streaming_response"
            } else {
                "finalize_streaming_response_without_buffer"
            };

            let resp = match crate::retry::execute_graphql_with_terminal_persistence_retry(
                &self.node,
                &mutation,
                operation,
            )
            .await
            {
                Ok(resp) => resp,
                Err(error) => {
                    tracing::Span::current().record("finalize_outcome", "mutation_error");
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

            let update_matched_response = resp
                .data
                .as_ref()
                .and_then(|data| data.get("update_AgentResponse"))
                .is_some_and(response_has_documents);
            tracing::Span::current().record("update_matched_response", update_matched_response);

            let persisted = if update_matched_response {
                tracing::Span::current().record("finalize_outcome", "updated");
                load_response_state(&self.node, doc_id).await?
            } else {
                match load_response_state(&self.node, doc_id).await? {
                    Some(existing) if existing.status == status.as_str() => {
                        tracing::Span::current().record(
                            "finalize_outcome",
                            "idempotent_terminal",
                        );
                        tracing::warn!(
                            doc_id = %doc_id,
                            status = %status.as_str(),
                            "finalize became an idempotent no-op because response was already terminal"
                        );
                        Some(existing)
                    }
                    Some(existing) => {
                        tracing::Span::current().record("finalize_outcome", "terminal_conflict");
                        anyhow::bail!(
                            "cannot finalize AgentResponse {} as {} because it is already {}",
                            doc_id,
                            status.as_str(),
                            existing.status
                        );
                    }
                    None => {
                        tracing::Span::current().record("finalize_outcome", "missing_response");
                        anyhow::bail!(
                            "cannot finalize AgentResponse {} as {} because the response document is missing",
                            doc_id,
                            status.as_str()
                        );
                    }
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

            tracing::Span::current().record("token_count", token_count as i64);
            tracing::Span::current().record("final_content_len", content.len() as i64);
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
        .instrument(span)
        .await
    }
}

impl StreamWriter for DefraStreamWriter {
    async fn begin(&self, session_id: &str, request_id: &str, behavior_id: &str) -> Result<String> {
        self.begin_with_requester_did(session_id, request_id, None, behavior_id, None)
            .await
    }

    async fn write_tokens(&self, doc_id: &str, tokens: &str) -> Result<bool> {
        DefraStreamWriter::write_tokens(self, doc_id, tokens).await
    }

    async fn write_reasoning(&self, doc_id: &str, reasoning: &str) -> Result<bool> {
        DefraStreamWriter::write_reasoning(self, doc_id, reasoning).await
    }

    async fn flush_pending(&self, doc_id: &str) -> Result<bool> {
        DefraStreamWriter::flush_pending(self, doc_id).await
    }

    async fn finalize(&self, doc_id: &str, status: StreamStatus) -> Result<StreamResult> {
        DefraStreamWriter::finalize(self, doc_id, status).await
    }
}

impl DefraStreamWriter {
    async fn begin_inner(
        &self,
        session_id: &str,
        request_id: &str,
        request_doc_id: Option<&str>,
        behavior_id: &str,
        requester_did: Option<&str>,
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
        let request_doc_id_field = request_doc_id
            .map(str::trim)
            .filter(|doc_id| !doc_id.is_empty())
            .map(|doc_id| format!("request_doc_id: \"{}\",", escape_graphql_string(doc_id)))
            .unwrap_or_default();
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
                reasoning_progress_seq: 0,
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
            append_live_reasoning_preview(&mut buf.reasoning, reasoning);
            buf.reasoning_progress_seq = buf.reasoning_progress_seq.saturating_add(1);
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
        self.finalize_inner(
            doc_id,
            status,
            None,
            RequestFinalizeMode::UpdateRequest,
            false,
        )
        .await
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

#[allow(clippy::too_many_arguments)]
fn build_finalize_mutation(
    existing: Option<&PersistedResponseState>,
    doc_id: &str,
    status: &StreamStatus,
    now: &str,
    snapshot: Option<&StreamBufferSnapshot>,
    error_message: Option<&str>,
    request_mode: RequestFinalizeMode,
    owner_did: &str,
    mark_interrupted: bool,
) -> String {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let escaped_now = escape_graphql_string(now);
    let effective_error_message = match status {
        StreamStatus::Error => error_message
            .or_else(|| existing.and_then(|response| response.error_message.as_deref())),
        StreamStatus::Complete | StreamStatus::Streaming => None,
    };
    let interrupted_at_already_set = existing
        .and_then(|response| response.interrupted_at.as_deref())
        .is_some_and(|value| !value.trim().is_empty());
    let interrupted_at_input = if mark_interrupted && !interrupted_at_already_set {
        format!(r#"interrupted_at: "{escaped_now}","#)
    } else {
        String::new()
    };
    let request_transition = match request_mode {
        RequestFinalizeMode::UpdateRequest => existing
            .map(|existing| {
                build_request_terminal_update(
                    &existing.request_id,
                    owner_did,
                    status,
                    now,
                    effective_error_message,
                )
            })
            .unwrap_or_default(),
        RequestFinalizeMode::ResponseOnly => String::new(),
    };
    let error_message_input = error_message
        .map(escape_graphql_string)
        .map(|message| format!(r#"error_message: "{message}","#))
        .unwrap_or_default();
    match snapshot {
        Some(snapshot) => format!(
            r#"mutation {{
                update_AgentResponse(
                    filter: {{
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        status: {{ _eq: "streaming" }}
                    }},
                    input: {{
                        content: "",
                        reasoning: "",
                        status: "{status}",
                        {error_message_input}
                        {interrupted_at_input}
                        token_count: {token_count},
                        completed_at: "{escaped_now}"
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
                        _docID: {{ _eq: "{escaped_doc_id}" }},
                        status: {{ _eq: "streaming" }}
                    }},
                    input: {{
                        content: "",
                        reasoning: "",
                        status: "{status}",
                        {error_message_input}
                        {interrupted_at_input}
                        completed_at: "{escaped_now}"
                    }}
                ) {{ _docID }}
                {request_transition}
            }}"#,
            status = status.as_str(),
        ),
    }
}

fn build_request_terminal_update(
    request_id: &str,
    owner_did: &str,
    status: &StreamStatus,
    terminalized_at: &str,
    failure_reason: Option<&str>,
) -> String {
    let (request_status, lifecycle_state) = match status {
        StreamStatus::Complete => ("completed", "completed"),
        StreamStatus::Error => ("error", "failed"),
        StreamStatus::Streaming => return String::new(),
    };
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_owner_did = escape_graphql_string(owner_did);
    let escaped_terminalized_at = escape_graphql_string(terminalized_at);
    let escaped_failure_reason = escape_graphql_string(failure_reason.unwrap_or_default());
    format!(
        r#"update_AgentRequest(
                    filter: {{
                        request_id: {{ _eq: "{escaped_request_id}" }},
                        agent_did: {{ _eq: "{escaped_owner_did}" }},
                        status: {{ _eq: "processing" }},
                        lifecycle_state: {{ _in: ["claimed", "processing"] }}
                    }},
                    input: {{
                        status: "{request_status}",
                        lifecycle_state: "{lifecycle_state}",
                        failure_reason: "{escaped_failure_reason}",
                        terminalized_at: "{escaped_terminalized_at}",
                        terminal_redrive_attempts: 0
                    }}
                ) {{ _docID }}"#
    )
}
