use super::*;

#[tokio::test]
async fn canonical_publication_wakes_whatever_goal_owns_the_session() {
    for case in gents_lean_vocab::families::contracts_r6_goal_owner_publication::cases() {
        let name = case.input.name.as_str();
        let status = case.input.goal_status.as_ref().map(|status| {
            serde_json::from_value::<gents_protocol::goal_status::GoalStatus>(serde_json::json!(
                status
            ))
            .unwrap()
        });
        let (db, mut fixture) = fixture(&format!("goal-publication-{name}")).await;
        let parent = fixture.parent.clone();
        let prepared = fixture
            .prepare_notification("terminal output")
            .await
            .unwrap();
        let queue = gents_toolcalls::test_support::queue::background_goal_repair::hints(&parent);
        let gate = gents_session::request::lifecycle::queue::background_completion_gate(
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
        let access = gents_store::config_client::ConfigAccess::local_managed_agent(
            db.node.clone(),
            identity::Did::new(db.agent_did()).unwrap(),
            db.agent_identity.as_ref(),
        )
        .unwrap();
        let goal = match status {
            Some(status) => Some(
                gents_automation::goal::seed_goal_from_access(
                    &access,
                    db.node_did(),
                    &parent.session_id,
                    Some("Goal owns continuation"),
                    Some(status),
                    Some(Some(10)),
                    None,
                )
                .await
                .unwrap(),
            ),
            None => None,
        };
        drop(held);
        let result = publication.await.unwrap().unwrap();
        let observed =
            gents_lean_vocab::families::contracts_r6_goal_owner_publication::GoalPublicationOutput {
                notification_persisted: message_ids(&db, "canonical-goal-notification")
                    .await
                    .len()
                    == 1,
                wake_created: result.request.is_some() && result.created_request,
            };
        assert_eq!(queue_rows(&db, &parent.session_id).await.len(), 2, "{name}");
        let after = access
            .transact_readonly("test.goal_wake_after", |txn| {
                Box::pin(gents_automation::goal::load_canonical_goal_in_txn(
                    txn,
                    db.node_did(),
                    &parent.session_id,
                ))
            })
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(goal).unwrap(),
            serde_json::to_value(after).unwrap(),
            "{name}"
        );
        case.check(observed);
    }
}

use gents_toolcalls::test_support::queue::background_goal_repair::*;
