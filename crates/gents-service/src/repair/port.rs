use crate::server_host::ServerConfig;
use gents_protocol::operations::install_repair::PortHolder;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServedIdentity {
    Matches,
    Different {
        node_did: Option<String>,
        home: Option<String>,
    },
}
pub fn served_identity(
    initialized_did: Option<&str>,
    payload: &serde_json::Value,
) -> ServedIdentity {
    let text = |key: &str| {
        payload
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let live = text("node_did");
    match (initialized_did, live) {
        (Some(initialized), Some(live))
            if !initialized.trim().is_empty() && initialized == live =>
        {
            ServedIdentity::Matches
        }
        _ => ServedIdentity::Different {
            node_did: live.map(str::to_string),
            home: text("home").map(str::to_string),
        },
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PortObservation {
    NotListening,
    NotGents,
    Gents {
        identity: ServedIdentity,
        payload: serde_json::Value,
    },
}

pub async fn observe_port(config: &ServerConfig, initialized_did: Option<&str>) -> PortObservation {
    match gents_desktop_core::local_runtime::fetch_runtime_connection_payload(
        &config.status_url(),
        None,
    )
    .await
    {
        Ok(payload) => PortObservation::Gents {
            identity: served_identity(initialized_did, &payload),
            payload,
        },
        Err(_) => {
            let address = std::net::SocketAddr::new(config.http_addr, config.http_port);
            let accepts = tokio::time::timeout(
                Duration::from_millis(250),
                tokio::net::TcpStream::connect(address),
            )
            .await
            .is_ok_and(|connected| connected.is_ok());
            if accepts {
                PortObservation::NotGents
            } else {
                PortObservation::NotListening
            }
        }
    }
}
pub fn parse_lsof_listener(output: &str) -> Option<PortHolder> {
    let mut pid = None;
    let mut program = None;
    for line in output.lines() {
        if let Some(value) = line.strip_prefix('p') {
            pid = pid.or_else(|| value.parse().ok());
        } else if let Some(value) = line.strip_prefix('c') {
            program = program.or_else(|| Some(value.to_string()));
        }
    }
    Some(PortHolder {
        pid: pid?,
        program: program?,
    })
}

#[cfg(target_os = "macos")]
pub async fn port_holder(port: u16) -> Option<PortHolder> {
    tokio::task::spawn_blocking(move || {
        let output = std::process::Command::new("/usr/sbin/lsof")
            .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fpc"])
            .output()
            .ok()?;
        parse_lsof_listener(&String::from_utf8_lossy(&output.stdout))
    })
    .await
    .ok()
    .flatten()
}

#[cfg(not(target_os = "macos"))]
pub async fn port_holder(_port: u16) -> Option<PortHolder> {
    None
}

pub async fn read_initialized_did(home: &Path) -> Option<String> {
    tokio::fs::read(home.join("init.json"))
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("node_did")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
}
