use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use serde_json::{json, Value};
use std::sync::Arc;

use super::*;
use crate::host::{
    protocol::{self, ClientCommand},
    ClientHost,
};

fn original(node_did: &str, client_did: &str) -> Result<ChatSendRequest> {
    Ok(serde_json::from_value(json!({
        "requestId":uuid::Uuid::new_v4().to_string(), "nodeDid":node_did, "clientDid":client_did,
        "agentId":"fixture-agent", "sessionId":null, "content":"Retain this authored message",
        "delivery":"queue"
    }))?)
}

fn query(request: &ChatSendRequest) -> ChatSubmissionQuery {
    ChatSubmissionQuery {
        client_did: request.client_did.clone().expect("caller identity"),
        node_did: request.node_did.clone(),
        request_id: request.request_id.clone().expect("request identity"),
    }
}

async fn rows(core: &ClientCore) -> Result<Value> {
    Ok(ConfigAccess::local(core.node_arc())
        .execute("{ AgentRequest { request_id session_id } }")
        .await?)
}

#[test]
fn submission_recovery_and_acknowledgement_are_mutating_wire_commands() -> Result<()> {
    let request = original("did:fixture:runtime", "did:fixture:client")?;
    let recover: ClientCommand = serde_json::from_value(json!({
        "name":"desktop_chat_submission_recover", "args":{"query":query(&request)}
    }))?;
    assert!(
        !recover.is_cancellable(),
        "recovery can reconcile an owner write"
    );
    assert!(ClientCommand::NAMES.contains(&"desktop_chat_submission_recover"));
    let acknowledge: ClientCommand = serde_json::from_value(json!({
        "name":"desktop_chat_submission_acknowledge", "args":{"receipt":{
            "nodeDid":"node", "requestId":"request", "sessionId":"session", "agentId":null,
            "envelopeHash":"hash", "signedPayloadHash":"payload"
        }}
    }))?;
    assert!(!acknowledge.is_cancellable());
    assert!(ClientCommand::NAMES.contains(&"desktop_chat_submission_acknowledge"));
    Ok(())
}

#[tokio::test]
async fn missing_preparation_is_unconfirmed_without_creating_a_session_or_request() -> Result<()> {
    let (core, _home) = crate::tests::support::boot_core().await;
    let request = original("did:fixture:runtime", core.node_identity().did())?;
    let result = async {
        let reply = serde_json::to_value(recover_chat_submission(&core, query(&request)).await?)?;
        assert_eq!(
            reply,
            json!({"kind":"unconfirmed", "nodeDid":request.node_did,
            "requestId":request.request_id})
        );
        assert_eq!(rows(&core).await?["data"]["AgentRequest"], json!([]));
        let sessions = ConfigAccess::local(core.node_arc())
            .execute("{ AgentSession { session_id } }")
            .await?;
        assert_eq!(sessions["data"]["AgentSession"], json!([]));
        Ok::<(), anyhow::Error>(())
    }
    .await;
    core.shutdown().await?;
    result
}

#[tokio::test]
async fn lost_send_reply_recovers_after_reopen_and_retains_its_result_after_acknowledgement(
) -> Result<()> {
    let home = tempfile::tempdir()?;
    let paths = DesktopPaths::from_root(home.path());
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(
            paths.clone(),
            ClientCoreOptions::local_simulated_route(),
        )
        .await?,
    );
    let node_did = core.node_identity().did().to_owned();
    core.add_local_standard_peer_for_test(&node_did).await?;
    let peers = core.peer_records().await;
    anyhow::ensure!(
        peers
            .iter()
            .any(|peer| peer.node_did == node_did && peer.is_chat_ready_at(chrono::Utc::now())),
        "fixture has no ready peer: {:?}",
        peers
            .iter()
            .map(|peer| (&peer.source, peer.pairing_ready))
            .collect::<Vec<_>>()
    );
    let request = original(&node_did, core.node_identity().did())?;
    let first = intercept_send_reply(&core, &request).await;
    let first = match first {
        Ok(first) => first,
        Err(error) => {
            core.shutdown().await?;
            return Err(error);
        }
    };
    let first_session = first["sessionId"]
        .as_str()
        .context("resolved native session")?
        .to_owned();
    assert_eq!(first["requestId"], request.request_id.clone().unwrap());
    assert!(!first_session.is_empty());
    drop(first);
    core.shutdown().await?;
    drop(core);

    let core = Arc::new(
        ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_simulated_route())
            .await?,
    );
    core.store()
        .set_focused_request_id(Some("another-chat-turn".into()));
    let result = async {
        let ChatSubmissionRecovery::Prepared { result: recovered } =
            recover_chat_submission(&core, query(&request)).await?
        else {
            anyhow::bail!("durable preparation disappeared after reopen");
        };
        assert_eq!(recovered.session_id, first_session);
        assert_eq!(
            Some(recovered.request_id.as_str()),
            request.request_id.as_deref()
        );
        assert!(recovered.outcome.is_none());
        assert_eq!(
            core.store().focused_request_id().as_deref(),
            Some("another-chat-turn")
        );
        let payloads = ConfigAccess::local(core.node_arc())
            .execute("{ PendingChatSubmission { payload } }")
            .await?;
        let payload: Value = serde_json::from_str(
            payloads["data"]["PendingChatSubmission"][0]["payload"]
                .as_str()
                .context("retained caller payload")?,
        )?;
        let pending: gents_desktop_core::client::UnresolvedSubmission =
            serde_json::from_value(payload["record"]["pending"].clone())?;
        let unknown = gents_desktop_core::client::SubmissionOutcomeUnknown {
            request_id: pending.request_id.clone(),
            session_id: pending.session_id.clone(),
            pending,
            cause: Some(gents_protocol::database_error::StorageFull.into()),
        };
        let intent = intent(&core, &request)?.context("caller intent")?;
        let uncertain = sent(&core, &intent, Err(unknown.into())).await?;
        assert_eq!(uncertain.request_id, recovered.request_id);
        assert_eq!(uncertain.session_id, recovered.session_id);
        assert_eq!(
            serde_json::to_value(&uncertain.submission_receipt)?,
            serde_json::to_value(&recovered.submission_receipt)?
        );
        let uncertain = serde_json::to_value(uncertain)?;
        assert_eq!(uncertain["outcome"], "deliveryUnknown");
        assert_eq!(uncertain["deliveryError"]["code"], "storageFull");
        assert_eq!(uncertain["deliveryError"]["retryable"], false);
        let receipt = recovered
            .submission_receipt
            .context("native caller receipt")?;
        let mut forged = receipt.clone();
        forged.signed_payload_hash.push('0');
        assert!(acknowledge_chat_submission(&core, forged).await.is_err());
        let mut foreign = request.clone();
        foreign.client_did = Some("did:fixture:another-client".into());
        assert!(recover_chat_submission(&core, query(&foreign))
            .await
            .is_err());
        let mut wrong_runtime = query(&request);
        wrong_runtime.node_did = "did:fixture:another-runtime".into();
        assert!(recover_chat_submission(&core, wrong_runtime).await.is_err());
        let mut changed = request.clone();
        changed.content.push_str(" with different content");
        assert!(crate::commands::send_chat_message(&core, changed)
            .await
            .is_err());
        acknowledge_chat_submission(&core, receipt.clone()).await?;
        acknowledge_chat_submission(&core, receipt).await?;
        let again = crate::commands::send_chat_message(&core, request.clone())
            .await
            .context("recovering acknowledged caller operation")?;
        assert_eq!(again.request_id, recovered.request_id);
        assert_eq!(again.session_id, first_session);
        assert_eq!(
            rows(&core).await?["data"]["AgentRequest"],
            json!([{
                "request_id":recovered.request_id, "session_id":first_session
            }])
        );
        let payloads = ConfigAccess::local(core.node_arc())
            .execute("{ PendingChatSubmission { payload } }")
            .await?;
        let payload: Value = serde_json::from_str(
            payloads["data"]["PendingChatSubmission"][0]["payload"]
                .as_str()
                .context("retained receipt payload")?,
        )?;
        assert!(payload["record"]["acknowledged"]
            .as_bool()
            .context("acknowledgement")?);
        assert!(
            payload["record"]["pending"].is_null(),
            "acknowledgement retires replay bytes"
        );
        Ok::<(), anyhow::Error>(())
    }
    .await;
    core.shutdown().await?;
    result
}

async fn intercept_send_reply(core: &Arc<ClientCore>, request: &ChatSendRequest) -> Result<Value> {
    use bytes::Bytes;
    use futures::{SinkExt, StreamExt};
    use tokio_util::{codec::LengthDelimitedCodec, sync::CancellationToken};

    let host = Arc::new(ClientHost::new(Arc::clone(core)));
    let (client, server) = tokio::io::duplex(64 * 1024);
    let serving = tokio::spawn(Arc::clone(&host).serve(server, CancellationToken::new()));
    let mut wire = LengthDelimitedCodec::builder()
        .max_frame_length(protocol::MAX_FRAME_BYTES)
        .new_framed(client);
    let result = async {
        wire.send(Bytes::from(serde_json::to_vec(
            &json!({"kind":"hello", "version":protocol::PROTOCOL_VERSION}),
        )?))
        .await?;
        let hello: Value = serde_json::from_slice(&wire.next().await.context("native hello")??)?;
        assert_eq!(hello["version"], protocol::PROTOCOL_VERSION);
        assert!(hello["commands"]
            .as_array()
            .context("command inventory")?
            .iter()
            .any(|command| command == "desktop_chat_submission_recover"));
        wire.send(Bytes::from(serde_json::to_vec(
            &json!({"kind":"call", "id":1,
            "command":{"name":"desktop_chat_send", "args":{"request":request}}}),
        )?))
        .await?;
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            loop {
                let response: Value =
                    serde_json::from_slice(&wire.next().await.context("native send response")??)?;
                if response["kind"] != "response" || response["id"] != 1 {
                    continue;
                }
                anyhow::ensure!(
                    response["error"].is_null(),
                    "native send failed: {}",
                    response["error"]
                );
                return Ok::<_, anyhow::Error>(response["result"].clone());
            }
        })
        .await
        .context("native send response deadline")?
    }
    .await;
    drop(wire);
    serving.await??;
    drop(host);
    result
}
