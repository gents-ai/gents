use super::super::*;
use crate::client::schema::ensure_runtime_schemas;
use defra_node::NodeBuilder;
use std::sync::Arc;

fn canonical_header(
    alias: &str,
    key: &str,
    session: &str,
    agent: &str,
    requester: Option<&str>,
    sequence: u32,
) -> String {
    let requester = requester
        .map(|did| format!("\"{did}\""))
        .unwrap_or_else(|| "null".into());
    format!(
        r#"{alias}: create_AgentMessage(input: {{
            message_key: "{key}", session_id: "{session}", agent_did: "{agent}",
            requester_did: {requester}, request_doc_id: "request-{session}",
            publication: {{kind: "request_execution", execution_generation: "test-generation"}},
            outcome: "complete", sequence: {sequence}, role: "assistant", blocks: [],
            created_at: "2026-08-25T00:00:00Z"
        }}) {{ _docID }}"#
    )
}

#[tokio::test]
async fn transcript_pages_bound_canonical_headers_and_keep_stable_cursors() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    let headers = (1..=100)
        .map(|sequence| {
            canonical_header(
                &format!("m{sequence}"),
                &format!("paged:{sequence}"),
                "paged",
                "did:test:agent",
                None,
                sequence,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let response = node.execute(&format!("mutation {{ {headers} }}")).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let tip = load_session_transcript_page(node.as_ref(), "paged", None, None, None, Some(10))
        .await
        .expect("tip page");
    assert_eq!(tip.query_count, 1);
    assert_eq!(tip.message_query_limit, 11);
    assert_eq!(tip.queried_rows, 11);
    let sequences = tip
        .store
        .transcript_messages
        .iter()
        .map(|row| row.message.sequence)
        .collect::<Vec<_>>();
    assert_eq!(sequences.iter().copied().min(), Some(91));
    assert_eq!(sequences.iter().copied().max(), Some(100));

    let older = load_session_transcript_page(
        node.as_ref(),
        "paged",
        None,
        None,
        Some("paged:91"),
        Some(10),
    )
    .await
    .expect("older page");
    assert_eq!(older.query_count, 2);
    assert!(older.has_newer);
    assert!(older
        .store
        .transcript_messages
        .iter()
        .all(|row| row.message.sequence < 91));
}

#[tokio::test]
async fn transcript_pages_preserve_acp_scope_and_sequence_atomicity() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    let headers = [
        canonical_header(
            "kept",
            "scope:kept",
            "shared",
            "did:test:selected",
            Some("did:test:local"),
            3,
        ),
        canonical_header(
            "kept_old",
            "scope:kept-old",
            "shared",
            "did:test:selected",
            Some("did:test:local"),
            2,
        ),
        canonical_header(
            "unscoped",
            "scope:unscoped",
            "shared",
            "did:test:selected",
            None,
            1,
        ),
        canonical_header(
            "wrong_agent",
            "scope:wrong-agent",
            "shared",
            "did:test:other",
            Some("did:test:local"),
            4,
        ),
        canonical_header(
            "wrong_requester",
            "scope:wrong-requester",
            "shared",
            "did:test:selected",
            Some("did:test:other"),
            5,
        ),
    ]
    .join("\n");
    let response = node.execute(&format!("mutation {{ {headers} }}")).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let page = load_session_transcript_page(
        node.as_ref(),
        "shared",
        Some("did:test:selected"),
        Some("did:test:local"),
        None,
        Some(10),
    )
    .await
    .expect("scoped page");
    assert_eq!(page.store.transcript_messages.len(), 2);
    assert!(page.store.transcript_messages.iter().all(|row| {
        row.message.agent_did == "did:test:selected"
            && row.message.requester_did.as_deref() == Some("did:test:local")
    }));

    let equal = [
        canonical_header("equal_a", "equal:a", "equal", "did:test:agent", None, 2),
        canonical_header("equal_b", "equal:b", "equal", "did:test:agent", None, 2),
        canonical_header("equal_old", "equal:old", "equal", "did:test:agent", None, 1),
    ]
    .join("\n");
    let response = node.execute(&format!("mutation {{ {equal} }}")).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let page = load_session_transcript_page(node.as_ref(), "equal", None, None, None, Some(2))
        .await
        .expect("equal-sequence page");
    assert_eq!(page.store.transcript_messages.len(), 2);
    assert!(page
        .store
        .transcript_messages
        .iter()
        .all(|row| row.message.sequence == 2));
}

#[tokio::test]
async fn transcript_pages_bound_tool_groups_without_reintroducing_response_rows() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    let tools = (1..=321)
        .map(|sequence| {
            format!(
                r#"t{sequence}: create_AgentToolCall(input: {{
                    tool_call_key: "tool-heavy:{sequence}", session_id: "tool-heavy",
                    message_sequence: {sequence}, tool_name: "bounded_tool",
                    tool_call_id: "call-{sequence}", status: "completed",
                    lifecycle_state: "completed"
                }}) {{ _docID }}"#
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let response = node.execute(&format!("mutation {{ {tools} }}")).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let page =
        load_session_transcript_page(node.as_ref(), "tool-heavy", None, None, None, Some(40))
            .await
            .expect("bounded tool page");
    assert_eq!(page.store.tool_calls.len(), 320);
    assert_eq!(page.queried_rows, 321);
    assert!(!page.source_exhausted);
}
