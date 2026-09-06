use super::*;

async fn legacy_fixture(name: &str) -> (TestDb, AgentRequest, String) {
    let db = test_db(name).await;
    let mut parent = parent_request(db.agent_did(), name);
    parent.subagent_depth = 0;
    parent.caused_by_parent_request_id = None;
    parent.caused_by_parent_request_doc_id = None;
    parent.caused_by_parent_tool_call_id = None;
    parent.caused_by_parent_tool_call_doc_id = None;
    parent.doc_id =
        insert_raw_queue_request(&db.node, db.agent_did(), &parent.request_id, name, "{}").await;
    let content = serde_json::to_string(&crate::llm::message::Message::User {
        content: vec![crate::llm::message::UserContent::Text(
            crate::llm::message::Text {
                text: "legacy terminal output".into(),
            },
        )],
    })
    .unwrap();
    let mutation = format!(
        r#"mutation {{ create_AgentMessage(input: {{
        message_key: "legacy-notification", agent_did: "{}", session_id: "{}",
        sequence: 1, role: "user", content: "{}", timestamp: "2026-07-15T00:00:00Z"
    }}) {{_docID}} }}"#,
        escape_graphql_string(db.agent_did()),
        escape_graphql_string(name),
        escape_graphql_string(&content)
    );
    let response = session::execute_mutation_with_retry(&db.node, &mutation, "seed legacy input")
        .await
        .unwrap();
    let doc =
        extract_single_doc_id(&response, "create_AgentMessage").expect("legacy message doc ID");
    (db, parent, doc)
}

fn repair_hints(parent: &AgentRequest) -> QueueHints {
    QueueHints {
        source: QueueSource::BackgroundCompletion,
        policy: QueuePolicy::Coalesce,
        key: Some(format!("background_completion:{}", parent.session_id)),
        queued_after_request_id: Some(parent.request_id.clone()),
        interrupted_request_id: None,
    }
}

async fn durable_input(db: &TestDb) -> serde_json::Value {
    let response = db.node.execute(
        "{ AgentMessage { _docID message_key agent_did session_id request_id request_doc_id sequence role content timestamp } }",
    ).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    response.data.unwrap()["AgentMessage"].clone()
}

#[tokio::test]
async fn legacy_repair_rechecks_goal_after_waiting_for_existing_enqueue_gate() {
    use std::future::Future;
    let (db, parent, legacy_doc) = legacy_fixture("goal-before-legacy-repair").await;
    let before = durable_input(&db).await;
    let hints = repair_hints(&parent);
    let gate = super::super::atomic_inputs::background_completion_gate(
        &db.node,
        &parent.session_id,
        &parent.agent_did,
        hints.key.as_deref().unwrap(),
    );
    let held = gate.lock().await;
    let mut repair = Box::pin(
        super::super::atomic_inputs::persist_background_completion_with_message(
            &db.node,
            &parent,
            "legacy terminal output",
            "background-completion-notification:legacy:tool",
            "review notifications",
            repair_hints(&parent),
            Some(&legacy_doc),
        ),
    );
    // Poll the actual operation into its held existing gate, without a spawned
    // unowned task or a timing assumption about scheduler progress.
    std::future::poll_fn(|cx| {
        assert!(repair.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    let goal = crate::goal::set_goal(
        &db.node,
        db.agent_did(),
        &parent.session_id,
        Some("Goal owns continuation"),
        Some(crate::goal::GoalStatus::Paused),
        Some(Some(10)),
    )
    .await
    .unwrap();
    drop(held);
    let result = repair.await.unwrap();
    assert!(result.request.is_none());
    assert!(!result.created_request);
    assert_eq!(
        durable_input(&db).await,
        before,
        "legacy input is immutable"
    );
    assert_eq!(queue_rows(&db.node, &parent.session_id).await.len(), 1);
    let after = crate::goal::load_canonical_goal(&db.node, db.agent_did(), &parent.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(goal).unwrap(),
        serde_json::to_value(after).unwrap()
    );
}

#[tokio::test]
async fn ordinary_legacy_repair_and_replay_preserve_one_input_and_one_wake() {
    let (db, parent, legacy_doc) = legacy_fixture("ordinary-legacy-repair").await;
    let before = durable_input(&db).await;
    let mut first_wake = None;
    for first in [true, false] {
        let result = super::super::atomic_inputs::persist_background_completion_with_message(
            &db.node,
            &parent,
            "legacy terminal output",
            "background-completion-notification:legacy:tool",
            "review notifications",
            repair_hints(&parent),
            Some(&legacy_doc),
        )
        .await
        .unwrap();
        assert_eq!(result.created_request, first);
        let returned_wake = result
            .request
            .expect("ordinary legacy repair returns its wake")
            .doc_id;
        if first {
            first_wake = Some(returned_wake.clone());
        }
        assert_eq!(Some(&returned_wake), first_wake.as_ref());
        assert_eq!(
            durable_input(&db).await,
            before,
            "repair cannot rewrite legacy input"
        );
        let requests = queue_rows(&db.node, &parent.session_id).await;
        assert_eq!(requests.len(), 2);
        assert!(requests
            .iter()
            .any(|request| Some(&request.doc_id) == first_wake.as_ref()));
    }
}
