use super::*;
use gents_protocol::goal_control::{SessionGoalControl, SessionGoalControlReceipt};

pub(super) async fn exercise(
    mut client: Attachment,
    id: &mut u32,
    node: &str,
    client_did: &str,
    agent: &str,
    identity: &dyn NodeIdentity,
    recovery: &mut EditRecoveryHost<'_>,
) -> Result<(Attachment, Value)> {
    let command = uuid::Uuid::new_v4().to_string();
    let session = uuid::Uuid::new_v4().to_string();
    let request = json!({
        "clientDid":client_did,"commandId":command,"nodeDid":node,"sessionId":session,
        "sessionDocId":null,"expected":null,"agentId":agent,"cwd":null,
        "action":{"kind":"create","objective":"Return the fixture response.","tokenBudget":"1000"},
        "capturedContext":[{"kind":"file","source":"editor","capturedAt":chrono::Utc::now().to_rfc3339(),
            "uri":"file:///fixture/retained.rs","workspaceUri":"file:///fixture","terminalName":null,
            "version":7,"unsaved":true,"range":null,"content":"saved before the lost Goal reply\n"}],
    });
    discard_successful_reply(
        &mut client,
        *id,
        "desktop_goal_control",
        json!({"request":request}),
    )
    .await?;
    *id += 1;
    let query = format!(
        r#"{{
        AgentSessionGoalControl(filter:{{command_id:{{_eq:"{}"}}}}){{_docID intent_json receipt_json}}
        AgentRequest(filter:{{node_did:{{_eq:"{}"}},session_id:{{_eq:"{}"}},purpose:{{_eq:"normal"}}}})
            {{_docID request_id session_id content input admission_signature created_at valid_until}}
    }}"#,
        gents::graphql::escape_graphql_string(&command),
        gents::graphql::escape_graphql_string(node),
        gents::graphql::escape_graphql_string(&session)
    );
    let admitted = recovery.access.execute(&query).await?;
    let controls = admitted["data"]["AgentSessionGoalControl"]
        .as_array()
        .context("Goal commands")?;
    ensure!(controls.len() == 1, "the opening command was not unique");
    let signed: SessionGoalControl = serde_json::from_str(
        controls[0]["intent_json"]
            .as_str()
            .context("signed command")?,
    )?;
    let receipt: SessionGoalControlReceipt = serde_json::from_str(
        controls[0]["receipt_json"]
            .as_str()
            .context("signed receipt")?,
    )?;
    receipt.validate_for(&signed)?;
    ensure!(
        identity
            .verify(
                &signed.requester_did,
                &signed.signing_payload()?,
                &signed.signature
            )
            .await?
    );
    ensure!(
        identity
            .verify(node, &receipt.signing_payload()?, &receipt.signature)
            .await?
    );
    let first = signed
        .initial_request
        .as_ref()
        .context("retained first request")?;
    ensure!(first.input.captured_context[0].unsaved);
    ensure!(first.input.captured_context[0].content == "saved before the lost Goal reply\n");
    let requests = admitted["data"]["AgentRequest"]
        .as_array()
        .context("admitted requests")?;
    ensure!(requests.len() == 1 && requests[0]["request_id"] == first.request_id);
    client.close().await;
    recovery.restart().await?;
    client = Attachment::start_with_binary(recovery.paths, recovery.binary).await;
    wait_chat_deployment(&mut client, id, node).await?;
    let lookup = json!({"query":{"clientDid":client_did,"nodeDid":node,"commandId":command}});
    let recovered = client
        .call(*id, "desktop_goal_submission_recover", lookup.clone())
        .await;
    *id += 1;
    ensure!(
        recovered["kind"] == "prepared" && recovered["acknowledged"] == false,
        "{recovered}"
    );
    let result = recovered["outcome"]["result"].clone();
    ensure!(
        recovered["outcome"]["kind"] == "applied"
            && result["sessionId"] == session
            && result["requestId"] == first.request_id,
        "{recovered}"
    );
    ensure!(
        recovery.access.execute(&query).await? == admitted,
        "checking delivery changed admitted work"
    );
    let resumed = client
        .call(
            *id,
            "desktop_goal_submission_resume",
            json!({"request":request,"receipt":recovered["receipt"]}),
        )
        .await;
    *id += 1;
    ensure!(
        resumed == result,
        "resuming changed the confirmed opening result"
    );
    ensure!(
        recovery.access.execute(&query).await? == admitted,
        "resuming changed signed work"
    );
    discard_successful_reply(
        &mut client,
        *id,
        "desktop_goal_submission_acknowledge",
        json!({"receipt":recovered["receipt"]}),
    )
    .await?;
    *id += 1;
    client.close().await;
    recovery.restart().await?;
    client = Attachment::start_with_binary(recovery.paths, recovery.binary).await;
    wait_chat_deployment(&mut client, id, node).await?;
    let acknowledged = client
        .call(*id, "desktop_goal_submission_recover", lookup)
        .await;
    *id += 1;
    ensure!(
        acknowledged["acknowledged"] == true && acknowledged["outcome"] == recovered["outcome"],
        "lost acknowledgement changed the outcome: {acknowledged}"
    );
    let replayed = client
        .call(
            *id,
            "desktop_goal_submission_resume",
            json!({"request":request,"receipt":acknowledged["receipt"]}),
        )
        .await;
    *id += 1;
    ensure!(
        replayed == result,
        "retired publication bytes caused another opening"
    );
    ensure!(
        recovery.access.execute(&query).await? == admitted,
        "acknowledgement or replay changed admitted work"
    );
    Ok((client, result))
}
