use super::ClientRuntime;
use anyhow::Result;
use gents_store::client_journal::paths::DesktopPaths;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expired_unretained_access_recovers_after_restart_without_restoring_grants() -> Result<()> {
    use gents_observe::client_projection::observe::RuntimeConnectionStatus;
    use gents_protocol::client_access::{
        ClientAccessDecision, ClientAccessRequest, ClientAccessState, DecideClientAccess,
    };

    use gents_protocol::runtime_connection::RuntimeConnection;

    let directory = tempfile::tempdir()?;
    let paths = DesktopPaths::from_root(directory.path());
    let core = Arc::new(
        ClientRuntime::start_with_paths_and_options(
            paths.clone(),
            crate::client_test_support::file_journal_options(),
        )
        .await?,
    );
    let runtime = gents::client_test_support::ServedRuntimeHome::start().await;
    let client_did = core.node_identity().did().to_owned();
    let issued = chrono::Utc::now() - chrono::Duration::minutes(10);
    let mut saved = ClientAccessRequest {
        request_id: "saved-before-publication".into(),
        node_did: runtime.node_did().into(),
        client_did: client_did.clone(),
        label: "Hosted runtime".into(),
        issued_at: issued.to_rfc3339(),
        expires_at: (issued + chrono::Duration::minutes(5)).to_rfc3339(),
        signature: Vec::new(),
    };
    saved.signature = core.node_identity().sign(&saved.signing_payload()?)?;
    core.connect_runtime(RuntimeConnection {
        node_did: runtime.node_did().into(),
        label: saved.label.clone(),
        graphql: runtime.graphql().into(),
        local_node_home: Some(runtime.home_path().to_string_lossy().into_owned()),
        access_request: Some(saved.clone()),
        access_state: None,
    })
    .await?;
    core.shutdown().await?;
    drop(core);
    let core = Arc::new(
        ClientRuntime::start_with_paths_and_options(
            paths,
            crate::client_test_support::file_journal_options(),
        )
        .await?,
    );
    assert_eq!(core.node_identity().did(), client_did);
    let (first, second) = tokio::join!(
        core.runtime_access_status(runtime.node_did()),
        core.runtime_access_status(runtime.node_did())
    );
    let pending = first?;
    assert_eq!(pending, second?);
    assert_eq!(pending.state, ClientAccessState::Pending);
    assert_ne!(pending.request_id, saved.request_id);
    assert_eq!(
        gents_store::identity::client_access::list_requests(
            &runtime.user_access(),
            runtime.node_did(),
        )
        .await?,
        vec![pending.clone()]
    );
    assert_eq!(
        core.reconnect_runtime_now(runtime.node_did()).await,
        Some(RuntimeConnectionStatus::Refused)
    );
    assert!(core
        .decide_runtime_access(
            runtime.node_did(),
            &pending.request_id,
            ClientAccessDecision::Approve
        )
        .await
        .is_err());
    let decision = DecideClientAccess {
        node_did: runtime.node_did().into(),
        request_id: pending.request_id.clone(),
        decision: ClientAccessDecision::Approve,
    };
    assert_eq!(
        runtime
            .user_endpoint()
            .decide_runtime_access(&decision)
            .await?
            .state,
        ClientAccessState::Approved
    );
    assert_eq!(
        core.runtime_access_status(runtime.node_did()).await?.state,
        ClientAccessState::Approved
    );
    assert_eq!(
        core.reconnect_runtime_now(runtime.node_did()).await,
        Some(RuntimeConnectionStatus::Connected)
    );
    assert_eq!(
        runtime
            .user_endpoint()
            .decide_runtime_access(&DecideClientAccess {
                decision: ClientAccessDecision::Revoke,
                ..decision
            })
            .await?
            .state,
        ClientAccessState::Revoked
    );
    assert_eq!(
        core.runtime_access_status(runtime.node_did()).await?.state,
        ClientAccessState::Revoked
    );
    let mut updates = core.store_updates();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while core.stores().runtime_connection(runtime.node_did())
            != Some(RuntimeConnectionStatus::Refused)
        {
            updates.changed().await?;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    assert_eq!(
        core.reconnect_runtime_now(runtime.node_did()).await,
        Some(RuntimeConnectionStatus::Refused)
    );
    let renewed = crate::commands::request_runtime_access(
        &core,
        crate::types::RuntimeAccessConnectRequest {
            server_address: runtime.graphql().into(),
        },
    )
    .await?;
    assert_eq!(renewed.state, "pending");
    assert_ne!(pending.request_id, renewed.request_id);
    assert!(core.node().p2p().is_none());
    core.shutdown().await?;
    runtime.shutdown().await;
    Ok(())
}
