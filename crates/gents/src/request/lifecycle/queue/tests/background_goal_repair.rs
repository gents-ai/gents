use super::*;

async fn fixture(name: &str) -> (TestDb, CanonicalBackgroundFixture) {
    let db = test_db(name).await;
    let fixture = canonical_background_fixture(&db, name).await;
    (db, fixture)
}

fn hints(parent: &AgentRequest) -> RequestQueue {
    RequestQueue {
        delivery: Default::default(),
        position: None,
        source: QueueSource::BackgroundCompletion,
        policy: QueuePolicy::Coalesce,
        key: Some(format!("background_completion:{}", parent.session_id)),
        queued_after_request_id: Some(parent.request_id.clone()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    }
}

async fn message_ids(db: &TestDb, key: &str) -> Vec<String> {
    let key = escape_graphql_string(key);
    let response = db
        .agent_access
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{message_key: {{_eq: "{key}"}}}}) {{ _docID }} }}"#
        ))
        .await
        .unwrap();
    response["data"]["AgentMessage"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["_docID"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn canonical_publication_wakes_whatever_goal_owns_the_session() {
    let cases = crate::lean_vocab_test::lean_r6_backgrounding_cases()
        .iter()
        .filter(|case| case.group == "completion_continuation_owner")
        .filter_map(|case| Some((case, case.goal_status.as_deref()?)))
        .collect::<Vec<_>>();
    assert!(cases.iter().any(|(_, status)| *status == "paused"));
    assert!(cases.iter().any(|(_, status)| *status == "complete"));
    for (case, status) in cases {
        let status: crate::goal::GoalStatus =
            serde_json::from_value(serde_json::json!(status)).unwrap();
        let (db, mut fixture) = fixture(&format!("goal-publication-{}", case.name)).await;
        let parent = fixture.parent.clone();
        let prepared = fixture
            .prepare_notification("terminal output")
            .await
            .unwrap();
        let queue = hints(&parent);
        let gate = super::super::atomic_inputs::background_completion_gate(
            &db.node,
            &parent.session_id,
            &parent.node_did,
            queue.key.as_deref().unwrap(),
        );
        let held = gate.lock().await;
        let publish_access = db.agent_access.clone();
        let publish_node = db.node.clone();
        let publish_parent = parent.clone();
        let publication = tokio::spawn(async move {
            persist_background_completion_with_message_canonical_with_access(
                &publish_access,
                &publish_node,
                &publish_parent,
                "terminal output",
                "canonical-goal-notification",
                "review notifications",
                queue,
                None,
                &prepared,
                gents_protocol::request_lifecycle::RequestHopCause::Continuation,
            )
            .await
        });
        let access = crate::ConfigAccess::local_managed_agent(
            db.node.clone(),
            identity::Did::new(db.agent_did()).unwrap(),
            db.agent_identity.as_ref(),
        )
        .unwrap();
        let goal = crate::goal::set_goal_from_access(
            &access,
            db.node_did(),
            &parent.session_id,
            Some("Goal owns continuation"),
            Some(status),
            Some(Some(10)),
            None,
        )
        .await
        .unwrap();
        drop(held);
        let result = publication.await.unwrap().unwrap();
        assert_eq!(
            result.request.is_some() && result.created_request,
            case.wake_created == Some(true),
            "{}",
            case.name
        );
        assert_eq!(
            message_ids(&db, "canonical-goal-notification").await.len() == 1,
            case.notification_persisted == Some(true),
            "{}",
            case.name
        );
        assert_eq!(queue_rows(&db, &parent.session_id).await.len(), 2);
        let after = access
            .transact_readonly("test.goal_wake_after", |txn| {
                Box::pin(crate::goal::load_canonical_goal_in_txn(
                    txn,
                    db.node_did(),
                    &parent.session_id,
                ))
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(goal).unwrap(),
            serde_json::to_value(after).unwrap(),
            "{}",
            case.name
        );
    }
}

#[tokio::test]
async fn canonical_publication_replay_preserves_one_input_and_one_wake() {
    let (db, mut fixture) = fixture("ordinary-canonical-publication").await;
    let parent = fixture.parent.clone();
    let prepared = fixture
        .prepare_notification("terminal output")
        .await
        .unwrap();
    let mut first_wake = None;
    let mut first_message = None;
    for first in [true, false] {
        let result = fixture
            .publish_prepared_notification(
                "terminal output",
                "canonical-ordinary-notification",
                "review notifications",
                hints(&parent),
                None,
                &prepared,
            )
            .await
            .unwrap();
        assert_eq!(result.created_request, first);
        let wake = result
            .request
            .expect("ordinary publication returns wake")
            .doc_id;
        if first {
            first_wake = Some(wake.clone());
        }
        assert_eq!(Some(&wake), first_wake.as_ref());
        let messages = message_ids(&db, "canonical-ordinary-notification").await;
        assert_eq!(messages.len(), 1);
        if first {
            first_message = messages.first().cloned();
        }
        assert_eq!(messages.first(), first_message.as_ref());
        assert_eq!(queue_rows(&db, &parent.session_id).await.len(), 2);
    }
}
