use super::*;

async fn request(
    access: &crate::config_client::ConfigAccess,
    session: &str,
    requester: Option<&str>,
    created: &str,
) -> String {
    let input = gents_protocol::graphql::graphql_input_literal(&json!({
            "purpose": "normal", "request_id": "shared-logical-id", "node_did": "did:test:accounting",
            "requester_did": requester, "session_id": session, "created_at": created,
            "lifecycle_state": "pending", "agent_id": "default", "backend_id": "backend", "content": "test",
        })).unwrap();
    let data = access
        .write(
            "context_accounting.request_fixture",
            &format!("mutation {{ create_AgentRequest(input: {input}) {{ _docID }} }}"),
        )
        .await
        .unwrap();
    crate::graphql::created_doc_id(&data, "AgentRequest").unwrap()
}

async fn call(
    access: &crate::config_client::ConfigAccess,
    doc: &str,
    call_id: &str,
    session: Option<&str>,
    queued_at: Option<&str>,
    sequence: Option<i64>,
) {
    let input = gents_protocol::graphql::graphql_input_literal(&json!({
        "call_id": call_id, "request_id": "shared-logical-id", "request_doc_id": doc,
        "session_id": session, "node_did": "did:test:accounting", "call_kind": "inference",
        "queued_at": queued_at, "call_seq": sequence, "context_accounting_json": "{}",
    }))
    .unwrap();
    access
        .write(
            "context_accounting.call_fixture",
            &format!("mutation {{ create_InferenceCall(input: {input}) {{ _docID }} }}"),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn inference_order_and_physical_scope_survive_index_hints() {
    let db = crate::support::test_db("indexed-context-scopes").await;
    let access = crate::config_client::ConfigAccess::local(db.node.clone());
    let old = request(
        &access,
        "session",
        Some("did:test:reader"),
        "2026-10-01T00:00:00Z",
    )
    .await;
    let new = request(
        &access,
        "session",
        Some("did:test:reader"),
        "2026-10-02T00:00:00Z",
    )
    .await;
    let foreign_session = request(
        &access,
        "other",
        Some("did:test:reader"),
        "2026-10-03T00:00:00Z",
    )
    .await;
    let foreign_requester = request(&access, "session", None, "2026-10-03T00:00:00Z").await;
    let input = gents_protocol::graphql::graphql_input_literal(&json!({
        "purpose": "normal", "request_id": "shared-logical-id", "node_did": "did:test:other-node",
        "requester_did": "did:test:reader", "session_id": "session",
        "created_at": "2026-10-03T00:00:00Z", "lifecycle_state": "pending",
        "agent_id": "default", "backend_id": "backend", "content": "other node",
    }))
    .unwrap();
    let other = access
        .write(
            "context_accounting.other_node_fixture",
            &format!("mutation {{ create_AgentRequest(input: {input}) {{ _docID }} }}"),
        )
        .await
        .unwrap();
    let other = crate::graphql::created_doc_id(&other, "AgentRequest").unwrap();
    let input = gents_protocol::graphql::graphql_input_literal(&json!({
        "call_id": "other-node-call", "request_id": "shared-logical-id", "request_doc_id": other,
        "node_did": "did:test:other-node", "session_id": "session", "call_kind": "inference",
        "queued_at": "2026-10-12T00:00:00Z", "call_seq": 12,
    }))
    .unwrap();
    access
        .write(
            "context_accounting.other_node_call",
            &format!("mutation {{ create_InferenceCall(input: {input}) {{ _docID }} }}"),
        )
        .await
        .unwrap();
    call(
        &access,
        &old,
        "old-executes-late",
        Some("session"),
        Some("2026-10-05T00:00:00Z"),
        Some(2),
    )
    .await;
    call(
        &access,
        &new,
        "new-executes-early",
        None,
        Some("2026-10-04T00:00:00Z"),
        Some(1),
    )
    .await;
    call(
        &access,
        &foreign_session,
        "spoofed-session-hint",
        Some("session"),
        Some("2026-10-06T00:00:00Z"),
        Some(3),
    )
    .await;
    call(
        &access,
        &foreign_requester,
        "null-requester",
        Some("session"),
        Some("2026-10-07T00:00:00Z"),
        Some(4),
    )
    .await;
    let rows = load_latest_session_context_calls(
        &access,
        "did:test:accounting",
        Some("did:test:reader"),
        "session",
    )
    .await
    .unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row["call_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["old-executes-late", "new-executes-early"]
    );
    let rows = load_latest_session_context_calls(&access, "did:test:accounting", None, "session")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["call_id"], "null-requester");

    for i in 0..CONTEXT_CALL_LIMIT + 2 {
        call(
            &access,
            &old,
            &format!("tied-{i:02}"),
            Some("session"),
            Some("2026-10-08T00:00:00Z"),
            Some(8),
        )
        .await;
    }
    let rows = load_latest_session_context_calls(
        &access,
        "did:test:accounting",
        Some("did:test:reader"),
        "session",
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), CONTEXT_CALL_LIMIT);
    assert_eq!(rows[0]["call_id"], "tied-11");
    assert_eq!(rows[9]["call_id"], "tied-02");

    call(
        &access,
        &old,
        "retained-after-modern-window",
        None,
        Some("2026-10-09T00:00:00Z"),
        Some(9),
    )
    .await;
    let rows = load_latest_session_context_calls(
        &access,
        "did:test:accounting",
        Some("did:test:reader"),
        "session",
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), CONTEXT_CALL_LIMIT);
    assert_eq!(rows[0]["call_id"], "retained-after-modern-window");
    assert_eq!(rows[9]["call_id"], "tied-03");

    let corrupt = gents_protocol::graphql::graphql_input_literal(&json!({
        "call_id": "mismatched-logical-request", "request_id": "another-logical-id",
        "request_doc_id": old, "session_id": "session", "node_did": "did:test:accounting",
        "call_kind": "inference", "queued_at": "2026-10-10T00:00:00Z",
    }))
    .unwrap();
    access
        .write(
            "context_accounting.identity_fixture",
            &format!("mutation {{ create_InferenceCall(input: {corrupt}) {{ _docID }} }}"),
        )
        .await
        .unwrap();
    let error = load_latest_session_context_calls(
        &access,
        "did:test:accounting",
        Some("did:test:reader"),
        "session",
    )
    .await
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("request identity does not match"));
}

#[tokio::test]
async fn retained_calls_page_past_foreign_rows_and_nullable_cursor_keys() {
    let db = crate::support::test_db("paged-context-null-keys").await;
    let access = crate::config_client::ConfigAccess::local(db.node.clone());
    let own = request(&access, "session", None, "2026-10-01T00:00:00Z").await;
    let foreign = request(&access, "other", None, "2026-10-01T00:00:00Z").await;
    for i in 0..PAGE_SIZE + 1 {
        call(
            &access,
            &foreign,
            &format!("z-foreign-{i:04}"),
            Some("session"),
            None,
            None,
        )
        .await;
    }
    call(&access, &own, "a-retained-empty-hint", Some(""), None, None).await;
    let rows = load_latest_session_context_calls(&access, "did:test:accounting", None, "session")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["call_id"], "a-retained-empty-hint");
}

#[tokio::test]
async fn retained_call_selection_pages_the_scoped_requests() {
    let db = crate::support::test_db("paged-context-retained-requests").await;
    let access = crate::config_client::ConfigAccess::local(db.node.clone());
    let mut ids = Vec::new();
    for i in 0..PAGE_SIZE + 1 {
        ids.push(
            request(
                &access,
                "session",
                None,
                &format!("2026-10-01T00:{:02}:{:02}Z", i / 60, i % 60),
            )
            .await,
        );
    }
    ids.sort();
    call(
        &access,
        ids.last().unwrap(),
        "last-physical-request",
        None,
        None,
        None,
    )
    .await;
    let rows = load_latest_session_context_calls(&access, "did:test:accounting", None, "session")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["call_id"], "last-physical-request");
}

mod query_cost;
