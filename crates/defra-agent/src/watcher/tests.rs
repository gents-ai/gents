use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use super::cooldown::{take_next_eligible_pending_request, PROCESSED_REQUEST_COOLDOWN};
use super::*;

// ---------------------------------------------------------------------------
// validate_agent_request_subagent_coherence
// ---------------------------------------------------------------------------

#[test]
fn validate_rejects_mixed_parent_linkage_request_id_only() {
    let req = AgentRequest {
        subagent_depth: 1,
        caused_by_parent_request_id: Some("parent-req-1".to_string()),
        caused_by_parent_tool_call_id: None,
        ..base_request()
    };
    assert!(validate_agent_request_subagent_coherence(&req).is_err());
}

#[test]
fn validate_rejects_mixed_parent_linkage_tool_call_id_only() {
    let req = AgentRequest {
        subagent_depth: 1,
        caused_by_parent_request_id: None,
        caused_by_parent_tool_call_id: Some("parent-tc-1".to_string()),
        ..base_request()
    };
    assert!(validate_agent_request_subagent_coherence(&req).is_err());
}

#[test]
fn validate_rejects_subagent_depth_zero_with_parent_fields() {
    let req = AgentRequest {
        subagent_depth: 0,
        caused_by_parent_request_id: Some("parent-req-1".to_string()),
        caused_by_parent_tool_call_id: Some("parent-tc-1".to_string()),
        ..base_request()
    };
    assert!(validate_agent_request_subagent_coherence(&req).is_err());
}

#[test]
fn validate_accepts_top_level_request() {
    let req = AgentRequest {
        subagent_depth: 0,
        caused_by_parent_request_id: None,
        caused_by_parent_tool_call_id: None,
        ..base_request()
    };
    assert!(validate_agent_request_subagent_coherence(&req).is_ok());
}

#[test]
fn validate_accepts_subagent_request() {
    let req = AgentRequest {
        subagent_depth: 1,
        caused_by_parent_request_id: Some("parent-req-1".to_string()),
        caused_by_parent_tool_call_id: Some("parent-tc-1".to_string()),
        ..base_request()
    };
    assert!(validate_agent_request_subagent_coherence(&req).is_ok());
}

#[test]
fn agent_request_clone() {
    let req = AgentRequest {
        doc_id: "abc".into(),
        request_id: "req-1".into(),
        agent_did: "did:key:z123".into(),
        behavior_id: Some("general".into()),
        session_id: "sess-1".into(),
        content: "hello".into(),
        temperature: None,
        top_p: None,
        top_k: None,
        max_tokens: None,
        metadata: None,
        execution_origin: None,
        created_at: "2026-03-12T00:00:00Z".into(),
        deadline: None,
        subagent_depth: 0,
        caused_by_parent_request_id: None,
        caused_by_parent_tool_call_id: None,
    };
    let cloned = req.clone();
    assert_eq!(cloned.doc_id, "abc");
    assert_eq!(cloned.content, "hello");
}

#[test]
fn cooling_down_request_does_not_block_other_pending_sessions() {
    let now = Instant::now();
    let mut processed_request_ids = HashMap::from([("req-1".to_string(), now)]);

    let request = take_next_eligible_pending_request(
        &mut processed_request_ids,
        vec![request("req-1", "sess-1"), request("req-2", "sess-2")],
        now,
    )
    .expect("eligible request");

    assert_eq!(request.request_id, "req-2");
    assert!(processed_request_ids.contains_key("req-1"));
    assert!(processed_request_ids.contains_key("req-2"));
}

#[test]
fn cooled_down_request_becomes_eligible_again() {
    let now = Instant::now();
    let mut processed_request_ids = HashMap::from([("req-1".to_string(), now)]);
    let later = now + PROCESSED_REQUEST_COOLDOWN + Duration::from_millis(1);

    let request = take_next_eligible_pending_request(
        &mut processed_request_ids,
        vec![request("req-1", "sess-1")],
        later,
    )
    .expect("eligible request");

    assert_eq!(request.request_id, "req-1");
    assert_eq!(processed_request_ids.get("req-1").copied(), Some(later));
}

fn base_request() -> AgentRequest {
    request("req-base", "sess-base")
}

fn request(request_id: &str, session_id: &str) -> AgentRequest {
    AgentRequest {
        doc_id: format!("doc-{request_id}"),
        request_id: request_id.to_string(),
        agent_did: "did:key:z123".into(),
        behavior_id: Some("general".into()),
        session_id: session_id.to_string(),
        content: "hello".into(),
        temperature: None,
        top_p: None,
        top_k: None,
        max_tokens: None,
        metadata: None,
        execution_origin: None,
        created_at: "2026-03-12T00:00:00Z".into(),
        deadline: None,
        subagent_depth: 0,
        caused_by_parent_request_id: None,
        caused_by_parent_tool_call_id: None,
    }
}

// ---------------------------------------------------------------------------
// Integration tests: validate_agent_request_subagent_coherence wired into
// the query path.  These tests write incoherent AgentRequest rows directly
// into DefraDB and verify that the watcher rejects them at query time.
// ---------------------------------------------------------------------------

async fn test_node() -> Arc<defra_node::EmbeddedNode> {
    Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap())
}

/// Insert an AgentRequest row with an incoherent parent linkage into DefraDB
/// and return its `_docID`.
///
/// `subagent_depth` = 1, `caused_by_parent_request_id` is set, but
/// `caused_by_parent_tool_call_id` is absent — one half of the pair is
/// missing, which the validator must reject.
async fn insert_incoherent_agent_request(
    node: &defra_node::EmbeddedNode,
    agent_did: &str,
    request_id: &str,
) -> String {
    use crate::graphql::escape_graphql_string;

    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let created_at = chrono::Utc::now().to_rfc3339();

    // subagent_depth = 1 but only caused_by_parent_request_id is set;
    // caused_by_parent_tool_call_id is absent.  This is the coherence
    // violation the validator checks for.
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                agent_did: "{escaped_agent_did}",
                session_id: "sess-incoherent",
                content: "test",
                status: "pending",
                lifecycle_state: "pending",
                backend_id: "",
                created_at: "{created_at}",
                retry_count: 0,
                max_retries: 0,
                subagent_depth: 1,
                caused_by_parent_request_id: "parent-req-exists"
            }}) {{ _docID }}
        }}"#
    );

    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create_AgentRequest (incoherent) failed: {:?}",
        response.errors
    );

    let lookup = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}, limit: 1) {{
                _docID
            }}
        }}"#
    );
    let response = node.execute(&lookup).await;
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .expect("AgentRequest _docID")
}

#[tokio::test]
async fn pending_requests_rejects_incoherent_subagent_linkage() {
    let node = test_node().await;
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let agent_did = "did:key:z-watcher-coherence-pending";
    insert_incoherent_agent_request(node.as_ref(), agent_did, "req-incoherent-pending").await;

    let watcher = DefraWatcher::new(node.clone(), agent_did);
    let result = watcher.pending_requests().await;
    assert!(
        result.is_err(),
        "pending_requests must fail for incoherent subagent linkage, got: {:?}",
        result
    );
}

#[tokio::test]
async fn try_fetch_request_rejects_incoherent_subagent_linkage() {
    let node = test_node().await;
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let agent_did = "did:key:z-watcher-coherence-fetch";
    let doc_id =
        insert_incoherent_agent_request(node.as_ref(), agent_did, "req-incoherent-fetch").await;

    let watcher = DefraWatcher::new(node.clone(), agent_did);
    let result = watcher.try_fetch_request(&doc_id).await;
    assert!(
        result.is_err(),
        "try_fetch_request must fail for incoherent subagent linkage, got: {:?}",
        result
    );
}
