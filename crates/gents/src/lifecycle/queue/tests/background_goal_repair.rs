use super::*;

async fn fixture(name: &str) -> (TestDb, AgentRequest) {
    let db = test_db(name).await;
    let mut parent = parent_request(db.node_did(), name);
    parent.doc_id = insert_raw_queue_request(
        &db.node,
        db.node_did(),
        &parent.request_id,
        name,
        &RequestInput::default(),
    )
    .await;
    (db, parent)
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

async fn message_ids(db: &TestDb) -> Vec<String> {
    let response = db.node.execute("{ AgentMessage { _docID } }").await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    response.data.unwrap()["AgentMessage"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["_docID"].as_str().unwrap().to_owned())
        .collect()
}

/// Lean `r6GoalOwnerCase` for every Goal status, including a Paused and a
/// Complete Goal: a Goal set while a completion is being published never
/// suppresses it. The notification and its wake are published, and the Goal
/// is left as it was.
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
        let (db, parent) = fixture(&format!("goal-publication-{}", case.name)).await;
        let queue = hints(&parent);
        let gate = super::super::atomic_inputs::background_completion_gate(
            &db.node,
            &parent.session_id,
            &parent.node_did,
            queue.key.as_deref().unwrap(),
        );
        let held = gate.lock().await;
        let publish_node = db.node.clone();
        let publish_parent = parent.clone();
        let publication = tokio::spawn(async move {
            super::super::atomic_inputs::persist_background_completion_with_message(
                &publish_node,
                &publish_parent,
                "terminal output",
                "canonical-goal-notification",
                "review notifications",
                hints(&publish_parent),
                None,
            )
            .await
        });
        let goal = crate::goal::set_goal(
            &db.node,
            db.node_did(),
            &parent.session_id,
            Some("Goal owns continuation"),
            Some(status),
            Some(Some(10)),
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
            message_ids(&db).await.len() == 1,
            case.notification_persisted == Some(true),
            "{}",
            case.name
        );
        assert_eq!(queue_rows(&db.node, &parent.session_id).await.len(), 2);
        let after = crate::goal::load_canonical_goal(&db.node, db.node_did(), &parent.session_id)
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
    let (db, parent) = fixture("ordinary-canonical-publication").await;
    let mut first_wake = None;
    let mut first_message = None;
    for first in [true, false] {
        let result = super::super::atomic_inputs::persist_background_completion_with_message(
            &db.node,
            &parent,
            "terminal output",
            "canonical-ordinary-notification",
            "review notifications",
            hints(&parent),
            None,
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
        let messages = message_ids(&db).await;
        assert_eq!(messages.len(), 1);
        if first {
            first_message = messages.first().cloned();
        }
        assert_eq!(messages.first(), first_message.as_ref());
        assert_eq!(queue_rows(&db.node, &parent.session_id).await.len(), 2);
    }
}
