use super::authority::{parse_tool_ceiling, suggested_tool_root};
use super::native::native_error;
use super::preference::read_initialized_did;
use crate::error::{BridgeError, BridgeErrorCode};
use crate::types::{ManagedServerState, ManagedServerStatus};
use gents_desktop_core::local_runtime::OperatorIdentityInteraction;
use gents_protocol::serve_lifecycle::ObservedServeLifecycle;
use std::path::Path;
use std::time::Duration;

pub(super) enum PortReadiness {
    Ready(ManagedServerStatus),
    Foreign(ForeignRuntime),
    Booting,
    Outdated { version: Option<String> },
    Occupied(u16),
    NotListening,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ForeignRuntime {
    pub(super) port: u16,
    pub(super) node_did: Option<String>,
    pub(super) home: Option<String>,
}

impl ForeignRuntime {
    fn from_payload(port: u16, payload: &serde_json::Value) -> Self {
        let field = |name: &str| {
            payload
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        Self {
            port,
            node_did: field("node_did"),
            home: field("home"),
        }
    }

    pub(super) fn message(&self) -> String {
        let port = self.port;
        let Some(did) = self.node_did.as_deref() else {
            return format!(
                "Port {port} is in use by a program that does not advertise a Gents identity, and the local Node needs that port. Quit that program, then start the Node again."
            );
        };
        let (located, home) = match self.home.as_deref() {
            Some(home) => (format!(" from home {home}"), home),
            None => (String::new(), "<its home>"),
        };
        format!(
            "Port {port} is already served by a different Gents Node ({did}){located}, and the local Node needs that port. Stop the other Node with `gents service stop --home {home}` if it runs as a background service, or with Ctrl-C in the terminal running `gents server`. Or restart it on another port with `gents server --home {home} --http-port <port>`."
        )
    }
}

pub(super) fn occupied_port_message(port: u16) -> String {
    format!(
        "Port {port} is in use by another program that is not a Gents Node, and the local Node needs that port. Quit that program, then start the Node again."
    )
}

pub(super) async fn observe_port_readiness(node_home: &Path) -> Result<PortReadiness, BridgeError> {
    observe_port_readiness_with_identity_interaction(
        node_home,
        OperatorIdentityInteraction::Prevent,
    )
    .await
}

pub(super) async fn observe_port_readiness_with_identity_interaction(
    node_home: &Path,
    interaction: OperatorIdentityInteraction,
) -> Result<PortReadiness, BridgeError> {
    let config = gents_server::server_host::ServerConfig::standard(node_home.to_path_buf());
    observe_port_readiness_at(&config, interaction).await
}

pub(super) async fn observe_port_readiness_at(
    config: &gents_server::server_host::ServerConfig,
    interaction: OperatorIdentityInteraction,
) -> Result<PortReadiness, BridgeError> {
    let node_home = &config.home;
    let Some(payload) = default_port_payload(config).await? else {
        let address = std::net::SocketAddr::new(config.http_addr, config.http_port);
        let accepts = tokio::time::timeout(
            Duration::from_millis(250),
            tokio::net::TcpStream::connect(address),
        )
        .await
        .is_ok_and(|connected| connected.is_ok());
        return Ok(if accepts {
            PortReadiness::Occupied(config.http_port)
        } else {
            PortReadiness::NotListening
        });
    };
    let initialized_did = if gents_server::server_host::initialized_home(node_home) {
        read_initialized_did(node_home).await
    } else {
        None
    };
    let classified = classify_port_payload(config.http_port, initialized_did.as_deref(), payload);
    let PortReadiness::Ready(status) = classified else {
        return Ok(classified);
    };
    let mut graphql = reqwest::Url::parse(&config.status_url())
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    graphql.set_path("/api/v0/graphql");
    let mut record = gents_desktop_core::client::PeerRecord::local_standard(
        "Local",
        "",
        initialized_did.as_deref().unwrap_or_default(),
        graphql.as_str(),
    );
    record.local_node_home = Some(node_home.to_string_lossy().into_owned());
    let private =
        gents_desktop_core::local_runtime::fetch_hosted_runtime_status(&record, interaction)
            .await
            .map_err(native_error)?;
    Ok(match private {
        Some(payload) => {
            classify_port_payload(config.http_port, initialized_did.as_deref(), payload)
        }
        None => PortReadiness::Ready(status),
    })
}

pub(super) fn classify_port_payload(
    port: u16,
    initialized_did: Option<&str>,
    payload: serde_json::Value,
) -> PortReadiness {
    let live_did = payload
        .get("node_did")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    if ensure_matching_identity(initialized_did, &live_did, port).is_err() {
        return PortReadiness::Foreign(ForeignRuntime::from_payload(port, &payload));
    }
    match ObservedServeLifecycle::observe(&payload) {
        ObservedServeLifecycle::Ready => {
            PortReadiness::Ready(managed_status_from_payload(payload, &live_did))
        }
        ObservedServeLifecycle::Starting => PortReadiness::Booting,
        ObservedServeLifecycle::Outdated { version } => PortReadiness::Outdated { version },
    }
}

pub(super) async fn matching_external_server(
    node_home: &std::path::Path,
    interaction: OperatorIdentityInteraction,
) -> Result<Option<ManagedServerStatus>, BridgeError> {
    Ok(
        match observe_port_readiness_with_identity_interaction(node_home, interaction).await? {
            PortReadiness::Ready(status) => Some(status),
            PortReadiness::Foreign(_)
            | PortReadiness::Booting
            | PortReadiness::Outdated { .. }
            | PortReadiness::Occupied(_)
            | PortReadiness::NotListening => None,
        },
    )
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
    }
}

async fn default_port_payload(
    config: &gents_server::server_host::ServerConfig,
) -> Result<Option<serde_json::Value>, BridgeError> {
    match gents_desktop_core::local_runtime::fetch_runtime_connection_payload(&config.status_url())
        .await
    {
        Ok(payload) => Ok(Some(payload)),
        Err(_) => Ok(None),
    }
}

pub(super) async fn ensure_default_port_identity(node_home: &Path) -> Result<(), BridgeError> {
    match observe_port_readiness(node_home).await? {
        PortReadiness::Foreign(foreign) => Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            foreign.message(),
        )),
        PortReadiness::Occupied(port) => Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            occupied_port_message(port),
        )),
        PortReadiness::Ready(_)
        | PortReadiness::Booting
        | PortReadiness::Outdated { .. }
        | PortReadiness::NotListening => Ok(()),
    }
}

pub(super) fn ensure_native_owns_running_endpoint(
    port: &PortReadiness,
    native_job_loaded: bool,
) -> Result<(), BridgeError> {
    if matches!(
        port,
        PortReadiness::Ready(_) | PortReadiness::Booting | PortReadiness::Outdated { .. }
    ) && !native_job_loaded
    {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "The running local Node is not owned by the native Gents service. Stop it explicitly before using managed controls.",
        ));
    }
    Ok(())
}

pub(super) fn ensure_matching_identity(
    initialized_did: Option<&str>,
    live_did: &str,
    port: u16,
) -> Result<(), BridgeError> {
    if !matches!(initialized_did, Some(initialized) if !initialized.trim().is_empty() && !live_did.trim().is_empty() && initialized == live_did)
    {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            format!("port {port} does not advertise the initialized Gents identity"),
        ));
    }
    Ok(())
}
