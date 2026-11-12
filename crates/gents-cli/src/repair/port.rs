use crate::server_host::ServerConfig;
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

/// Whether the runtime that answered on the port is the node the home
/// initialized: both ids must be non-empty and equal.
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
    match gents_desktop_core::local_runtime::fetch_runtime_connection_payload(&config.status_url())
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortHolder {
    pub pid: u32,
    pub program: String,
}

/// Parses `lsof -F pc` output: `p<pid>` and `c<command>` field lines.
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(port: u16) -> ServerConfig {
        let mut config = ServerConfig::standard(std::env::temp_dir());
        config.http_port = port;
        config
    }

    #[test]
    fn the_same_node_id_matches() {
        assert_eq!(
            served_identity(Some("did:a"), &json!({"node_did":"did:a"})),
            ServedIdentity::Matches
        );
    }

    #[test]
    fn a_home_without_an_id_never_matches() {
        assert!(matches!(
            served_identity(None, &json!({"node_did":"did:a","home":"/h"})),
            ServedIdentity::Different {
                node_did: Some(_),
                home: Some(_)
            }
        ));
    }

    #[test]
    fn an_empty_live_id_is_different() {
        assert!(matches!(
            served_identity(Some("did:a"), &json!({"node_did":" "})),
            ServedIdentity::Different { node_did: None, .. }
        ));
    }

    #[test]
    fn lsof_fields_name_the_listener() {
        assert_eq!(
            parse_lsof_listener("p4312\ncDocker\n"),
            Some(PortHolder {
                pid: 4312,
                program: "Docker".into()
            })
        );
        assert_eq!(parse_lsof_listener(""), None);
    }

    #[tokio::test]
    async fn nothing_on_the_port_is_not_listening() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(matches!(
            observe_port(&config(port), None).await,
            PortObservation::NotListening
        ));
    }

    #[tokio::test]
    async fn a_silent_listener_is_not_gents() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(matches!(
            observe_port(&config(port), None).await,
            PortObservation::NotGents
        ));
        drop(listener);
    }
}
