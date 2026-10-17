use std::sync::Arc;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_util::codec::LengthDelimitedCodec;
use tokio_util::sync::CancellationToken;

use super::{protocol, ClientHost};

#[tokio::test]
async fn two_attachments_share_a_core_and_detaching_does_not_close_it() {
    let (core, _dir) = crate::tests::support::boot_core().await;
    let host = Arc::new(ClientHost::new(core.clone()));
    let mut tasks = Vec::new();
    let mut clients = Vec::new();
    for _ in 0..2 {
        let (client, server) = tokio::io::duplex(64 * 1024);
        tasks.push(tokio::spawn(
            host.clone().serve(server, CancellationToken::new()),
        ));
        let mut client = LengthDelimitedCodec::builder()
            .max_frame_length(protocol::MAX_FRAME_BYTES)
            .new_framed(client);
        client
            .send(Bytes::from(
                serde_json::to_vec(&json!({
                    "kind": "hello", "version": protocol::PROTOCOL_VERSION
                }))
                .unwrap(),
            ))
            .await
            .unwrap();
        let hello: Value = serde_json::from_slice(&client.next().await.unwrap().unwrap()).unwrap();
        assert_eq!(hello["version"], protocol::PROTOCOL_VERSION);
        assert_eq!(hello["commands"], json!(protocol::ClientCommand::NAMES));
        client
            .send(Bytes::from_static(
                br#"{"kind":"call","id":1,"command":{"name":"desktop_client_snapshot","args":{}}}"#,
            ))
            .await
            .unwrap();
        loop {
            let response: Value =
                serde_json::from_slice(&client.next().await.unwrap().unwrap()).unwrap();
            if response["kind"] != "response" {
                continue;
            }
            assert_eq!(response["id"], 1);
            assert!(response["error"].is_null(), "{response}");
            assert_eq!(
                response["result"]["client"]["localPeerId"],
                core.local_peer_id()
            );
            assert_eq!(response["result"]["bootstrap"]["desktopHome"], "");
            break;
        }
        clients.push(client);
    }
    drop(clients.remove(0));
    tasks.remove(0).await.unwrap().unwrap();
    assert!(!core.local_peer_id().is_empty());
    let client = &mut clients[0];
    client
        .send(Bytes::from_static(
            br#"{"kind":"call","id":2,"command":{"name":"desktop_client_snapshot","args":{}}}"#,
        ))
        .await
        .unwrap();
    loop {
        let response: Value =
            serde_json::from_slice(&client.next().await.unwrap().unwrap()).unwrap();
        if response["kind"] == "response" {
            assert_eq!(response["id"], 2);
            break;
        }
    }
    drop(clients);
    tasks.remove(0).await.unwrap().unwrap();
    core.shutdown().await.unwrap();
}

#[tokio::test]
async fn initialization_rejects_incompatible_protocols_and_oversized_frames() {
    let (core, _dir) = crate::tests::support::boot_core().await;
    let host = Arc::new(ClientHost::new(core.clone()));
    for payload in [
        serde_json::to_vec(&json!({"kind":"hello", "version": protocol::PROTOCOL_VERSION + 1}))
            .unwrap(),
        serde_json::to_vec(&json!({"kind":"hello", "version": protocol::PROTOCOL_VERSION - 1}))
            .unwrap(),
        br#"{"kind":"call","id":1,"command":{"name":"desktop_client_snapshot","args":{}}}"#
            .to_vec(),
    ] {
        let (client, server) = tokio::io::duplex(4096);
        let task = tokio::spawn(host.clone().serve(server, CancellationToken::new()));
        let mut client = LengthDelimitedCodec::builder().new_framed(client);
        client.send(Bytes::from(payload)).await.unwrap();
        assert!(task.await.unwrap().is_err());
    }
    let (mut client, server) = tokio::io::duplex(4096);
    let task = tokio::spawn(host.serve(server, CancellationToken::new()));
    tokio::io::AsyncWriteExt::write_all(
        &mut client,
        &((protocol::MAX_FRAME_BYTES + 1) as u32).to_be_bytes(),
    )
    .await
    .unwrap();
    assert!(task.await.unwrap().is_err());
    core.shutdown().await.unwrap();
}

#[test]
fn wire_commands_reject_privileged_raw_access_and_unscoped_session_reads() {
    for command in [
        json!({"name":"execute_graphql", "args":{"query":"mutation {}"}}),
        json!({"name":"desktop_session_snapshot", "args":{"sessionId":"session"}}),
        json!({"name":"desktop_session_live_delta", "args":{"sessionId":"session"}}),
    ] {
        assert!(serde_json::from_value::<protocol::ClientCommand>(command).is_err());
    }
}

#[test]
fn inference_setup_state_uses_scoped_wire_arguments() {
    let command: protocol::ClientCommand = serde_json::from_value(json!({
        "name":"client_inference_setup_state", "args":{"nodeDid":"did:fixture"}
    }))
    .unwrap();
    assert!(command.is_cancellable());
    assert!(serde_json::from_value::<protocol::ClientCommand>(json!({
        "name":"client_inference_setup_state", "args":{}
    }))
    .is_err());
}

#[test]
fn agent_model_selection_is_scoped_and_not_cancellable() {
    let mut request = json!({
        "nodeDid":"did:fixture", "agentId":"agent", "backendId":"backend",
        "modelName":"model", "reasoningEffort":"high", "serviceTier":"fast"
    });
    let command: protocol::ClientCommand = serde_json::from_value(json!({
        "name":"client_agent_model_select", "args":{"request":request}
    }))
    .unwrap();
    assert!(!command.is_cancellable());
    request["sessionId"] = "session".into();
    assert!(serde_json::from_value::<protocol::ClientCommand>(json!({
        "name":"client_agent_model_select", "args":{"request":request}
    }))
    .is_err());
}

#[tokio::test]
async fn inference_setup_state_rejects_an_unconfigured_runtime() {
    let (core, _dir) = crate::tests::support::boot_core().await;
    let host = ClientHost::new(core.clone());
    let error = host
        .execute(
            protocol::ClientCommand::InferenceSetupState {
                node_did: "did:unconfigured".into(),
            },
            &super::approvals::AttachmentApprovals::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, crate::BridgeErrorCode::InvalidArgument);
    core.shutdown().await.unwrap();
}

#[tokio::test]
async fn runtime_location_rejects_an_unconfigured_node() {
    let (core, _dir) = crate::tests::support::boot_core().await;
    let host = ClientHost::new(core.clone());
    let command: protocol::ClientCommand = serde_json::from_value(json!({
        "name":"client_runtime_is_local", "args":{"nodeDid":"did:unconfigured"}
    }))
    .unwrap();
    assert!(command.is_cancellable());
    let error = host
        .execute(
            command,
            &super::approvals::AttachmentApprovals::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, crate::BridgeErrorCode::InvalidArgument);
    core.shutdown().await.unwrap();
}
