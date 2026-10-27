use std::sync::Arc;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::super::{approvals::AttachmentApprovals, protocol, ClientHost};

#[tokio::test]
async fn runtime_location_rejects_an_unknown_node() {
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
            &AttachmentApprovals::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, crate::BridgeErrorCode::InvalidArgument);
    core.shutdown().await.unwrap();
}

#[tokio::test]
async fn runtime_location_accepts_a_paired_runtime_without_operator_access() {
    use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};

    let directory = tempfile::tempdir().unwrap();
    let paths = DesktopPaths::from_root(directory.path());
    let mut peer = gents_desktop_core::client::PeerRecord::local_standard(
        "Paired runtime",
        "iroh://remote-runtime",
        "did:paired",
        "http://127.0.0.1:1/api/v0/graphql",
    );
    peer.source = Some("enrollment".into());
    peer.graphql = None;
    std::fs::write(
        paths.peer_directory_path(),
        json!({"peers": [peer]}).to_string(),
    )
    .unwrap();
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only())
            .await
            .unwrap(),
    );
    let host = ClientHost::new(core.clone());
    let approvals = AttachmentApprovals::default();
    assert_eq!(
        host.execute(
            protocol::ClientCommand::RuntimeIsLocal {
                node_did: "did:paired".into()
            },
            &approvals,
            CancellationToken::new(),
        )
        .await
        .unwrap(),
        false
    );
    assert!(host.require_configured_runtime("did:paired").await.is_err());
    core.shutdown().await.unwrap();
}

#[tokio::test]
async fn paired_model_settings_use_the_authenticated_client_endpoint() {
    use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};

    let server = wiremock::MockServer::start().await;
    let directory = tempfile::tempdir().unwrap();
    let paths = DesktopPaths::from_root(directory.path());
    let mut peer = gents_desktop_core::client::PeerRecord::local_standard(
        "Paired runtime",
        "iroh://remote-runtime",
        "did:paired",
        format!("{}/api/v0/graphql", server.uri()),
    );
    peer.source = Some("enrollment".into());
    std::fs::write(
        paths.peer_directory_path(),
        json!({"peers": [peer]}).to_string(),
    )
    .unwrap();
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only())
            .await
            .unwrap(),
    );
    let settings = gents_desktop_core::inference::InferenceSetupState::project(
        &gents_desktop_core::client::ClientStore::default(),
        "did:paired",
    );
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(
            gents_desktop_core::inference::INFERENCE_SETTINGS_PATH,
        ))
        .and(wiremock::matchers::header_exists("authorization"))
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
    let host = ClientHost::new(core.clone());
    let result = host
        .execute(
            protocol::ClientCommand::InferenceSetupState {
                node_did: "did:paired".into(),
            },
            &AttachmentApprovals::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result["nodeDid"], "did:paired");
    assert!(host.require_configured_runtime("did:paired").await.is_err());
    core.shutdown().await.unwrap();
}

#[tokio::test]
async fn private_snapshot_keeps_the_current_requesters_questions_without_config_authority() {
    use gents_desktop_core::client::{
        ClientCore, ClientCoreOptions, ClientStore, ClientStoreRows, DesktopPaths, PeerRecord,
    };
    let directory = tempfile::tempdir().unwrap();
    let paths = DesktopPaths::from_root(directory.path());
    let mut peer = PeerRecord::local_standard(
        "Paired runtime",
        "iroh://remote-runtime",
        "did:paired",
        "http://127.0.0.1:1/api/v0/graphql",
    );
    peer.source = Some("enrollment".into());
    peer.graphql = None;
    std::fs::write(
        paths.peer_directory_path(),
        json!({"peers":[peer]}).to_string(),
    )
    .unwrap();
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only())
            .await
            .unwrap(),
    );
    let requester = core.node_identity().did();
    let mailbox_items = [
        ("current", requester, "did:paired", "open"),
        ("other-requester", "did:other", "did:paired", "open"),
        ("other-node", requester, "did:other-node", "open"),
        ("closed", requester, "did:paired", "acted"),
    ].map(|(id, requester, node, status)| serde_json::from_value(json!({
        "_docID":id,"item_key":id,"requester_did":requester,"node_did":node,"status":status,"kind":"ask","action":"start_request","title":"Choose work",
        "source_kind":"agent","source_id":"request","session_id":"chat","request_id":"request","target_node_did":node,"target_agent_id":"agent","created_at":"2026-10-27T00:00:00Z",
        "payload":json!({"version":3,"prompt":"What should happen next?","options":[],"allow_free_text":true}).to_string()
    })).unwrap()).to_vec();
    core.store()
        .merge_snapshot(ClientStore::from_rows(ClientStoreRows {
            mailbox_items,
            ..Default::default()
        }));
    let host = ClientHost::new(core.clone());
    let result = host
        .execute(
            protocol::ClientCommand::Snapshot {},
            &AttachmentApprovals::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let deployments = result["client"]["deployments"].as_array().unwrap();
    let [deployment] = deployments.as_slice() else {
        panic!("one paired runtime: {result}")
    };
    let items = deployment["mailboxItems"].as_array().unwrap();
    assert_eq!(
        items.len(),
        1,
        "private snapshot must preserve only the current requester's open question: {result}"
    );
    assert_eq!(items[0]["itemId"], "current");
    assert!(items[0]["questionHash"]
        .as_str()
        .is_some_and(|hash| !hash.is_empty()));
    assert!(result["bootstrap"]["desktopHome"]
        .as_str()
        .unwrap()
        .is_empty());
    assert!(!host.policy.snapshot_grants.config_read);
    assert!(!host.policy.snapshot_grants.runtime_admin);
    core.shutdown().await.unwrap();
}
