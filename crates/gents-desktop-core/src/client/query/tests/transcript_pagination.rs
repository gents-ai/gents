use super::super::*;
use crate::client::schema::ensure_runtime_schemas;
use defra_node::NodeBuilder;
use std::sync::Arc;

fn canonical_header(
    alias: &str,
    key: &str,
    session: &str,
    node_did: &str,
    requester: Option<&str>,
    sequence: u32,
) -> String {
    let requester = requester
        .map(|did| format!("\"{did}\""))
        .unwrap_or_else(|| "null".into());
    format!(
        r#"{alias}: create_AgentMessage(input: {{
            message_key: "{key}", session_id: "{session}", node_did: "{node_did}",
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
                "did:test:node",
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
async fn transcript_pages_preserve_acp_scope_and_sequence_cursors() {
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
            "wrong_node",
            "scope:wrong-node",
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
        row.message.node_did == "did:test:selected"
            && row.message.requester_did.as_deref() == Some("did:test:local")
    }));

    let equal = [
        canonical_header("equal_a", "equal:a", "equal", "did:test:node", None, 3),
        canonical_header("equal_b", "equal:b", "equal", "did:test:node", None, 2),
        canonical_header("equal_old", "equal:old", "equal", "did:test:node", None, 1),
    ]
    .join("\n");
    let response = node.execute(&format!("mutation {{ {equal} }}")).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let page = load_session_transcript_page(node.as_ref(), "equal", None, None, None, Some(2))
        .await
        .expect("newest sequence page");
    assert_eq!(page.store.transcript_messages.len(), 2);
    let mut sequences = page
        .store
        .transcript_messages
        .iter()
        .map(|row| row.message.sequence)
        .collect::<Vec<_>>();
    sequences.sort_unstable();
    assert_eq!(sequences, vec![2, 3]);

    let older =
        load_session_transcript_page(node.as_ref(), "equal", None, None, Some("equal:b"), Some(2))
            .await
            .expect("older sequence page");
    assert_eq!(older.store.transcript_messages.len(), 1);
    assert_eq!(older.store.transcript_messages[0].message.sequence, 1);
}

#[tokio::test]
async fn transcript_page_rejects_same_session_sequence_twins() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    let twins = [
        canonical_header("first", "twins:first", "twins", "did:test:node", None, 2),
        canonical_header("second", "twins:second", "twins", "did:test:node", None, 2),
    ]
    .join("\n");
    let response = node.execute(&format!("mutation {{ {twins} }}")).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let error = load_session_transcript_page(node.as_ref(), "twins", None, None, None, Some(2))
        .await
        .err()
        .expect("same-session sequence twins must conflict");
    assert!(error
        .to_string()
        .contains("canonical header origin: Conflict"));
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

fn session_row(session_id: &str, node_did: &str, requester: Option<&str>) -> AgentSession {
    AgentSession {
        session_id: session_id.into(),
        node_did: node_did.into(),
        requester_did: requester.map(str::to_owned),
        agent_id: "default".into(),
        created_at: "2026-08-25T00:00:00Z".into(),
        closed_at: None,
        title: None,
        tags: Vec::new(),
        provenance: None,
        observation: None,
    }
}

#[tokio::test]
async fn operator_reads_a_local_child_session_under_its_own_scope() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    let node_did = "did:test:node";
    let desktop = "did:test:desktop";
    // A LocalChild is admitted with the node's own DID as requester.
    let headers = [
        canonical_header("child_a", "child:a", "child", node_did, Some(node_did), 1),
        canonical_header("child_b", "child:b", "child", node_did, Some(node_did), 2),
        canonical_header(
            "other",
            "other:a",
            "other",
            node_did,
            Some("did:test:other"),
            1,
        ),
    ]
    .join("\n");
    let response = node.execute(&format!("mutation {{ {headers} }}")).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let desktop_page = load_session_transcript_page(
        node.as_ref(),
        "child",
        Some(node_did),
        Some(desktop),
        None,
        None,
    )
    .await
    .expect("desktop-scoped page");
    assert!(
        desktop_page.store.transcript_messages.is_empty(),
        "the desktop scope cannot see a LocalChild transcript"
    );

    let child = session_row("child", node_did, Some(node_did));
    let scope =
        session_transcript_requester_scope(Some(&child), Some(node_did), Some(desktop), true);
    assert_eq!(scope.as_deref(), Some(node_did));
    let page = load_session_transcript_page(
        node.as_ref(),
        "child",
        Some(node_did),
        scope.as_deref(),
        None,
        None,
    )
    .await
    .expect("child page");
    assert_eq!(page.store.transcript_messages.len(), 2);

    // Without operator authority the desktop scope stands.
    assert_eq!(
        session_transcript_requester_scope(Some(&child), Some(node_did), Some(desktop), false)
            .as_deref(),
        Some(desktop)
    );
    // Another requester's session is never widened into view.
    let other = session_row("other", node_did, Some("did:test:other"));
    assert_eq!(
        session_transcript_requester_scope(Some(&other), Some(node_did), Some(desktop), true)
            .as_deref(),
        Some(desktop)
    );
    // A session of a different node does not borrow this node's scope.
    let foreign = session_row("child", "did:test:foreign", Some(node_did));
    assert_eq!(
        session_transcript_requester_scope(Some(&foreign), Some(node_did), Some(desktop), true)
            .as_deref(),
        Some(desktop)
    );
    // The operator's own sessions keep the desktop scope.
    let own = session_row("own", node_did, Some(desktop));
    assert_eq!(
        session_transcript_requester_scope(Some(&own), Some(node_did), Some(desktop), true)
            .as_deref(),
        Some(desktop)
    );
    assert_eq!(
        session_transcript_requester_scope(None, Some(node_did), Some(desktop), true).as_deref(),
        Some(desktop)
    );
}

#[test]
fn unreadable_reason_follows_the_exact_read_scope() {
    let node_did = "did:test:node";
    let desktop = "did:test:desktop";
    let own = session_row("own", node_did, Some(desktop));
    assert_eq!(session_unreadable_reason(&own, Some(desktop), false), None);

    let node_owned = session_row("child", node_did, Some(node_did));
    assert_eq!(
        session_unreadable_reason(&node_owned, Some(desktop), true),
        None,
        "the operator reads the node's own session under its own scope"
    );
    assert!(session_unreadable_reason(&node_owned, Some(desktop), false)
        .is_some_and(|reason| reason.contains("owned by its node")));

    let other = session_row("other", node_did, Some("did:test:other"));
    assert!(session_unreadable_reason(&other, Some(desktop), true)
        .is_some_and(|reason| reason.contains("another requester")));

    let unscoped = session_row("unscoped", node_did, None);
    assert!(session_unreadable_reason(&unscoped, Some(desktop), true).is_some());
    assert_eq!(session_unreadable_reason(&unscoped, None, false), None);
}

#[tokio::test]
async fn transcript_payload_page_batches_exact_dependencies_without_reading_other_history() {
    use crate::client::canonical_output::{project_canonical_message, CanonicalMessageProjection};
    use gents::config_client::ConfigAccess;

    let node = NodeBuilder::default().build().await.expect("node");
    ensure_runtime_schemas(&node).await.expect("schemas");
    for sequence in 1..=40 {
        let response = ConfigAccess::write_local(&node, "test.payload_page", &format!(r#"mutation {{
            segment: create_AgentOutputSegment(input: {{
                node_did: "node", requester_did: "reader", session_id: "paged-payload",
                request_doc_id: "request-{sequence}", source: {{kind: "authored", key: "answer"}},
                writer: {{kind: "request_execution", execution_generation: "generation"}},
                ordinal: 0, runs: [{{stream: 0, bytes: 1, declaration: {{block_index: 0, part_index: 0, payload: {{kind: "text"}}}}}}],
                payload: "x", close: {{kind: "closed", outcome: "complete", segments: 1, stream_bytes: [1]}},
                created_at: "2026-10-03T00:00:00Z"
            }}) {{ _docID }}
        }}"#)).await.expect("segment");
        let close = response["data"]["segment"][0]["_docID"]
            .as_str()
            .expect("segment id");
        let close = escape_graphql_string(close);
        ConfigAccess::write_local(&node, "test.payload_page", &format!(r#"mutation {{
            create_AgentMessage(input: {{
                message_key: "answer-{sequence}", session_id: "paged-payload", node_did: "node", requester_did: "reader",
                request_doc_id: "request-{sequence}", publication: {{kind: "request_execution", execution_generation: "generation"}},
                outcome: "complete", sequence: {sequence}, role: "assistant",
                blocks: [{{type: "text", text: {{output: {{close_doc_id: "{close}", stream: 0}}, presentation: {{kind: "full"}}}}}}],
                created_at: "2026-10-03T00:00:00Z"
            }}) {{ _docID }}
        }}"#)).await.expect("header");
    }
    ConfigAccess::write_local(&node, "test.unrelated_history", r#"mutation {
        create_AgentOutputSegment(input: {node_did:"node", requester_did:"reader", session_id:"paged-payload",
            request_doc_id:"unreferenced", source:{kind:"not_a_source"}, payload:"must not read"}) { _docID }
    }"#).await.expect("unrelated history");
    let page = load_session_transcript_page(
        &node,
        "paged-payload",
        Some("node"),
        Some("reader"),
        None,
        Some(40),
    )
    .await
    .expect("page");
    assert_eq!(page.store.transcript_messages.len(), 40);
    assert_eq!(page.canonical_dependencies.output_segments.len(), 40);
    assert_eq!(
        page.query_count, 8,
        "one page, two closure batches, five source batches"
    );
    for header in &page.store.transcript_messages {
        assert!(matches!(
            project_canonical_message(
                header,
                &page.canonical_dependencies.output_segments,
                &[],
                &[]
            ),
            CanonicalMessageProjection::Ready(_)
        ));
    }
    let other = load_session_transcript_page(
        &node,
        "paged-payload",
        Some("node"),
        Some("other-reader"),
        None,
        Some(40),
    )
    .await
    .expect("other scope");
    assert!(other.store.transcript_messages.is_empty());
    assert!(other.canonical_dependencies.output_segments.is_empty());
}
