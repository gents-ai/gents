use super::ClientRuntime;
use anyhow::Result;
use gents_desktop_core::client::DesktopPaths;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn runtime_access_is_approved_revoked_and_requested_again_over_http() -> Result<()> {
    use gents_observe::client_projection::observe::RuntimeConnectionStatus;
    use gents_protocol::client_access::{
        ClientAccessDecision, ClientAccessState, DecideClientAccess,
    };

    let directory = tempfile::tempdir()?;
    let core = Arc::new(
        ClientRuntime::start_with_paths_and_options(
            DesktopPaths::from_root(directory.path()),
            crate::client_test_support::file_journal_options(),
        )
        .await?,
    );
    let runtime = gents::client_test_support::ServedRuntimeHome::start().await;
    let pending = crate::commands::request_runtime_access(
        &core,
        crate::types::RuntimeAccessConnectRequest {
            server_address: runtime.graphql().into(),
        },
    )
    .await?;
    assert_eq!(pending.state, "pending");
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
