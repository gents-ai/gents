mod connection;
#[cfg(unix)]
pub mod local;
pub mod protocol;
#[cfg(unix)]
mod service;

#[cfg(unix)]
pub use service::ClientHostService;

use std::sync::Arc;

use gents_desktop_core::client::ClientCore;
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;

use crate::config::{AppMeta, BootstrapPolicy, ManagedServerPolicy, ResolvedBridgePolicy};
use crate::error::{BridgeError, BridgeErrorCode};
use crate::snapshot::projection::SnapshotGrants;
use protocol::ClientCommand;

/// Transport attachments share the existing core and never own its shutdown.
/// The installation host must stop and join attachments before closing the core.
pub struct ClientHost {
    core: Arc<ClientCore>,
    policy: ResolvedBridgePolicy,
}

impl ClientHost {
    pub fn new(core: Arc<ClientCore>) -> Self {
        Self {
            policy: ResolvedBridgePolicy {
                desktop_paths: core.paths().clone(),
                node_home: None,
                bootstrap: BootstrapPolicy::PairedRemoteOnly,
                app_meta: AppMeta {
                    app_name: "gents-client-host".into(),
                    app_version: env!("CARGO_PKG_VERSION").into(),
                },
                snapshot_grants: SnapshotGrants {
                    session_read: true,
                    fleet_read: true,
                    ..SnapshotGrants::default()
                },
                managed_server: ManagedServerPolicy::Disabled,
                client_host: false,
                store_key_custody: core.options().store_key_custody,
            },
            core,
        }
    }

    pub async fn serve<S>(self: Arc<Self>, stream: S, stop: CancellationToken) -> anyhow::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        connection::serve(self, stream, stop).await
    }

    async fn execute(&self, command: ClientCommand) -> Result<Value, BridgeError> {
        match command {
            ClientCommand::Snapshot {} => encode(
                crate::snapshot::build_client_snapshot_with_grants(
                    Some(&self.core),
                    Some(&self.policy),
                    self.policy.snapshot_grants,
                )
                .await
                .map_err(BridgeError::untyped)?,
            ),
            ClientCommand::Enroll { request } => {
                encode(crate::commands::request_status_enrollment(&self.core, request).await?)
            }
            ClientCommand::SessionSnapshot(args) => encode(
                crate::commands::session_snapshot(
                    &self.core,
                    args.session_id,
                    Some(args.node_did),
                    args.request_id,
                    args.timeline_limit,
                    args.timeline_before_item_key,
                )
                .await?,
            ),
            ClientCommand::SessionLiveDelta(args) => encode(
                crate::snapshot::build_session_live_delta(
                    &self.core,
                    &args.session_id,
                    Some(&args.node_did),
                    &args.request_id,
                    &args.base_live_cursor,
                    args.base_content_byte_len,
                    &args.base_content_hash,
                    args.base_reasoning_byte_len,
                    &args.base_reasoning_hash,
                )
                .await
                .map_err(backend_error)?,
            ),
            ClientCommand::Send { request } => encode(
                crate::commands::send_chat_message(&self.core, request)
                    .await
                    .map_err(backend_error)?,
            ),
            ClientCommand::SessionGoal(args) => encode(
                crate::commands::session_goal(&self.core, &args.node_did, &args.session_id)
                    .await
                    .map_err(backend_error)?,
            ),
            ClientCommand::GoalControl { request } => encode(
                crate::commands::control_goal(&self.core, request)
                    .await
                    .map_err(backend_error)?,
            ),
            ClientCommand::EditPending { request } => encode(
                crate::commands::edit_pending_queue(&self.core, request)
                    .await
                    .map_err(backend_error)?,
            ),
            ClientCommand::Rename { request } => encode(
                crate::commands::rename_session(&self.core, request)
                    .await
                    .map_err(backend_error)?,
            ),
            ClientCommand::Interrupt { request } => encode(
                crate::interrupt::interrupt_request(&self.core, &request)
                    .await
                    .map_err(BridgeError::untyped)?,
            ),
        }
    }
}

fn encode(value: impl Serialize) -> Result<Value, BridgeError> {
    serde_json::to_value(value).map_err(|error| backend_error(error.into()))
}

fn backend_error(error: anyhow::Error) -> BridgeError {
    BridgeError::new(BridgeErrorCode::Backend, error.to_string())
}

#[cfg(test)]
mod tests;
