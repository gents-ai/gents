use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use defra_node::EmbeddedNode;
use serde_json::json;

use super::queries::extract_mutation_doc_id;
use super::*;

async fn build_test_node(name: &str) -> (Arc<EmbeddedNode>, PathBuf) {
    let data_path = std::env::temp_dir().join(format!("streaming-{name}-{}", uuid::Uuid::new_v4()));
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
                    reasoning
                    error_message
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
    assert!(!mutation.contains("reasoning:"));
    assert!(!mutation.contains("token_count:"));
}

#[tokio::test]
async fn finalize_removes_buffer_after_successful_mutation() {
    let (node, data_path) = build_test_node("finalize-success").await;
    let writer = DefraStreamWriter::new(
        node.clone(),
        "did:defra-agent:test",
        Duration::from_secs(60),
    );
    let request_id = uuid::Uuid::new_v4().to_string();
    let doc_id = writer
        .begin("session-1", &request_id, "general")
        .await
        .unwrap();

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
        row.get("reasoning").and_then(|value| value.as_str()),
        Some("")
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
    let writer = DefraStreamWriter::new(
        node.clone(),
        "did:defra-agent:test",
        Duration::from_secs(60),
    );
    let invalid_doc_id = r#"doc"broken"#.to_string();

    writer.buffers.lock().await.insert(
        invalid_doc_id.clone(),
        StreamBuffer {
            content: "lost tail".to_string(),
            reasoning: String::new(),
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
    let writer = DefraStreamWriter::new(
        node.clone(),
        "did:defra-agent:test",
        Duration::from_secs(60),
    );
    let request_id = uuid::Uuid::new_v4().to_string();
    let doc_id = writer
        .begin("session-1", &request_id, "general")
        .await
        .unwrap();

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
        row.get("reasoning").and_then(|value| value.as_str()),
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
async fn error_message_persists_on_error_response() {
    let (node, data_path) = build_test_node("error-message").await;
    let writer = DefraStreamWriter::new(
        node.clone(),
        "did:defra-agent:test",
        Duration::from_secs(60),
    );
    let request_id = uuid::Uuid::new_v4().to_string();
    let doc_id = writer
        .begin("session-1", &request_id, "general")
        .await
        .unwrap();

    writer
        .set_error_message(
            &doc_id,
            "stream liveness timeout: no data received for 120s",
        )
        .await
        .unwrap();
    writer.finalize(&doc_id, StreamStatus::Error).await.unwrap();

    let row = load_response(&node, &doc_id).await;
    assert_eq!(
        row.get("error_message").and_then(|value| value.as_str()),
        Some("stream liveness timeout: no data received for 120s")
    );
    assert_eq!(
        row.get("reasoning").and_then(|value| value.as_str()),
        Some("")
    );

    let _ = fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn write_tokens_fails_when_response_document_is_missing() {
    let (node, data_path) = build_test_node("missing-response-write").await;
    let writer = DefraStreamWriter::new(
        node.clone(),
        "did:defra-agent:test",
        Duration::from_millis(1),
    );
    let missing_doc_id = "missing-response-doc".to_string();

    writer.buffers.lock().await.insert(
        missing_doc_id.clone(),
        StreamBuffer {
            content: String::new(),
            reasoning: String::new(),
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
    let writer = DefraStreamWriter::new(
        node.clone(),
        "did:defra-agent:test",
        Duration::from_secs(60),
    );
    let request_id = uuid::Uuid::new_v4().to_string();
    let doc_id = writer
        .begin("session-1", &request_id, "general")
        .await
        .unwrap();

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
    assert_eq!(
        row.get("reasoning").and_then(|value| value.as_str()),
        Some("")
    );

    let _ = fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn write_reasoning_persists_on_response() {
    let (node, data_path) = build_test_node("reasoning-write").await;
    let writer = DefraStreamWriter::new(
        node.clone(),
        "did:defra-agent:test",
        Duration::from_millis(1),
    );
    let request_id = uuid::Uuid::new_v4().to_string();
    let doc_id = writer
        .begin("session-1", &request_id, "general")
        .await
        .unwrap();

    writer
        .write_reasoning(&doc_id, "Need to inspect the repo structure first.")
        .await
        .unwrap();
    writer
        .finalize(&doc_id, StreamStatus::Complete)
        .await
        .unwrap();

    let row = load_response(&node, &doc_id).await;
    assert_eq!(
        row.get("reasoning").and_then(|value| value.as_str()),
        Some("Need to inspect the repo structure first.")
    );

    let _ = fs::remove_dir_all(&data_path);
}

#[tokio::test]
async fn begin_rejects_existing_response_document() {
    let (node, data_path) = build_test_node("begin-existing-response").await;
    let writer = DefraStreamWriter::new(
        node.clone(),
        "did:defra-agent:test",
        Duration::from_secs(60),
    );
    let request_id = uuid::Uuid::new_v4().to_string();

    let doc_id = writer
        .begin("session-1", &request_id, "general")
        .await
        .unwrap();
    writer.finalize(&doc_id, StreamStatus::Error).await.unwrap();

    let error = writer
        .begin("session-1", &request_id, "general")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already exists"));

    let _ = fs::remove_dir_all(&data_path);
}
