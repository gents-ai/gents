mod file_inventory;
mod runtime;

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
        json!({"name":"desktop_request_retry", "args":{"requestId":"request"}}),
    ] {
        assert!(serde_json::from_value::<protocol::ClientCommand>(command).is_err());
    }
}

#[test]
fn file_change_reads_require_complete_scope_and_are_cancellable() {
    let args = json!({"nodeDid":"node", "sessionId":"session", "requestId":"request", "toolCallDocId":"physical"});
    for field in ["nodeDid", "sessionId", "requestId", "toolCallDocId"] {
        let mut missing = args.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<protocol::ClientCommand>(json!({
                "name":"desktop_tool_file_changes", "args": missing
            }))
            .is_err(),
            "{field}"
        );
    }
    let command: protocol::ClientCommand = serde_json::from_value(json!({
        "name":"desktop_tool_file_changes", "args": args
    }))
    .unwrap();
    assert!(command.is_cancellable());
    assert!(protocol::ClientCommand::NAMES.contains(&"desktop_tool_file_changes"));
}

#[test]
fn inventory_reads_keep_scope_actions_and_cancellation() {
    for action in [
        json!({"kind":"read"}),
        json!({"kind":"advance"}),
        json!({"kind":"refresh", "attemptId":"attempt"}),
    ] {
        let args = json!({"nodeDid":"node", "sessionId":"session", "action":action});
        let command: protocol::ClientCommand = serde_json::from_value(json!({
            "name":"desktop_session_file_inventory", "args":args,
        }))
        .unwrap();
        assert!(command.is_cancellable());
        for field in ["nodeDid", "sessionId"] {
            let mut missing = args.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<protocol::ClientCommand>(json!({
                "name":"desktop_session_file_inventory", "args":missing,
            }))
            .is_err());
        }
    }
    for action in [
        json!({"kind":"execute", "query":"mutation {}"}),
        json!({"kind":"advance", "requesterDid":"another"}),
    ] {
        assert!(serde_json::from_value::<protocol::ClientCommand>(json!({
            "name":"desktop_session_file_inventory", "args":{
                "nodeDid":"node", "sessionId":"session", "action":action,
            },
        }))
        .is_err());
    }
    assert!(protocol::ClientCommand::NAMES.contains(&"desktop_session_file_inventory"));
}

#[test]
fn inventory_image_reads_accept_only_received_coordinates() {
    let reference = json!({"cursor":{"attempt_id":"attempt", "snapshot_digest":"snapshot", "offset":64}, "entry_index":0});
    let args = json!({"nodeDid":"node", "sessionId":"session", "reference":reference});
    let command: protocol::ClientCommand = serde_json::from_value(json!({
        "name":"desktop_inventory_file_change", "args":args,
    }))
    .unwrap();
    assert!(command.is_cancellable());
    assert!(protocol::ClientCommand::NAMES.contains(&"desktop_inventory_file_change"));
    for field in ["nodeDid", "sessionId", "reference"] {
        let mut missing = args.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<protocol::ClientCommand>(json!({
            "name":"desktop_inventory_file_change", "args":missing,
        }))
        .is_err());
    }
    for reference in [
        json!({"cursor":{"attempt_id":"attempt", "snapshot_digest":"snapshot"}, "entry_index":0}),
        json!({"cursor":{"attempt_id":"attempt", "snapshot_digest":"snapshot", "offset":0}, "entry_index":-1}),
        json!({"cursor":{"attempt_id":"attempt", "snapshot_digest":"snapshot", "offset":0}, "entry_index":256}),
        json!({"cursor":{"attempt_id":"attempt", "snapshot_digest":"snapshot", "offset":0}, "entry_index":0, "scope":{"node":"other"}}),
    ] {
        assert!(serde_json::from_value::<protocol::ClientCommand>(json!({
            "name":"desktop_inventory_file_change", "args":{"nodeDid":"node", "sessionId":"session", "reference":reference},
        })).is_err());
    }
}

#[test]
fn request_retry_is_scoped_and_not_cancellable() {
    let command: protocol::ClientCommand = serde_json::from_value(json!({
        "name":"desktop_request_retry", "args":{"requestId":"request", "nodeDid":"node"}
    }))
    .unwrap();
    assert!(
        matches!(&command, protocol::ClientCommand::Retry { request_id, node_did }
        if request_id == "request" && node_did == "node")
    );
    assert!(!command.is_cancellable());
    assert!(protocol::ClientCommand::NAMES.contains(&"desktop_request_retry"));
}

#[tokio::test]
async fn request_retry_reaches_the_existing_parent_owner() {
    let (core, _dir) = crate::tests::support::boot_core().await;
    let host = ClientHost::new(core.clone());
    let error = host
        .execute(
            protocol::ClientCommand::Retry {
                request_id: "missing-parent".into(),
                node_did: "node".into(),
            },
            &super::approvals::AttachmentApprovals::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("request missing-parent not found"),
        "{error}"
    );
    core.shutdown().await.unwrap();
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
async fn passive_snapshots_keep_the_runtime_identity_locked() {
    use identity::Identity as _;

    let server = wiremock::MockServer::start().await;
    let graphql = format!("{}/api/v0/graphql", server.uri());
    let directory = tempfile::tempdir().unwrap();
    let paths = gents_desktop_core::client::DesktopPaths::from_root(directory.path());
    let options = gents_desktop_core::client::ClientCoreOptions {
        operator_identity_interaction:
            gents_desktop_core::local_runtime::OperatorIdentityInteraction::Prevent,
        ..gents_desktop_core::client::ClientCoreOptions::local_only()
    };
    let core = Arc::new(
        gents_desktop_core::client::ClientCore::start_with_paths_and_options(paths, options)
            .await
            .unwrap(),
    );
    let home = directory.path().join("runtime");
    std::fs::create_dir_all(&home).unwrap();
    let key_path = home.join("node.key");
    let identity = gents::identity::load_or_create_file_identity(&key_path).unwrap();
    let did = identity.did().unwrap().to_string();
    std::fs::write(
        home.join("init.json"),
        json!({ "node_name": "Local", "node_did": did, "key_path": key_path }).to_string(),
    )
    .unwrap();
    std::fs::write(
        home.join("runtime.json"),
        json!({
            "node_name": "Local", "node_did": did, "graphql": graphql,
            "p2p_transport": "iroh"
        })
        .to_string(),
    )
    .unwrap();
    core.persist_local_standard_peer(
        "Local",
        "iroh://unavailable-runtime",
        &did,
        &graphql,
        &home.to_string_lossy(),
    )
    .await
    .unwrap();
    assert!(!gents::identity::can_mint_defradb_bearer(&did));
    let host = ClientHost::new(core.clone());
    let approvals = super::approvals::AttachmentApprovals::default();
    assert_eq!(
        host.execute(
            protocol::ClientCommand::RuntimeIsLocal {
                node_did: did.clone()
            },
            &approvals,
            CancellationToken::new(),
        )
        .await
        .unwrap(),
        true
    );
    host.execute(
        protocol::ClientCommand::Snapshot {},
        &approvals,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let _ = host
        .execute(
            protocol::ClientCommand::AccountsList {
                request: crate::accounts::ProviderAccountsRequest {
                    node_did: did.clone(),
                },
            },
            &approvals,
            CancellationToken::new(),
        )
        .await;
    let _ = core
        .session_goal_observation(&did, "unavailable-session")
        .await;
    let _ = core
        .pending_user_queue_projection(&did, "unavailable-session", &[])
        .await;
    assert!(!gents::identity::can_mint_defradb_bearer(&did));
    let requests = server.received_requests().await.unwrap();
    assert!(!requests.is_empty());
    assert!(requests
        .iter()
        .all(|request| !request.headers.contains_key("authorization")));
    let settings = gents_desktop_core::inference::InferenceSetupState::project(
        &gents_desktop_core::client::ClientStore::default(),
        &did,
    );
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(
            gents_desktop_core::inference::INFERENCE_SETTINGS_PATH,
        ))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
            gents_desktop_core::inference::InferenceSettingsResponse {
                contract_version:
                    gents_desktop_core::inference::INFERENCE_SETTINGS_CONTRACT_VERSION,
                state: settings,
            },
        ))
        .expect(1)
        .mount(&server)
        .await;
    let settings = host
        .execute(
            protocol::ClientCommand::InferenceSetupState {
                node_did: did.clone(),
            },
            &approvals,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(settings["nodeDid"], did);
    assert!(!gents::identity::can_mint_defradb_bearer(&did));
    let requests = server.received_requests().await.unwrap();
    let settings_read = requests
        .iter()
        .find(|request| {
            request.url.path() == gents_desktop_core::inference::INFERENCE_SETTINGS_PATH
        })
        .unwrap();
    assert!(settings_read
        .headers
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("Bearer "));
    core.shutdown().await.unwrap();
}

#[test]
fn workspace_permissions_reads_cannot_supply_an_identity_or_mutate_the_selection() {
    let mut request = json!({
        "nodeDid": "did:key:runtime", "agentId": "engineer", "workspacePath": "/project"
    });
    let decode = |request: Value| {
        serde_json::from_value::<protocol::ClientCommand>(json!({
            "name": "client_workspace_permissions", "args": {"request": request}
        }))
    };
    assert!(decode(request.clone()).unwrap().is_cancellable());
    for field in ["requesterDid", "mode", "revision", "home", "endpoint"] {
        request[field] = "untrusted".into();
        assert!(decode(request.clone()).is_err(), "accepted {field}");
        request.as_object_mut().unwrap().remove(field);
    }
    for field in ["nodeDid", "agentId", "workspacePath"] {
        let mut incomplete = request.clone();
        incomplete.as_object_mut().unwrap().remove(field);
        assert!(decode(incomplete).is_err(), "missing {field}");
    }
}

#[test]
fn workspace_permission_saves_are_strict_and_finish_submitted_mutations() {
    let request = json!({
        "scope":{"nodeDid":"runtime","agentId":"agent","workspacePath":"/project"},
        "expectedRevision":"0","expectedWorkspacePath":"/project","mode":"autoReview"
    });
    let decode = |request: Value| {
        serde_json::from_value::<protocol::ClientCommand>(json!({
            "name":"client_workspace_permissions_save","args":{"request":request}
        }))
    };
    assert!(!decode(request.clone()).unwrap().is_cancellable());
    let mut invalid = request;
    invalid["taskId"] = "another-task".into();
    assert!(decode(invalid).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_provenance_read_reaches_the_existing_lineage_owner() {
    let (core, _directory, parent_doc_id) = crate::tests::support::seed_provenance_fixture().await;
    let host = Arc::new(ClientHost::new(core.clone()));
    let wire_command = json!({
        "name": "desktop_session_provenance", "args": { "request": {
            "nodeDid": crate::tests::support::OPERATOR,
            "sessionId": "sess_parent",
            "requesterDid": crate::tests::support::OPERATOR
        }}
    });
    let command: protocol::ClientCommand = serde_json::from_value(wire_command.clone()).unwrap();
    assert!(command.is_cancellable());
    let (client, server) = tokio::io::duplex(64 * 1024);
    let task = tokio::spawn(host.serve(server, CancellationToken::new()));
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
    assert!(hello["commands"]
        .as_array()
        .unwrap()
        .contains(&json!("desktop_session_provenance")));
    client
        .send(Bytes::from(
            serde_json::to_vec(&json!({
                "kind": "call", "id": 41, "command": wire_command
            }))
            .unwrap(),
        ))
        .await
        .unwrap();
    let observed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let response: Value =
                serde_json::from_slice(&client.next().await.unwrap().unwrap()).unwrap();
            if response["kind"] == "response" && response["id"] == 41 {
                assert!(response["error"].is_null(), "{response}");
                break response["result"].clone();
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(observed["sessionId"], "sess_parent");
    let started = observed["started"].as_array().unwrap();
    assert_eq!(started.len(), 2);
    assert!(started
        .iter()
        .all(|link| link["causeRequestDocId"] == parent_doc_id));
    assert!(started.iter().any(|link| link["sessionId"] == "sess_child"));
    assert!(started.iter().any(|link| link["sessionId"] == "sess_peer"));
    drop(client);
    task.await.unwrap().unwrap();
    core.shutdown().await.unwrap();
}
