use super::*;

use crate::request::lifecycle::RequestTerminalOutcome;
use crate::session;

/// Agent-owned session and request rows are read through the owning Agent's managed access;
/// the node identity has no read grant over them.
async fn agent_data(db: &TestDb, query: &str) -> serde_json::Value {
    db.agent_access.execute(query).await.unwrap()["data"].clone()
}

/// Agent-owned request rows are written through the owning Agent's managed access.
async fn agent_write(db: &TestDb, operation: &'static str, mutation: &str) {
    db.agent_access
        .transact_write(operation, |txn| {
            Box::pin(async move { Ok::<_, anyhow::Error>(txn.execute(mutation).await?) })
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn append_sequence_index_fetches_only_latest_header() {
    let db = test_db("sequence-index").await;
    let session = "sequence-index-session";
    let query = super::super::atomic_inputs::append_sequence_query(db.node_did(), session);
    let access = gents_store::config_client::ConfigAccess::local(db.node.clone());
    let empty = access.execute(&query).await.unwrap();
    assert!(empty["data"]["AgentMessage"].as_array().unwrap().is_empty());
    for sequence in 1..=49 {
        let response = gents_store::config_client::ConfigAccess::write_local(
            &db.node,
            "test.sequence_index",
            &format!(
                r#"mutation {{ create_AgentMessage(input: {{node_did: "{}", session_id: "{}", requester_did: "reader-{sequence}", sequence: {sequence}}}) {{_docID}} }}"#,
                escape_graphql_string(db.node_did()),
                escape_graphql_string(session),
            ),
        ).await.unwrap();
        gents_store::graphql::created_doc_id(&response, "AgentMessage").unwrap();
    }
    for (owner, other_session) in [
        ("did:key:foreign", session),
        (db.node_did(), "another-session"),
    ] {
        gents_store::config_client::ConfigAccess::write_local(
            &db.node,
            "test.sequence_index",
            &format!(
                r#"mutation {{ create_AgentMessage(input: {{node_did: "{}", session_id: "{}", sequence: 700}}) {{_docID}} }}"#,
                escape_graphql_string(owner),
                escape_graphql_string(other_session),
            ),
        ).await.unwrap();
    }
    let latest = access.execute(&query).await.unwrap();
    assert_eq!(latest["data"]["AgentMessage"][0]["sequence"], 49);
    fn metric(value: &serde_json::Value) -> Option<u64> {
        match value {
            serde_json::Value::Object(object) => object
                .get("docFetches")
                .and_then(serde_json::Value::as_u64)
                .or_else(|| object.values().find_map(metric)),
            serde_json::Value::Array(array) => array.iter().find_map(metric),
            _ => None,
        }
    }
    let response = access
        .execute(&format!("query @explain(type: execute) {query}"))
        .await
        .unwrap();
    let plan = &response["data"]["explain"];
    assert_eq!(plan["executionSuccess"], true, "{response}");
    let roots = plan["operationNode"].as_array().unwrap();
    assert_eq!(roots.len(), 2);
    assert_eq!(metric(&roots[0]), Some(1), "{response}");
    for (row_session, sequence, key) in [
        (session, "null", "null-sequence"),
        ("only-null-sequence", "null", "only-null"),
    ] {
        gents_store::config_client::ConfigAccess::write_local(
            &db.node,
            "test.sequence_index",
            &format!(
                r#"mutation {{ create_AgentMessage(input: {{node_did: "{}", session_id: "{}", message_key: "{}", sequence: {sequence}}}) {{_docID}} }}"#,
                escape_graphql_string(db.node_did()),
                escape_graphql_string(row_session),
                escape_graphql_string(key),
            ),
        ).await.unwrap();
    }
    let duplicate = gents_store::config_client::ConfigAccess::write_local(
        &db.node,
        "test.sequence_index_duplicate",
        &format!(
            r#"mutation {{ create_AgentMessage(input: {{node_did: "{}", session_id: "{}", message_key: "duplicate-maximum", sequence: 49}}) {{_docID}} }}"#,
            escape_graphql_string(db.node_did()),
            escape_graphql_string(session),
        ),
    )
    .await
    .expect_err("a committed session sequence cannot be claimed twice");
    assert!(format!("{duplicate:#}").contains("unique index"));
    for row_session in [session, "only-null-sequence"] {
        let indexed =
            super::super::atomic_inputs::append_sequence_query(db.node_did(), row_session);
        let original = indexed.replace(
            "order: [{ node_did: DESC }, { session_id: DESC }, { sequence: DESC }]",
            "order: { sequence: DESC }",
        );
        assert_eq!(
            access.execute(&indexed).await.unwrap()["data"]["AgentMessage"],
            access.execute(&original).await.unwrap()["data"]["AgentMessage"],
            "index ordering must preserve nullable sequence results",
        );
    }
    let txn = ConfigApplyTxn::begin_local(&db.node, None).await.unwrap();
    let next = super::super::atomic_inputs::next_append_sequence_in_transaction(
        &txn,
        db.node_did(),
        session,
    )
    .await
    .unwrap();
    txn.discard().await.unwrap();
    assert_eq!(next, 50);
}

#[tokio::test]
async fn append_sequence_excludes_foreign_session_messages_and_reservations() {
    let db = test_db("sequence-owner-scope").await;
    let session = "same-session-label";
    for (owner, sequence) in [(db.node_did(), 4), ("did:key:foreign", 700)] {
        crate::session::import_history_observation(
            &db.node,
            &format!("observed-request-{sequence}"),
            session,
            owner,
            None,
            "input",
            &format!("observed-input-{sequence}"),
            sequence,
            None,
        )
        .await;
    }
    for (id, owner, sequence) in [
        ("own-reservation", db.node_did(), 5),
        ("foreign-reservation", "did:key:foreign", 900),
    ] {
        let response = db.node.execute(&format!(r#"mutation {{ create_AgentToolCall(input:{{tool_call_id:"{}",session_id:"{}",node_did:"{}",message_sequence:{sequence},await_mode:"background"}}){{_docID}} }}"#, escape_graphql_string(id), escape_graphql_string(session), escape_graphql_string(owner))).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
    }
    let txn = ConfigApplyTxn::begin_local(&db.node, None).await.unwrap();
    let next = super::super::atomic_inputs::next_append_sequence_in_transaction(
        &txn,
        db.node_did(),
        session,
    )
    .await
    .unwrap();
    txn.discard().await.unwrap();
    assert_eq!(next, 7);
}
