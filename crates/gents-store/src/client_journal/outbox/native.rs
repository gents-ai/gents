use super::*;
use crate::identity::NodeIdentity as _;
use gents_protocol::{
    authored_submission::AuthoredGoal, goal_control::GoalOperation,
    operations::request::MessageSendInput,
};

#[tokio::test]
async fn offline_authored_intents_retain_order_binding_and_preparation_after_reopen(
) -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let client =
        crate::identity::KeyIdentity::load_or_create(home.path().join("client.key"), None)?;
    let data = home.path().join("journal");
    let open = || {
        defra_node::Node::builder()
            .with_storage_backend(defra_node::StorageBackend::Regolith)
            .data_path(&data)
            .with_node_identity_did(client.did())
    };
    let node = Arc::new(open().build().await?);
    crate::schema::journal::ensure_journal_schemas(node.clone()).await?;
    let chat = AuthoredSubmission::Chat(Box::new(MessageSendInput {
        node_did: "did:test:offline-runtime".into(),
        session_id: Some("offline-session".into()),
        request_id: Some("offline-chat".into()),
        content: "Continue this work after reconnect".into(),
        ..Default::default()
    }));
    let goal = AuthoredSubmission::Goal(Box::new(AuthoredGoal {
        node_did: chat.node_did().into(),
        command_id: "offline-goal".into(),
        session_id: "offline-session".into(),
        session_doc_id: None,
        expected: None,
        operation: GoalOperation::Create {
            objective: "Finish the retained work".into(),
            token_budget: None,
        },
        agent_did: None,
        input: Default::default(),
    }));
    let first = reserve(&node, client.did(), &chat).await?;
    let second = reserve(&node, client.did(), &goal).await?;
    assert!(first.ordinal < second.ordinal);
    assert_eq!(
        reserve(&node, client.did(), &chat).await?.ordinal,
        first.ordinal
    );
    let mut changed = chat.clone();
    if let AuthoredSubmission::Chat(input) = &mut changed {
        input.content = "Different authored content".into();
    }
    assert!(matches!(
        reserve(&node, client.did(), &changed).await,
        Err(OutboxError::Binding)
    ));
    assert!(!acknowledge(&node, &first, false).await?);
    let staged = prepare(&node, &second, "exact retained signed preparation").await?;
    assert!(matches!(
        prepare(&node, &staged, "replacement preparation").await,
        Err(OutboxError::Binding)
    ));
    let before = pending(&node, client.did(), chat.node_did()).await?;
    assert_eq!(
        before
            .iter()
            .map(|record| record.submission_id.as_str())
            .collect::<Vec<_>>(),
        vec!["offline-chat", "offline-goal"]
    );
    assert_eq!(
        next(&node, client.did(), chat.node_did(), Some("offline-goal"))
            .await?
            .unwrap()
            .submission_id,
        second.submission_id
    );
    assert_eq!(
        pending(&node, client.did(), chat.node_did()).await?.len(),
        2
    );
    node.shutdown().await?;
    drop(node);

    let node = Arc::new(open().build().await?);
    crate::schema::journal::ensure_journal_schemas(node.clone()).await?;
    let restored = pending(&node, client.did(), chat.node_did()).await?;
    assert_eq!(
        restored
            .iter()
            .map(|record| record.ordinal)
            .collect::<Vec<_>>(),
        before
            .iter()
            .map(|record| record.ordinal)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        restored[1].prepared.as_deref(),
        Some("exact retained signed preparation")
    );
    assert_eq!(
        serde_json::to_value(restored[0].authored()?)?,
        serde_json::to_value(&chat)?
    );
    let mut unrelated = restored[0].clone();
    unrelated.digest = restored[1].digest.clone();
    assert!(!acknowledge(&node, &unrelated, true).await?);
    assert!(acknowledge(&node, &restored[0], true).await?);
    assert_eq!(
        next(&node, client.did(), chat.node_did(), None)
            .await?
            .unwrap()
            .submission_id,
        second.submission_id
    );
    let confirmed = reserve(&node, client.did(), &chat).await?;
    assert!(!confirmed.pending);
    assert!(confirmed.payload.is_none());
    assert!(confirmed.prepared.is_none());
    assert!(matches!(
        reserve(&node, client.did(), &changed).await,
        Err(OutboxError::Binding)
    ));
    node.shutdown().await?;
    Ok(())
}
