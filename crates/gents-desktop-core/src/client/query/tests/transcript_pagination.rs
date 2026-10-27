use super::super::*;
use crate::client::schema::ensure_runtime_schemas;
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
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
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
async fn transcript_windows_read_either_way_from_a_message_number() {
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    let headers = (1..=100)
        .map(|sequence| {
            canonical_header(
                &format!("m{sequence}"),
                &format!("windowed:{sequence}"),
                "windowed",
                "did:test:node",
                None,
                sequence,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let response = node.execute(&format!("mutation {{ {headers} }}")).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let read = |window| {
        let node = node.clone();
        async move {
            let page = load_session_transcript_window(
                node.as_ref(),
                "windowed",
                None,
                None,
                window,
                Some(10),
            )
            .await
            .expect("window");
            let mut sequences = page
                .store
                .transcript_messages
                .iter()
                .map(|row| row.message.sequence)
                .collect::<Vec<_>>();
            sequences.sort_unstable();
            (
                sequences,
                page.covered,
                page.source_exhausted,
                page.query_count,
            )
        }
    };

    let (tip, covered, exhausted, queries) = read(TranscriptWindow::Tip).await;
    assert_eq!(tip, (91..=100).collect::<Vec<_>>());
    assert_eq!(
        covered,
        TranscriptSpan {
            from: Some(91),
            through: None
        }
    );
    assert!(!exhausted);
    /* a window by message number needs no cursor lookup: one read for the
    rows and one for the request they belong to */
    assert_eq!(queries, 2);

    let (older, covered, _, _) = read(TranscriptWindow::Before(91)).await;
    assert_eq!(older, (81..=90).collect::<Vec<_>>());
    assert_eq!(
        covered,
        TranscriptSpan {
            from: Some(81),
            through: Some(90)
        }
    );

    let (newer, covered, _, _) = read(TranscriptWindow::After(50)).await;
    assert_eq!(newer, (51..=60).collect::<Vec<_>>());
    assert_eq!(
        covered,
        TranscriptSpan {
            from: Some(51),
            through: Some(60)
        }
    );

    /* at either end the read proves the rest of the order */
    let (last, covered, exhausted, _) = read(TranscriptWindow::After(95)).await;
    assert_eq!(last, (96..=100).collect::<Vec<_>>());
    assert_eq!(
        covered,
        TranscriptSpan {
            from: Some(96),
            through: None
        }
    );
    assert!(exhausted);
    let (first, covered, exhausted, _) = read(TranscriptWindow::Before(5)).await;
    assert_eq!(first, (1..=4).collect::<Vec<_>>());
    assert_eq!(
        covered,
        TranscriptSpan {
            from: Some(0),
            through: Some(4)
        }
    );
    assert!(exhausted);
}

#[tokio::test]
async fn transcript_pages_preserve_acp_scope_and_sequence_cursors() {
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
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
async fn storage_rejects_same_session_sequence_twins() {
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    let twins = [
        canonical_header("first", "twins:first", "twins", "did:test:node", None, 2),
        canonical_header("second", "twins:second", "twins", "did:test:node", None, 2),
    ]
    .join("\n");
    let response = node.execute(&format!("mutation {{ {twins} }}")).await;
    assert!(
        response.has_errors(),
        "same-session sequence twins must be rejected"
    );
    assert!(
        response
            .errors
            .iter()
            .any(|error| error.message.contains("violates unique index")),
        "unexpected storage rejection: {:?}",
        response.errors
    );
}

#[tokio::test]
async fn transcript_pages_bound_tool_groups_without_reintroducing_response_rows() {
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
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
        archived_at: None,
        archive_revision: 0,
        stopped_at: None,
        title: None,
        tags: Vec::new(),
        provenance: None,
        observation: None,
    }
}

#[tokio::test]
async fn operator_reads_a_local_child_session_under_its_own_scope() {
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    let node_did = "did:test:node";
    let desktop = "did:test:desktop";
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

    assert_eq!(
        session_transcript_requester_scope(Some(&child), Some(node_did), Some(desktop), false)
            .as_deref(),
        Some(desktop)
    );
    let other = session_row("other", node_did, Some("did:test:other"));
    assert_eq!(
        session_transcript_requester_scope(Some(&other), Some(node_did), Some(desktop), true)
            .as_deref(),
        Some(desktop)
    );
    let foreign = session_row("child", "did:test:foreign", Some(node_did));
    assert_eq!(
        session_transcript_requester_scope(Some(&foreign), Some(node_did), Some(desktop), true)
            .as_deref(),
        Some(desktop)
    );
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
        None
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

#[test]
fn unsendable_reason_follows_the_session_owner() {
    let node_did = "did:test:node";
    let desktop = "did:test:desktop";
    let own = session_row("own", node_did, Some(desktop));
    assert_eq!(session_unsendable_reason(&own, desktop), None);

    let node_owned = session_row("triggered", node_did, Some(node_did));
    assert!(session_unsendable_reason(&node_owned, desktop)
        .is_some_and(|reason| reason.contains("belongs to the node")));
    assert_eq!(session_unsendable_reason(&node_owned, node_did), None);

    let mut worker = session_row("worker", node_did, Some(node_did));
    worker.provenance = Some(gents_protocol::session::SessionProvenance {
        parent_request_doc_id: Some("parent-request".to_string()),
        ..Default::default()
    });
    assert!(session_unsendable_reason(&worker, desktop)
        .is_some_and(|reason| reason.contains("session that started it")));

    let other = session_row("other", node_did, Some("did:test:other"));
    assert!(session_unsendable_reason(&other, desktop)
        .is_some_and(|reason| reason.contains("someone else")));

    let unscoped = session_row("unscoped", node_did, None);
    assert!(session_unsendable_reason(&unscoped, desktop).is_some());
}

#[tokio::test]
async fn transcript_payload_page_batches_exact_dependencies_without_reading_other_history() {
    use crate::client::canonical_output::{project_canonical_message, CanonicalMessageProjection};
    use gents::config_client::ConfigAccess;

    let node = std::sync::Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
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
            request_doc_id:"request-40", source:{kind:"authored", key:"unreferenced"}, payload:"must not read"}) { _docID }
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
        page.queried_rows, 120,
        "read only page headers, closures, and referenced sources"
    );
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

#[tokio::test]
async fn a_window_brings_results_delivered_after_its_cut_but_not_as_rows() {
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    let created = node
        .execute(
            r#"mutation { t: create_AgentToolCall(input: {
                tool_call_key: "results:tool", session_id: "results",
                request_doc_id: "request-results", message_sequence: 2,
                tool_name: "list_files", tool_call_id: "call-1",
                status: "completed", lifecycle_state: "completed"
            }) { _docID } }"#,
        )
        .await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let tool_doc_id = created.data.as_ref().expect("created tool")["t"][0]["_docID"]
        .as_str()
        .expect("tool doc id")
        .to_owned();
    let message = |alias: &str, key: &str, sequence: u32, role: &str, blocks: &str| {
        format!(
            r#"{alias}: create_AgentMessage(input: {{
                message_key: "{key}", session_id: "results", node_did: "did:test:node",
                requester_did: null, request_doc_id: "request-results",
                publication: {{kind: "request_execution", execution_generation: "test-generation"}},
                outcome: "complete", sequence: {sequence}, role: "{role}", blocks: {blocks},
                created_at: "2026-08-25T00:00:00Z"
            }}) {{ _docID }}"#
        )
    };
    let delivery = format!(
        r#"[{{type: "tool_result", tool_call_doc_id: "{tool_doc_id}", id: "result-1", call_id: "call-1", parts: []}}]"#
    );
    let messages = [
        message("m1", "results:prompt", 1, "user", "[]"),
        message("m2", "results:call", 2, "assistant", "[]"),
        message("m3", "results:delivery", 3, "user", &delivery),
        message("m4", "results:reply", 4, "assistant", "[]"),
    ]
    .join("\n");
    let response = node.execute(&format!("mutation {{ {messages} }}")).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let cut = load_session_transcript_window(
        node.as_ref(),
        "results",
        None,
        None,
        TranscriptWindow::Before(3),
        Some(10),
    )
    .await
    .expect("cut before the delivery");
    let rows = cut
        .store
        .transcript_messages
        .iter()
        .map(|row| row.message.sequence)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(rows, std::collections::BTreeSet::from([1, 2]));
    assert_eq!(cut.store.tool_calls.len(), 1);
    assert_eq!(
        cut.result_messages
            .iter()
            .map(|row| row.message.message_key.as_str())
            .collect::<Vec<_>>(),
        ["results:delivery"]
    );

    let whole = load_session_transcript_window(
        node.as_ref(),
        "results",
        None,
        None,
        TranscriptWindow::Tip,
        Some(10),
    )
    .await
    .expect("whole session");
    assert!(whole.result_messages.is_empty());
}

#[tokio::test]
async fn running_tool_pages_read_only_their_open_output_source() {
    use crate::client::canonical_output::project_canonical_live;
    use gents::config_client::ConfigAccess;
    use gents_protocol::output::{
        live::{LiveView, OwnerLiveness},
        OutputSource, OutputWriter,
    };

    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    ensure_runtime_schemas(&node).await.unwrap();
    let response = ConfigAccess::write_local(
        &node,
        "test.live_tool_page",
        r#"mutation {
        call: create_AgentToolCall(input: {
            tool_call_key:"running-tool", node_did:"node", requester_did:"reader",
            request_doc_id:"request", request_id:"request-id", session_id:"live-page",
            tool_call_id:"call", tool_name:"bash_unrestricted", message_sequence:1,
            spawned_by_tool_call_doc_id:"parent-call",
            status:"running", lifecycle_state:"running"
        }) { _docID }
    }"#,
    )
    .await
    .unwrap();
    let tool = response["data"]["call"][0]["_docID"].as_str().unwrap();
    for (owner, reader, source) in [
        ("node", "reader", tool),
        ("node", "other-reader", tool),
        ("other-node", "reader", tool),
        ("node", "reader", "unrelated-tool"),
    ] {
        ConfigAccess::write_local(&node, "test.live_tool_output", &format!(r#"mutation {{
            create_AgentOutputSegment(input: {{
                node_did:"{}", requester_did:"{}", session_id:"live-page", request_doc_id:"request",
                source:{{kind:"tool_call", tool_call_doc_id:"{}"}},
                writer:{{kind:"tool_execution", tool_call_doc_id:"{}"}}, ordinal:0,
                runs:[{{stream:0, bytes:11, declaration:{{block_index:0, part_index:0, payload:{{kind:"tool_output"}}}}}}],
                payload:"live output", created_at:"2026-10-03T00:00:00Z"
            }}) {{ _docID }}
        }}"#, escape_graphql_string(owner), escape_graphql_string(reader), escape_graphql_string(source), escape_graphql_string(source))).await.unwrap();
    }
    let page = load_session_transcript_page(
        &node,
        "live-page",
        Some("node"),
        Some("reader"),
        None,
        Some(10),
    )
    .await
    .unwrap();
    assert_eq!(page.store.tool_calls.len(), 1);
    assert_eq!(page.canonical_dependencies.output_segments.len(), 1);
    assert_eq!(page.query_count, 2);
    let source = OutputSource::ToolCall {
        tool_call_doc_id: tool.into(),
    };
    let writer = OutputWriter::ToolExecution {
        tool_call_doc_id: tool.into(),
    };
    let view = project_canonical_live(
        "request",
        "live-page",
        "request",
        &source,
        &writer,
        None,
        "node",
        Some("reader"),
        &[],
        &page.canonical_dependencies.output_segments,
        &[],
        &[],
        &[],
        OwnerLiveness {
            current_request: None,
            live_tools: vec![tool],
        },
        false,
        None,
    );
    let LiveView::Live { streams } = view else {
        panic!("running output was not projected: {view:?}");
    };
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].text, "live output");

    let tool_id = escape_graphql_string(tool);
    ConfigAccess::write_local(&node, "test.completed_spawn_output", &format!(r#"mutation {{
        update_AgentToolCall(docID:"{tool_id}", input:{{
            lifecycle_state:"completed"
        }}) {{ _docID }}
        create_AgentOutputSegment(input:{{
            node_did:"node", requester_did:"reader", session_id:"live-page", request_doc_id:"request",
            source:{{kind:"tool_call", tool_call_doc_id:"{tool_id}"}},
            writer:{{kind:"tool_execution", tool_call_doc_id:"{tool_id}"}},
            payload:"", close:{{kind:"closed", outcome:"complete", segments:1, stream_bytes:[11]}},
            created_at:"2026-10-03T00:00:01Z"
        }}) {{ _docID }}
    }}"#)).await.unwrap();
    let retained = load_session_transcript_page(
        &node,
        "live-page",
        Some("node"),
        Some("reader"),
        None,
        Some(10),
    )
    .await
    .unwrap();
    assert_eq!(
        retained.store.tool_calls[0]
            .spawned_by_tool_call_doc_id
            .as_deref(),
        Some("parent-call")
    );
    assert_eq!(retained.canonical_dependencies.output_segments.len(), 2);
    assert_eq!(retained.query_count, 2);
    let rows = &retained.canonical_dependencies.output_segments;
    let closing = rows.iter().find(|row| row.segment.close.is_some()).unwrap();
    let observed = rows
        .iter()
        .map(
            |row| gents_protocol::output::reconstruction::ObservedSegment {
                doc_id: &row.doc_id,
                segment: &row.segment,
            },
        )
        .collect::<Vec<_>>();
    let output = gents_protocol::output::reconstruction::reconstruct_stream(
        &observed,
        &[],
        &[],
        &gents_protocol::output::PayloadRef {
            close_doc_id: closing.doc_id.clone(),
            stream: 0,
        },
    )
    .unwrap();
    assert_eq!(output.text, "live output");
}
