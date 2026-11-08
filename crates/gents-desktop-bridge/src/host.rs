mod connection;
#[cfg(any(unix, windows))]
pub mod local;
pub mod protocol;
#[cfg(any(unix, windows))]
mod service;

#[cfg(any(unix, windows))]
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
    installation_build: Option<String>,
    advertised_commands: Vec<String>,
    policy: ResolvedBridgePolicy,
}

impl ClientHost {
    /// The executable digest is compatibility metadata, never runtime identity
    /// or document authority. Embedded application hosts retain their own lifetime.
    pub fn for_installation(core: Arc<ClientCore>, build: String) -> Self {
        let mut host = Self::new(core);
        host.installation_build = Some(build);
        host
    }

    pub fn for_installation_with_command_inventory(
        core: Arc<ClientCore>,
        build: String,
        commands: Vec<String>,
    ) -> Self {
        assert!(commands
            .iter()
            .all(|command| { ClientCommand::NAMES.contains(&command.as_str()) }));
        let mut host = Self::for_installation(core, build);
        host.advertised_commands = commands;
        host
    }

    /// Private client attachments present the current identity's mailbox,
    /// including decision questions. Embedding webview grants remain
    /// independent; no runtime document authorization is conferred here.
    pub fn new(core: Arc<ClientCore>) -> Self {
        Self {
            installation_build: None,
            advertised_commands: ClientCommand::NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
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
                    mailbox_read: true,
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

    async fn execute(
        &self,
        command: ClientCommand,
        cancel: CancellationToken,
    ) -> Result<Value, BridgeError> {
        match command {
            ClientCommand::BackendAuthentication { request } => {
                self.require_configured_runtime(&request.node_did).await?;
                let (revision, backend) = self.core.read_backend_authentication(&request.node_did, &request.backend_id)
                    .await.map_err(|_| BridgeError::untyped("Authentication settings could not be read. Reopen Accounts after checking the runtime."))?;
                encode(crate::accounts::BackendAuthenticationView::from_backend(
                    revision, &backend,
                ))
            }
            ClientCommand::BackendAuthenticationSave { request } => {
                self.require_configured_runtime(&request.node_did).await?;
                self.core.save_backend_authentication(&request.node_did, &request.backend_id, &request.revision, &request.auth)
                    .await.map_err(|_| BridgeError::untyped("Authentication save could not be confirmed. Reopen Accounts before trying again."))?;
                encode(())
            }
            ClientCommand::RuntimeIsLocal { node_did } => {
                let record = self.require_connected_runtime(&node_did).await?;
                encode(record.is_managed_runtime())
            }
            ClientCommand::InferenceCatalog {} => {
                encode(gents::inference::backends::setup::inference_setup_catalog())
            }
            ClientCommand::InferenceSetupState { node_did } => {
                let record = self.require_connected_runtime(&node_did).await?;
                if record.operator_graphql().is_none()
                    || matches!(
                        self.core.options().operator_identity_interaction,
                        gents_desktop_core::local_runtime::OperatorIdentityInteraction::Prevent
                    )
                {
                    return encode(
                        self.core
                            .read_inference_setup_state(&node_did)
                            .await
                            .map_err(backend_error)?,
                    );
                }
                let access = self
                    .core
                    .operator_access(&node_did)
                    .map_err(backend_error)?;
                encode(
                    gents_desktop_core::inference::load_inference_setup_state(&access, &node_did)
                        .await
                        .map_err(|error| {
                            BridgeError::new(
                                BridgeErrorCode::EndpointUnreachable,
                                format!("Runtime configuration is not available: {error}"),
                            )
                        })?,
                )
            }
            ClientCommand::InferenceModelsDiscover { request } => {
                self.require_configured_runtime(&request.node_did).await?;
                encode(
                    crate::inference_setup::discover_inference_models_for_core(
                        request,
                        Some(&self.core),
                    )
                    .await?,
                )
            }
            ClientCommand::InferenceModelRecommendation { request } => encode(
                crate::inference_setup::inference_model_recommendation(request)?,
            ),
            ClientCommand::AgentModelSelect { request } => {
                self.require_configured_runtime(&request.node_did).await?;
                encode(
                    self.core
                        .select_agent_model(
                            &request.node_did,
                            &request.agent_did,
                            &request.backend_id,
                            &request.model_name,
                            request.reasoning_effort,
                            request.service_tier,
                        )
                        .await
                        .map_err(backend_error)?,
                )
            }
            ClientCommand::ConfigApply { request } => {
                self.require_configured_runtime(&request.document.node.node_did)
                    .await?;
                crate::commands::apply_config_components(&self.core, *request)
                    .await
                    .map_err(backend_error)?;
                encode(())
            }
            ClientCommand::ProviderUsageRead { request } => {
                self.require_configured_runtime(&request.node_did).await?;
                encode(crate::accounts::usage::read(&self.core, request).await?)
            }
            ClientCommand::SessionFileInventory {
                node_did,
                session_id,
                action,
            } => encode(
                crate::commands::session_file_inventory(
                    &self.core,
                    node_did,
                    session_id,
                    action.unwrap_or_default(),
                )
                .await
                .map_err(backend_error)?,
            ),
            ClientCommand::InventoryFileChange {
                node_did,
                session_id,
                reference,
                retry_id,
            } => encode(
                crate::commands::inventory_file_change(
                    &self.core, node_did, session_id, reference, retry_id,
                )
                .await
                .map_err(backend_error)?,
            ),
            ClientCommand::ToolFileChanges {
                node_did,
                session_id,
                request_id,
                tool_call_doc_id,
                retry_id,
            } => encode(
                crate::commands::tool_file_changes(
                    &self.core,
                    node_did,
                    session_id,
                    request_id,
                    tool_call_doc_id,
                    retry_id,
                )
                .await
                .map_err(backend_error)?,
            ),
            ClientCommand::RequestOutput {
                node_did,
                session_id,
                request_id,
            } => encode(
                crate::commands::request_output(&self.core, node_did, session_id, request_id)
                    .await
                    .map_err(backend_error)?,
            ),
            ClientCommand::RequestAttachments {
                node_did,
                session_id,
                request_id,
                message_key,
                index,
            } => encode(crate::commands::bounded_attachment_response(
                crate::commands::request_attachments(
                    &self.core,
                    &node_did,
                    &session_id,
                    &request_id,
                    message_key.as_deref(),
                    index,
                )
                .await?,
            )?),
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
                self.core
                    .request_p2p_repair()
                    .await
                    .map_err(backend_error)?;
                encode(())
            }
            ClientCommand::EnrollLocal { node_home } => {
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
                let peer = self
                    .core
                    .refresh_local_standard_peer(&home, "Local runtime")
                    .await
                    .map_err(backend_error)?;
                let target = crate::local_pairing::ManagedPairingTarget {
                    node_name: peer.label,
                    node_did: peer.node_did,
                    graphql: peer.graphql.ok_or_else(|| {
                        BridgeError::untyped(
                            "The local runtime did not provide a configuration endpoint.",
                        )
                    })?,
                };
                crate::local_pairing::retry_managed_pairing(
                    &cancel,
                    crate::local_pairing::MANAGED_PAIRING_ATTEMPTS,
                    crate::local_pairing::MANAGED_PAIRING_RETRY_DELAY,
                    || {
                        crate::local_pairing::ensure_managed_runtime_pairing(
                            Arc::clone(&self.core),
                            &home,
                            &target,
                            &cancel,
                        )
                    },
                )
                .await
                .map_err(|error| BridgeError::untyped(error.to_string()))?;
                encode(())
            }
            ClientCommand::Snapshot {} => encode(
                crate::snapshot::build_client_snapshot_with_grants(
                    Some(&self.core),
                    &self.policy,
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
                )
                .await?,
            ),
            ClientCommand::SessionRows(args) => encode(
                crate::commands::session_rows(
                    &self.core,
                    args.session_id,
                    Some(args.node_did),
                    args.window,
                    args.limit,
                )
                .await?,
            ),
            ClientCommand::SessionContextDetails { request } => {
                encode(crate::commands::session_context_details(&self.core, request).await?)
            }
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
            ClientCommand::RecoverSubmission { query } => encode(
                crate::commands::recover_chat_submission(&self.core, query)
                    .await
                    .map_err(backend_error)?,
            ),
            ClientCommand::AcknowledgeSubmission { receipt } => encode(
                crate::commands::acknowledge_chat_submission(&self.core, receipt)
                    .await
                    .map_err(backend_error)?,
            ),
            ClientCommand::Retry {
                request_id,
                node_did,
            } => encode(
                crate::commands::retry_request(&self.core, &request_id, Some(&node_did))
                    .await
                    .map_err(backend_error)?,
            ),
            ClientCommand::SessionGoal(args) => encode(
                crate::commands::session_goal(&self.core, &args.node_did, &args.session_id)
                    .await
                    .map_err(backend_error)?,
            ),
            ClientCommand::SessionProvenance { request } => encode(
                crate::provenance::session_provenance_request(&self.core, request)
                    .await
                    .map_err(BridgeError::untyped)?,
            ),
            ClientCommand::GoalControl { request } => {
                encode(crate::commands::control_goal(&self.core, *request).await?)
            }
            ClientCommand::RecoverGoalSubmission { query } => encode(
                crate::commands::recover_goal_submission(&self.core, query)
                    .await
                    .map_err(backend_error)?,
            ),
            ClientCommand::ResumeGoalSubmission { request, receipt } => encode(
                crate::commands::resume_goal_submission(&self.core, *request, receipt).await?,
            ),
            ClientCommand::AcknowledgeGoalSubmission { receipt } => encode(
                crate::commands::acknowledge_goal_submission(&self.core, receipt)
                    .await
                    .map_err(backend_error)?,
            ),
            ClientCommand::SteerPending { request } => encode(
                crate::commands::steer_pending_message(&self.core, request)
                    .await
                    .map_err(backend_error)?,
            ),
            ClientCommand::EditPending { request } => encode(
                crate::commands::edit_pending_queue(&self.core, request)
                    .await
                    .map_err(backend_error)?,
            ),
            ClientCommand::ForkBeforeMessage { request } => encode(
                crate::commands::fork_session_before_message(&self.core, request)
                    .await
                    .map_err(backend_error)?,
            ),
            ClientCommand::SessionHistory { request } => encode(
                crate::commands::session_history(&self.core, request)
                    .await
                    .map_err(backend_error)?,
            ),
            ClientCommand::Archive { request } => encode(
                crate::commands::set_session_archived(&self.core, request)
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
            ClientCommand::ProcessControl { request } => {
                encode(crate::process_control::process_control(&self.core, &request).await?)
            }
        }
    }

    async fn require_connected_runtime(
        &self,
        node_did: &str,
    ) -> Result<gents_desktop_core::client::PeerRecord, BridgeError> {
        let records = self.core.peer_records().await;
        let mut matches = records.iter().filter(|record| record.node_did == node_did);
        matches
            .next()
            .filter(|_| matches.next().is_none())
            .cloned()
            .ok_or_else(|| {
                BridgeError::new(
                    BridgeErrorCode::InvalidArgument,
                    "The runtime connection is missing or ambiguous. Select a connected runtime.",
                )
            })
    }

    async fn require_configured_runtime(&self, node_did: &str) -> Result<(), BridgeError> {
        if self
            .require_connected_runtime(node_did)
            .await?
            .operator_graphql()
            .is_none()
        {
            return Err(BridgeError::new(
                BridgeErrorCode::InvalidArgument,
                "Choose a configured runtime. Use Gents: Connect Local Runtime to connect a runtime on this machine.",
            ));
        }
        Ok(())
    }
}

fn encode(value: impl Serialize) -> Result<Value, BridgeError> {
    serde_json::to_value(value).map_err(|error| backend_error(error.into()))
}

fn backend_error(error: anyhow::Error) -> BridgeError {
    let error = BridgeError::classify_transport_error(error);
    if error.code == BridgeErrorCode::Unknown {
        BridgeError::new(BridgeErrorCode::Backend, error.message)
    } else {
        error
    }
}

#[cfg(test)]
mod tests;
