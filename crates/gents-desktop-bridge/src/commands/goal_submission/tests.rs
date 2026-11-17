use super::*;
use crate::host::protocol::ClientCommand;
use gents_store::config_client::ConfigAccess;
use serde_json::{json, Value};

#[test]
fn goal_delivery_wire_commands_preserve_native_receipt_lifetime() -> Result<()> {
    for (name, args) in [
        (
            "desktop_goal_submission_recover",
            json!({"query": {
                "clientDid":"client", "nodeDid":"node", "commandId":"command",
            }}),
        ),
        ("desktop_goal_submission_resume", {
            let mut args = submission();
            args["request"] = original("client", "node");
            args
        }),
        ("desktop_goal_submission_acknowledge", submission()),
    ] {
        let command: ClientCommand = serde_json::from_value(json!({"name":name,"args":args}))?;
        assert!(!command.is_cancellable());
        assert!(ClientCommand::NAMES.contains(&name));
    }
    Ok(())
}

fn submission() -> Value {
    json!({"receipt": {"clientDid":"client", "nodeDid":"node", "sessionId":"session",
        "reference": {"commandId":"command", "commandDigest":"sha256:digest",
        "initialRequestId":null,"requesterDid":"requester","agentDid":"fixture-agent"}}})
}

fn original(client: &str, node: &str) -> Value {
    original_for(client, node, "fixture-agent")
}

fn original_for(client: &str, node: &str, agent: &str) -> Value {
    json!({"clientDid":client,"commandId":"command","nodeDid":node,
        "sessionId":"session","sessionDocId":null,"expected":null,"agentDid":agent,
        "cwd":null,"capturedContext":[],"action":{"kind":"create","objective":"Retain this Goal","tokenBudget":null}})
}

#[tokio::test]
async fn missing_goal_preparation_never_allocates_or_resubmits_work() -> Result<()> {
    let (core, _home) = crate::tests::support::boot_core().await;
    let result = async {
        let client = core.node_identity().did().to_owned();
        let query = GoalSubmissionQuery {
            client_did:client.clone(), node_did:"did:fixture:runtime".into(), command_id:"original-command".into(),
        };
        core.stores().ensure("did:fixture:runtime").set_focused_request_id(Some("another-chat".into()));
        let recovered = serde_json::to_value(recover_goal_submission(&core, query.clone()).await?)?;
        assert_eq!(recovered,json!({"kind":"unconfirmed","nodeDid":query.node_did,"commandId":query.command_id}));
        let mut foreign = query;
        foreign.client_did = "another-native-client".into();
        assert!(recover_goal_submission(&core, foreign).await.is_err());
        for (client_did, command_id, session_id) in [
            (Some(client.as_str()),Some("original-command"),None),
            (Some("another-native-client"),Some("original-command"),Some("session")),
            (None,Some("original-command"),Some("session")),
        ] {
            let request = serde_json::from_value(json!({
                "clientDid":client_did,"commandId":command_id,"sessionId":session_id,
                "nodeDid":"did:fixture:runtime","sessionDocId":null,"expected":null,
                "agentDid":"fixture-agent","cwd":null,"action": {"kind":"create","objective":"Retain this Goal","tokenBudget":null},
            }))?;
            assert!(super::super::goal::control_goal(&core, request).await.is_err());
        }
        let request: crate::types::GoalControlRequest = serde_json::from_value(original(&client, "node"))?;
        let mut receipt = submission()["receipt"].clone();
        receipt["clientDid"] = json!(client);
        let error = resume_goal_submission(&core, request, serde_json::from_value(receipt)?).await.unwrap_err();
        assert!(error.to_string().contains("preparation is unavailable"), "{error}");
        assert_eq!(core.stores().ensure("did:fixture:runtime").focused_request_id().as_deref(),Some("another-chat"));
        let rows=ConfigAccess::local(core.node_arc()).execute("{ClientAuthoredSubmission {submission_id} PendingChatSubmission {request_id}}").await?;
        for name in ["ClientAuthoredSubmission","PendingChatSubmission"] {
            assert_eq!(rows["data"][name],json!([]),"{name}");
        }
        Ok::<_, anyhow::Error>(())
    }.await;
    core.shutdown().await?;
    result
}

#[tokio::test]
async fn goal_resume_refuses_reauthored_input_without_replacing_signed_preparation() -> Result<()> {
    use gents_store::client_journal::paths::DesktopPaths;
    use std::sync::Arc;
    use std::time::Duration;

    let home = tempfile::tempdir()?;
    let core = Arc::new(
        ClientRuntime::start_with_paths_and_options(
            DesktopPaths::from_root(home.path()),
            crate::client_test_support::file_journal_options(),
        )
        .await?,
    );
    // The desktop's client key is the User of a served runtime it routes to.
    let runtime = gents::client_test_support::ServedRuntimeHome::start_unpublished_for(Arc::new(
        core.node_identity().clone(),
    ))
    .await;
    runtime.publish_node(json!({})).await;
    let agent = runtime.publish_chat_agent().await;
    let proxy =
        crate::tests::support::ServingStatusProxy::start(runtime.graphql(), runtime.node_did())
            .await;
    core.connect_runtime(gents_protocol::runtime_connection::RuntimeConnection {
        node_did: (runtime.node_did()).to_string(),
        label: ("Goal fixture").to_string(),
        graphql: (proxy.url()).to_string(),
        local_node_home: Some(runtime.home_path().display().to_string()),
        access_request: None,
        access_state: None,
    })
    .await?;
    core.refresh_node(runtime.node_did()).await?;
    assert_eq!(
        core.reconnect_runtime_now(runtime.node_did()).await,
        Some(gents_observe::client_projection::observe::RuntimeConnectionStatus::Connected)
    );
    let result = async {
        let did = core.node_identity().did().to_owned();
        let node = runtime.node_did().to_owned();
        let mut request = original_for(&did, &node, &agent.agent_did);
        request["capturedContext"] = json!([{
            "kind":"file", "source":"editor", "capturedAt":"2026-10-27T00:00:00Z",
            "uri":"file:///workspace/retained.rs", "terminalName":null,
            "workspaceUri":"file:///workspace", "version":7, "unsaved":true,
            "range":null, "content":"captured before the lost reply\n",
        }]);
        request["attachments"] = json!([{
            "name":"capture.png", "mediaType":"image/png", "data":"aW1hZ2U=",
        }]);
        let submitted = request.clone();
        let submitting_core = core.clone();
        let submitting = tokio::spawn(async move {
            super::super::goal::control_goal(&submitting_core, serde_json::from_value(submitted).unwrap()).await
        });
        let access = ConfigAccess::local(core.node_arc());
        let pending = async {
            loop {
                let response = access.execute("{PendingChatSubmission(filter:{request_id:{_eq:\"command\"}},limit:2){payload}}").await?;
                if let Some(row) = response["data"]["PendingChatSubmission"].as_array().and_then(|rows| rows.first()) {
                    let record: Value = serde_json::from_str(row["payload"].as_str().context("retained command")?)?;
                    return Ok::<_, anyhow::Error>(record);
                }
                if submitting.is_finished() { anyhow::bail!("Goal call finished without retaining its preparation"); }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        let retained = tokio::time::timeout(Duration::from_secs(5), pending).await;
        submitting.abort();
        let submitted = submitting.await;
        let record = retained?.with_context(|| format!("native Goal preparation ended as {submitted:?}"))?;
        assert_eq!(record["record"]["prepared"]["initial_request"]["input"]["attachments"], request["attachments"]);
        let mut receipt = submission()["receipt"].clone();
        receipt["clientDid"] = json!(did);
        receipt["nodeDid"] = json!(node);
        receipt["reference"] = record["record"]["reference"].clone();
        let receipt: GoalSubmissionReceipt = serde_json::from_value(receipt)?;
        let mut changed = request.clone();
        changed["action"]["objective"] = json!("Changed after the lost reply");
        let error = resume_goal_submission(&core, serde_json::from_value(changed)?, receipt.clone()).await.unwrap_err();
        assert_eq!(error.message, gents_store::client_journal::outbox::OutboxError::Binding.to_string());
        let mut changed_revision = request.clone();
        changed_revision["expected"] = json!({"document":"another-goal","composite":"another-revision"});
        assert!(resume_goal_submission(&core, serde_json::from_value(changed_revision)?, receipt.clone()).await.is_err());
        let mut changed_image = request.clone();
        changed_image["attachments"][0]["data"] = json!("bmV3LWltYWdl");
        let error = resume_goal_submission(&core, serde_json::from_value(changed_image)?, receipt.clone()).await.unwrap_err();
        assert_eq!(error.message, gents_store::client_journal::outbox::OutboxError::Binding.to_string());
        let mut removed_image = request.clone();
        removed_image["attachments"] = json!([]);
        assert!(resume_goal_submission(&core, serde_json::from_value(removed_image)?, receipt.clone()).await.is_err());
        let mut changed_capture = request;
        changed_capture["capturedContext"][0]["content"] = json!("recaptured later");
        assert!(resume_goal_submission(&core, serde_json::from_value(changed_capture)?, receipt).await.is_err());
        let after = access.execute("{PendingChatSubmission(filter:{request_id:{_eq:\"command\"}},limit:2){payload}}").await?;
        let rows = after["data"]["PendingChatSubmission"].as_array().context("journal rows")?;
        assert_eq!(rows.len(),1);
        assert_eq!(serde_json::from_str::<Value>(rows[0]["payload"].as_str().context("payload")?)?, record);
        Ok::<_, anyhow::Error>(())
    }.await;
    core.shutdown().await?;
    runtime.shutdown().await;
    result
}
