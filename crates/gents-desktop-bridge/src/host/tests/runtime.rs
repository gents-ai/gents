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
