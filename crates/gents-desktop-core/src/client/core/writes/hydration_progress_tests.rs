use super::*;

#[tokio::test]
async fn local_hydration_counts_exact_origin_documents_without_reading_the_origin_session() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::client::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let mut roots = BTreeSet::new();
    let mut origins = BTreeSet::new();
    let mut manifest = BTreeSet::new();
    for (collection_name, collection) in [
        ("AgentRequest", SessionHydrationCollection::AgentRequest),
        ("AgentMessage", SessionHydrationCollection::AgentMessage),
        (
            "AgentOutputSegment",
            SessionHydrationCollection::AgentOutputSegment,
        ),
        ("AgentToolCall", SessionHydrationCollection::AgentToolCall),
        (
            "CompactionEntry",
            SessionHydrationCollection::CompactionEntry,
        ),
    ] {
        for (ordinal, (label, requester, session, owner)) in [
            ("root", "reader", "child", "owner"),
            ("origin", "reader", "parent", "owner"),
            ("unreferenced", "reader", "parent", "owner"),
            ("foreign-reader", "other-reader", "parent", "owner"),
            ("foreign-owner", "reader", "parent", "other-owner"),
        ]
        .into_iter()
        .enumerate()
        {
            let label = gents::graphql::escape_graphql_string(label);
            let requester = gents::graphql::escape_graphql_string(requester);
            let session = gents::graphql::escape_graphql_string(session);
            let owner = gents::graphql::escape_graphql_string(owner);
            let identity = match collection {
                SessionHydrationCollection::AgentMessage => {
                    format!("message_key: \"{label}\", sequence: {ordinal},")
                }
                SessionHydrationCollection::AgentRequest => {
                    format!("request_id: \"{label}\", purpose: \"normal\",")
                }
                SessionHydrationCollection::AgentOutputSegment => format!("payload: \"{label}\","),
                SessionHydrationCollection::AgentToolCall => format!("tool_call_id: \"{label}\","),
                SessionHydrationCollection::CompactionEntry => {
                    format!("compaction_key: \"{label}\",")
                }
            };
            let response = ConfigAccess::write_local(
                &node,
                "test.hydration_document",
                &format!(
                    "mutation {{ create_{collection_name}(input: {{ {identity} requester_did: \"{requester}\", session_id: \"{session}\", node_did: \"{owner}\" }}) {{ _docID }} }}"
                ),
            )
            .await
            .unwrap();
            let key = SessionHydrationDocumentKey {
                collection,
                doc_id: gents::graphql::created_doc_id(&response, collection_name).unwrap(),
            };
            match label.as_str() {
                "root" => {
                    roots.insert(key);
                }
                "origin" => {
                    origins.insert(key.clone());
                    manifest.insert(key);
                }
                "foreign-reader" | "foreign-owner" => {
                    manifest.insert(key);
                }
                _ => {}
            }
        }
    }
    let local = load_local_hydration_documents(&node, "reader", "child", "owner", None)
        .await
        .unwrap();
    assert_eq!(local, roots);
    let response = gents::graphql::graphql_with_transaction_retry(
        &node,
        &local_hydration_query("reader", "child", "owner", Some(&manifest)),
        "test hydration manifest coverage",
    )
    .await
    .unwrap();
    let actual = local_hydration_documents_from_response(&response).unwrap();
    let expected = roots.union(&origins).cloned().collect();
    assert_eq!(
        actual, expected,
        "a fork's signed origin keys are already local"
    );
    let served = roots.union(&origins).cloned().collect::<BTreeSet<_>>();
    use gents::agent::p2p_reconcile::session_hydration::{
        project_durable_hydration_progress, ClientHydrationPhase, ClientHydrationRequestState,
    };
    let complete = project_durable_hydration_progress(
        "child",
        "owner",
        actual.clone(),
        ClientHydrationRequestState::Served(served.clone()),
    );
    assert_eq!(complete.phase, ClientHydrationPhase::Complete);
    assert_eq!(complete.covered_count, served.len());
    let incomplete = project_durable_hydration_progress(
        "child",
        "owner",
        actual,
        ClientHydrationRequestState::Served(manifest),
    );
    assert_eq!(incomplete.phase, ClientHydrationPhase::Serving);
    assert_eq!(incomplete.covered_count, origins.len());
    node.shutdown().await;
}

#[test]
fn local_hydration_escapes_manifest_keys_and_keeps_empty_collections_session_scoped() {
    let manifest = BTreeSet::from([SessionHydrationDocumentKey {
        collection: SessionHydrationCollection::AgentOutputSegment,
        doc_id: "origin\"escaped".into(),
    }]);
    let query = local_hydration_query("reader", "child", "owner", Some(&manifest));
    assert_eq!(query.matches(r#"origin\"escaped"#).count(), 1);
    assert!(!query.contains("_in: []"));
    assert!(!query.contains("docIDs: []"));
    assert_eq!(query.matches("_or:").count(), 1);
    assert_eq!(
        query.matches("session_id:").count(),
        HYDRATION_COLLECTIONS.len()
    );
    assert_eq!(
        local_hydration_query("reader", "child", "owner", Some(&BTreeSet::new())),
        local_hydration_query("reader", "child", "owner", None),
    );
}
