use super::*;
use crate::session::model_test_support::{model_session, model_time};
use gents_protocol::session::SessionArchiveVisibility;

#[test]
fn authorized_archive_transitions_match_executed_model() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot()
        .session_document_cases
        .archive;
    assert!(!cases.is_empty());
    let mut checked = 0;
    let mut checked_restored_generation = false;
    for case in cases.iter().filter(|case| case["authorized"] == true) {
        let before = model_session(&case["before"]);
        let expected = model_session(&case["after"]);
        let now = model_time(&case["now"]);
        assert_eq!(case["limit"].as_u64(), Some(ARCHIVE_REVISION_LIMIT));
        let (decision, actual) = apply_transition(
            before,
            case["expected_revision"].as_u64().unwrap(),
            case["archived"].as_bool().unwrap(),
            now.as_str().unwrap(),
        );
        assert_eq!(serde_json::to_value(decision).unwrap(), case["decision"]);
        assert_eq!(actual, expected);
        for (visibility, key) in [
            (SessionArchiveVisibility::Active, "active_visible"),
            (SessionArchiveVisibility::Archived, "archived_visible"),
            (SessionArchiveVisibility::All, "all_visible"),
        ] {
            assert_eq!(visibility.includes(actual.archived_at.is_some()), case[key]);
        }
        checked_restored_generation |= case["before"]["archive_revision"] == 2
            && case["expected_revision"] == 0
            && case["archived"] == true;
        checked += 1;
    }
    assert!(checked > 0 && checked_restored_generation);
}

async fn key(home: &std::path::Path, name: &str) -> crate::identity::KeyIdentity {
    let path = home.join(format!("{name}.key"));
    tokio::task::spawn_blocking(move || crate::identity::KeyIdentity::load_or_create(path, None))
        .await
        .unwrap()
        .unwrap()
}

async fn read_as(node: &Arc<EmbeddedNode>, actor: &dyn NodeIdentity) -> Result<AgentSession> {
    ConfigAccess::transact_local_readonly(
        node,
        Some(::identity::Did::new(actor.did())?),
        "test.archive.read",
        |txn| {
            Box::pin(async move {
                let rows = txn
                    .cursor_rows("AgentSession", crate::session::AGENT_SESSION_FIELDS, "")
                    .await?;
                anyhow::ensure!(rows.len() == 1, "expected one authorized session");
                Ok(crate::session::decode_session_row(&rows[0])?.session)
            })
        },
    )
    .await
}

async fn create_session(node: &Arc<EmbeddedNode>, actor: &dyn NodeIdentity) -> Result<String> {
    ConfigAccess::transact_local_write(
        node, Some(::identity::Did::new(actor.did())?), "test.archive.create",
        |txn| Box::pin(async move {
            let result = txn.execute(r#"mutation { create_AgentSession(input: {
                session_id: "archive-session", node_did: "runtime", requester_did: "other-requester",
                agent_id: "engineer", created_at: "2026-01-01T00:00:00Z",
                tags: ["keep"], title: {text: "Keep this title", source: "user"},
                provenance: {task_id: "original-task"},
                observation: {last_activity_at: "2026-01-02T00:00:00Z", preview: "working",
                    latest_request: {request_doc_id: "physical", request_id: "running",
                        lifecycle_state: "processing"}}
            }) {_docID} }"#).await?;
            Ok::<_, anyhow::Error>(crate::graphql::created_doc_id(&result, "AgentSession")?)
        }),
    ).await
}

async fn create_work(node: &Arc<EmbeddedNode>, actor: &dyn NodeIdentity) {
    let physical = ConfigAccess::transact_local_write(
        node, Some(::identity::Did::new(actor.did()).unwrap()), "test.archive.work",
        |txn| Box::pin(async move {
            let value = txn.execute(r#"mutation { create_AgentRequest(input: {
                request_id: "running", purpose: "normal", node_did: "runtime",
                requester_did: "other-requester", session_id: "archive-session",
                agent_id: "engineer", lifecycle_state: "processing", content: "Original input",
                created_at: "2026-01-01T00:00:00Z"
            }) {_docID} }"#).await?;
            let physical = crate::graphql::created_doc_id(&value, "AgentRequest")?;
            txn.execute(&format!(r#"mutation {{ update_AgentSession(
                filter: {{session_id: {{_eq: "archive-session"}}, node_did: {{_eq: "runtime"}}}},
                input: {{observation: {{last_activity_at: "2026-01-02T00:00:00Z", preview: "working",
                    latest_request: {{request_doc_id: "{}", request_id: "running", lifecycle_state: "processing"}}}}}}
            ) {{_docID}} }}"#, escape_graphql_string(&physical))).await?;
            Ok::<_, anyhow::Error>(physical)
        }),
    ).await.unwrap();
    crate::session::import_history_observation(
        node,
        &physical,
        "archive-session",
        "runtime",
        Some("other-requester"),
        "Original transcript",
        "archive:original",
        1,
        None,
    )
    .await;
}

async fn work_rows(node: &Arc<EmbeddedNode>, actor: &dyn NodeIdentity) -> serde_json::Value {
    ConfigAccess::transact_local_readonly(
        node,
        Some(::identity::Did::new(actor.did()).unwrap()),
        "test.archive.work_read",
        |txn| {
            Box::pin(async move {
                let value = txn
                    .execute(&format!(
                        "{{ AgentRequest {{ _docID request_id lifecycle_state content }} \
                AgentMessage {{ {} }} AgentOutputSegment {{ {} }} }}",
                        crate::session::canonical_rows::AGENT_MESSAGE_FIELDS,
                        crate::session::canonical_rows::AGENT_OUTPUT_SEGMENT_FIELDS
                    ))
                    .await?;
                Ok::<_, anyhow::Error>(value["data"].clone())
            })
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn archive_restore_survive_reopen_and_reject_stale_windows_without_stopping_work() {
    let home = tempfile::tempdir().unwrap();
    let actor = key(home.path(), "actor").await;
    let data = home.path().join("data");
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(&data)
            .with_node_identity_did(actor.did())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    create_session(&node, &actor).await.unwrap();
    create_work(&node, &actor).await;
    let work = work_rows(&node, &actor).await;
    let before = read_as(&node, &actor).await.unwrap();
    let archived = set_session_archived(&node, "runtime", "archive-session", &actor, 0, true)
        .await
        .unwrap();
    assert_eq!(archived.outcome, SessionArchiveOutcome::Applied);
    assert_eq!(archived.revision, "1");
    let mut expected = before.clone();
    expected.archived_at = archived.archived_at.clone();
    expected.archive_revision = 1;
    assert_eq!(read_as(&node, &actor).await.unwrap(), expected);
    assert_eq!(work_rows(&node, &actor).await, work);
    // A lost receipt may be retried without rewriting its timestamp or revision.
    assert_eq!(
        set_session_archived(&node, "runtime", "archive-session", &actor, 0, true)
            .await
            .unwrap()
            .outcome,
        SessionArchiveOutcome::Unchanged
    );
    assert_eq!(read_as(&node, &actor).await.unwrap(), expected);
    assert_eq!(work_rows(&node, &actor).await, work);
    node.shutdown().await;
    drop(node);
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(&data)
            .with_node_identity_did(actor.did())
            .build()
            .await
            .unwrap(),
    );
    assert_eq!(read_as(&node, &actor).await.unwrap(), expected);
    assert_eq!(work_rows(&node, &actor).await, work);
    let other_window = Arc::clone(&node);
    assert_eq!(
        set_session_archived(
            &other_window,
            "runtime",
            "archive-session",
            &actor,
            0,
            false
        )
        .await
        .unwrap()
        .outcome,
        SessionArchiveOutcome::Conflict
    );
    let restored = set_session_archived(&node, "runtime", "archive-session", &actor, 1, false)
        .await
        .unwrap();
    assert_eq!(restored.outcome, SessionArchiveOutcome::Applied);
    expected = before;
    expected.archive_revision = 2;
    assert_eq!(read_as(&other_window, &actor).await.unwrap(), expected);
    assert_eq!(work_rows(&other_window, &actor).await, work);
    assert_eq!(
        set_session_archived(&other_window, "runtime", "archive-session", &actor, 0, true)
            .await
            .unwrap()
            .outcome,
        SessionArchiveOutcome::Conflict
    );
    assert!(set_session_archived(
        &node,
        "different-runtime",
        "archive-session",
        &actor,
        2,
        true
    )
    .await
    .is_err());
    assert_eq!(read_as(&node, &actor).await.unwrap(), expected);
    assert_eq!(work_rows(&node, &actor).await, work);
    node.shutdown().await;
}

#[tokio::test]
async fn archive_authority_is_native_acp_and_never_requester_scope() {
    let home = tempfile::tempdir().unwrap();
    let owner = key(home.path(), "owner").await;
    let writer = key(home.path(), "writer").await;
    let reader = key(home.path(), "reader").await;
    let denied = key(home.path(), "denied").await;
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    let policy = node
        .add_dac_policy(
            owner.did(),
            r#"
name: Archive authority
resources:
  - name: sessions
    relations:
      - name: reader
      - name: writer
    permissions:
      - name: read
        expr: reader + writer
      - name: update
        expr: writer
      - name: delete
"#,
        )
        .await
        .unwrap();
    let schema = gents_protocol::schemas::AGENT_SESSION.replace(
        "type AgentSession ",
        &format!("type AgentSession @policy(id: \"{policy}\", resource: \"sessions\") "),
    );
    ConfigAccess::local(Arc::clone(&node))
        .add_schema(&schema)
        .await
        .unwrap();
    let id = create_session(&node, &owner).await.unwrap();
    node.add_dac_actor_relationship(owner.did(), "AgentSession", &id, "reader", reader.did())
        .await
        .unwrap();
    node.add_dac_actor_relationship(owner.did(), "AgentSession", &id, "writer", writer.did())
        .await
        .unwrap();
    let before = read_as(&node, &owner).await.unwrap();
    assert!(
        set_session_archived(&node, "runtime", "archive-session", &denied, 0, true)
            .await
            .is_err()
    );
    assert!(read_as(&node, &denied).await.is_err());
    assert!(
        set_session_archived(&node, "runtime", "archive-session", &reader, 0, true)
            .await
            .is_err()
    );
    assert_eq!(read_as(&node, &owner).await.unwrap(), before);
    let result = set_session_archived(&node, "runtime", "archive-session", &writer, 0, true)
        .await
        .unwrap();
    assert_eq!(result.outcome, SessionArchiveOutcome::Applied);
    assert_eq!(
        read_as(&node, &reader).await.unwrap().archived_at,
        result.archived_at
    );
    node.shutdown().await;
}

#[tokio::test]
async fn concurrent_archive_calls_commit_one_revision() {
    let home = tempfile::tempdir().unwrap();
    let actor = key(home.path(), "owner").await;
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    node.add_schema(gents_protocol::schemas::AGENT_SESSION)
        .await
        .unwrap();
    create_session(&node, &actor).await.unwrap();
    let ready = tokio::sync::Barrier::new(16);
    let calls = (0..16).map(|_| async {
        ready.wait().await;
        set_session_archived(&node, "runtime", "archive-session", &actor, 0, true)
            .await
            .unwrap()
    });
    let receipts = futures::future::join_all(calls).await;
    assert_eq!(
        receipts
            .iter()
            .filter(|receipt| receipt.outcome == SessionArchiveOutcome::Applied)
            .count(),
        1
    );
    assert!(receipts.iter().all(|receipt| receipt.revision == "1"));
    assert_eq!(read_as(&node, &actor).await.unwrap().archive_revision, 1);
    node.shutdown().await;
}

#[tokio::test]
async fn authenticated_http_archive_uses_the_same_durable_transaction_owner() {
    let home = tempfile::tempdir().unwrap();
    let actor = key(home.path(), "operator").await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(actor.did())
            .with_node_acp_enabled()
            .with_http(defra_node::HttpConfig::with_listener(listener.into_std().unwrap()).unwrap())
            .build()
            .await
            .unwrap(),
    );
    node.add_schema(gents_protocol::schemas::AGENT_SESSION)
        .await
        .unwrap();
    create_session(&node, &actor).await.unwrap();
    let access = ConfigAccess::Graphql(crate::config_client::GraphqlEndpoint::as_principal(
        endpoint,
        actor.did(),
    ));
    let receipt = set_session_archived_on(&access, "runtime", "archive-session", &actor, 0, true)
        .await
        .unwrap();
    assert_eq!(receipt.outcome, SessionArchiveOutcome::Applied);
    assert_eq!(
        read_as(&node, &actor).await.unwrap().archived_at,
        receipt.archived_at
    );
    let receipt = set_session_archived_on(&access, "runtime", "archive-session", &actor, 1, false)
        .await
        .unwrap();
    assert_eq!(receipt.outcome, SessionArchiveOutcome::Applied);
    assert_eq!(read_as(&node, &actor).await.unwrap().archive_revision, 2);
    node.shutdown().await;
}
