//! Token batch writer — streams inference output to DefraDB documents.
//!
//! Creates an AgentResponse document in DefraDB at the start of generation,
//! then batches tokens and updates the document as they arrive. Amy receives
//! these updates via P2P gossip replication.
//!
//! Design decision: "build and see" — timer-based batching
//! (flush every 1s), tuned to reduce P2P gossip thrash.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::graphql::{escape_graphql_string, response_has_documents};
use crate::session::execute_mutation_with_retry;

/// Status of a streaming response.
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

/// Metadata about a completed stream.
#[derive(Debug, Clone)]
pub struct StreamResult {
    /// DefraDB doc ID of the AgentResponse document.
    pub doc_id: String,
    /// Total accumulated content.
    pub content: String,
    /// Final status.
    pub status: StreamStatus,
    /// Total tokens written.
    pub token_count: usize,
}

/// Streams tokens to DefraDB documents during inference.
pub trait StreamWriter: Send + Sync {
    /// Begin streaming a response for the given session.
    /// Creates an AgentResponse document, returns its doc ID.
    fn begin(
        &self,
        session_id: &str,
        request_id: &str,
    ) -> impl std::future::Future<Output = Result<String>> + Send;

    /// Write tokens to the response buffer.
    /// Returns true when this call flushes a batched update to DefraDB.
    fn write_tokens(
        &self,
        doc_id: &str,
        tokens: &str,
    ) -> impl std::future::Future<Output = Result<bool>> + Send;

    /// Finalize the response document with the given status.
    fn finalize(
        &self,
        doc_id: &str,
        status: StreamStatus,
    ) -> impl std::future::Future<Output = Result<StreamResult>> + Send;
}

/// DefraDB-backed stream writer that updates AgentResponse documents.
pub struct DefraStreamWriter {
    node: Arc<EmbeddedNode>,
    agent_did: String,
    batch_interval: Duration,
    /// Accumulated content for each active stream.
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

#[derive(Debug, Clone, Deserialize)]
struct PersistedResponseState {
    #[serde(rename = "_docID")]
    doc_id: String,
    content: String,
    status: String,
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
    async fn begin(&self, session_id: &str, request_id: &str) -> Result<String> {
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
        let mutation = format!(
            r#"mutation {{
                create_AgentResponse(input: {{
                    response_key: "{response_key}",
                    request_id: "{request_id}",
                    agent_did: "{agent_did}",
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

        // Initialize buffer.
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

fn extract_mutation_doc_id<'a>(
    data: &'a serde_json::Value,
    collection_name: &str,
) -> Option<&'a str> {
    for field_name in [
        format!("upsert_{collection_name}"),
        format!("create_{collection_name}"),
        format!("add_{collection_name}"),
    ] {
        if let Some(value) = data.get(&field_name) {
            if let Some(doc_id) = value.get("_docID").and_then(|value| value.as_str()) {
                return Some(doc_id);
            }

            if let Some(doc_id) = value
                .as_array()
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("_docID"))
                .and_then(|value| value.as_str())
            {
                return Some(doc_id);
            }
        }
    }

    None
}

async fn load_response_state(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<PersistedResponseState>> {
    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                limit: 1
            ) {{
                _docID
                content
                status
                token_count
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading AgentResponse state for doc_id={doc_id}: {:?}",
            resp.errors
        );
    }

    let mut rows: Vec<PersistedResponseState> = match resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentResponse"))
    {
        Some(value) => serde_json::from_value(value.clone())?,
        None => Vec::new(),
    };

    Ok(rows.pop())
}

async fn load_response_state_by_key(
    node: &EmbeddedNode,
    response_key: &str,
) -> Result<Option<PersistedResponseState>> {
    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{ response_key: {{ _eq: "{response_key}" }} }},
                limit: 1
            ) {{
                _docID
                content
                status
                token_count
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading AgentResponse state for response_key={response_key}: {:?}",
            resp.errors
        );
    }

    let mut rows: Vec<PersistedResponseState> = match resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentResponse"))
    {
        Some(value) => serde_json::from_value(value.clone())?,
        None => Vec::new(),
    };

    Ok(rows.pop())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;

    async fn build_test_node(name: &str) -> (Arc<EmbeddedNode>, PathBuf) {
        let data_path =
            std::env::temp_dir().join(format!("streaming-{name}-{}", uuid::Uuid::new_v4()));
        let node = Arc::new(
            defra_node::EmbeddedNode::builder()
                .data_path(&data_path)
                .build()
                .await
                .unwrap(),
        );
        crate::schema::ensure_runtime_schemas(&node).await.unwrap();
        (node, data_path)
    }

    async fn load_response(
        node: &EmbeddedNode,
        doc_id: &str,
    ) -> serde_json::Map<String, serde_json::Value> {
        let query = format!(
            r#"{{
                AgentResponse(
                    filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                    limit: 1
                ) {{
                    _docID
                    content
                    status
                    token_count
                    completed_at
                }}
            }}"#
        );
        let resp = node.execute(&query).await;
        assert!(!resp.has_errors(), "query failed: {:?}", resp.errors);
        resp.data
            .as_ref()
            .and_then(|data| data.get("AgentResponse"))
            .and_then(|value| value.as_array())
            .and_then(|rows| rows.first())
            .and_then(|row| row.as_object())
            .cloned()
            .expect("response row")
    }

    #[test]
    fn stream_status_as_str() {
        assert_eq!(StreamStatus::Streaming.as_str(), "streaming");
        assert_eq!(StreamStatus::Complete.as_str(), "complete");
        assert_eq!(StreamStatus::Error.as_str(), "error");
    }

    #[test]
    fn extract_mutation_doc_id_accepts_upsert_create_and_add_shapes() {
        let upsert_data = json!({
            "upsert_AgentResponse": [{ "_docID": "doc-upsert" }]
        });
        assert_eq!(
            extract_mutation_doc_id(&upsert_data, "AgentResponse"),
            Some("doc-upsert")
        );

        let create_data = json!({
            "create_AgentResponse": { "_docID": "doc-create" }
        });
        assert_eq!(
            extract_mutation_doc_id(&create_data, "AgentResponse"),
            Some("doc-create")
        );

        let add_data = json!({
            "add_AgentResponse": [{ "_docID": "doc-add" }]
        });
        assert_eq!(
            extract_mutation_doc_id(&add_data, "AgentResponse"),
            Some("doc-add")
        );
    }

    #[test]
    fn build_finalize_mutation_omits_content_fields_without_buffer() {
        let mutation = build_finalize_mutation(
            "doc-1",
            &StreamStatus::Complete,
            "2026-03-24T00:00:00Z",
            None,
        );

        assert!(mutation.contains(r#"status: "complete""#));
        assert!(mutation.contains(r#"completed_at: "2026-03-24T00:00:00Z""#));
        assert!(!mutation.contains("content:"));
        assert!(!mutation.contains("token_count:"));
    }

    #[tokio::test]
    async fn finalize_removes_buffer_after_successful_mutation() {
        let (node, data_path) = build_test_node("finalize-success").await;
        let writer =
            DefraStreamWriter::new(node.clone(), "did:defra-agent:test", Duration::from_secs(60));
        let request_id = uuid::Uuid::new_v4().to_string();
        let doc_id = writer.begin("session-1", &request_id).await.unwrap();

        writer.write_tokens(&doc_id, "tail content").await.unwrap();
        let result = writer
            .finalize(&doc_id, StreamStatus::Complete)
            .await
            .unwrap();

        assert_eq!(result.content, "tail content");
        assert_eq!(result.token_count, 2);
        assert!(!writer.buffers.lock().await.contains_key(&doc_id));

        let row = load_response(&node, &doc_id).await;
        assert_eq!(
            row.get("content").and_then(|value| value.as_str()),
            Some("tail content")
        );
        assert_eq!(
            row.get("status").and_then(|value| value.as_str()),
            Some("complete")
        );
        assert_eq!(
            row.get("token_count").and_then(|value| value.as_u64()),
            Some(2)
        );
        assert!(row
            .get("completed_at")
            .and_then(|value| value.as_str())
            .is_some_and(|value| !value.is_empty()));

        let _ = fs::remove_dir_all(&data_path);
    }

    #[tokio::test]
    async fn finalize_keeps_buffer_when_mutation_fails() {
        let (node, data_path) = build_test_node("finalize-failure").await;
        let writer =
            DefraStreamWriter::new(node.clone(), "did:defra-agent:test", Duration::from_secs(60));
        let invalid_doc_id = r#"doc"broken"#.to_string();

        writer.buffers.lock().await.insert(
            invalid_doc_id.clone(),
            StreamBuffer {
                content: "lost tail".to_string(),
                token_count: 2,
                last_flush_at: Instant::now(),
            },
        );

        let error = writer
            .finalize(&invalid_doc_id, StreamStatus::Error)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("failed after"));
        assert!(writer.buffers.lock().await.contains_key(&invalid_doc_id));

        let _ = fs::remove_dir_all(&data_path);
    }

    #[tokio::test]
    async fn finalize_without_buffer_uses_fallback_mutation() {
        let (node, data_path) = build_test_node("finalize-fallback").await;
        let writer =
            DefraStreamWriter::new(node.clone(), "did:defra-agent:test", Duration::from_secs(60));
        let request_id = uuid::Uuid::new_v4().to_string();
        let doc_id = writer.begin("session-1", &request_id).await.unwrap();

        writer.buffers.lock().await.remove(&doc_id);

        let result = writer.finalize(&doc_id, StreamStatus::Error).await.unwrap();

        assert_eq!(result.content, "");
        assert_eq!(result.token_count, 0);

        let row = load_response(&node, &doc_id).await;
        assert_eq!(
            row.get("content").and_then(|value| value.as_str()),
            Some("")
        );
        assert_eq!(
            row.get("status").and_then(|value| value.as_str()),
            Some("error")
        );
        assert!(row
            .get("completed_at")
            .and_then(|value| value.as_str())
            .is_some_and(|value| !value.is_empty()));

        let _ = fs::remove_dir_all(&data_path);
    }

    #[tokio::test]
    async fn write_tokens_fails_when_response_document_is_missing() {
        let (node, data_path) = build_test_node("missing-response-write").await;
        let writer =
            DefraStreamWriter::new(node.clone(), "did:defra-agent:test", Duration::from_millis(1));
        let missing_doc_id = "missing-response-doc".to_string();

        writer.buffers.lock().await.insert(
            missing_doc_id.clone(),
            StreamBuffer {
                content: String::new(),
                token_count: 0,
                last_flush_at: Instant::now() - Duration::from_secs(1),
            },
        );

        let error = writer
            .write_tokens(&missing_doc_id, "partial")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("missing"));
        assert!(writer.buffers.lock().await.contains_key(&missing_doc_id));

        let _ = fs::remove_dir_all(&data_path);
    }

    #[tokio::test]
    async fn finalize_rejects_conflicting_terminal_state() {
        let (node, data_path) = build_test_node("finalize-conflict").await;
        let writer =
            DefraStreamWriter::new(node.clone(), "did:defra-agent:test", Duration::from_secs(60));
        let request_id = uuid::Uuid::new_v4().to_string();
        let doc_id = writer.begin("session-1", &request_id).await.unwrap();

        writer.write_tokens(&doc_id, "final answer").await.unwrap();
        writer
            .finalize(&doc_id, StreamStatus::Complete)
            .await
            .unwrap();

        let error = writer
            .finalize(&doc_id, StreamStatus::Error)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cannot finalize AgentResponse"));

        let row = load_response(&node, &doc_id).await;
        assert_eq!(
            row.get("status").and_then(|value| value.as_str()),
            Some("complete")
        );

        let _ = fs::remove_dir_all(&data_path);
    }

    #[tokio::test]
    async fn begin_rejects_existing_response_document() {
        let (node, data_path) = build_test_node("begin-existing-response").await;
        let writer =
            DefraStreamWriter::new(node.clone(), "did:defra-agent:test", Duration::from_secs(60));
        let request_id = uuid::Uuid::new_v4().to_string();

        let doc_id = writer.begin("session-1", &request_id).await.unwrap();
        writer.finalize(&doc_id, StreamStatus::Error).await.unwrap();

        let error = writer.begin("session-1", &request_id).await.unwrap_err();
        assert!(error.to_string().contains("already exists"));

        let _ = fs::remove_dir_all(&data_path);
    }
}
