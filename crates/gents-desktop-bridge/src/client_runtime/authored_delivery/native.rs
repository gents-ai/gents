use super::*;
use anyhow::Context;
use gents_desktop_core::client::DesktopPaths;
use gents_protocol::{
    goal_control::GoalOperation, operations::request::MessageSendInput,
    runtime_connection::RuntimeConnection,
};
use gents_store::client_journal::outbox;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disconnected_chat_and_goal_are_delivered_once_after_client_restart() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("gents_desktop_bridge::client_runtime::authored_delivery=debug")
        .with_test_writer()
        .try_init();
    let directory = tempfile::tempdir()?;
    let paths = DesktopPaths::from_root(directory.path());
    let options = crate::client_test_support::file_journal_options();
    let core = Arc::new(
        ClientRuntime::start_with_paths_and_options(paths.clone(), options.clone()).await?,
    );
    assert!(core.node().p2p().is_none());
    core.stop_observer_for_test().await;
    let runtime = gents::client_test_support::ServedRuntimeHome::start_unpublished_for(Arc::new(
        core.node_identity().clone(),
    ))
    .await;
    runtime.publish_node(serde_json::json!({})).await;
    let agent = runtime.publish_chat_agent().await;
    core.connect_runtime(RuntimeConnection {
        node_did: runtime.node_did().into(),
        label: "Hosted runtime".into(),
        graphql: runtime.graphql().into(),
        local_node_home: None,
        access_request: None,
        access_state: None,
    })
    .await?;
    let message = MessageSendInput {
        node_did: runtime.node_did().into(),
        session_id: Some("offline-chat-session".into()),
        request_id: Some("offline-chat-request".into()),
        agent_did: Some(agent.agent_did.clone()),
        content: "Retain this while disconnected".into(),
        ..Default::default()
    };
    let retained = core.send_message(message.clone()).await.unwrap_err();
    assert!(
        retained.downcast_ref::<SavedForDelivery>().is_some(),
        "{retained:#}"
    );
    let goal = GoalOperation::Create {
        objective: "Retain this offline Goal".into(),
        token_budget: None,
    };
    let retained = core
        .control_session_goal_correlated(
            "offline-goal-command",
            runtime.node_did(),
            "offline-goal-session",
            None,
            None,
            goal,
            Some(&agent.agent_did),
            Default::default(),
            None,
        )
        .await
        .unwrap_err();
    assert!(
        retained.downcast_ref::<SavedForDelivery>().is_some(),
        "{retained:#}"
    );
    assert_eq!(
        outbox::pending(
            &core.node_arc(),
            core.node_identity().did(),
            runtime.node_did()
        )
        .await?
        .len(),
        2
    );
    assert!(runtime
        .user_access()
        .execute("{ AgentRequest { _docID } }")
        .await?["data"]["AgentRequest"]
        .as_array()
        .unwrap()
        .is_empty());
    core.shutdown().await?;
    drop(core);
    let core = Arc::new(
        ClientRuntime::start_with_paths_and_options(paths.clone(), options.clone()).await?,
    );
    let mut updates = core.store_updates();
    core.start_authored_delivery();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            if outbox::pending(
                &core.node_arc(),
                core.node_identity().did(),
                runtime.node_did(),
            )
            .await?
            .is_empty()
            {
                return Ok::<_, anyhow::Error>(());
            }
            updates.changed().await?;
        }
    })
    .await??;
    let receipt = core
        .recover_goal_submission(runtime.node_did(), "offline-goal-command")
        .await?
        .context("retained Goal receipt")?;
    assert!(matches!(
        receipt.receipt.as_ref().map(|receipt| &receipt.outcome),
        Some(gents_protocol::goal_control::GoalControlOutcome::Applied { .. })
    ));
    let response = runtime
        .user_access()
        .execute("{ AgentRequest { request_id session_id content } }")
        .await?;
    let requests = response["data"]["AgentRequest"].as_array().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|row| row["request_id"] == "offline-chat-request")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|row| row["session_id"] == "offline-goal-session")
            .count(),
        1
    );
    let replay = core.send_message(message).await?;
    assert_eq!(replay.request_id, "offline-chat-request");
    assert_eq!(replay.session_id, "offline-chat-session");
    core.shutdown().await?;
    drop(core);
    let core = Arc::new(ClientRuntime::start_with_paths_and_options(paths, options).await?);
    core.start_authored_delivery();
    assert!(outbox::pending(
        &core.node_arc(),
        core.node_identity().did(),
        runtime.node_did()
    )
    .await?
    .is_empty());
    assert_eq!(
        runtime
            .user_access()
            .execute("{ AgentRequest { request_id session_id content } }")
            .await?["data"],
        response["data"]
    );
    assert!(!core
        .node()
        .get_all_collection_versions()
        .await?
        .iter()
        .any(|collection| matches!(
            collection.name.as_str(),
            "AgentRequest" | "AgentSession" | "Agent" | "Node" | "NodeJoinRequest"
        )));
    core.shutdown().await?;
    runtime.shutdown().await;
    Ok(())
}
