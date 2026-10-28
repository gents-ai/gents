use super::*;

#[tokio::test]
async fn canonical_selection_reuses_the_callers_read_snapshot() {
    let builder = tokio::task::spawn_blocking(crate::test_support::node_builder)
        .await
        .unwrap();
    let node = Arc::new(builder.build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let did = node.node_identity_did().unwrap().to_owned();
    let request = HydrationRequest {
        request_key: "atomic-peer:atomic-session".into(),
        peer_id: "atomic-peer".into(),
        requester_did: did.clone(),
        node_did: did,
        session_id: "atomic-session".into(),
    };
    ConfigAccess::transact_local_readonly(&node, None, "test.hydration.atomic_selection", |txn| {
        let request = &request;
        let writer = Arc::clone(&node);
        Box::pin(async move {
            let before = load_hydration_selection_in_txn(txn, request).await?;
            assert!(before.documents.is_empty());
            let did = escape_graphql_string(&request.node_did);
            tokio::spawn(async move {
                ConfigAccess::write_local(
                    &writer,
                    "test.hydration.concurrent_selection",
                    &format!(
                        r#"mutation {{ create_CompactionEntry(input: {{
                    compaction_key: "atomic-compaction", session_id: "atomic-session",
                    node_did: "{did}", requester_did: "{did}", sequence: 1
                    }}) {{ _docID }} }}"#,
                    ),
                )
                .await
            })
            .await??;
            let after = load_hydration_selection_in_txn(txn, request).await?;
            assert_eq!(
                after.documents, before.documents,
                "a concurrent commit must not change the selected read snapshot"
            );
            Ok::<_, anyhow::Error>(())
        })
    })
    .await
    .unwrap();
    let fresh = load_hydration_selection(&node, &request).await.unwrap();
    assert_eq!(fresh.documents.len(), 1);
    assert!(fresh.documents.iter().all(|document| document.collection
        == gents_protocol::session_hydration::SessionHydrationCollection::CompactionEntry));
    node.shutdown().await;
}

#[test]
fn missing_authorized_roots_do_not_certify_empty_selection() {
    let omitted = defra_node::QueryResponse::success(serde_json::json!({}));
    assert!(required_rows::<serde_json::Value>(&omitted, "AgentToolCall").is_err());
    let explicit = defra_node::QueryResponse::success(serde_json::json!({"AgentToolCall": []}));
    assert!(
        required_rows::<serde_json::Value>(&explicit, "AgentToolCall")
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn canonical_selection_enumerates_multiple_native_cursor_pages() {
    use gents_protocol::session_hydration::SessionHydrationCollection;
    let builder = tokio::task::spawn_blocking(crate::test_support::node_builder)
        .await
        .unwrap();
    let node = Arc::new(builder.build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let did = node.node_identity_did().unwrap().to_owned();
    let request = HydrationRequest {
        request_key: "paged-peer:paged-session".into(),
        peer_id: "paged-peer".into(),
        requester_did: did.clone(),
        node_did: did,
        session_id: "paged-session".into(),
    };
    let expected = ConfigAccess::transact_local_write(
        &node,
        None,
        "test.hydration.seed_cursor_pages",
        |txn| {
            let request = &request;
            Box::pin(async move {
                let mut expected = BTreeSet::new();
                let did = escape_graphql_string(&request.node_did);
                for index in 0..132 {
                    let session = if index < 129 {
                        "paged-session"
                    } else {
                        "other-session"
                    };
                    let response = txn
                        .execute_local_response(&format!(
                            r#"mutation {{ create_CompactionEntry(input: {{
                        compaction_key: "paged-{index}", session_id: "{session}",
                        node_did: "{did}", requester_did: "{did}", sequence: {index}
                        }}) {{ _docID }} }}"#
                        ))
                        .await?;
                    let row = crate::graphql::single_mutation_document(
                        &response,
                        "create_CompactionEntry",
                    )?
                    .context("expected one created compaction")?;
                    if index < 129 {
                        expected.insert(row["_docID"].as_str().unwrap().to_owned());
                    }
                }
                Ok::<_, anyhow::Error>(expected)
            })
        },
    )
    .await
    .unwrap();
    ConfigAccess::transact_local_readonly(
        &node,
        None,
        "test.hydration.enumerate_cursor_pages",
        |txn| {
            let request = &request;
            let expected = &expected;
            Box::pin(async move {
                let filter = crate::session::session_scope_filter(
                    &request.node_did,
                    &request.session_id,
                    Some(&request.requester_did),
                );
                let response = txn
                    .execute_local_response(&pages::page_query(
                        SessionHydrationCollection::CompactionEntry,
                        &filter,
                        None,
                    ))
                    .await?;
                let (first, next) =
                    crate::graphql::cursor_page::<serde_json::Value>(&response, "CompactionEntry")?;
                assert_eq!(first.len(), pages::PAGE_SIZE);
                assert!(next.is_some());
                let selected = load_hydration_selection_in_txn(txn, request).await?;
                assert!(selected
                    .documents
                    .iter()
                    .all(|document| document.collection
                        == SessionHydrationCollection::CompactionEntry));
                let actual = selected
                    .documents
                    .iter()
                    .map(|document| document.doc_id.clone())
                    .collect::<BTreeSet<_>>();
                assert_eq!(&actual, expected);
                assert_eq!(actual.len(), 129);
                Ok::<_, anyhow::Error>(())
            })
        },
    )
    .await
    .unwrap();
    node.shutdown().await;
}

#[tokio::test]
async fn hydration_segment_selection_uses_the_session_index() {
    use gents_protocol::session_hydration::SessionHydrationCollection;
    let builder = tokio::task::spawn_blocking(crate::test_support::node_builder)
        .await
        .unwrap();
    let node = Arc::new(builder.build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let request = HydrationRequest {
        request_key: "plan-test".into(),
        peer_id: "peer-1".into(),
        session_id: "session-1".into(),
        node_did: "did:key:agent-1".into(),
        requester_did: "did:key:requester-1".into(),
    };
    let collection = SessionHydrationCollection::AgentOutputSegment;
    let scope = crate::session_origin::SessionScope {
        node_did: request.node_did,
        session_id: request.session_id,
        requester_did: Some(request.requester_did),
    };
    let filter = pages::session_filter(collection, &scope).unwrap();
    let query = pages::page_query(collection, &filter, None);
    let plan = ConfigAccess::local(node.clone())
        .execute(&format!("query @explain {query}"))
        .await
        .unwrap()
        .to_string();
    assert!(plan.contains("AgentOutputSegment_session_id_ASC"), "{plan}");
    assert!(!plan.contains("AgentOutputSegment_node_did_ASC"), "{plan}");
    node.shutdown().await;
}

#[tokio::test]
async fn generated_terminal_selections_preserve_superseded_session_hydration() -> Result<()> {
    let node = Arc::new(crate::test_support::node_builder().build().await?);
    crate::ensure_runtime_schemas(&node).await?;
    let did = node.node_identity_did().unwrap().to_owned();
    let access = ConfigAccess::local(node.clone());
    let cases = crate::lean_vocab_test::lean_request_terminal_selection_cases();
    assert_eq!(cases.len(), 27);
    for (index, case) in cases.iter().enumerate() {
        let state = case["state"].as_str().unwrap();
        let session = format!("terminal-case-{index}");
        let value = serde_json::json!({
            "request_id":format!("terminal-request-{index}"), "purpose":"normal",
            "node_did":did, "requester_did":did, "session_id":session,
            "lifecycle_state":state, "terminal_output":case["selection"],
        });
        let response = access
            .write(
                "test.hydration.terminal_selection",
                &format!(
                    "mutation {{ create_AgentRequest(input: {}) {{ _docID }} }}",
                    gents_protocol::graphql::graphql_input_literal(&value)?
                ),
            )
            .await?;
        let physical = gents_protocol::graphql::extract_mutation_doc_id(&response, "AgentRequest")?;
        let request = HydrationRequest {
            request_key: format!("peer:{session}"),
            peer_id: "peer".into(),
            requester_did: did.clone(),
            node_did: did.clone(),
            session_id: session,
        };
        let mut row: gents_protocol::row::AgentRequestRow = serde_json::from_value(value.clone())?;
        row.doc_id = Some(physical.clone());
        let expected: Option<gents_protocol::output::TerminalOutput> =
            serde_json::from_value(case["expected"].clone())?;
        assert_eq!(row.observed_terminal_output(), expected.as_ref(), "{case}");
        if row.is_terminal() {
            let observed = crate::session::observe_request_output(&access, &row).await?;
            if expected == Some(gents_protocol::output::TerminalOutput::NoMessage) {
                assert!(matches!(
                    observed,
                    crate::session::CanonicalRequestOutput::TerminalNoMessage
                ));
            } else {
                assert!(matches!(
                    observed,
                    crate::session::CanonicalRequestOutput::Loading
                ));
            }
        }
        let retained = access
            .execute(&format!(
                "{{ AgentRequest(docIDs: [\"{}\"]) {{ terminal_output }} }}",
                escape_graphql_string(&physical),
            ))
            .await?;
        assert_eq!(
            gents_protocol::graphql::first_graphql_row(&retained, "AgentRequest")?
                ["terminal_output"],
            case["selection"],
            "observing output must not rewrite its durable selection"
        );
        let selected = load_hydration_selection(&node, &request).await;
        assert_eq!(
            selected.is_ok(),
            case["hydration_admitted"].as_bool().unwrap(),
            "model case {index}: {case}; native error: {:?}",
            selected.as_ref().err()
        );
        if let Ok(selected) = selected {
            assert!(selected.documents.iter().any(|document| document.collection
                == gents_protocol::session_hydration::SessionHydrationCollection::AgentRequest
                && document.doc_id == physical));
            assert!(
                selected.headers.is_empty(),
                "no terminal header is invented or borrowed"
            );
        }
    }
    node.shutdown().await;
    Ok(())
}
