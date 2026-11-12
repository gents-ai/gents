use super::history_fixture::signed_claimed_authored_request;
use super::*;
use crate::test_support::{
    fork_reader_node, history_on, sequenced_history_on, session_rewrite_rows,
};
use gents_inference::llm::message::{Text, ToolResult, ToolResultContent, UserContent};
use gents_store::test_support::first_content;

#[tokio::test]
async fn history_reconstruction_scans_a_shared_request_once() {
    use gents_store::identity::NodeIdentity;
    let (db, user, client, binding) = gents_inference::test_support::signed_test_node(
        "history-shared-scan",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let session_id = "session-shared-request-scan";
    let messages = (1..=3)
        .map(|sequence| format!("message {sequence}"))
        .collect::<Vec<_>>();
    publish_shared_signed_history(&db, &user, &client, &binding, session_id, &messages).await;
    let (history, scans) = count_request_output_scans(history_on(
        &client,
        session_id,
        db.node_identity.did(),
        Some(user.did()),
        None,
    ))
    .await;
    assert_eq!(history.unwrap().len(), 3);
    assert_eq!(
        scans, 1,
        "one history pass scans a shared request's output once"
    );
    db.node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn history_reconstruction_batches_distinct_requests() {
    use gents_store::identity::NodeIdentity;
    let (db, user, client, binding) = gents_inference::test_support::signed_test_node(
        "history-batch-scan",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let session_id = "session-many-request-scans";
    let count = gents_inference::rendered_request::commits::FIELD_COMMIT_ALIAS_BATCH + 3;
    let messages = (0..count)
        .map(|index| format!("message {index}"))
        .collect::<Vec<_>>();
    publish_signed_history(&db, &user, &client, &binding, session_id, &messages).await;
    let (history, scans) = count_request_output_scans(history_on(
        &client,
        session_id,
        db.node_identity.did(),
        Some(user.did()),
        None,
    ))
    .await;
    let history = history.unwrap();
    assert_eq!(history.len(), count);
    for (index, message) in history.iter().enumerate() {
        assert_eq!(message, &Message::user(format!("message {index}")));
    }
    assert!(
        scans <= 2,
        "history used {scans} output scans for {count} requests"
    );
    db.node.shutdown().await.expect("shut down test node");
}

async fn signed_compaction_fixture(
    name: &str,
    session: &str,
    count: usize,
) -> (
    gents_inference::test_support::TestDb,
    gents_store::identity::KeyIdentity,
    std::sync::Arc<dyn gents_store::identity::NodeIdentity>,
    gents_store::config_client::ConfigAccess,
    Vec<(String, String)>,
) {
    let (db, user, client, binding) = gents_inference::test_support::signed_test_node(
        name,
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let agent =
        gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding).unwrap();
    let access = gents_store::config_client::ConfigAccess::local_managed_agent(
        db.node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let messages = (0..count)
        .map(|index| format!("work {index}"))
        .collect::<Vec<_>>();
    publish_signed_history(&db, &user, &client, &binding, session, &messages).await;
    let mut requests = Vec::new();
    for index in 0..count {
        let request = format!("{session}-request-{index}");
        let response = access.execute(&format!(
            r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}, limit: 2) {{ _docID }} }}"#,
            escape_graphql_string(&request),
        )).await.unwrap();
        let [row] = response["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .as_slice()
        else {
            panic!("compaction fixture request must be unique");
        };
        requests.push((request, row["_docID"].as_str().unwrap().to_owned()));
    }
    (db, user, agent, access, requests)
}

#[tokio::test]
async fn compaction_entries_track_files_cumulatively() {
    use gents_store::identity::NodeIdentity;
    let (db, user, agent, access, requests) =
        signed_compaction_fixture("compaction-cumulative", "session-1", 3).await;
    let node_did = db.node_identity.did();
    for (index, summary, read, modified, count, cursor, original, compacted) in [
        (
            0,
            "First summary",
            vec!["/tmp/a.rs".to_string()],
            vec!["/tmp/b.rs".to_string()],
            5,
            10,
            1000,
            200,
        ),
        (
            1,
            "Second summary",
            vec!["/tmp/c.rs".to_string(), "/tmp/a.rs".to_string()],
            vec!["/tmp/d.rs".to_string()],
            7,
            30,
            1200,
            250,
        ),
        (2, "Third summary", vec![], vec![], 3, 40, 500, 100),
    ] {
        let generation = load_prompt_compaction_state_with_access(
            &access,
            "session-1",
            node_did,
            Some(user.did()),
            None,
        )
        .await
        .unwrap()
        .generation;
        compaction_entries::save_compaction_entry_with_access(
            &access,
            agent.did(),
            "session-1",
            node_did,
            Some(user.did()),
            &requests[index].0,
            &requests[index].1,
            summary,
            &read,
            &modified,
            count,
            cursor,
            original,
            compacted,
            &generation,
        )
        .await
        .unwrap();
    }

    let entries = load_compaction_entries(&access, "session-1", node_did, Some(user.did()))
        .await
        .unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].files_read, vec!["/tmp/a.rs"]);
    assert_eq!(entries[1].files_read, vec!["/tmp/a.rs", "/tmp/c.rs"]);
    assert_eq!(entries[1].files_modified, vec!["/tmp/b.rs", "/tmp/d.rs"]);
    assert_eq!(entries[2].compacted_through_sequence, Some(40));
    let stored = access.execute(r#"{ CompactionEntry(filter: { session_id: { _eq: "session-1" } }, order: { sequence: ASC }) { files_read files_modified } }"#).await.unwrap();
    let stored_rows = stored["data"]["CompactionEntry"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["files_read"].as_str().unwrap().to_owned(),
                row["files_modified"].as_str().unwrap().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        stored_rows,
        vec![
            (
                r#"{"version":1,"paths":["/tmp/a.rs"]}"#.to_owned(),
                r#"{"version":1,"paths":["/tmp/b.rs"]}"#.to_owned(),
            ),
            (
                r#"{"version":1,"paths":["/tmp/c.rs"]}"#.to_owned(),
                r#"{"version":1,"paths":["/tmp/d.rs"]}"#.to_owned(),
            ),
            (
                r#"{"version":1,"paths":[]}"#.to_owned(),
                r#"{"version":1,"paths":[]}"#.to_owned(),
            ),
        ]
    );
    let prompt_state = load_prompt_compaction_state_with_access(
        &access,
        "session-1",
        node_did,
        Some(user.did()),
        None,
    )
    .await
    .unwrap();
    assert_eq!(prompt_state.summaries.len(), 3);
    assert_eq!(prompt_state.total_messages_compacted, 15);
    assert_eq!(prompt_state.compacted_through_sequence, Some(40));
    let before_cursor = load_prompt_compaction_state_with_access(
        &access,
        "session-1",
        node_did,
        Some(user.did()),
        Some(20),
    )
    .await
    .unwrap();
    assert_eq!(before_cursor.summaries, vec!["First summary"]);
    assert_eq!(before_cursor.total_messages_compacted, 5);
    assert_eq!(before_cursor.compacted_through_sequence, Some(10));
    let at_cursor = load_prompt_compaction_state_with_access(
        &access,
        "session-1",
        node_did,
        Some(user.did()),
        Some(40),
    )
    .await
    .unwrap();
    assert_eq!(at_cursor.summaries.len(), 3);
    assert_eq!(at_cursor.total_messages_compacted, 15);
    assert_eq!(at_cursor.compacted_through_sequence, Some(40));

    db.node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn concurrent_compactions_from_one_generation_persist_exactly_one_fact() {
    use gents_store::identity::NodeIdentity;
    let (db, user, agent, access, requests) =
        signed_compaction_fixture("compaction-race", "session-race", 2).await;
    let node_did = db.node_identity.did();
    let generation = load_prompt_compaction_state_with_access(
        &access,
        "session-race",
        node_did,
        Some(user.did()),
        None,
    )
    .await
    .unwrap()
    .generation;

    let left_files = ["/tmp/left.rs".to_string()];
    let right_files = ["/tmp/right.rs".to_string()];
    let left = compaction_entries::save_compaction_entry_with_access(
        &access,
        agent.did(),
        "session-race",
        node_did,
        Some(user.did()),
        &requests[0].0,
        &requests[0].1,
        "left summary",
        &left_files,
        &[],
        2,
        20,
        100,
        20,
        &generation,
    );
    let right = compaction_entries::save_compaction_entry_with_access(
        &access,
        agent.did(),
        "session-race",
        node_did,
        Some(user.did()),
        &requests[1].0,
        &requests[1].1,
        "right summary",
        &[],
        &right_files,
        3,
        30,
        200,
        40,
        &generation,
    );
    let (left, right) = tokio::join!(left, right);
    assert_ne!(
        left.is_ok(),
        right.is_ok(),
        "exactly one stale writer must win"
    );
    let loser = left.err().unwrap_or_else(|| right.unwrap_err());
    assert!(
        format!("{loser:#}").contains("stale compaction generation"),
        "unexpected loser error: {loser:#}"
    );

    let entries = load_compaction_entries(&access, "session-race", node_did, Some(user.did()))
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    let winner = &entries[0];
    match winner.summary.as_str() {
        "left summary" => {
            assert_eq!(winner.messages_compacted, 2);
            assert_eq!(winner.compacted_through_sequence, Some(20));
            assert_eq!(winner.files_read, vec!["/tmp/left.rs"]);
            assert!(winner.files_modified.is_empty());
        }
        "right summary" => {
            assert_eq!(winner.messages_compacted, 3);
            assert_eq!(winner.compacted_through_sequence, Some(30));
            assert!(winner.files_read.is_empty());
            assert_eq!(winner.files_modified, vec!["/tmp/right.rs"]);
        }
        summary => panic!("mixed or unexpected winner payload: {summary}"),
    }
}

#[tokio::test]
async fn exact_compaction_redelivery_is_idempotent() {
    use gents_store::identity::NodeIdentity;
    let (db, user, agent, access, requests) =
        signed_compaction_fixture("compaction-redelivery", "session-redelivery", 2).await;
    let node_did = db.node_identity.did();
    let generation = load_prompt_compaction_state_with_access(
        &access,
        "session-redelivery",
        node_did,
        Some(user.did()),
        None,
    )
    .await
    .unwrap()
    .generation;

    let files = ["/tmp/a.rs".to_string()];
    let save = || {
        compaction_entries::save_compaction_entry_with_access(
            &access,
            agent.did(),
            "session-redelivery",
            node_did,
            Some(user.did()),
            &requests[0].0,
            &requests[0].1,
            "same summary",
            &files,
            &[],
            2,
            20,
            100,
            20,
            &generation,
        )
    };
    let first = save().await.unwrap();
    let second = save().await.unwrap();
    assert_eq!(second, first);
    let next_generation = load_prompt_compaction_state_with_access(
        &access,
        "session-redelivery",
        node_did,
        Some(user.did()),
        None,
    )
    .await
    .unwrap()
    .generation;
    compaction_entries::save_compaction_entry_with_access(
        &access,
        agent.did(),
        "session-redelivery",
        node_did,
        Some(user.did()),
        &requests[1].0,
        &requests[1].1,
        "later summary",
        &[],
        &[],
        1,
        30,
        80,
        10,
        &next_generation,
    )
    .await
    .unwrap();
    let replay_after_later = save().await.unwrap();
    assert_eq!(replay_after_later, first);
    assert_eq!(
        load_compaction_entries(&access, "session-redelivery", node_did, Some(user.did()))
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn history_sequence_cursor_loads_only_the_sparse_suffix() {
    use gents_store::identity::NodeIdentity;
    let (db, user, client, binding) = gents_inference::test_support::signed_test_node(
        "history-cursor",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let session_id = "session-cursor";
    let keys = publish_signed_history(
        &db,
        &user,
        &client,
        &binding,
        session_id,
        &[
            "sparse-old".into(),
            "removed".into(),
            "sparse-latest".into(),
        ],
    )
    .await;
    let response = client
        .execute(&format!(
            r#"{{AgentMessage(filter: {{message_key: {{_eq: "{}"}}}}, limit: 2) {{_docID}}}}"#,
            escape_graphql_string(&keys[1])
        ))
        .await
        .unwrap();
    let [row] = response["data"]["AgentMessage"]
        .as_array()
        .unwrap()
        .as_slice()
    else {
        panic!("middle message must be unique")
    };
    let doc = escape_graphql_string(row["_docID"].as_str().unwrap());
    // Ordinary mutations are not client-signed; the owning Agent removes the
    // message it authored through its own managed signer.
    let agent =
        gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding).unwrap();
    let agent_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        db.node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    // The Agent baseline carries no node-level delete; the runtime grants it
    // the same way the runtime fixtures do before the Agent removes its message.
    gents_store::config_client::ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(db.node_identity.did()).unwrap(),
    )
    .grant_node_permission(
        &identity::Did::new(agent.did()).unwrap(),
        gents_store::config_client::NodePermission::DocumentDelete,
    )
    .await
    .unwrap();
    let deleted = agent_access
        .write(
            "test.remove_middle_header",
            &format!(r#"mutation {{delete_AgentMessage(filter: {{_docID: {{_eq: "{doc}"}}}}) {{_docID}}}}"#,),
        )
        .await
        .unwrap();
    let deleted_rows = gents_store::graphql::mutation_documents(&deleted, "delete_AgentMessage")
        .unwrap()
        .unwrap();
    assert_eq!(deleted_rows.len(), 1);
    assert_eq!(deleted_rows[0]["_docID"], row["_docID"]);
    let node_did = db.node_identity.did();
    let local = sequenced_history_on(&client, session_id, node_did, Some(user.did()), None, None)
        .await
        .unwrap();
    assert_eq!(
        local.iter().map(|row| row.sequence).collect::<Vec<_>>(),
        vec![1, 3]
    );
    let rows = sequenced_history_on(
        &client,
        session_id,
        node_did,
        Some(user.did()),
        Some(1),
        None,
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].sequence, 3);
    assert_eq!(rows[0].message, Message::user("sparse-latest"));
    db.node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn close_session_preserves_creation_time() {
    use gents_store::identity::NodeIdentity;
    let (db, user, client, binding) = gents_inference::test_support::signed_test_node(
        "close-session",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node_did = db.node_identity.did();
    reserve_signed_session(&db, &user, &client, &binding, "session-1").await;
    let before = client
        .execute(r#"{ AgentSession(filter: {session_id: {_eq: "session-1"}}) {created_at} }"#)
        .await
        .unwrap();
    let original_created_at = before["data"]["AgentSession"][0]["created_at"].clone();
    let agent =
        gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding).unwrap();
    let agent_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        db.node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    sessions::close_session_with_access(&agent_access, node_did, "session-1", Some(user.did()))
        .await
        .unwrap();
    let resp = client.execute(r#"{ AgentSession(filter: {session_id: {_eq: "session-1"}}, limit: 1) { agent_did created_at closed_at } }"#).await.unwrap();
    let row = &resp["data"]["AgentSession"][0];
    assert_eq!(row["agent_did"].as_str(), Some(binding.agent_did.as_str()));
    assert!(row["created_at"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert!(row["closed_at"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert_eq!(row["created_at"], original_created_at);
}

#[tokio::test]
async fn create_session_with_id_is_idempotent() {
    let (db, user, client, binding) = gents_inference::test_support::signed_test_node(
        "session-idempotent",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node_did = db.node_identity.did();
    reserve_signed_session(&db, &user, &client, &binding, "session-1").await;
    let before = client
        .execute(
            r#"{ AgentSession(filter: {session_id: {_eq: "session-1"}}) { _docID created_at } }"#,
        )
        .await
        .unwrap();
    let original_created_at = before["data"]["AgentSession"][0]["created_at"].clone();
    let doc = escape_graphql_string(
        before["data"]["AgentSession"][0]["_docID"]
            .as_str()
            .unwrap(),
    );
    let mutation = format!(
        r#"mutation($input: AgentSessionMutationInputArg!) {{ update_AgentSession(docID: "{doc}", input: $input) {{ _docID }} }}"#
    );
    client
        .transact_write("test.session.patch", |txn| {
            let mutation = &mutation;
            Box::pin(async move {
                txn.execute_client_signed_update(mutation, &serde_json::json!({"input": {
                "tags": ["keep-on-resume"], "title": {"text": "User title", "source": "user"}
            }}), gents_protocol::schemas::AGENT_SESSION).await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    reserve_signed_session(&db, &user, &client, &binding, "session-1").await;
    let resp = client.execute(r#"{ AgentSession(filter: {session_id: {_eq: "session-1"}}) { session_id created_at tags title node_did agent_did } }"#).await.unwrap();
    let rows = resp["data"]["AgentSession"]
        .as_array()
        .expect("session rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["created_at"], original_created_at);
    assert_eq!(rows[0]["tags"], serde_json::json!(["keep-on-resume"]));
    assert_eq!(
        rows[0]["title"],
        serde_json::json!({"text": "User title", "source": "user"})
    );
    assert_eq!(rows[0]["node_did"].as_str(), Some(node_did));
    assert_eq!(
        rows[0]["agent_did"].as_str(),
        Some(binding.agent_did.as_str())
    );
}

#[tokio::test]
async fn create_session_with_agent_did_rejects_mismatched_existing_binding() {
    use gents_store::identity::NodeIdentity;

    let (db, user, access, binding) = gents_inference::test_support::signed_test_node(
        "session-binding",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node_did = db.node_identity.did();
    let other = gents_store::identity::provision_agent_identity_authorized(
        &db.node,
        db.node_identity.as_ref(),
        &identity::Did::new(user.did()).unwrap(),
    )
    .await
    .unwrap();
    gents_inference::test_support::install_test_agent_binding_with_access(
        node_did,
        "Other fixture agent",
        &other,
        &access,
    )
    .await;
    assert_ne!(binding.agent_did, other.agent_did);
    crate::request::lifecycle::build_signed_client_request(
        &access,
        crate::request::lifecycle::RequestSpec::new(
            gents_protocol::request_admission::RequestPurpose::Normal,
            crate::request::lifecycle::RequestIdentity {
                request_id: "session-binding-request".into(),
                node_did: node_did.into(),
                requester_did: Some(user.did().into()),
                agent_did: binding.agent_did.clone(),
                session_id: "session-1".into(),
                content: "work".into(),
                execution_origin: crate::request::lifecycle::ExecutionOrigin::Interactive,
                created_at: "2026-09-22T00:00:00Z".into(),
            },
            gents_protocol::request_admission::AgentRequestAdmissionRecord::client(user.did()),
        ),
        crate::request::lifecycle::RequestSigner::Identity(&user),
    )
    .await
    .unwrap();

    async fn ensure(
        access: &gents_store::config_client::ConfigAccess,
        node_did: &str,
        requester_did: &str,
        agent_did: &str,
    ) -> anyhow::Result<bool> {
        access
            .transact_write("test.session.binding", |txn| {
                Box::pin(async move {
                    ensure_session_in_txn(
                        txn,
                        "session-1",
                        node_did,
                        agent_did,
                        Some(requester_did),
                        None,
                        None,
                        "2026-09-22T00:00:00Z",
                    )
                    .await
                })
            })
            .await
    }
    assert!(!ensure(&access, node_did, user.did(), &binding.agent_did)
        .await
        .unwrap());
    let error = ensure(&access, node_did, user.did(), &other.agent_did)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("agent mismatch"));
}

#[tokio::test]
async fn editing_a_canonical_user_message_forks_its_prefix_and_preserves_the_source() {
    use gents_store::identity::NodeIdentity;
    const SOURCE: &str = "edit-source";
    let case = gents_lean_vocab::lean_contract_snapshot()
        .session_document_edit_anchor_cases
        .iter()
        .find(|case| case["name"] == "three_user_turns")
        .unwrap();
    let (db, user, client, binding) = fork_reader_node("edit-fork").await;
    let node = &db.node;
    let node_did = db.node_identity.did();
    let modeled_messages = case["messages"].as_array().unwrap();
    for (index, message) in modeled_messages.iter().enumerate() {
        assert_eq!(message["sequence"].as_u64().unwrap(), (index + 1) as u64);
    }
    let messages = modeled_messages
        .iter()
        .map(|message| message["key"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    let keys = publish_signed_history(&db, &user, &client, &binding, SOURCE, &messages).await;
    let anchor = modeled_messages
        .iter()
        .position(|message| message["key"] == case["key"])
        .unwrap();
    let before = history_on(&client, SOURCE, node_did, Some(user.did()), None)
        .await
        .unwrap();
    let receipt = fork_on(
        &client,
        ForkParams {
            source_session_id: SOURCE,
            boundary: ForkBoundary::BeforeMessageKey(&keys[anchor]),
            caller_node_did: node_did,
            caller_requester_did: Some(user.did()),
            target_agent_did: None,
        },
    )
    .await
    .unwrap();
    let retained = usize::try_from(case["retained_count"].as_u64().unwrap()).unwrap();
    assert_eq!(receipt.copied_messages as usize, retained);
    assert_eq!(
        history_on(
            &client,
            &receipt.session_id,
            node_did,
            Some(user.did()),
            None
        )
        .await
        .unwrap(),
        before[..retained]
    );
    assert_eq!(
        history_on(&client, SOURCE, node_did, Some(user.did()), None)
            .await
            .unwrap(),
        before
    );
    node.shutdown().await.expect("shut down test node");
}

async fn session_doc_id(
    client: &gents_store::config_client::ConfigAccess,
    session: &str,
) -> String {
    let session = escape_graphql_string(session);
    let response = client
        .execute(&format!(
            r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{session}" }} }}, limit: 2) {{ _docID }} }}"#
        ))
        .await
        .unwrap();
    let [row] = response["data"]["AgentSession"]
        .as_array()
        .unwrap()
        .as_slice()
    else {
        panic!("session must be unique")
    };
    row["_docID"].as_str().unwrap().to_owned()
}

async fn delete_session_rows(
    client: &gents_store::config_client::ConfigAccess,
    collection: &str,
    session: &str,
) {
    let session = escape_graphql_string(session);
    let response = client
        .write(
            "test.remove_source_rows",
            &format!(
                r#"mutation {{ delete_{collection}(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{ _docID }} }}"#
            ),
        )
        .await
        .unwrap();
    let deleted =
        gents_store::graphql::mutation_documents(&response, &format!("delete_{collection}"))
            .unwrap()
            .unwrap();
    assert!(
        !deleted.is_empty(),
        "{collection} rows of {session} must exist"
    );
    let remaining = client
        .execute(&format!(
            r#"{{ {collection}(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{ _docID }} }}"#
        ))
        .await
        .unwrap();
    assert!(remaining["data"][collection].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_reader_granted_only_the_fork_reads_its_whole_retained_transcript() {
    use gents_store::config_client::NodePermission;
    use gents_store::identity::NodeIdentity;
    const SOURCE: &str = "shared-fork-source";
    let case = gents_lean_vocab::lean_contract_snapshot()
        .session_document_fork_cases
        .iter()
        .find(|case| case["name"] == "exact_copy")
        .unwrap();
    let reads = |grant: &str| {
        case["readers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|reader| reader["grant"] == grant)
            .unwrap()["reads_transcript"]
            .as_bool()
            .unwrap()
    };
    let (db, user, client, binding) = fork_reader_node("shared-fork-reader").await;
    let node_did = db.node_identity.did();
    let messages = (1..=3)
        .map(|turn| format!("shared-{turn}"))
        .collect::<Vec<_>>();
    publish_signed_history(&db, &user, &client, &binding, SOURCE, &messages).await;
    let fork_of = |source: &'static str, turn: u32| ForkParams {
        source_session_id: source,
        boundary: ForkBoundary::UserTurn(turn),
        caller_node_did: node_did,
        caller_requester_did: Some(user.did()),
        target_agent_did: None,
    };
    let receipt = fork_on(&client, fork_of(SOURCE, 2)).await.unwrap();
    assert_eq!(receipt.copied_messages, 2);
    let child: &'static str = Box::leak(receipt.session_id.clone().into_boxed_str());
    let owned = history_on(&client, child, node_did, Some(user.did()), None)
        .await
        .unwrap();
    assert_eq!(
        owned,
        messages[..2]
            .iter()
            .map(|text| Message::user(text.as_str()))
            .collect::<Vec<_>>()
    );
    let grandchild = fork_on(&client, fork_of(child, 2)).await.unwrap();
    assert_eq!(grandchild.copied_messages, 2);

    let sharee =
        tokio::task::spawn_blocking(|| gents_store::test_support::test_identity("fork-sharee"))
            .await
            .unwrap();
    let sharee_did = identity::Did::new(sharee.did()).unwrap();
    let admin = gents_store::config_client::ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(node_did).unwrap(),
    );
    for permission in [NodePermission::DocumentRead, NodePermission::CollectionGet] {
        admin
            .grant_node_permission(&sharee_did, permission)
            .await
            .unwrap();
    }
    let sharee_access =
        gents_store::config_client::ConfigAccess::local_as(db.node.clone(), sharee_did.clone());
    let source_doc = session_doc_id(&client, SOURCE).await;
    let child_doc = session_doc_id(&client, child).await;
    let grandchild_doc = session_doc_id(&client, &grandchild.session_id).await;
    let requester = user.did();
    let sharee = &sharee_access;
    let sharee_history = |session: String| async move {
        history_on(sharee, &session, node_did, Some(requester), None).await
    };
    let sharee_reads = |session: String| {
        let expected = &owned;
        async move {
            sharee_history(session)
                .await
                .is_ok_and(|history| history == *expected)
        }
    };
    let share = |doc: &str, grant: bool| {
        let doc = doc.to_owned();
        let client = &client;
        let sharee_did = &sharee_did;
        async move {
            if grant {
                client
                    .add_actor_relationship("AgentSession", &doc, "reader", sharee_did)
                    .await
                    .unwrap();
            } else {
                client
                    .delete_actor_relationship("AgentSession", &doc, "reader", sharee_did)
                    .await
                    .unwrap();
            }
        }
    };

    assert_eq!(
        sharee_reads(child.to_owned()).await,
        reads("unrelated_session")
    );
    share(&source_doc, true).await;
    assert_eq!(
        sharee_history(SOURCE.to_owned()).await.unwrap().len(),
        3,
        "the source grant is effective"
    );
    assert_eq!(
        sharee_reads(child.to_owned()).await,
        reads("source_session")
    );
    share(&source_doc, false).await;
    share(&child_doc, true).await;
    assert!(reads("child_session"));
    assert!(
        sharee_reads(child.to_owned()).await,
        "a direct reader of only the fork reads its retained transcript"
    );
    assert!(
        sharee_history(SOURCE.to_owned())
            .await
            .map_or(true, |history| history.is_empty()),
        "the fork grant reveals nothing of the source session"
    );
    share(&child_doc, false).await;
    assert_eq!(
        sharee_reads(child.to_owned()).await,
        reads("unrelated_session")
    );
    share(&grandchild_doc, true).await;
    assert!(
        sharee_reads(grandchild.session_id.clone()).await,
        "a reader of only a fork of a fork reads its retained transcript"
    );
    assert!(!sharee_reads(child.to_owned()).await);

    let agent =
        gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding).unwrap();
    admin
        .grant_node_permission(
            &identity::Did::new(agent.did()).unwrap(),
            NodePermission::DocumentDelete,
        )
        .await
        .unwrap();
    let source_owner = gents_store::config_client::ConfigAccess::local_managed_agent(
        db.node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    delete_session_rows(&source_owner, "AgentOutputSegment", SOURCE).await;
    assert_eq!(
        history_on(&client, child, node_did, Some(user.did()), None)
            .await
            .unwrap(),
        owned,
        "an owner whose origin headers remain reads the fork from its copies"
    );
    delete_session_rows(&source_owner, "AgentMessage", SOURCE).await;
    share(&child_doc, true).await;
    for session in [child.to_owned(), grandchild.session_id.clone()] {
        assert_eq!(
            history_on(&client, &session, node_did, Some(user.did()), None)
                .await
                .unwrap(),
            owned
        );
        assert!(
            sharee_reads(session).await,
            "deleting the source keeps the fork readable"
        );
    }
    db.node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn edit_fork_reuses_canonical_request_reads_for_the_retained_prefix() {
    use gents_store::identity::NodeIdentity as _;
    const SOURCE: &str = "fork-shared-source";
    const CHILD: &str = "fork-shared-child";
    let (db, user, client, binding) = fork_reader_node("fork-shared-reads").await;
    let node_did = db.node_identity.did().to_owned();
    let mut lifecycle =
        signed_claimed_authored_request(&db, &user, &client, &binding, "shared-request", SOURCE)
            .await;
    let writer = crate::request::streaming::DefraStreamWriter::new_for_lifecycle(
        db.node.clone(),
        &lifecycle,
        std::time::Duration::ZERO,
    )
    .unwrap();
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    let mut keys = Vec::new();
    for sequence in 1..=21 {
        let name = format!("message-{sequence}");
        keys.push(gents_protocol::output::rows::authored_message_key(
            &lifecycle.request().doc_id,
            &name,
        ));
        writer
            .publish_authored_message(&lifecycle, &name, &Message::user(&name))
            .await
            .unwrap();
    }
    lifecycle
        .terminalize_owned(
            crate::request::lifecycle::RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::NoMessage,
            None,
        )
        .await
        .unwrap();
    let params = ForkParams {
        source_session_id: SOURCE,
        boundary: ForkBoundary::BeforeMessageKey(&keys[20]),
        caller_node_did: &node_did,
        caller_requester_did: Some(user.did()),
        target_agent_did: None,
    };
    let (result, scans) =
        count_request_output_scans(fork_edit_on(&client, params, CHILD, user.did())).await;
    assert_eq!(result.unwrap(), CHILD);
    assert_eq!(
        scans, 1,
        "retained messages from one request share one output scan"
    );
    assert_eq!(
        history_on(&client, CHILD, &node_did, Some(user.did()), None)
            .await
            .unwrap()
            .len(),
        20
    );
    db.node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn fork_rolls_back_at_every_mutation_position() {
    use gents_store::identity::NodeIdentity;
    const SOURCE: &str = "fork-rollback-source";
    let (db, user, client, binding) = fork_reader_node("fork-rollback").await;
    let node = &db.node;
    let node_did = db.node_identity.did();
    let agent =
        gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding).unwrap();
    let agent_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let messages = (1..=3)
        .map(|sequence| format!("turn {sequence}"))
        .collect::<Vec<_>>();
    publish_signed_history(&db, &user, &client, &binding, SOURCE, &messages).await;
    for (index, summary, files, count, cursor) in [
        (0, "early turns", vec!["/tmp/a.rs".to_string()], 2, 2),
        (1, "later turns", vec!["/tmp/b.rs".to_string()], 1, 3),
    ] {
        let request_id = format!("{SOURCE}-request-{index}");
        let response = client.execute(&format!(
            r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}, limit: 2) {{ _docID }} }}"#,
            escape_graphql_string(&request_id),
        )).await.unwrap();
        let [row] = response["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .as_slice()
        else {
            panic!("compaction source request must be unique");
        };
        let doc = row["_docID"].as_str().unwrap();
        let state = load_prompt_compaction_state(
            node,
            &binding.agent_did,
            SOURCE,
            node_did,
            Some(user.did()),
            None,
        )
        .await
        .unwrap();
        compaction_entries::save_compaction_entry_with_access(
            &agent_access,
            &binding.agent_did,
            SOURCE,
            node_did,
            Some(user.did()),
            &request_id,
            doc,
            summary,
            &files,
            &[],
            count,
            cursor,
            100,
            10,
            &state.generation,
        )
        .await
        .unwrap();
    }
    let params = ForkParams {
        source_session_id: SOURCE,
        boundary: ForkBoundary::UserTurn(3),
        caller_node_did: node_did,
        caller_requester_did: Some(user.did()),
        target_agent_did: None,
    };
    let (publication, writes) =
        gents_store::config_client::ConfigApplyTxn::assert_every_mutation_rolls_back(
            || super::fork::commit_fork(&client, params.clone()),
            || session_rewrite_rows(&client),
        )
        .await;
    let outcome = publication.outcome;
    super::fork::publish_fork_relationships(&client, publication.intent)
        .await
        .unwrap();
    assert_eq!(outcome.copied_messages, 3);
    assert_eq!(outcome.copied_compaction_entries, 2);
    assert!(
        writes > 4,
        "headers, compaction, and session header are separate writes: {writes}"
    );
    let child_entries =
        load_compaction_entries(&client, &outcome.session_id, node_did, Some(user.did()))
            .await
            .unwrap();
    assert_eq!(child_entries.len(), 2);
    assert_eq!(child_entries[0].files_read, vec!["/tmp/a.rs"]);
    assert_eq!(child_entries[1].files_read, vec!["/tmp/a.rs", "/tmp/b.rs"]);
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn compaction_save_fault_leaves_chain_and_generation_intact() {
    use gents_store::identity::NodeIdentity;
    const SESSION: &str = "compaction-rollback";
    let (db, user, client, binding) = gents_inference::test_support::signed_test_node(
        "compaction-rollback",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = &db.node;
    let node_did = db.node_identity.did();
    let agent =
        gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding).unwrap();
    let access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    publish_signed_history(
        &db,
        &user,
        &client,
        &binding,
        SESSION,
        &["first".into(), "second".into()],
    )
    .await;
    let mut docs = Vec::new();
    for index in 0..2 {
        let request = format!("{SESSION}-request-{index}");
        let response = access.execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{_eq: "{}"}}}}, limit: 2) {{ _docID }} }}"#,
            escape_graphql_string(&request),
        )).await.unwrap();
        let [row] = response["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .as_slice()
        else {
            panic!("compaction source request must be unique");
        };
        docs.push(row["_docID"].as_str().unwrap().to_owned());
    }
    let state = load_prompt_compaction_state_with_access(
        &access,
        SESSION,
        node_did,
        Some(user.did()),
        None,
    )
    .await
    .unwrap();
    compaction_entries::save_compaction_entry_with_access(
        &access,
        agent.did(),
        SESSION,
        node_did,
        Some(user.did()),
        "compaction-rollback-request-0",
        &docs[0],
        "first summary",
        &["/tmp/a.rs".to_string()],
        &[],
        2,
        10,
        100,
        20,
        &state.generation,
    )
    .await
    .unwrap();
    let chain = || async {
        let entries = load_compaction_entries(&access, SESSION, node_did, Some(user.did()))
            .await
            .unwrap();
        let state = load_prompt_compaction_state_with_access(
            &access,
            SESSION,
            node_did,
            Some(user.did()),
            None,
        )
        .await
        .unwrap();
        (entries, state)
    };
    let (_, before) = chain().await;
    let files = ["/tmp/b.rs".to_string()];
    let (saved, _) = gents_store::config_client::ConfigApplyTxn::assert_every_mutation_rolls_back(
        || {
            compaction_entries::save_compaction_entry_with_access(
                &access,
                agent.did(),
                SESSION,
                node_did,
                Some(user.did()),
                "compaction-rollback-request-1",
                &docs[1],
                "second summary",
                &files,
                &[],
                3,
                20,
                200,
                30,
                &before.generation,
            )
        },
        chain,
    )
    .await;
    assert_eq!(saved.sequence, 2);
    assert_eq!(saved.files_read, vec!["/tmp/a.rs", "/tmp/b.rs"]);
    let (entries, after) = chain().await;
    assert_eq!(entries.len(), 2);
    assert_ne!(after.generation, before.generation);
    assert_eq!(after.compacted_through_sequence, Some(20));
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn history_reads_exact_node_and_requester_scope() {
    use gents_store::identity::NodeIdentity;
    let (local_db, local_user, local_client, local_binding) =
        gents_inference::test_support::signed_test_node(
            "history-local",
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
    let (foreign_db, foreign_user, foreign_client, foreign_binding) =
        gents_inference::test_support::signed_test_node(
            "history-foreign",
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
    let local_node = local_db.node_identity.did();
    let foreign_node = foreign_db.node_identity.did();
    assert_ne!(local_node, foreign_node);
    publish_signed_history(
        &local_db,
        &local_user,
        &local_client,
        &local_binding,
        "shared-session",
        &["local".into()],
    )
    .await;
    publish_signed_history(
        &foreign_db,
        &foreign_user,
        &foreign_client,
        &foreign_binding,
        "shared-session",
        &["remote requester".into()],
    )
    .await;
    assert_eq!(
        history_on(
            &local_client,
            "shared-session",
            local_node,
            Some(local_user.did()),
            None
        )
        .await
        .unwrap(),
        vec![Message::user("local")]
    );
    assert_eq!(
        history_on(
            &foreign_client,
            "shared-session",
            foreign_node,
            Some(foreign_user.did()),
            None
        )
        .await
        .unwrap(),
        vec![Message::user("remote requester")]
    );
    assert!(history_on(
        &local_client,
        "shared-session",
        local_node,
        Some(foreign_user.did()),
        None
    )
    .await
    .unwrap()
    .is_empty());
    assert!(history_on(
        &local_client,
        "shared-session",
        foreign_node,
        Some(local_user.did()),
        None
    )
    .await
    .unwrap()
    .is_empty());
    assert!(history_on(
        &foreign_client,
        "shared-session",
        foreign_node,
        Some(local_user.did()),
        None
    )
    .await
    .unwrap()
    .is_empty());
    local_db
        .node
        .shutdown()
        .await
        .expect("shut down local test node");
    foreign_db
        .node
        .shutdown()
        .await
        .expect("shut down foreign test node");
}

#[tokio::test]
async fn authored_keys_are_immutable_and_sequences_are_node_scoped() {
    use gents_store::identity::NodeIdentity;
    let (first_db, first_user, first_client, first_binding) =
        gents_inference::test_support::signed_test_node(
            "authored-local",
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
    let (second_db, second_user, second_client, second_binding) =
        gents_inference::test_support::signed_test_node(
            "authored-foreign",
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
    let first_node = first_db.node_identity.did();
    let second_node = second_db.node_identity.did();
    assert_ne!(first_node, second_node);
    for (db, user, client, binding, foreign_node, text) in [
        (
            &first_db,
            &first_user,
            &first_client,
            &first_binding,
            second_node,
            "local",
        ),
        (
            &second_db,
            &second_user,
            &second_client,
            &second_binding,
            first_node,
            "foreign",
        ),
    ] {
        let node_did = db.node_identity.did();
        let mut lifecycle =
            signed_claimed_authored_request(db, user, client, binding, "same-request", "same")
                .await;
        let writer = crate::request::streaming::DefraStreamWriter::new_for_lifecycle(
            db.node.clone(),
            &lifecycle,
            std::time::Duration::ZERO,
        )
        .unwrap();
        lifecycle.begin_owned_execution(&writer).await.unwrap();
        let message = Message::user(text);
        let first = writer
            .publish_authored_message(&lifecycle, "prompt", &message)
            .await
            .unwrap();
        assert_eq!(
            writer
                .publish_authored_message(&lifecycle, "prompt", &message)
                .await
                .unwrap(),
            first
        );
        assert!(
            writer
                .publish_authored_message(&lifecycle, "prompt", &Message::user("replacement"))
                .await
                .is_err(),
            "an existing canonical key cannot overwrite immutable content"
        );
        let (header, native) = load_canonical_message(client, &first, node_did, Some(user.did()))
            .await
            .unwrap();
        assert_eq!(header.sequence, 1);
        assert_eq!(native, message);
        assert_eq!(
            history_on(client, "same", node_did, Some(user.did()), None)
                .await
                .unwrap(),
            vec![message]
        );
        assert!(
            history_on(client, "same", foreign_node, Some(user.did()), None)
                .await
                .unwrap()
                .is_empty(),
            "another runtime's Node scope must not resolve local history"
        );
        lifecycle
            .terminalize_owned(
                crate::request::lifecycle::RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::NoMessage,
                None,
            )
            .await
            .unwrap();
    }
    assert!(history_on(
        &first_client,
        "same",
        first_node,
        Some(second_user.did()),
        None
    )
    .await
    .unwrap()
    .is_empty());
    first_db
        .node
        .shutdown()
        .await
        .expect("shut down first test node");
    second_db
        .node
        .shutdown()
        .await
        .expect("shut down second test node");
}

#[tokio::test]
async fn compaction_chains_with_equal_session_labels_remain_owner_scoped() {
    use gents_store::identity::NodeIdentity;
    let (local_db, local_user, local_client, local_binding) =
        gents_inference::test_support::signed_test_node(
            "compaction-local",
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
    let (foreign_db, foreign_user, foreign_client, foreign_binding) =
        gents_inference::test_support::signed_test_node(
            "compaction-foreign",
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
    let local_node = local_db.node_identity.did();
    let foreign_node = foreign_db.node_identity.did();
    assert_ne!(local_node, foreign_node);
    for (db, user, client, binding, summary) in [
        (
            &local_db,
            &local_user,
            &local_client,
            &local_binding,
            "local summary",
        ),
        (
            &foreign_db,
            &foreign_user,
            &foreign_client,
            &foreign_binding,
            "foreign summary",
        ),
    ] {
        let session = "same-chain";
        let agent =
            gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), binding)
                .unwrap();
        let agent_access = gents_store::config_client::ConfigAccess::local_managed_agent(
            db.node.clone(),
            identity::Did::new(agent.did()).unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        publish_signed_history(db, user, client, binding, session, &["work".into()]).await;
        let request_id = format!("{session}-request-0");
        let response = client.execute(&format!(
            r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}, limit: 2) {{ _docID }} }}"#,
            escape_graphql_string(&request_id),
        )).await.unwrap();
        let [row] = response["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .as_slice()
        else {
            panic!("compaction request must be unique")
        };
        let doc = row["_docID"].as_str().unwrap();
        let state = load_prompt_compaction_state(
            &db.node,
            &binding.agent_did,
            session,
            db.node_identity.did(),
            Some(user.did()),
            None,
        )
        .await
        .unwrap();
        compaction_entries::save_compaction_entry_with_access(
            &agent_access,
            &binding.agent_did,
            session,
            db.node_identity.did(),
            Some(user.did()),
            &request_id,
            doc,
            summary,
            &[],
            &[],
            1,
            1,
            100,
            10,
            &state.generation,
        )
        .await
        .unwrap();
    }
    let local = load_compaction_entries(
        &local_client,
        "same-chain",
        local_node,
        Some(local_user.did()),
    )
    .await
    .unwrap();
    let foreign = load_compaction_entries(
        &foreign_client,
        "same-chain",
        foreign_node,
        Some(foreign_user.did()),
    )
    .await
    .unwrap();
    assert_eq!(local.len(), 1);
    assert_eq!(foreign.len(), 1);
    assert_eq!(local[0].summary, "local summary");
    assert_eq!(foreign[0].summary, "foreign summary");
    assert!(load_compaction_entries(
        &local_client,
        "same-chain",
        foreign_node,
        Some(local_user.did())
    )
    .await
    .unwrap()
    .is_empty());
    assert!(load_compaction_entries(
        &foreign_client,
        "same-chain",
        local_node,
        Some(foreign_user.did())
    )
    .await
    .unwrap()
    .is_empty());
    assert!(load_compaction_entries(
        &local_client,
        "same-chain",
        local_node,
        Some(foreign_user.did())
    )
    .await
    .unwrap()
    .is_empty());
}

async fn reserve_signed_session(
    db: &gents_inference::test_support::TestDb,
    user: &dyn gents_store::identity::NodeIdentity,
    client: &gents_store::config_client::ConfigAccess,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
    session_id: &str,
) {
    crate::request::lifecycle::build_signed_client_request(
        client,
        crate::request::lifecycle::RequestSpec::new(
            gents_protocol::request_admission::RequestPurpose::Normal,
            crate::request::lifecycle::RequestIdentity {
                request_id: format!("{session_id}-reservation"),
                node_did: db.node_identity.did().into(),
                requester_did: Some(user.did().into()),
                agent_did: binding.agent_did.clone(),
                session_id: session_id.into(),
                content: "work".into(),
                execution_origin: crate::request::lifecycle::ExecutionOrigin::Interactive,
                created_at: "2026-09-22T00:00:00Z".into(),
            },
            gents_protocol::request_admission::AgentRequestAdmissionRecord::client(user.did()),
        ),
        crate::request::lifecycle::RequestSigner::Identity(user),
    )
    .await
    .unwrap();
}

pub(crate) async fn publish_shared_signed_history(
    db: &gents_inference::test_support::TestDb,
    user: &dyn gents_store::identity::NodeIdentity,
    client: &gents_store::config_client::ConfigAccess,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
    session_id: &str,
    messages: &[String],
) {
    let mut lifecycle = signed_claimed_authored_request(
        db,
        user,
        client,
        binding,
        &format!("{session_id}-shared"),
        session_id,
    )
    .await;
    let writer = crate::request::streaming::DefraStreamWriter::new_for_lifecycle(
        db.node.clone(),
        &lifecycle,
        std::time::Duration::ZERO,
    )
    .unwrap();
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    for (index, text) in messages.iter().enumerate() {
        writer
            .publish_authored_message(&lifecycle, &format!("prompt-{index}"), &Message::user(text))
            .await
            .unwrap();
    }
    lifecycle
        .terminalize_owned(
            crate::request::lifecycle::RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::NoMessage,
            None,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn concurrent_keyed_appends_resolve_sequence_conflicts_without_duplicates() {
    use crate::request::streaming::DefraStreamWriter;
    use gents_store::identity::NodeIdentity;
    use std::time::Duration;
    let (db, user, client, binding) = gents_inference::test_support::signed_test_node(
        "append-race",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = db.node.clone();
    let node_did = db.node_identity.did();
    let mut lifecycle = signed_claimed_authored_request(
        &db,
        &user,
        &client,
        &binding,
        "append-race-request",
        "append-race",
    )
    .await;
    let writer =
        DefraStreamWriter::new_for_lifecycle(node.clone(), &lifecycle, Duration::ZERO).unwrap();
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    let left_message = Message::user("left");
    let right_message = Message::user("right");
    let (left, right) = tokio::join!(
        writer.publish_authored_message(&lifecycle, "left", &left_message),
        writer.publish_authored_message(&lifecycle, "right", &right_message),
    );
    let left = left.unwrap();
    let right = right.unwrap();
    assert_ne!(left, right);
    assert_eq!(
        writer
            .publish_authored_message(&lifecycle, "left", &left_message)
            .await
            .unwrap(),
        left
    );
    let (left_header, left_native) =
        load_canonical_message(&client, &left, node_did, Some(user.did()))
            .await
            .unwrap();
    let (right_header, right_native) =
        load_canonical_message(&client, &right, node_did, Some(user.did()))
            .await
            .unwrap();
    assert_ne!(left_header.sequence, right_header.sequence);
    let mut sequences = [left_header.sequence, right_header.sequence];
    sequences.sort();
    assert_eq!(
        sequences,
        [1, 2],
        "racing publications must not leave a sequence hole"
    );
    assert_eq!(left_native, left_message);
    assert_eq!(right_native, right_message);
    assert_eq!(
        history_on(&client, "append-race", node_did, Some(user.did()), None)
            .await
            .unwrap()
            .len(),
        2
    );
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn single_header_transaction_matches_bulk_coordinate_validation() {
    use gents_protocol::output::rows::{
        decode_transcript_message_row, transcript_message_create_variables, AGENT_MESSAGE_FIELDS,
        CREATE_AGENT_MESSAGE_MUTATION,
    };
    use gents_protocol::output::MessagePublication;
    use gents_store::identity::NodeIdentity;

    let (foreign_db, foreign_user, _foreign_client, _foreign_binding) =
        gents_inference::test_support::signed_test_node(
            "header-coordinate-foreign",
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
    for case in [
        "unrelated",
        "key",
        "sequence",
        "both",
        "foreign",
        "requester",
        "fork",
    ] {
        let (db, user, client, binding) = gents_inference::test_support::signed_test_node(
            &format!("header-coordinate-{case}"),
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
        let node_did = db.node_identity.did();
        let foreign_did = foreign_db.node_identity.did();
        let user_did = user.did();
        let foreign_user_did = foreign_user.did();
        let agent =
            gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
                .unwrap();
        let access = gents_store::config_client::ConfigAccess::local_managed_agent(
            db.node.clone(),
            identity::Did::new(agent.did()).unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        publish_signed_history(
            &db,
            &user,
            &client,
            &binding,
            "session",
            &["accepted".into()],
        )
        .await;
        let result = access
            .transact_write("test.single_header_coordinates", |txn| {
                Box::pin(async move {
                    let response = txn
                        .execute(&format!("{{ AgentMessage {{ {AGENT_MESSAGE_FIELDS} }} }}"))
                        .await?;
                    let original =
                        decode_transcript_message_row(&response["data"]["AgentMessage"][0])?;
                    let mut noise = transcript_message_create_variables(&original.message)?;
                    noise["input"]["message_key"] = serde_json::json!("unrelated-key");
                    noise["input"]["sequence"] = serde_json::json!(99);
                    noise["input"]["blocks"] = serde_json::json!({"invalid": "unrelated"});
                    for index in 0..if case == "unrelated" { 49 } else { 1 } {
                        noise["input"]["message_key"] =
                            serde_json::json!(format!("unrelated-{index}"));
                        noise["input"]["sequence"] = serde_json::json!(99 + index);
                        txn.execute_with_variables(CREATE_AGENT_MESSAGE_MUTATION, &noise)
                            .await?;
                    }

                    let mut other = original.message.clone();
                    let mut target = original.doc_id.clone();
                    match case {
                        "key" => other.sequence = 2,
                        "sequence" => other.message_key = "sequence-twin".into(),
                        "both" => other.native_id = Some("distinct-physical-twin".into()),
                        "foreign" => other.node_did = foreign_did.into(),
                        "requester" => {
                            other.requester_did = Some(foreign_user_did.into());
                            other.sequence = 2;
                        }
                        "fork" => {
                            other.session_id = "child-session".into();
                            other.message_key = "fork-key".into();
                            other.request_doc_id = None;
                            other.publication = MessagePublication::Fork {
                                origin_message_doc_id: original.doc_id.clone(),
                            };
                        }
                        _ => {}
                    }
                    if case != "unrelated" {
                        let created = txn
                            .execute_with_variables(
                                CREATE_AGENT_MESSAGE_MUTATION,
                                &transcript_message_create_variables(&other)?,
                            )
                            .await?;
                        assert_ne!(
                            gents_store::graphql::created_doc_id(&created, "AgentMessage")?,
                            original.doc_id,
                        );
                        if case == "fork" {
                            target =
                                gents_store::graphql::created_doc_id(&created, "AgentMessage")?;
                        }
                    }
                    let single = output::reader::load_canonical_message_in_txn(
                        txn,
                        &target,
                        node_did,
                        Some(user_did),
                    )
                    .await
                    .map_err(|error| format!("{error:#}"));
                    let bulk =
                        output::reader::TxnCanonicalReader::new(txn, node_did, Some(user_did))
                            .load_message(&target)
                            .await
                            .map_err(|error| format!("{error:#}"));
                    assert_eq!(single, bulk, "single/bulk mismatch for {case}");
                    assert_eq!(single.is_err(), case == "key", "{case}");
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await;
        if matches!(case, "sequence" | "both") {
            let error = result.expect_err("the existing session sequence is already occupied");
            assert!(format!("{error:#}").contains("unique index"), "{error:#}");
        } else {
            result.unwrap();
        }
        db.node.shutdown().await.expect("shut down test node");
    }
    foreign_db
        .node
        .shutdown()
        .await
        .expect("shut down foreign test node");
}

#[tokio::test]
async fn unsigned_starter_cannot_populate_the_index() {
    let case = gents_lean_vocab::lean_contract_snapshot()
        .session_document_projection_cases
        .iter()
        .find(|case| case["operation"] == "rename")
        .unwrap();
    use gents_store::identity::NodeIdentity;
    let (db, user, access, binding) = gents_inference::test_support::signed_test_node(
        "unsigned-starter",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let mut session = super::model_test_support::model_session(&case["before"]);
    session.node_did = db.node_identity.did().to_owned();
    session.requester_did = Some(user.did().to_owned());
    session.agent_did = binding.agent_did.clone();
    let source = session
        .provenance
        .as_mut()
        .unwrap()
        .started_by
        .as_mut()
        .unwrap();
    source.node_did = session.node_did.clone();
    source.requester_did = session.requester_did.clone();
    let source = session
        .provenance
        .as_ref()
        .unwrap()
        .started_by
        .as_ref()
        .unwrap();
    let scope = gents_protocol::session::SessionScope::from(source);
    for provenance in [session.provenance.clone(), None] {
        let session = &session;
        access
            .transact_write("test.session_starter", |txn| {
                let provenance = provenance.clone();
                Box::pin(async move {
                    ensure_session_in_txn(
                        txn,
                        &session.session_id,
                        &session.node_did,
                        &session.agent_did,
                        session.requester_did.as_deref(),
                        session.title.clone(),
                        provenance,
                        &session.created_at,
                    )
                    .await
                })
            })
            .await
            .unwrap();
    }
    let child_scope = gents_protocol::session::SessionScope::from(&session);
    assert!(
        super::origin::started_by(&access, &child_scope)
            .await
            .unwrap()
            .is_none(),
        "unsigned descriptive source cannot populate the index"
    );
    assert!(super::origin::lineage(&access, &scope)
        .await
        .unwrap()
        .started
        .is_empty());
    db.node.shutdown().await.expect("shut down test node");
}
