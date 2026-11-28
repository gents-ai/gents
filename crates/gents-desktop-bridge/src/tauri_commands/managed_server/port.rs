use super::authority::suggested_tool_root;
use crate::state::DesktopAppState;
use crate::types::{ManagedServerState, ManagedServerStatus};
use gents::client_runtime::HostedRuntimeStatusError;
use gents_desktop_core::preference::parse_tool_ceiling;
use gents_protocol::operations::install_repair::Blocker;
use gents_protocol::serve_lifecycle::ObservedServeLifecycle;
use gents_service::repair::port::{
    observe_port, read_initialized_did, served_identity, PortObservation, ServedIdentity,
};
use gents_store::client_error::BridgeError;
use gents_store::client_error::BridgeErrorCode;
use gents_store::config_client::ClientSigningCapability;
use std::path::Path;

pub(super) enum PortReadiness {
    Ready(ManagedServerStatus),
    Foreign(ForeignRuntime),
    Booting,
    Outdated {
        version: Option<String>,
    },
    Occupied(u16),
    NotListening,
    /// The runtime publicly reports ready, but the User could not read its
    /// private status, so its authority (tool root, ceiling) is unobserved.
    /// It reports only on the managed service; the client's runtime
    /// connection status has one owner, `client::observe`
    /// (`RuntimeConnectionObservation`), and this never writes it.
    StatusUnreadable {
        public: ManagedServerStatus,
        cause: HostedRuntimeStatusError,
    },
}

pub(super) async fn user_status_signer(state: &DesktopAppState) -> Option<ClientSigningCapability> {
    use gents_store::identity::NodeIdentity as _;
    let running = crate::state::current_core(state)
        .and_then(|core| core.node_identity().client_signing_capability().ok());
    gents::client_runtime::hosted_status_signer(&state.policy.desktop_paths, running).await
}

pub(super) fn status_unreadable_error(cause: &HostedRuntimeStatusError) -> BridgeError {
    let code = match cause {
        HostedRuntimeStatusError::NoUserSigner => BridgeErrorCode::ClientNotRunning,
        HostedRuntimeStatusError::Denied { .. } => BridgeErrorCode::PermissionDenied,
        HostedRuntimeStatusError::Failed(_) => BridgeErrorCode::Backend,
    };
    BridgeError::new(
        code,
        format!(
            "The local Node is running, but this client could not read its status: {}",
            gents_protocol::error_chain::ErrorChain(cause)
        ),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ForeignRuntime {
    pub(super) port: u16,
    pub(super) node_did: Option<String>,
    pub(super) home: Option<String>,
}

impl ForeignRuntime {
    pub(super) fn message(&self) -> String {
        sentence(&match self.node_did {
            Some(_) => Blocker::HandStartedNode {
                port: self.port,
                home: self.home.clone(),
            },
            None => Blocker::PortInUse {
                port: self.port,
                holder: None,
            },
        })
    }
}

pub(super) fn occupied_port_message(port: u16) -> String {
    sentence(&Blocker::PortInUse { port, holder: None })
}

/// Blockers read as lowercase clauses after "couldn't start:"; the bridge
/// shows them as sentences.
pub(super) fn sentence(blocker: &Blocker) -> String {
    capitalize(blocker.to_string())
}

pub(super) fn capitalize(text: String) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => text,
    }
}

pub(super) async fn observe_port_readiness(
    node_home: &Path,
    user: Option<&ClientSigningCapability>,
) -> Result<PortReadiness, BridgeError> {
    let config = gents_service::server_host::ServerConfig::for_home(node_home.to_path_buf())
        .unwrap_or_else(|error| {
            tracing::warn!(
                target: "gents_desktop::managed_server",
                error = %format!("{error:#}"),
                "the home's network settings are unreadable; observing the standard port"
            );
            gents_service::server_host::ServerConfig::standard(node_home.to_path_buf())
        });
    observe_port_readiness_at(&config, user).await
}

pub(super) async fn observe_port_readiness_at(
    config: &gents_service::server_host::ServerConfig,
    user: Option<&ClientSigningCapability>,
) -> Result<PortReadiness, BridgeError> {
    let node_home = &config.home;
    let initialized_did = if gents_service::server_host::initialized_home(node_home) {
        read_initialized_did(node_home).await
    } else {
        None
    };
    let (identity, payload) = match observe_port(config, initialized_did.as_deref()).await {
        PortObservation::NotListening => return Ok(PortReadiness::NotListening),
        PortObservation::NotGents => return Ok(PortReadiness::Occupied(config.http_port)),
        PortObservation::Gents { identity, payload } => (identity, payload),
    };
    let classified = classify_identified_payload(config.http_port, identity, payload);
    let PortReadiness::Ready(status) = classified else {
        return Ok(classified);
    };
    let mut graphql = reqwest::Url::parse(&config.status_url())
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    graphql.set_path("/api/v0/graphql");
    let record = gents_protocol::runtime_connection::RuntimeConnection {
        node_did: initialized_did.as_deref().unwrap_or_default().to_owned(),
        label: "Local".to_owned(),
        graphql: graphql.to_string(),
        local_node_home: Some(node_home.to_string_lossy().into_owned()),
        access_request: None,
        access_state: None,
    };
    Ok(
        match gents::client_runtime::fetch_hosted_runtime_status(&record, user).await {
            Ok(Some(payload)) => {
                classify_port_payload(config.http_port, initialized_did.as_deref(), payload)
            }
            Ok(None) => PortReadiness::Ready(status),
            Err(cause) => PortReadiness::StatusUnreadable {
                public: status,
                cause,
            },
        },
    )
}

pub(super) fn classify_port_payload(
    port: u16,
    initialized_did: Option<&str>,
    payload: serde_json::Value,
) -> PortReadiness {
    let identity = served_identity(initialized_did, &payload);
    classify_identified_payload(port, identity, payload)
}

fn classify_identified_payload(
    port: u16,
    identity: ServedIdentity,
    payload: serde_json::Value,
) -> PortReadiness {
    if let ServedIdentity::Different { node_did, home } = identity {
        return PortReadiness::Foreign(ForeignRuntime {
            port,
            node_did,
            home,
        });
    }
    let live_did = payload
        .get("node_did")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    match ObservedServeLifecycle::observe(&payload) {
        ObservedServeLifecycle::Ready => {
            PortReadiness::Ready(managed_status_from_payload(payload, &live_did))
        }
        ObservedServeLifecycle::Starting => PortReadiness::Booting,
        ObservedServeLifecycle::Outdated { version } => PortReadiness::Outdated { version },
    }
}

/// A ready runtime whose private status the User cannot read is an error with
/// its cause: callers must not treat it as absent and launch over it.
pub(super) async fn matching_external_server(
    node_home: &std::path::Path,
    user: Option<&ClientSigningCapability>,
) -> Result<Option<ManagedServerStatus>, BridgeError> {
    Ok(match observe_port_readiness(node_home, user).await? {
        PortReadiness::Ready(status) => Some(status),
        PortReadiness::StatusUnreadable { cause, .. } => {
            return Err(status_unreadable_error(&cause))
        }
        PortReadiness::Foreign(_)
        | PortReadiness::Booting
        | PortReadiness::Outdated { .. }
        | PortReadiness::Occupied(_)
        | PortReadiness::NotListening => None,
    })
}

fn managed_status_from_payload(payload: serde_json::Value, live_did: &str) -> ManagedServerStatus {
    ManagedServerStatus {
        state: ManagedServerState::External,
        auto_start: false,
        node_name: payload
            .get("node_name")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        node_did: (!live_did.is_empty()).then(|| live_did.to_string()),
        graphql: payload
            .get("desktop_graphql")
            .or_else(|| payload.get("graphql"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        effective_tool_ceiling: payload
            .get("tool_ceiling")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_tool_ceiling),
        effective_tool_root: payload
            .get("tool_root")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        suggested_tool_root: suggested_tool_root(),
        pairing_ready: false,
        approval_required: false,
        runtime_booting: false,
        error: None,
        error_code: None,
        deleted_home: None,
    }
}

/// Where the home listens before its service is installed (Lean
/// `NodePort.settle`).
pub(super) async fn settle_home_port(
    node_home: &Path,
    user: Option<&ClientSigningCapability>,
) -> Result<(), BridgeError> {
    let unreadable = |error: String| BridgeError::new(BridgeErrorCode::InvalidArgument, error);
    let config = gents_service::server_host::ServerConfig::for_home(node_home.to_path_buf())
        .map_err(|error| unreadable(format!("{error:#}")))?;
    let port = gents_service::node_port::port_source(node_home)
        .map_err(|error| unreadable(gents_protocol::error_chain::ErrorChain(&error).to_string()))?;
    let served = gents_service::home_state::has_served(node_home);
    settle_port_at(&config, port, served, user).await
}

pub(super) async fn settle_port_at(
    config: &gents_service::server_host::ServerConfig,
    port: gents_service::node_port::PortSource,
    served: bool,
    user: Option<&ClientSigningCapability>,
) -> Result<(), BridgeError> {
    use gents_service::node_port::{move_http_port, settle, PortOccupant, PortSettlement};
    let (occupant, blocker) = match observe_port_readiness_at(config, user).await? {
        PortReadiness::NotListening => (PortOccupant::Nobody, None),
        PortReadiness::Ready(_)
        | PortReadiness::StatusUnreadable { .. }
        | PortReadiness::Booting
        | PortReadiness::Outdated { .. } => (PortOccupant::ThisHome, None),
        PortReadiness::Foreign(foreign) => (
            if foreign.node_did.is_some() {
                PortOccupant::AnotherNode
            } else {
                PortOccupant::AnotherApp
            },
            Some(foreign.message()),
        ),
        PortReadiness::Occupied(port) => {
            (PortOccupant::AnotherApp, Some(occupied_port_message(port)))
        }
    };
    match settle(port, served, occupant) {
        PortSettlement::Keep => Ok(()),
        PortSettlement::Move => {
            let server = config.clone();
            let port = tokio::task::spawn_blocking(move || move_http_port(&server))
                .await
                .map_err(|error| BridgeError::untyped(error.to_string()))?
                .map_err(|error| {
                    let code = match error {
                        gents_service::node_port::MoveError::NoFreePort { .. } => {
                            BridgeErrorCode::InvalidArgument
                        }
                        gents_service::node_port::MoveError::Settings(_) => {
                            BridgeErrorCode::Backend
                        }
                    };
                    BridgeError::new(
                        code,
                        gents_protocol::error_chain::ErrorChain(&error).to_string(),
                    )
                })?;
            tracing::info!(
                target: "gents_desktop::managed_server",
                taken = config.http_port,
                port,
                "the port is in use; the new node listens on another port"
            );
            Ok(())
        }
        PortSettlement::BlockedByNode | PortSettlement::BlockedByApp => Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            blocker.unwrap_or_default(),
        )),
    }
}

pub(super) fn ensure_native_owns_running_endpoint(
    port: &PortReadiness,
    native_job_loaded: bool,
) -> Result<(), BridgeError> {
    if matches!(
        port,
        PortReadiness::Ready(_)
            | PortReadiness::StatusUnreadable { .. }
            | PortReadiness::Booting
            | PortReadiness::Outdated { .. }
    ) && !native_job_loaded
    {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "The running local Node is not owned by the native Gents service. Stop it explicitly before using managed controls.",
        ));
    }
    Ok(())
}
