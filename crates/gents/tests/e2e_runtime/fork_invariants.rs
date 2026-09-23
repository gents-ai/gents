use std::sync::Arc;

use gents::graphql::escape_graphql_string;
use gents::session::canonical_rows::{
    decode_transcript_message_row, output_segment_create_variables,
    transcript_message_create_variables, AGENT_MESSAGE_FIELDS, CREATE_AGENT_MESSAGE_MUTATION,
    CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents::session::{fork, fork_via_http, ForkError, ForkParams};
use gents_protocol::output::*;

use crate::support::snapshots::{
    fetch_compaction_entry_snapshots_for_session, fetch_session_snapshot,
};
use crate::support::{
    create_agent_behavior, create_agent_session, create_agent_tool_call, create_compaction_entry,
    create_request, test_db, AGENT_DID, AGENT_NAME,
};

#[derive(Debug)]
struct Imported {
    header: String,
    close: Option<String>,
}

/// Import a native observation without claiming live publication authority.
async fn import_message(
    node: &defra_node::EmbeddedNode,
    session: &str,
    sequence: u32,
    role: MessageRole,
    text: &str,
    valid_ref: bool,
) -> Imported {
    import_scoped_message(node, session, sequence, role, text, valid_ref, None).await
}

async fn import_scoped_message(
    node: &defra_node::EmbeddedNode,
    session: &str,
    sequence: u32,
    role: MessageRole,
    text: &str,
    valid_ref: bool,
    requester_did: Option<&str>,
) -> Imported {
    let request = format!("fixture-request:{session}:{sequence}");
    let close = if valid_ref {
        let segment = OutputSegment {
            agent_did: AGENT_DID.into(),
            requester_did: requester_did.map(str::to_owned),
            session_id: session.into(),
            request_doc_id: request.clone(),
            source: if role == MessageRole::Assistant {
                OutputSource::ProviderTurn {
                    scope: gents_protocol::rendered_request::CaptureScope {
                        kind: gents_protocol::rendered_request::CaptureScopeKind::Inference,
                        seq: u64::from(sequence),
                    },
                    turn_index: 0,
                    attempt: 0,
                }
            } else {
                OutputSource::Authored {
                    key: format!("fixture:{sequence}"),
                }
            },
            writer: OutputWriter::RequestExecution {
                execution_generation: "fixture-generation".into(),
            },
            ordinal: Some(0),
            runs: vec![SegmentRun {
                stream: 0,
                bytes: text.len().try_into().unwrap(),
                declaration: Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::Text,
                }),
            }],
            payload: text.into(),
            close: Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 1,
                stream_bytes: vec![text.len() as u64],
            }),
            created_at: format!("2026-04-21T10:00:{sequence:02}Z"),
        };
        let response = node
            .execute_request_with_retry(
                defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                    .with_variables(output_segment_create_variables(&segment).unwrap()),
                defra_node::ExecuteRetryPolicy::default(),
            )
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        Some(created_id(&response, "create_AgentOutputSegment"))
    } else {
        None
    };
    let message = TranscriptMessage {
        message_key: format!("fixture:{session}:{sequence}"),
        session_id: session.into(),
        agent_did: AGENT_DID.into(),
        requester_did: requester_did.map(str::to_owned),
        request_doc_id: Some(request),
        publication: MessagePublication::RequestExecution {
            execution_generation: "fixture-generation".into(),
        },
        outcome: OutputOutcome::Complete,
        sequence,
        role,
        native_id: (role == MessageRole::Assistant).then(|| format!("native-{sequence}")),
        blocks: vec![MessageBlock::Text {
            text: PresentedPayload {
                output: PayloadRef {
                    close_doc_id: close.clone().unwrap_or_else(|| "missing-close".into()),
                    stream: 0,
                },
                presentation: PayloadPresentation::Full,
            },
        }],
        created_at: format!("2026-04-21T10:00:{sequence:02}Z"),
    };
    let response = node
        .execute_request_with_retry(
            defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&message).unwrap()),
            defra_node::ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    Imported {
        header: created_id(&response, "create_AgentMessage"),
        close,
    }
}

fn created_id(response: &defra_node::QueryResponse, collection: &str) -> String {
    gents::graphql::single_mutation_document(response, collection)
        .unwrap()
        .unwrap()["_docID"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn headers(
    node: &defra_node::EmbeddedNode,
    session: &str,
) -> Vec<gents::session::canonical_rows::TranscriptMessageRow> {
    let session = escape_graphql_string(session);
    let response = node.execute(&format!(r#"{{ AgentMessage(filter: {{session_id: {{_eq: "{session}"}}}}, order: {{sequence: ASC}}) {{ {AGENT_MESSAGE_FIELDS} }} }}"#)).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    response.data.unwrap()["AgentMessage"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| decode_transcript_message_row(row).unwrap())
        .collect()
}

async fn setup(node: &defra_node::EmbeddedNode, session: &str) {
    create_agent_session(node, session, AGENT_NAME, "2026-04-21T10:00:00Z").await;
    create_agent_behavior(node, AGENT_NAME, AGENT_DID).await;
}

fn params(session: &str, turn: u32) -> ForkParams<'_> {
    ForkParams {
        source_session_id: session,
        fork_at_user_turn: turn,
        caller_agent_did: AGENT_DID,
        caller_requester_did: None,
        target_behavior_id: None,
    }
}

fn origin(
    session: &gents_protocol::session::AgentSession,
) -> &gents_protocol::session::SessionFork {
    session
        .provenance
        .as_ref()
        .and_then(|value| value.fork.as_ref())
        .unwrap()
}

#[tokio::test]
async fn fork_copies_exact_header_prefix_by_reference() {
    let db = test_db("fork-prefix").await;
    setup(&db.node, "parent").await;
    let one = import_message(&db.node, "parent", 1, MessageRole::User, "u1", true).await;
    let two = import_message(&db.node, "parent", 2, MessageRole::Assistant, "a1", true).await;
    import_message(&db.node, "parent", 3, MessageRole::User, "u2", true).await;
    let outcome = fork(&db.node, params("parent", 1)).await.unwrap();
    assert_eq!(
        (
            outcome.copied_messages,
            outcome.copied_tool_calls,
            outcome.copied_tool_results
        ),
        (2, 0, 0)
    );
    let copied = headers(&db.node, &outcome.session_id).await;
    let unchanged_parent = headers(&db.node, "parent").await;
    assert_eq!(unchanged_parent.len(), 3);
    assert!(matches!(
        &unchanged_parent[0].message.publication,
        MessagePublication::RequestExecution { .. }
    ));
    assert_eq!(copied.len(), 2);
    for (copy, source) in copied.iter().zip([one, two]) {
        assert_eq!(copy.message.request_doc_id, None);
        assert!(
            matches!(&copy.message.publication, MessagePublication::Fork { origin_message_doc_id } if origin_message_doc_id == &source.header)
        );
        let MessageBlock::Text { text } = &copy.message.blocks[0] else {
            panic!("text")
        };
        assert_eq!(&text.output.close_doc_id, source.close.as_ref().unwrap());
    }
    let (_, native) = gents::session::load_canonical_message_from_node(
        &db.node,
        &copied[0].doc_id,
        AGENT_DID,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        gents_protocol::transcript::present_message(&native).body_markdown,
        "u1"
    );
    let (_, assistant) = gents::session::load_canonical_message_from_node(
        &db.node,
        &copied[1].doc_id,
        AGENT_DID,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        gents_protocol::transcript::present_message(&assistant).body_markdown,
        "a1"
    );
    let segments = db
        .node
        .execute("{ AgentOutputSegment { session_id } }")
        .await
        .data
        .unwrap();
    let segments = segments["AgentOutputSegment"].as_array().unwrap();
    assert_eq!(segments.len(), 3);
    assert!(segments.iter().all(|row| row["session_id"] == "parent"));
}

#[tokio::test]
async fn fork_via_http_uses_same_header_contract() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .with_http(defra_node::HttpConfig::with_addr(address))
            .build()
            .await
            .unwrap(),
    );
    gents::ensure_runtime_schemas(&node).await.unwrap();
    let endpoint = format!("http://{address}/api/v0/graphql");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if reqwest::Client::new()
                .post(&endpoint)
                .json(&serde_json::json!({"query":"{__typename}"}))
                .send()
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    setup(&node, "parent-http").await;
    let source = import_message(&node, "parent-http", 1, MessageRole::User, "u", true).await;
    let outcome = fork_via_http(&endpoint, params("parent-http", 1))
        .await
        .unwrap();
    let copied = headers(&node, &outcome.session_id).await;
    assert!(
        matches!(&copied[0].message.publication, MessagePublication::Fork { origin_message_doc_id } if origin_message_doc_id == &source.header)
    );
}

#[tokio::test]
async fn fork_preserves_exact_requester_scope_and_denies_cross_requester_access() {
    let db = test_db("fork-requester-scope").await;
    let requester = "did:key:requester-a";
    let mut session =
        crate::support::session_document("requester-parent", AGENT_NAME, "2026-04-21T10:00:00Z");
    session.requester_did = Some(requester.into());
    crate::support::create_session_document(&db.node, &session).await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;
    let source = import_scoped_message(
        &db.node,
        "requester-parent",
        1,
        MessageRole::User,
        "tenant-bound",
        true,
        Some(requester),
    )
    .await;
    let outcome = fork(
        &db.node,
        ForkParams {
            caller_requester_did: Some(requester),
            ..params("requester-parent", 1)
        },
    )
    .await
    .unwrap();
    let child_session = fetch_session_snapshot(&db.node, &outcome.session_id)
        .await
        .unwrap();
    assert_eq!(child_session.requester_did.as_deref(), Some(requester));
    let copied = headers(&db.node, &outcome.session_id).await;
    assert_eq!(copied[0].message.requester_did.as_deref(), Some(requester));
    assert!(
        matches!(&copied[0].message.publication, MessagePublication::Fork { origin_message_doc_id } if origin_message_doc_id == &source.header)
    );
    let (_, native) = gents::session::load_canonical_message_from_node(
        &db.node,
        &copied[0].doc_id,
        AGENT_DID,
        Some(requester),
    )
    .await
    .unwrap();
    assert_eq!(
        gents_protocol::transcript::present_message(&native).body_markdown,
        "tenant-bound"
    );
    let denied = fork(
        &db.node,
        ForkParams {
            caller_requester_did: Some("did:key:requester-b"),
            ..params("requester-parent", 1)
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(
        denied,
        ForkError::ForkSourceNotFound(_) | ForkError::ForkNotSameAgent
    ));
}

#[tokio::test]
async fn fork_never_copies_executable_tool_rows() {
    let db = test_db("fork-no-tools").await;
    setup(&db.node, "parent-tools").await;
    import_message(&db.node, "parent-tools", 1, MessageRole::User, "u", true).await;
    create_agent_tool_call(
        &db.node,
        "parent-tools",
        1,
        "call",
        "read_file",
        "{}",
        "done",
        "completed",
        "2026-04-21T10:00:01Z",
        "2026-04-21T10:00:02Z",
    )
    .await;
    let outcome = fork(&db.node, params("parent-tools", 1)).await.unwrap();
    assert_eq!(
        (outcome.copied_tool_calls, outcome.copied_tool_results),
        (0, 0)
    );
    let child = escape_graphql_string(&outcome.session_id);
    let response = db
        .node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{session_id: {{_eq: "{child}"}}}}) {{_docID}} }}"#
        ))
        .await;
    assert!(response.data.unwrap()["AgentToolCall"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn fork_copies_only_compaction_cursors_in_retained_prefix() {
    let db = test_db("fork-compaction").await;
    setup(&db.node, "parent-compaction").await;
    import_message(
        &db.node,
        "parent-compaction",
        1,
        MessageRole::User,
        "u1",
        true,
    )
    .await;
    import_message(
        &db.node,
        "parent-compaction",
        2,
        MessageRole::User,
        "u2",
        true,
    )
    .await;
    create_compaction_entry(
        &db.node,
        "parent-compaction",
        1,
        "early",
        1,
        1,
        "2026-04-21T10:00:03Z",
    )
    .await;
    create_compaction_entry(
        &db.node,
        "parent-compaction",
        2,
        "late",
        2,
        2,
        "2026-04-21T10:00:04Z",
    )
    .await;
    let outcome = fork(&db.node, params("parent-compaction", 1))
        .await
        .unwrap();
    let rows = fetch_compaction_entry_snapshots_for_session(&db.node, &outcome.session_id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].summary, "early");
    assert_eq!(rows[0].compacted_through_sequence, Some(1));
    assert_eq!(rows[0].request_doc_id, None);
}

#[tokio::test]
async fn fork_boundaries_and_provenance_are_exact() {
    let db = test_db("fork-boundaries").await;
    setup(&db.node, "parent-boundaries").await;
    for (seq, role) in [
        (1, MessageRole::User),
        (2, MessageRole::Assistant),
        (3, MessageRole::User),
        (4, MessageRole::Assistant),
    ] {
        import_message(&db.node, "parent-boundaries", seq, role, "text", true).await;
    }
    let empty = fork(&db.node, params("parent-boundaries", 0))
        .await
        .unwrap();
    let full = fork(&db.node, params("parent-boundaries", 2))
        .await
        .unwrap();
    assert_eq!(empty.copied_messages, 0);
    assert_eq!(full.copied_messages, 4);
    let session = fetch_session_snapshot(&db.node, &empty.session_id)
        .await
        .unwrap();
    assert_eq!(origin(&session).source_session_id, "parent-boundaries");
    assert_eq!(origin(&session).at_user_turn, 0);
}

#[tokio::test]
async fn fork_rejects_invalid_cut_and_rolls_back_invalid_reference() {
    let db = test_db("fork-invalid").await;
    setup(&db.node, "parent-invalid").await;
    import_message(
        &db.node,
        "parent-invalid",
        1,
        MessageRole::User,
        "valid",
        true,
    )
    .await;
    let range = fork(&db.node, params("parent-invalid", 2))
        .await
        .unwrap_err();
    assert!(matches!(range, ForkError::ForkAtUserTurnOutOfRange(2, 1)));
    import_message(
        &db.node,
        "parent-invalid",
        2,
        MessageRole::Assistant,
        "invalid",
        false,
    )
    .await;
    let missing = fork(&db.node, params("parent-invalid", 1))
        .await
        .unwrap_err();
    let ForkError::ForkCopyFailed(cause) = missing else {
        panic!("missing closure must reject the copy step: {missing:?}");
    };
    let missing_detail = format!("{cause:#}");
    assert!(
        missing_detail.contains("unresolved closing segment missing-close"),
        "fixture's only defect must be its missing closure: {missing_detail}"
    );
    let sessions = db
        .node
        .execute("{ AgentSession { session_id } }")
        .await
        .data
        .unwrap();
    assert_eq!(sessions["AgentSession"].as_array().unwrap().len(), 1);
    let messages = db
        .node
        .execute("{ AgentMessage { session_id } }")
        .await
        .data
        .unwrap();
    assert!(messages["AgentMessage"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["session_id"] == "parent-invalid"));
}

#[tokio::test]
async fn fork_accepts_same_principal_behavior_swap() {
    let db = test_db("fork-behavior").await;
    setup(&db.node, "parent-behavior").await;
    create_agent_behavior(&db.node, "alternate", AGENT_DID).await;
    create_agent_behavior(&db.node, "foreign", "did:key:foreign").await;
    import_message(&db.node, "parent-behavior", 1, MessageRole::User, "u", true).await;
    let outcome = fork(
        &db.node,
        ForkParams {
            target_behavior_id: Some("alternate"),
            ..params("parent-behavior", 1)
        },
    )
    .await
    .unwrap();
    assert_eq!(
        fetch_session_snapshot(&db.node, &outcome.session_id)
            .await
            .unwrap()
            .behavior_id,
        "alternate"
    );
    assert!(matches!(
        fork(
            &db.node,
            ForkParams {
                target_behavior_id: Some("missing"),
                ..params("parent-behavior", 1)
            }
        )
        .await
        .unwrap_err(),
        ForkError::ForkBehaviorNotFound(_)
    ));
    assert!(matches!(
        fork(
            &db.node,
            ForkParams {
                target_behavior_id: Some("foreign"),
                ..params("parent-behavior", 1)
            }
        )
        .await
        .unwrap_err(),
        ForkError::ForkBehaviorNotFound(_)
    ));
}

#[tokio::test]
async fn concurrent_forks_are_disjoint() {
    let db = test_db("fork-concurrent").await;
    setup(&db.node, "parent-concurrent").await;
    import_message(
        &db.node,
        "parent-concurrent",
        1,
        MessageRole::User,
        "u",
        true,
    )
    .await;
    let (left, right) = tokio::join!(
        fork(&db.node, params("parent-concurrent", 1)),
        fork(&db.node, params("parent-concurrent", 1))
    );
    let (left, right) = (left.unwrap(), right.unwrap());
    assert_ne!(left.session_id, right.session_id);
    let left = headers(&db.node, &left.session_id).await.remove(0);
    let right = headers(&db.node, &right.session_id).await.remove(0);
    assert_ne!(left.doc_id, right.doc_id);
    assert_ne!(left.message.message_key, right.message.message_key);
}

#[tokio::test]
async fn fork_of_fork_names_immediate_origin_and_hydrates() {
    let db = test_db("fork-ancestry").await;
    setup(&db.node, "grandparent").await;
    import_message(
        &db.node,
        "grandparent",
        1,
        MessageRole::User,
        "ancestry",
        true,
    )
    .await;
    let child = fork(&db.node, params("grandparent", 1)).await.unwrap();
    let child_header = headers(&db.node, &child.session_id).await.remove(0);
    let grandchild = fork(&db.node, params(&child.session_id, 1)).await.unwrap();
    let grandchild_header = headers(&db.node, &grandchild.session_id).await.remove(0);
    assert!(
        matches!(&grandchild_header.message.publication, MessagePublication::Fork { origin_message_doc_id } if origin_message_doc_id == &child_header.doc_id)
    );
    let (_, native) = gents::session::load_canonical_message_from_node(
        &db.node,
        &grandchild_header.doc_id,
        AGENT_DID,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        gents_protocol::transcript::present_message(&native).body_markdown,
        "ancestry"
    );
    assert_eq!(
        origin(
            &fetch_session_snapshot(&db.node, &grandchild.session_id)
                .await
                .unwrap()
        )
        .source_session_id,
        child.session_id
    );
}

#[tokio::test]
async fn fork_rejects_busy_wrong_principal_and_missing_source() {
    let db = test_db("fork-rejections").await;
    setup(&db.node, "parent-rejections").await;
    create_request(
        &db.node,
        "pending",
        "parent-rejections",
        "pending",
        "2026-04-21T10:00:01Z",
    )
    .await;
    assert!(matches!(
        fork(&db.node, params("parent-rejections", 0))
            .await
            .unwrap_err(),
        ForkError::ForkSourceBusy
    ));
    let wrong = fork(
        &db.node,
        ForkParams {
            caller_agent_did: "did:key:foreign",
            ..params("parent-rejections", 0)
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(
        wrong,
        ForkError::ForkSourceNotFound(_) | ForkError::ForkNotSameAgent
    ));
    assert!(matches!(
        fork(&db.node, params("absent", 0)).await.unwrap_err(),
        ForkError::ForkSourceNotFound(_)
    ));
}
