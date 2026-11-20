use super::*;
use anyhow::Context;
use gents_protocol::{
    goal_control::GoalOperation, operations::request::MessageSendInput,
    runtime_connection::RuntimeConnection,
};
use gents_store::client_journal::outbox;
use gents_store::client_journal::paths::DesktopPaths;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disconnected_chat_and_goal_are_delivered_once_after_client_restart() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            "warn,gents_desktop_bridge::client_runtime=debug,gents_observe::client_projection=debug",
        )
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
        graphql: "http://127.0.0.1:9/api/v0/graphql".into(),
        local_node_home: None,
        access_request: None,
        access_state: None,
    })
    .await?;
    let invalid = core
        .send_message(MessageSendInput {
            node_did: runtime.node_did().into(),
            request_id: Some("empty-chat-request".into()),
            content: " \n ".into(),
            ..Default::default()
        })
        .await;
    assert!(invalid.is_err());
    assert!(outbox::pending(
        &core.node_arc(),
        core.node_identity().did(),
        runtime.node_did()
    )
    .await?
    .is_empty());
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
    core.connect_runtime(RuntimeConnection {
        node_did: runtime.node_did().into(),
        label: "Hosted runtime".into(),
        graphql: runtime.graphql().into(),
        local_node_home: None,
        access_request: None,
        access_state: None,
    })
    .await?;
    core.shutdown().await?;
    drop(core);
    let core = Arc::new(
        ClientRuntime::start_with_paths_and_options(paths.clone(), options.clone()).await?,
    );
    let mut updates = gents_store::subscription_source::subscribe_collections(
        core.node(),
        ["ClientAuthoredSubmission"],
    )?;
    let mut controls = gents_store::subscription_source::subscribe_collections(
        runtime.node(),
        ["AgentSessionGoalControl"],
    )?;
    let endpoint = runtime.user_endpoint();
    let node_did = runtime.node_did().to_owned();
    let agent_did = agent.agent_did.clone();
    let goal_answer = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        let document = loop {
            let batch =
                tokio::time::timeout(gents_store::test_support::STALL_CEILING, controls.recv())
                    .await
                    .context("no Goal command reached the runtime within the stall ceiling")?
                    .context("Goal command stream closed")?;
            if let Some(change) = batch
                .changes
                .into_iter()
                .find(|change| !change.doc_id.is_empty())
            {
                break change.doc_id;
            }
        };
        let mut url = reqwest::Url::parse(endpoint.url())?;
        url.set_path(gents_protocol::goal_control::APPLY_GOAL_CONTROL_PATH);
        let response = endpoint
            .authorize_runtime(reqwest::Client::new().post(url))
            .await?
            .json(&gents_protocol::goal_control::ApplyGoalControl {
                node_did,
                command_doc_id: document,
                agent_did,
            })
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        anyhow::ensure!(
            status.is_success(),
            "runtime Goal application failed: {status}: {body}"
        );
        Ok::<_, anyhow::Error>(())
    }));
    core.start_authored_delivery();
    let settled = async {
        let delivered = async {
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
                tokio::time::timeout(gents_store::test_support::STALL_CEILING, updates.recv())
                    .await
                    .context("the client journal did not change within the stall ceiling")?
                    .context("client journal stream closed")?;
            }
        };
        let answered = async move {
            goal_answer.await??;
            Ok::<_, anyhow::Error>(())
        };
        tokio::try_join!(delivered, answered)?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(error) = settled {
        let pending = outbox::pending(
            &core.node_arc(),
            core.node_identity().did(),
            runtime.node_did(),
        )
        .await?;
        let states = pending
            .iter()
            .map(|record| (&record.submission_id, record.prepared.is_some()))
            .collect::<Vec<_>>();
        return Err(error).with_context(|| {
            format!(
                "retained delivery did not settle; connection={:?}; pending={states:?}",
                core.stores().runtime_connection(runtime.node_did())
            )
        });
    }
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blank_request_or_session_id_is_retained_under_the_identity_delivery_uses() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let paths = DesktopPaths::from_root(directory.path());
    let core = Arc::new(
        ClientRuntime::start_with_paths_and_options(
            paths,
            crate::client_test_support::file_journal_options(),
        )
        .await?,
    );
    core.stop_observer_for_test().await;
    let node_did = "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK";
    core.connect_runtime(RuntimeConnection {
        node_did: node_did.into(),
        label: "Unreachable runtime".into(),
        graphql: "http://127.0.0.1:9/api/v0/graphql".into(),
        local_node_home: None,
        access_request: None,
        access_state: None,
    })
    .await?;
    let saved = core
        .send_message(MessageSendInput {
            node_did: node_did.into(),
            session_id: Some("  ".into()),
            request_id: Some(" ".into()),
            content: "Retain this under a real identity".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    let saved = saved
        .downcast_ref::<SavedForDelivery>()
        .context("the chat is saved for delivery")?;
    assert!(!saved.submission_id.trim().is_empty());
    assert_eq!(saved.submission_id, saved.submission_id.trim());
    assert_eq!(saved.session_id, saved.submission_id);
    let pending = outbox::pending(&core.node_arc(), core.node_identity().did(), node_did).await?;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].submission_id, saved.submission_id);
    core.shutdown().await?;
    Ok(())
}
