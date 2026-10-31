use super::*;
use crate::defra_node::EmbeddedNode;
use gents_protocol::session::SessionProvenance;
use serde_json::json;

async fn create(node: &EmbeddedNode, mutation: String, collection: &str) -> String {
    let response = node.execute(&mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    crate::store::graphql::single_mutation_document(&response, &format!("create_{collection}"))
        .unwrap()
        .and_then(|document| document["_docID"].as_str())
        .unwrap()
        .to_owned()
}

async fn seed(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    created_at: &str,
    cause_doc_id: Option<&str>,
) -> String {
    let cause = cause_doc_id
        .map(|doc_id| {
            format!(
                r#"caused_by_parent_request_id: "p1", caused_by_parent_request_doc_id: "{}", caused_by_parent_tool_call_id: "call-1","#,
                escape_graphql_string(doc_id)
            )
        })
        .unwrap_or_default();
    create(
        node,
        format!(
            r#"mutation {{ create_AgentRequest(input: {{ purpose: "normal", request_id: "{request_id}", node_did: "did:test:node", requester_did: "did:test:node", agent_id: "worker", session_id: "{session_id}", {cause} content: "work", lifecycle_state: "completed", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "{created_at}", retry_count: 0, max_retries: 3 }}) {{ _docID }} }}"#
        ),
        "AgentRequest",
    )
    .await
}

async fn seed_session(node: &EmbeddedNode, session_id: &str, parent: Option<&str>) {
    let session = AgentSession {
        session_id: session_id.into(),
        node_did: "did:test:node".into(),
        requester_did: Some("did:test:node".into()),
        agent_id: "worker".into(),
        created_at: "2026-09-26T00:00:00Z".into(),
        closed_at: None,
        archived_at: None,
        archive_revision: 0,
        stopped_at: None,
        title: None,
        tags: Vec::new(),
        provenance: parent.map(|doc_id| SessionProvenance {
            parent_request_doc_id: Some(doc_id.into()),
            ..Default::default()
        }),
        observation: None,
    };
    let input =
        gents_protocol::graphql::graphql_input_literal(&serde_json::to_value(&session).unwrap())
            .unwrap();
    create(
        node,
        format!("mutation {{ create_AgentSession(input: {input}) {{ _docID }} }}"),
        "AgentSession",
    )
    .await;
}

async fn seed_opened_session(node: &EmbeddedNode, node_did: &str, session_id: &str, opening: &str) {
    let session = AgentSession {
        session_id: session_id.into(),
        node_did: node_did.into(),
        requester_did: Some("did:test:node".into()),
        agent_id: "worker".into(),
        created_at: "2026-09-26T00:00:00Z".into(),
        closed_at: None,
        archived_at: None,
        archive_revision: 0,
        stopped_at: None,
        title: None,
        tags: Vec::new(),
        provenance: Some(SessionProvenance {
            parent_request_doc_id: Some("bae-parent".into()),
            opening_request_doc_id: Some(opening.into()),
            ..Default::default()
        }),
        observation: None,
    };
    let input =
        gents_protocol::graphql::graphql_input_literal(&serde_json::to_value(&session).unwrap())
            .unwrap();
    create(
        node,
        format!("mutation {{ create_AgentSession(input: {input}) {{ _docID }} }}"),
        "AgentSession",
    )
    .await;
}

fn caused_row(doc_id: &str, request_id: &str, created_at: &str, call: &str) -> Value {
    serde_json::json!({
        "_docID": doc_id,
        "request_id": request_id,
        "node_did": "did:test:node",
        "session_id": "child",
        "requester_did": "did:test:node",
        "created_at": created_at,
        "caused_by_parent_request_doc_id": "bae-parent",
        "caused_by_parent_tool_call_doc_id": call,
    })
}

#[test]
fn group_caused_orders_by_created_at_then_doc_id() {
    let grouped = group_caused(&[
        caused_row("bae-a", "r1", "2026-09-26T00:00:03Z", "call-1"),
        caused_row("bae-c", "r3", "2026-09-26T00:00:01Z", "call-1"),
        caused_row("bae-b", "r2", "2026-09-26T00:00:01Z", "call-1"),
    ]);
    let doc_ids = grouped["call-1"]
        .iter()
        .map(|request| request.request_doc_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(doc_ids, ["bae-b", "bae-c", "bae-a"]);
}

#[test]
fn group_caused_keeps_every_request_of_one_call() {
    let grouped = group_caused(&[
        caused_row("bae-a", "r1", "2026-09-26T00:00:01Z", "call-1"),
        caused_row("bae-b", "r2", "2026-09-26T00:00:02Z", "call-1"),
        caused_row("bae-c", "r3", "2026-09-26T00:00:03Z", "call-2"),
    ]);
    let ids = |call: &str| {
        grouped[call]
            .iter()
            .map(|request| request.request_id.as_str())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids("call-1"), ["r1", "r2"]);
    assert_eq!(ids("call-2"), ["r3"]);
    assert_eq!(grouped["call-1"][0].scope, scope("child"));
}

#[tokio::test]
async fn caused_requests_projection_omits_a_call_with_two_requests() {
    let dir = tempfile::tempdir().unwrap();
    let node = std::sync::Arc::new(
        crate::test_support::node_builder()
            .data_path(dir.path().join("node"))
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    for (request_id, call) in [("a1", "call-one"), ("b1", "call-two"), ("b2", "call-two")] {
        create(
            &node,
            format!(
                r#"mutation {{ create_AgentRequest(input: {{ purpose: "normal", request_id: "{request_id}", node_did: "did:test:node", requester_did: "did:test:node", agent_id: "worker", session_id: "child", caused_by_parent_request_id: "p1", caused_by_parent_request_doc_id: "bae-parent", caused_by_parent_tool_call_id: "{call}", caused_by_parent_tool_call_doc_id: "{call}", content: "work", lifecycle_state: "completed", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "2026-09-26T00:00:01Z", retry_count: 0, max_retries: 3 }}) {{ _docID }} }}"#
            ),
            "AgentRequest",
        )
        .await;
    }
    let access = ConfigAccess::local(node.clone());
    let projection = caused_requests(&access, ["call-one", "call-two"])
        .await
        .unwrap();
    assert_eq!(
        projection,
        std::collections::BTreeMap::from([("call-one".to_owned(), "a1".to_owned())])
    );
    node.shutdown().await;
}

fn scope(session_id: &str) -> SessionScope {
    SessionScope {
        node_did: "did:test:node".into(),
        session_id: session_id.into(),
        requester_did: Some("did:test:node".into()),
    }
}

fn ids(links: &[SessionLink]) -> Vec<&str> {
    links
        .iter()
        .map(|link| link.scope.session_id.as_str())
        .collect()
}

#[tokio::test]
async fn physical_request_scope_batch_keeps_requested_missing_and_foreign_rows_distinct() {
    let dir = tempfile::tempdir().unwrap();
    let node = std::sync::Arc::new(
        crate::test_support::node_builder()
            .data_path(dir.path().join("node"))
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let requested = seed(
        &node,
        "requested",
        "requested-session",
        "2026-09-26T00:00:00Z",
        None,
    )
    .await;
    let _foreign = seed(
        &node,
        "foreign",
        "foreign-session",
        "2026-09-26T00:00:01Z",
        None,
    )
    .await;
    let access = ConfigAccess::local(node.clone());

    let missing = document::DocID::new_v0_from_seed("missing-session-origin-request").to_string();
    let scopes = request_scopes(&access, [requested.as_str(), missing.as_str()])
        .await
        .unwrap();
    assert_eq!(
        scopes,
        vec![(requested, scope("requested-session"))],
        "the direct-ID read must return only found requested documents with their stored scope"
    );
    node.shutdown().await;
}

#[tokio::test]
async fn a_parent_pointer_without_a_verified_starter_is_only_messaging() {
    let dir = tempfile::tempdir().unwrap();
    let node = std::sync::Arc::new(
        crate::test_support::node_builder()
            .data_path(dir.path().join("node"))
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let parent = seed(&node, "p1", "parent", "2026-09-26T00:00:01Z", None).await;
    seed_session(&node, "parent", None).await;
    seed(&node, "s1", "existing", "2026-09-26T00:00:00Z", None).await;
    seed(
        &node,
        "s2",
        "existing",
        "2026-09-26T00:00:02Z",
        Some(&parent),
    )
    .await;
    seed_session(&node, "existing", None).await;
    seed(
        &node,
        "c1",
        "started",
        "2026-09-26T00:00:02Z",
        Some(&parent),
    )
    .await;
    seed_session(&node, "started", Some(&parent)).await;
    let other = seed(&node, "o1", "other", "2026-09-26T00:00:01Z", None).await;
    seed_session(&node, "other", None).await;
    seed(&node, "a0", "started", "2026-09-26T00:00:02Z", Some(&other)).await;
    let access = ConfigAccess::local(node.clone());

    let from_parent = lineage(&access, &scope("parent")).await.unwrap();
    assert_eq!(from_parent.started_by, None);
    assert!(from_parent.started.is_empty());
    let mut sent = ids(&from_parent.sent);
    sent.sort_unstable();
    assert_eq!(sent, ["existing", "started"]);
    assert!(from_parent.received.is_empty());

    let from_started = lineage(&access, &scope("started")).await.unwrap();
    assert_eq!(from_started.started_by, None);
    let mut received = ids(&from_started.received);
    received.sort_unstable();
    assert_eq!(received, ["other", "parent"]);

    let from_existing = lineage(&access, &scope("existing")).await.unwrap();
    assert_eq!(from_existing.started_by, None);
    assert_eq!(ids(&from_existing.received), ["parent"]);
    node.shutdown().await;
}

/// The recorded opening request is read by the session's exact scope: a
/// session with the same label on another node is never read, and the
/// label-only read is ambiguous there.
#[tokio::test]
async fn recorded_opening_reads_the_exact_session_scope() {
    let dir = tempfile::tempdir().unwrap();
    let node = std::sync::Arc::new(
        crate::test_support::node_builder()
            .data_path(dir.path().join("node"))
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    seed_opened_session(&node, "did:test:node", "child", "bae-own-opening").await;
    seed_opened_session(&node, "did:test:peer", "child", "bae-peer-opening").await;
    seed_session(&node, "plain", None).await;
    let access = ConfigAccess::local(node.clone());

    assert_eq!(
        recorded_opening(&access, &scope("child")).await.unwrap(),
        Some("bae-own-opening".to_owned())
    );
    assert_eq!(
        recorded_opening(&access, &scope("plain")).await.unwrap(),
        None
    );
    assert_eq!(
        recorded_opening(&access, &scope("missing")).await.unwrap(),
        None
    );
    assert!(load_session(&access, "child", Some("did:test:node"))
        .await
        .is_err());
    node.shutdown().await;
}

fn link() -> SessionLink {
    SessionLink {
        scope: SessionScope {
            node_did: "did:test:node".into(),
            session_id: "child".into(),
            requester_did: Some("did:test:node".into()),
        },
        cause_request_doc_id: "bae-parent".into(),
    }
}

fn row(doc_id: &str, created_at: &str, call: Option<&str>) -> Value {
    json!({
        "_docID": doc_id,
        "node_did": "did:test:node",
        "session_id": "child",
        "requester_did": "did:test:node",
        "created_at": created_at,
        "caused_by_parent_request_doc_id": "bae-parent",
        "caused_by_parent_tool_call_doc_id": call,
    })
}

fn doc_id(row: Option<Value>) -> Option<String> {
    row.map(|row| row["_docID"].as_str().unwrap().to_owned())
}

#[test]
fn opening_request_prefers_agent_new_over_a_later_message() {
    let rows = [
        row("bae-message", "2026-09-26T00:00:02Z", Some("call-message")),
        row("bae-new", "2026-09-26T00:00:01Z", Some("call-new")),
    ];
    assert_eq!(
        doc_id(opening_request(&rows, &link())).as_deref(),
        Some("bae-new")
    );
}

#[test]
fn opening_request_is_the_earliest_of_one_call() {
    let rows = [
        row("bae-b", "2026-09-26T00:00:01Z", Some("call-1")),
        row("bae-c", "2026-09-26T00:00:02Z", Some("call-1")),
        row("bae-a", "2026-09-26T00:00:01Z", Some("call-1")),
    ];
    assert_eq!(
        doc_id(opening_request(&rows, &link())).as_deref(),
        Some("bae-a")
    );
}

#[test]
fn opening_request_needs_the_cause_the_call_edge_and_the_session() {
    let mut other_cause = row("bae-cause", "2026-09-26T00:00:00Z", Some("call-1"));
    other_cause["caused_by_parent_request_doc_id"] = json!("bae-other");
    let mut other_session = row("bae-session", "2026-09-26T00:00:00Z", Some("call-1"));
    other_session["session_id"] = json!("elsewhere");
    let rows = [
        other_cause,
        other_session,
        row("bae-no-call", "2026-09-26T00:00:00Z", None),
    ];
    assert_eq!(opening_request(&rows, &link()), None);
}

fn recording_opening(mut row: Value) -> Value {
    row["opening_request_doc_id"] = json!("bae-b-new");
    row
}

#[test]
fn opening_request_follows_the_recorded_opening_in_one_second() {
    let rows = [
        recording_opening(row(
            "bae-a-message",
            "2026-09-26T00:00:01Z",
            Some("call-message"),
        )),
        recording_opening(row("bae-b-new", "2026-09-26T00:00:01Z", Some("call-new"))),
    ];
    assert_eq!(
        doc_id(opening_request(&rows, &link())).as_deref(),
        Some("bae-b-new")
    );
}

#[test]
fn opening_request_follows_the_recorded_opening_over_an_earlier_clock() {
    let rows = [
        recording_opening(row(
            "bae-a-message",
            "2026-09-26T00:00:00Z",
            Some("call-message"),
        )),
        recording_opening(row("bae-b-new", "2026-09-26T00:00:01Z", Some("call-new"))),
    ];
    assert_eq!(
        doc_id(opening_request(&rows, &link())).as_deref(),
        Some("bae-b-new")
    );
}

#[test]
fn opening_request_is_none_while_the_recorded_opening_is_not_visible() {
    let rows = [recording_opening(row(
        "bae-a-message",
        "2026-09-26T00:00:00Z",
        Some("call-message"),
    ))];
    assert_eq!(opening_request(&rows, &link()), None);
}
