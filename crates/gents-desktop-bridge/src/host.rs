mod actions;
mod approvals;
mod connection;
#[cfg(unix)]
pub mod local;
pub mod protocol;
#[cfg(unix)]
mod service;
pub mod settings;
pub mod speech;

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
    speech: speech::SpeechClient,
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
            speech: speech::SpeechClient::default(),
        }
    }

    pub async fn serve<S>(self: Arc<Self>, stream: S, stop: CancellationToken) -> anyhow::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        connection::serve(self, stream, stop).await
    }

    async fn execute(
        &self,
        command: ClientCommand,
        approvals: &approvals::AttachmentApprovals,
    ) -> Result<Value, BridgeError> {
        match command {
            ClientCommand::ConfigCatalog { node_did } => {
                self.require_configured_runtime(&node_did).await?;
                encode(
                    settings::catalog(&self.core, &node_did)
                        .await
                        .map_err(backend_error)?,
                )
            }
            ClientCommand::ConfigRead {
                node_did,
                collection,
                id,
            } => {
                self.require_configured_runtime(&node_did).await?;
                let target = settings::target(&collection).map_err(backend_error)?;
                encode(
                    self.core
                        .read_config_editor(&node_did, target, &id)
                        .await
                        .map_err(backend_error)?,
                )
            }
            ClientCommand::ConfigSave {
                node_did,
                collection,
                id,
                revision,
                content,
            } => {
                self.require_configured_runtime(&node_did).await?;
                let target = settings::target(&collection).map_err(backend_error)?;
                encode(
                    self.core
                        .save_config_editor(&node_did, target, &id, &revision, &content)
                        .await
                        .map_err(backend_error)?,
                )
            }
            ClientCommand::ActionCatalog { node_did } => {
                self.require_configured_runtime(&node_did).await?;
                encode(
                    actions::catalog(&self.core, &node_did)
                        .await
                        .map_err(backend_error)?,
                )
            }
            ClientCommand::StartTask {
                node_did,
                task_id,
                arguments_json,
            } => {
                self.require_configured_runtime(&node_did).await?;
                if arguments_json.len() > 64 * 1024 {
                    return Err(BridgeError::new(
                        BridgeErrorCode::InvalidArgument,
                        "Task arguments exceed 64 KiB.",
                    ));
                }
                let arguments = serde_json::from_str(&arguments_json).map_err(|_| {
                    BridgeError::new(
                        BridgeErrorCode::InvalidArgument,
                        "Enter valid JSON task arguments.",
                    )
                })?;
                let submitted = self
                    .core
                    .fire_task_now_for_node(&node_did, &task_id, arguments)
                    .await
                    .map_err(backend_error)?;
                encode(crate::types::ChatSendResult::from(submitted))
            }
            ClientCommand::RunSchedule {
                node_did,
                schedule_id,
            } => {
                self.require_configured_runtime(&node_did).await?;
                let submitted = self
                    .core
                    .fire_schedule_now_for_node(&node_did, &schedule_id)
                    .await
                    .map_err(backend_error)?;
                encode(crate::types::ChatSendResult::from(submitted))
            }
            ClientCommand::RequestOutput {
                node_did,
                session_id,
                request_id,
            } => encode(
                crate::commands::request_output(&self.core, node_did, session_id, request_id)
                    .await
                    .map_err(backend_error)?,
            ),
            ClientCommand::SpeechServices { node_did } => encode(
                self.speech
                    .services(&self.core, &node_did)
                    .await
                    .map_err(backend_error)?,
            ),
            ClientCommand::SpeechTools {
                node_did,
                service_id,
            } => encode(
                self.speech
                    .tools(&self.core, &node_did, &service_id)
                    .await
                    .map_err(backend_error)?,
            ),
            ClientCommand::SpeechRecognize { request } => encode(
                self.speech
                    .recognize(&self.core, request)
                    .await
                    .map_err(backend_error)?,
            ),
            ClientCommand::SpeechSynthesize { request } => encode(
                self.speech
                    .synthesize(&self.core, request)
                    .await
                    .map_err(backend_error)?,
            ),
            ClientCommand::ConnectLocal { node_home } => {
                let home = match node_home {
                    Some(path) if !path.trim().is_empty() => std::path::PathBuf::from(path.trim()),
                    _ => gents_desktop_core::local_runtime::default_node_home()
                        .map_err(backend_error)?,
                };
                if !home.is_absolute() {
                    return Err(BridgeError::new(
                        BridgeErrorCode::InvalidArgument,
                        "use an absolute local runtime home path",
                    ));
                }
                self.core
                    .refresh_local_standard_peer(&home, "Local runtime")
                    .await
                    .map_err(backend_error)?;
                encode(())
            }
            ClientCommand::ApprovalSubscribe { node_did, token } => {
                approvals.subscribe(&self.core, &node_did, &token).await?;
                encode(())
            }
            ClientCommand::ApprovalUnsubscribe { token } => {
                approvals.unsubscribe(&token);
                encode(())
            }
            ClientCommand::ApprovalPending { node_did, token } => {
                encode(approvals.pending(&self.core, &node_did, &token).await?)
            }
            ClientCommand::ApprovalDecide {
                node_did,
                token,
                id,
                decision,
            } => {
                approvals
                    .decide(&self.core, &node_did, &token, &id, &decision)
                    .await?;
                encode(())
            }
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

    async fn require_configured_runtime(&self, node_did: &str) -> Result<(), BridgeError> {
        let records = self.core.peer_records().await;
        let mut matches = records.iter().filter(|record| record.node_did == node_did);
        let record = matches.next().filter(|_| matches.next().is_none());
        if record.is_none_or(|record| record.operator_graphql().is_none()) {
            return Err(BridgeError::new(BridgeErrorCode::InvalidArgument,
                "Choose a configured runtime. Use Gents: Connect Local Runtime to connect a runtime on this machine."));
        }
        Ok(())
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
