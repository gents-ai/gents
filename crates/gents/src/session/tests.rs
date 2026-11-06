use super::*;
use crate::inference::llm::message::{Text, ToolResult, ToolResultContent, UserContent};
use crate::test_support::first_content;

async fn sequenced_history_on(
    access: &crate::ConfigAccess,
    session_id: &str,
    node_did: &str,
    requester: Option<&str>,
    after: Option<u32>,
    excluded_request: Option<&str>,
) -> anyhow::Result<Vec<SequencedMessage>> {
    output::sequenced::load_sequenced_messages_using(
        output::access::ReadAccess::Config(access),
        session_id,
        node_did,
        requester,
        None,
        after,
        excluded_request,
        None,
    )
    .await
}

async fn history_on(
    access: &crate::ConfigAccess,
    session_id: &str,
    node_did: &str,
    requester: Option<&str>,
    excluded_request: Option<&str>,
) -> anyhow::Result<Vec<Message>> {
    Ok(sequenced_history_on(
        access,
        session_id,
        node_did,
        requester,
        None,
        excluded_request,
    )
    .await?
    .into_iter()
    .map(|row| row.message)
    .collect())
}

#[tokio::test]
async fn provider_history_excludes_current_input_but_keeps_its_tool_results() {
    use crate::store::identity::NodeIdentity;
    use crate::tools::call::lifecycle::admission_fixture::publish_accepted_on_claimed_request;
    use crate::tools::call::lifecycle::AwaitMode;
    let (db, user, client, binding) =
        crate::test_support::signed_test_node("provider-history").await;
    let session_id = "session-steering-provider-history";
    publish_signed_history(
        &db,
        &user,
        &client,
        &binding,
        session_id,
        &["older steering".into()],
    )
    .await;
    let mut current = signed_claimed_authored_request(
        &db,
        &user,
        &client,
        &binding,
        "current-steering",
        session_id,
    )
    .await;
    let mut tool = publish_accepted_on_claimed_request(
        db.node.clone(),
        &mut current,
        db.node_identity.did(),
        0,
        "fixture",
        "call-current",
        serde_json::json!({}),
        AwaitMode::Foreground,
        true,
    )
    .await
    .unwrap();
    let writer = crate::request::streaming::DefraStreamWriter::new_for_lifecycle(
        db.node.clone(),
        &current,
        std::time::Duration::ZERO,
    )
    .unwrap();
    writer
        .publish_authored_message(&current, "prompt", &Message::user("current steering"))
        .await
        .unwrap();
    tool.complete("tool finished").await.unwrap();
    let history = history_on(
        &client,
        session_id,
        db.node_identity.did(),
        Some(user.did()),
        Some(&current.request().doc_id),
    )
    .await
    .unwrap();
    assert_eq!(history.len(), 3);
    assert!(matches!(history.first(), Some(Message::User {content})
        if matches!(first_content(content), UserContent::Text(Text {text}) if text == "older steering")));
    assert!(
        matches!(&history[1], Message::Assistant {content, ..} if content.iter().any(|block|
        matches!(block, crate::inference::llm::message::AssistantContent::ToolCall(call) if call.id == "call-current")))
    );
    let tool_result = Message::User {
        content: vec![UserContent::ToolResult(ToolResult {
            id: "call-current".into(),
            call_id: Some("fixture-provider-call-0".into()),
            content: vec![ToolResultContent::Text(Text {
                text: "tool finished".into(),
            })],
        })],
    };
    assert_eq!(history.get(2), Some(&tool_result));
    db.node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn history_reconstruction_scans_a_shared_request_once() {
    use crate::store::identity::NodeIdentity;
    let (db, user, client, binding) =
        crate::test_support::signed_test_node("history-shared-scan").await;
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
    use crate::store::identity::NodeIdentity;
    let (db, user, client, binding) =
        crate::test_support::signed_test_node("history-batch-scan").await;
    let session_id = "session-many-request-scans";
    let count = crate::inference::rendered_request::commits::FIELD_COMMIT_ALIAS_BATCH + 3;
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
    crate::support::TestDb,
    crate::KeyIdentity,
    std::sync::Arc<dyn crate::store::identity::NodeIdentity>,
    crate::ConfigAccess,
    Vec<(String, String)>,
) {
    let (db, user, client, binding) = crate::test_support::signed_test_node(name).await;
    let agent = crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
        .unwrap();
    let access = crate::ConfigAccess::local_managed_agent(
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
    use crate::store::identity::NodeIdentity;
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
async fn legacy_cumulative_compaction_paths_extend_and_redeliver_as_deltas() {
    use crate::store::identity::NodeIdentity;
    let session_id = "session-legacy-file-activity";
    let (db, user, agent, access, requests) =
        signed_compaction_fixture("compaction-legacy", session_id, 2).await;
    let node_did = db.node_identity.did();
    let response = access.execute(r#"{ AgentSession(filter: {session_id: {_eq: "session-legacy-file-activity"}}, limit: 2) { _docID } }"#).await.unwrap();
    let [row] = response["data"]["AgentSession"]
        .as_array()
        .unwrap()
        .as_slice()
    else {
        panic!("fixture session must be unique")
    };
    let session_doc = row["_docID"].as_str().unwrap().to_owned();
    let input = serde_json::json!({
        "compaction_key": compaction_key(node_did, session_id, Some(user.did()), 1),
        "session_id": session_id, "node_did": node_did, "requester_did": user.did(),
        "request_id": requests[0].0, "request_doc_id": requests[0].1,
        "sequence": 1, "summary": "legacy", "files_read": "[\"/legacy/read.rs\"]",
        "files_modified": "[\"/legacy/write.rs\"]", "messages_compacted": 1,
        "compacted_through_sequence": 10, "original_tokens": 100, "compacted_tokens": 20,
        "created_at": "2026-10-15T00:00:00Z",
    });
    access.transact_write("test.legacy_compaction", |txn| {
        let input = &input;
        let session_doc = &session_doc;
        Box::pin(async move {
            let response = txn.execute_with_variables(
                "mutation($input: CompactionEntryMutationInputArg!) { create_CompactionEntry(input: $input) { _docID } }",
                &serde_json::json!({"input": input}),
            ).await?;
            let doc = crate::store::graphql::created_doc_id(&response, "CompactionEntry")?;
            txn.add_resource_relationship("CompactionEntry", &doc, "parent", "sessions", session_doc).await?;
            Ok::<_, anyhow::Error>(())
        })
    }).await.unwrap();
    let prior = load_prompt_compaction_state_with_access(
        &access,
        session_id,
        node_did,
        Some(user.did()),
        None,
    )
    .await
    .unwrap();

    let files_read = ["/legacy/read.rs".to_string(), "/new/read.rs".to_string()];
    let files_modified = ["/new/write.rs".to_string()];
    let save = || {
        compaction_entries::save_compaction_entry_with_access(
            &access,
            agent.did(),
            session_id,
            node_did,
            Some(user.did()),
            &requests[1].0,
            &requests[1].1,
            "new summary",
            &files_read,
            &files_modified,
            1,
            20,
            100,
            20,
            &prior.generation,
        )
    };
    let first = save().await.unwrap();
    assert_eq!(first.files_read, vec!["/legacy/read.rs", "/new/read.rs"]);
    assert_eq!(
        first.files_modified,
        vec!["/legacy/write.rs", "/new/write.rs"]
    );
    assert_eq!(save().await.unwrap(), first);
    let entries = load_compaction_entries(&access, session_id, node_did, Some(user.did()))
        .await
        .unwrap();
    assert_eq!(entries[0].files_read, vec!["/legacy/read.rs"]);
    assert_eq!(entries[1].files_read, first.files_read);
    assert_eq!(entries[1].files_modified, first.files_modified);
    let stored = access.execute(r#"{ CompactionEntry(filter: {session_id: {_eq: "session-legacy-file-activity"}, sequence: {_eq: 2}}) { files_read files_modified } }"#).await.unwrap();
    let stored_data = &stored["data"];
    let stored_row = &stored_data["CompactionEntry"][0];
    assert_eq!(
        stored_row["files_read"].as_str(),
        Some(r#"{"version":1,"paths":["/new/read.rs"]}"#)
    );
    assert_eq!(
        stored_row["files_modified"].as_str(),
        Some(r#"{"version":1,"paths":["/new/write.rs"]}"#)
    );
    db.node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn concurrent_compactions_from_one_generation_persist_exactly_one_fact() {
    use crate::store::identity::NodeIdentity;
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
    use crate::store::identity::NodeIdentity;
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
    use crate::store::identity::NodeIdentity;
    let (db, user, client, binding) = crate::test_support::signed_test_node("history-cursor").await;
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
    let deleted = client
        .write(
            "test.remove_middle_header",
            &format!(r#"mutation {{delete_AgentMessage(filter: {{_docID: {{_eq: "{doc}"}}}}) {{_docID}}}}"#,),
        )
        .await
        .unwrap();
    let deleted_rows = crate::store::graphql::mutation_documents(&deleted, "delete_AgentMessage")
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
    use crate::store::identity::NodeIdentity;
    let (db, user, client, binding) = crate::test_support::signed_test_node("close-session").await;
    let node_did = db.node_identity.did();
    reserve_signed_session(&db, &user, &client, &binding, "session-1").await;
    let before = client
        .execute(r#"{ AgentSession(filter: {session_id: {_eq: "session-1"}}) {created_at} }"#)
        .await
        .unwrap();
    let original_created_at = before["data"]["AgentSession"][0]["created_at"].clone();
    let agent = crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
        .unwrap();
    let agent_access = crate::ConfigAccess::local_managed_agent(
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
    let (db, user, client, binding) =
        crate::test_support::signed_test_node("session-idempotent").await;
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
    use crate::store::identity::NodeIdentity;

    let (db, user, access, binding) =
        crate::test_support::signed_test_node("session-binding").await;
    let node_did = db.node_identity.did();
    let other = crate::test_support::install_test_agent_as(
        &db.node,
        db.node_identity.as_ref(),
        &user,
        "Other fixture agent",
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
        access: &crate::store::config_client::ConfigAccess,
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

async fn session_rewrite_rows(access: &crate::ConfigAccess) -> Vec<Vec<String>> {
    let response = access
        .execute(
            "{ AgentSession { _docID session_id agent_did } \
           AgentMessage { _docID session_id sequence message_key } \
           CompactionEntry { _docID session_id sequence compaction_key summary } }",
        )
        .await
        .unwrap();
    let data = &response["data"];
    ["AgentSession", "AgentMessage", "CompactionEntry"]
        .into_iter()
        .map(|collection| {
            let mut rows = data[collection]
                .as_array()
                .expect("collection rows")
                .iter()
                .map(|row| row.to_string())
                .collect::<Vec<_>>();
            rows.sort();
            rows
        })
        .collect()
}

#[tokio::test]
async fn editing_a_canonical_user_message_forks_its_prefix_and_preserves_the_source() {
    use crate::store::identity::NodeIdentity;
    const SOURCE: &str = "edit-source";
    let case = crate::lean_vocab_test::lean_contract_snapshot()
        .session_document_cases
        .edit_anchors
        .iter()
        .find(|case| case["name"] == "three_user_turns")
        .unwrap();
    let (db, user, client, binding) = crate::test_support::signed_test_node("edit-fork").await;
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

#[tokio::test]
async fn retained_edit_fork_survives_reopen_without_copying_or_rechecking_current_agent() {
    use crate::NodeIdentity as _;
    const SOURCE: &str = "retained-source";
    const CHILD: &str = "retained-child";
    let (mut db, user, client, binding) =
        crate::test_support::signed_test_node("retained-edit-fork").await;
    let node_did = db.node_identity.did().to_owned();
    let messages = (1..=3)
        .map(|sequence| format!("retained-{sequence}"))
        .collect::<Vec<_>>();
    let keys = publish_signed_history(&db, &user, &client, &binding, SOURCE, &messages).await;
    let params = ForkParams {
        source_session_id: SOURCE,
        boundary: ForkBoundary::BeforeMessageKey(&keys[2]),
        caller_node_did: &node_did,
        caller_requester_did: Some(user.did()),
        target_agent_did: None,
    };
    assert_eq!(
        fork_edit_on(&client, params.clone(), CHILD, user.did())
            .await
            .unwrap(),
        CHILD
    );
    let agent = escape_graphql_string(&binding.agent_did);
    let response = client
        .execute(&format!(
            r#"{{ Agent(filter: {{agent_did: {{_eq: "{agent}"}}}}, limit: 2) {{ _docID }} }}"#
        ))
        .await
        .unwrap();
    let [row] = response["data"]["Agent"].as_array().unwrap().as_slice() else {
        panic!("fixture Agent must be unique")
    };
    let doc = escape_graphql_string(row["_docID"].as_str().unwrap());
    let mutation = format!(
        r#"mutation($input: AgentMutationInputArg!) {{ update_Agent(docID: "{doc}", input: $input) {{ _docID }} }}"#
    );
    client
        .transact_write("test.disable_fork_agent", |txn| {
            let mutation = &mutation;
            Box::pin(async move {
                txn.execute_client_signed_update(
                    mutation,
                    &serde_json::json!({"input":{"enabled":false}}),
                    gents_protocol::schemas::AGENT,
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    let before = session_rewrite_rows(&client).await;
    let history = history_on(&client, CHILD, &node_did, Some(user.did()), None)
        .await
        .unwrap();
    assert_eq!(history.len(), 2);
    drop(client);
    db.simulate_process_crash().await.unwrap();
    let client = db.client_access(&user);
    assert_eq!(
        fork_edit_on(&client, params.clone(), CHILD, user.did())
            .await
            .unwrap(),
        CHILD
    );
    assert_eq!(session_rewrite_rows(&client).await, before);
    assert_eq!(
        history_on(&client, CHILD, &node_did, Some(user.did()), None)
            .await
            .unwrap(),
        history
    );
    let foreign =
        tokio::task::spawn_blocking(|| crate::support::fixtures::test_identity("retained-foreign"))
            .await
            .unwrap();
    for changed in [
        ForkParams {
            boundary: ForkBoundary::BeforeMessageKey(&keys[1]),
            ..params.clone()
        },
        ForkParams {
            source_session_id: "another-source",
            ..params.clone()
        },
        ForkParams {
            caller_requester_did: Some(foreign.did()),
            ..params.clone()
        },
    ] {
        assert!(fork_edit_on(&client, changed, CHILD, user.did())
            .await
            .is_err());
        assert_eq!(session_rewrite_rows(&client).await, before);
    }
    assert!(fork_edit_on(&client, params.clone(), CHILD, foreign.did())
        .await
        .is_err());
    assert_eq!(session_rewrite_rows(&client).await, before);
    client
        .transact_write("test.unbound_fork_session", |txn| {
            let binding = &binding;
            let node_did = &node_did;
            let user = &user;
            Box::pin(async move {
                ensure_session_in_txn(
                    txn,
                    "unbound-child",
                    node_did,
                    &binding.agent_did,
                    Some(user.did()),
                    None,
                    None,
                    "2026-09-22T00:00:00Z",
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    let unbound_before = session_rewrite_rows(&client).await;
    assert!(fork_edit_on(&client, params, "unbound-child", user.did())
        .await
        .is_err());
    assert_eq!(session_rewrite_rows(&client).await, unbound_before);
    db.node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn fork_rolls_back_at_every_mutation_position() {
    use crate::store::identity::NodeIdentity;
    const SOURCE: &str = "fork-rollback-source";
    let (db, user, client, binding) = crate::test_support::signed_test_node("fork-rollback").await;
    let node = &db.node;
    let node_did = db.node_identity.did();
    let agent = crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
        .unwrap();
    let agent_access = crate::ConfigAccess::local_managed_agent(
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
    let (outcome, writes) =
        crate::store::config_client::ConfigApplyTxn::assert_every_mutation_rolls_back(
            || fork_on(&client, params.clone()),
            || session_rewrite_rows(&client),
        )
        .await;
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
    use crate::store::identity::NodeIdentity;
    const SESSION: &str = "compaction-rollback";
    let (db, user, client, binding) =
        crate::test_support::signed_test_node("compaction-rollback").await;
    let node = &db.node;
    let node_did = db.node_identity.did();
    let agent = crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
        .unwrap();
    let access = crate::ConfigAccess::local_managed_agent(
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
    let (saved, _) = crate::store::config_client::ConfigApplyTxn::assert_every_mutation_rolls_back(
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

#[expect(
    clippy::too_many_arguments,
    reason = "shared test helper with call sites in modules outside session"
)]
pub(crate) async fn import_history_observation(
    node: &std::sync::Arc<defra_node::Node>,
    request_id: &str,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    text: &str,
    message_key_suffix: &str,
    sequence: u32,
    tool_call: Option<(&str, &str)>,
) {
    use canonical_rows::*;
    use gents_protocol::output::*;
    let now = chrono::Utc::now().to_rfc3339();
    let segment = OutputSegment {
        node_did: node_did.into(),
        requester_did: requester_did.map(str::to_owned),
        session_id: session_id.into(),
        request_doc_id: request_id.into(),
        source: match tool_call {
            Some((doc_id, _)) => OutputSource::ToolCall {
                tool_call_doc_id: doc_id.into(),
            },
            None => OutputSource::Authored {
                key: message_key_suffix.into(),
            },
        },
        writer: match tool_call {
            Some((doc_id, _)) => OutputWriter::ToolExecution {
                tool_call_doc_id: doc_id.into(),
            },
            None => OutputWriter::RequestExecution {
                execution_generation: "observed-generation".into(),
            },
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: text.len().try_into().unwrap(),
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: if tool_call.is_some() {
                    StreamPayload::ToolOutput
                } else {
                    StreamPayload::Text
                },
            }),
        }],
        payload: text.into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![text.len() as u64],
        }),
        created_at: now.clone(),
    };
    let response = node
        .execute_request(
            defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                .with_variables(output_segment_create_variables(&segment).unwrap()),
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row =
        crate::store::graphql::single_mutation_document(&response, "create_AgentOutputSegment")
            .unwrap()
            .unwrap();
    let header = TranscriptMessage {
        native_request_id: None,
        message_key: message_key_suffix.into(),
        session_id: session_id.into(),
        node_did: node_did.into(),
        requester_did: requester_did.map(str::to_owned),
        request_doc_id: Some(request_id.into()),
        publication: match tool_call {
            Some((doc_id, _)) => MessagePublication::ToolDelivery {
                tool_call_doc_id: doc_id.into(),
            },
            None => MessagePublication::RequestExecution {
                execution_generation: "observed-generation".into(),
            },
        },
        outcome: OutputOutcome::Complete,
        sequence,
        role: MessageRole::User,
        native_id: None,
        blocks: vec![{
            let payload = PresentedPayload {
                output: PayloadRef {
                    close_doc_id: row["_docID"].as_str().unwrap().into(),
                    stream: 0,
                },
                presentation: PayloadPresentation::Full,
            };
            match tool_call {
                Some((doc_id, call_id)) => MessageBlock::ToolResult {
                    tool_call_doc_id: doc_id.into(),
                    id: call_id.into(),
                    call_id: Some(call_id.into()),
                    parts: vec![ToolResultPart::Text { text: payload }],
                },
                None => MessageBlock::Text { text: payload },
            }
        }],
        created_at: now,
    };
    let response = node
        .execute_request(
            defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&header).unwrap()),
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row = crate::store::graphql::single_mutation_document(&response, "create_AgentMessage")
        .unwrap()
        .unwrap();
    let (_, reconstructed) = load_canonical_message_from_node(
        node,
        row["_docID"].as_str().unwrap(),
        node_did,
        requester_did,
    )
    .await
    .unwrap();
    let expected = match tool_call {
        Some((_, call_id)) => Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                id: call_id.into(),
                call_id: Some(call_id.into()),
                content: vec![ToolResultContent::Text(Text { text: text.into() })],
            })],
        },
        None => Message::user(text),
    };
    assert_eq!(reconstructed, expected);
}

#[tokio::test]
async fn history_reads_exact_node_and_requester_scope() {
    use crate::store::identity::NodeIdentity;
    let (local_db, local_user, local_client, local_binding) =
        crate::test_support::signed_test_node("history-local").await;
    let (foreign_db, foreign_user, foreign_client, foreign_binding) =
        crate::test_support::signed_test_node("history-foreign").await;
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
    use crate::store::identity::NodeIdentity;
    let (first_db, first_user, first_client, first_binding) =
        crate::test_support::signed_test_node("authored-local").await;
    let (second_db, second_user, second_client, second_binding) =
        crate::test_support::signed_test_node("authored-foreign").await;
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
    use crate::store::identity::NodeIdentity;
    let (local_db, local_user, local_client, local_binding) =
        crate::test_support::signed_test_node("compaction-local").await;
    let (foreign_db, foreign_user, foreign_client, foreign_binding) =
        crate::test_support::signed_test_node("compaction-foreign").await;
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
            crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), binding)
                .unwrap();
        let agent_access = crate::ConfigAccess::local_managed_agent(
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
    db: &crate::support::TestDb,
    user: &dyn crate::store::identity::NodeIdentity,
    client: &crate::ConfigAccess,
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
    db: &crate::support::TestDb,
    user: &dyn crate::store::identity::NodeIdentity,
    client: &crate::ConfigAccess,
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

pub(crate) async fn publish_signed_history(
    db: &crate::support::TestDb,
    user: &dyn crate::store::identity::NodeIdentity,
    client: &crate::ConfigAccess,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
    session_id: &str,
    messages: &[String],
) -> Vec<String> {
    let mut keys = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        let request_id = format!("{session_id}-request-{index}");
        let mut lifecycle =
            signed_claimed_authored_request(db, user, client, binding, &request_id, session_id)
                .await;
        let writer = crate::request::streaming::DefraStreamWriter::new_for_lifecycle(
            db.node.clone(),
            &lifecycle,
            std::time::Duration::ZERO,
        )
        .unwrap();
        lifecycle.begin_owned_execution(&writer).await.unwrap();
        let key = canonical_rows::authored_message_key(&lifecycle.request().doc_id, "prompt");
        writer
            .publish_authored_message(&lifecycle, "prompt", &Message::user(message))
            .await
            .unwrap();
        lifecycle
            .terminalize_owned(
                crate::request::lifecycle::RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::NoMessage,
                None,
            )
            .await
            .unwrap();
        keys.push(key);
    }
    keys
}

async fn signed_claimed_authored_request(
    db: &crate::support::TestDb,
    user: &dyn crate::store::identity::NodeIdentity,
    client: &crate::ConfigAccess,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
    request_id: &str,
    session_id: &str,
) -> crate::request::lifecycle::RequestLifecycle {
    signed_claimed_authored_request_with_retry(
        db, user, client, binding, request_id, session_id, None,
    )
    .await
}

async fn signed_claimed_authored_request_with_retry(
    db: &crate::support::TestDb,
    user: &dyn crate::store::identity::NodeIdentity,
    client: &crate::ConfigAccess,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
    request_id: &str,
    session_id: &str,
    retry: Option<crate::request::lifecycle::RetryLink>,
) -> crate::request::lifecycle::RequestLifecycle {
    use crate::request::lifecycle::{
        ClaimOutcome, RequestIdentity, RequestLifecycle, RequestSigner, RequestSpec,
    };
    let agent =
        crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), binding).unwrap();
    let agent_access = crate::ConfigAccess::local_managed_agent(
        db.node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let mut spec = RequestSpec::new(
        gents_protocol::request_admission::RequestPurpose::Normal,
        RequestIdentity {
            request_id: request_id.into(),
            node_did: db.node_identity.did().into(),
            requester_did: Some(user.did().into()),
            agent_did: binding.agent_did.clone(),
            session_id: session_id.into(),
            content: "race".into(),
            execution_origin: crate::request::lifecycle::ExecutionOrigin::Interactive,
            created_at: chrono::Utc::now().to_rfc3339(),
        },
        gents_protocol::request_admission::AgentRequestAdmissionRecord::client(user.did()),
    );
    spec.retry = retry;
    let create = crate::request::lifecycle::build_signed_client_request(
        client,
        spec,
        RequestSigner::Identity(user),
    )
    .await
    .unwrap();
    crate::request::lifecycle::submit_client_request(client, &create)
        .await
        .unwrap();
    let response = agent_access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}, limit: 2) {{ {} }} }}"#,
            escape_graphql_string(request_id),
            crate::request::watcher::AGENT_REQUEST_FIELDS,
        ))
        .await
        .unwrap();
    let [row] = response["data"]["AgentRequest"]
        .as_array()
        .unwrap()
        .as_slice()
    else {
        panic!("signed fixture request must be unique");
    };
    let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(row.clone()).unwrap();
    let request = row.try_into().unwrap();
    let (_owner, authority) = crate::runtime::agent::p2p_reconcile::enrollment_authority_channel();
    let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
        db.node.clone(),
        db.node_identity.clone(),
        authority,
    );
    let (request, admission) = verifier
        .verify_fresh_for_claim_with_identity(&request, agent.as_ref())
        .await
        .unwrap();
    assert!(
        admission.is_some(),
        "client request needs verified claim admission"
    );
    let mut lifecycle = RequestLifecycle::new_with_node_did(
        db.node.clone(),
        agent.did(),
        db.node_identity.did(),
        request,
        60,
    );
    lifecycle.bind_managed_agent(agent.as_ref()).unwrap();
    lifecycle.set_client_claim_admission(admission);
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    lifecycle
}

#[tokio::test]
async fn concurrent_keyed_appends_resolve_sequence_conflicts_without_duplicates() {
    use crate::request::streaming::DefraStreamWriter;
    use crate::store::identity::NodeIdentity;
    use std::time::Duration;
    let (db, user, client, binding) = crate::test_support::signed_test_node("append-race").await;
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
    use crate::store::identity::NodeIdentity;
    use canonical_rows::{
        decode_transcript_message_row, transcript_message_create_variables, AGENT_MESSAGE_FIELDS,
        CREATE_AGENT_MESSAGE_MUTATION,
    };
    use gents_protocol::output::MessagePublication;

    let (foreign_db, foreign_user, _foreign_client, _foreign_binding) =
        crate::test_support::signed_test_node("header-coordinate-foreign").await;
    for case in [
        "unrelated",
        "key",
        "sequence",
        "both",
        "foreign",
        "requester",
        "fork",
    ] {
        let (db, user, client, binding) =
            crate::test_support::signed_test_node(&format!("header-coordinate-{case}")).await;
        let node_did = db.node_identity.did();
        let foreign_did = foreign_db.node_identity.did();
        let user_did = user.did();
        let foreign_user_did = foreign_user.did();
        let agent =
            crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
                .unwrap();
        let access = crate::ConfigAccess::local_managed_agent(
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
                            crate::store::graphql::created_doc_id(&created, "AgentMessage")?,
                            original.doc_id,
                        );
                        if case == "fork" {
                            target =
                                crate::store::graphql::created_doc_id(&created, "AgentMessage")?;
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
async fn retry_frontier_matches_lean_admission() {
    use crate::store::identity::NodeIdentity;
    use crate::tools::call::lifecycle::admission_fixture::publish_accepted_on_claimed_request;
    use crate::tools::call::lifecycle::AwaitMode;
    let (db, user, client, binding) = crate::test_support::signed_test_node("retry-frontier").await;
    let node = &db.node;
    let node_did = db.node_identity.did();
    let other = tokio::task::spawn_blocking(|| {
        crate::support::fixtures::test_identity("retry-foreign-user")
    })
    .await
    .unwrap();
    for (index, case) in crate::lean_vocab_test::lean_contract_snapshot()
        .retry_frontier_cases
        .iter()
        .enumerate()
    {
        let session_id = format!("retry-frontier-{index}");
        let parent_request_id = format!("retry-frontier-parent-{index}");
        let mut parent = signed_claimed_authored_request(
            &db,
            &user,
            &client,
            &binding,
            &parent_request_id,
            &session_id,
        )
        .await;
        let writer = crate::request::streaming::DefraStreamWriter::new_for_lifecycle(
            node.clone(),
            &parent,
            std::time::Duration::ZERO,
        )
        .unwrap();
        if case["running"].as_bool().unwrap() {
            let mode = if case["background"].as_bool().unwrap() {
                AwaitMode::Background
            } else {
                AwaitMode::Foreground
            };
            let _tool = publish_accepted_on_claimed_request(
                node.clone(),
                &mut parent,
                node_did,
                0,
                "fixture",
                &format!("retry-tool-{index}"),
                serde_json::json!({}),
                mode,
                true,
            )
            .await
            .unwrap();
        } else {
            parent.begin_owned_execution(&writer).await.unwrap();
        }
        if case["published"].as_bool().unwrap() {
            writer
                .publish_authored_message(&parent, "prompt", &Message::user("Do the work"))
                .await
                .unwrap();
        }
        let parent_doc = parent.request().doc_id.clone();
        let mut request = parent.request().clone();
        request.doc_id = "successor".into();
        request.request_id = "successor".into();
        request.retry_parent_request_doc_id = Some(parent_doc.clone());
        if !case["same_requester"].as_bool().unwrap() {
            request.requester_did = Some(other.did().into());
        }
        if !case["same_session"].as_bool().unwrap() {
            request.session_id = format!("fresh-session-{index}");
        }
        if case["scoped_terminal"].as_bool().unwrap() {
            parent
                .terminalize_owned(
                    crate::request::lifecycle::RequestTerminalOutcome::Dead,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    Some("Stale"),
                )
                .await
                .unwrap();
        }
        let actual = history::retry_has_published_input(node, &request).await;
        assert_eq!(actual.ok(), case["expected"].as_bool(), "{case}");
        if case["name"] == "ready" {
            let retry = crate::request::lifecycle::RetryLink {
                parent_request_id: Some(parent_request_id.clone()),
                parent_request_doc_id: Some(parent_doc.clone()),
                root_request_id: parent_request_id.clone(),
                retry_count: 1,
                max_retries: 3,
            };
            let mut second = signed_claimed_authored_request_with_retry(
                &db,
                &user,
                &client,
                &binding,
                "retry-frontier-second",
                &session_id,
                Some(retry),
            )
            .await;
            let second_writer = crate::request::streaming::DefraStreamWriter::new_for_lifecycle(
                node.clone(),
                &second,
                std::time::Duration::ZERO,
            )
            .unwrap();
            second.begin_owned_execution(&second_writer).await.unwrap();
            second
                .terminalize_owned(
                    crate::request::lifecycle::RequestTerminalOutcome::Failed,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    None,
                )
                .await
                .unwrap();
            request.retry_parent_request_doc_id = Some(second.request().doc_id.clone());
            assert_eq!(
                history::retry_has_published_input(node, &request)
                    .await
                    .ok(),
                case["expected"].as_bool(),
                "retry chain must find the same authored input"
            );
        }
    }
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn unsigned_starter_cannot_populate_the_index() {
    let starter_cases = &crate::lean_vocab_test::lean_contract_snapshot()
        .session_document_cases
        .starter_verification;
    assert_eq!(
        starter_cases
            .iter()
            .find(|case| case["name"] == "signature")
            .unwrap()["accepted"],
        false
    );
    let case = crate::lean_vocab_test::lean_contract_snapshot()
        .session_document_cases
        .projection
        .iter()
        .find(|case| case["operation"] == "rename")
        .unwrap();
    use crate::store::identity::NodeIdentity;
    let (db, user, access, binding) =
        crate::test_support::signed_test_node("unsigned-starter").await;
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
    let scope = crate::session::origin::SessionScope::from(source);
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
    let child_scope = crate::session::origin::SessionScope::from(&session);
    assert!(
        crate::session::origin::started_by(&access, &child_scope)
            .await
            .unwrap()
            .is_none(),
        "unsigned descriptive source cannot populate the index"
    );
    assert!(crate::session::origin::lineage(&access, &scope)
        .await
        .unwrap()
        .started
        .is_empty());
    db.node.shutdown().await.expect("shut down test node");
}
