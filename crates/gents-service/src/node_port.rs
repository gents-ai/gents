use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

use crate::network_settings::NetworkSettings;
use crate::server_host::ServerConfig;

/// Who answers on the port a home would listen on (Lean `NodePort.Holder`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortOccupant {
    Nobody,
    ThisHome,
    AnotherNode,
    AnotherApp,
}

/// Where the home's recorded port came from (Lean `NodePort.PortSource`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortSource {
    Unset,
    Picked,
    Chosen,
}

/// Lean `NodePort.Settlement`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortSettlement {
    Keep,
    Move,
    BlockedByNode,
    BlockedByApp,
}

/// Lean `NodePort.settle`.
pub fn settle(port: PortSource, served: bool, occupant: PortOccupant) -> PortSettlement {
    let movable = !served && port != PortSource::Chosen;
    match occupant {
        PortOccupant::Nobody | PortOccupant::ThisHome => PortSettlement::Keep,
        PortOccupant::AnotherNode if movable => PortSettlement::Move,
        PortOccupant::AnotherNode => PortSettlement::BlockedByNode,
        PortOccupant::AnotherApp if movable => PortSettlement::Move,
        PortOccupant::AnotherApp => PortSettlement::BlockedByApp,
    }
}

/// Lean `NodePort.candidates`.
pub fn candidate_ports(taken: u16) -> impl Iterator<Item = u16> {
    taken
        .checked_add(1)
        .into_iter()
        .flat_map(move |first| first..=taken.saturating_add(100))
}

const CONNECT_WAIT: Duration = Duration::from_millis(150);

/// The first listener that accepts a connection on `port` at `addr` or at
/// either loopback address. Blocks; run it off the async workers.
pub fn listener_answering(addr: IpAddr, port: u16) -> Option<SocketAddr> {
    [addr, Ipv4Addr::LOCALHOST.into(), Ipv6Addr::LOCALHOST.into()]
        .into_iter()
        .map(|ip| SocketAddr::new(ip, port))
        .find(|target| TcpStream::connect_timeout(target, CONNECT_WAIT).is_ok())
}

/// Binds without address reuse: with it, macOS lets a bind succeed beside
/// another program's wildcard listener.
fn binds_alone(addr: SocketAddr) -> bool {
    socket2::Socket::new(
        socket2::Domain::for_address(addr),
        socket2::Type::STREAM,
        None,
    )
    .and_then(|socket| socket.bind(&addr.into()))
    .is_ok()
}

/// Whether a server could listen on `port` at `addr` without sharing it.
pub fn port_is_free(addr: IpAddr, port: u16) -> bool {
    binds_alone(SocketAddr::new(addr, port)) && listener_answering(addr, port).is_none()
}

/// The first candidate after `taken` that is free at `addr` now. Blocks.
pub fn free_port_after(addr: IpAddr, taken: u16) -> Option<u16> {
    candidate_ports(taken).find(|port| port_is_free(addr, *port))
}

#[derive(Debug, thiserror::Error)]
pub enum MoveError {
    #[error("port {taken} is in use and none of the next hundred ports is free")]
    NoFreePort { taken: u16 },
    #[error("reading or recording the home's network settings")]
    Settings(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Where the home's network settings got their HTTP port.
pub fn port_source(home: &Path) -> Result<PortSource, MoveError> {
    let settings =
        NetworkSettings::read(home).map_err(|error| MoveError::Settings(error.into()))?;
    Ok(match (settings.http_port, settings.http_port_picked) {
        (None, _) => PortSource::Unset,
        (Some(_), true) => PortSource::Picked,
        (Some(_), false) => PortSource::Chosen,
    })
}

/// Records a free port after the server's as the home's picked HTTP port and
/// returns it. Blocks.
pub fn move_http_port(server: &ServerConfig) -> Result<u16, MoveError> {
    let taken = server.http_port;
    let port = free_port_after(server.http_addr, taken).ok_or(MoveError::NoFreePort { taken })?;
    let settings =
        NetworkSettings::read(&server.home).map_err(|error| MoveError::Settings(error.into()))?;
    NetworkSettings {
        http_port: Some(port),
        http_port_picked: true,
        ..settings
    }
    .persist(&server.home)
    .map_err(|error| MoveError::Settings(error.into()))?;
    Ok(port)
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;
    use gents_lean_vocab::families::{node_port_candidate_ports, node_port_settle};

    #[test]
    fn settlement_matches_lean() {
        node_port_settle::replay::<PortOccupant, PortSource, PortSettlement>(|i| {
            settle(i.port, i.served, i.holder)
        });
    }

    #[test]
    fn candidates_match_lean() {
        node_port_candidate_ports::replay(|i| {
            candidate_ports(u16::try_from(i.taken).expect("a TCP port"))
                .map(Into::into)
                .collect()
        });
    }

    fn server_at(home: &Path, port: u16) -> ServerConfig {
        ServerConfig {
            http_port: port,
            ..ServerConfig::standard(home.to_path_buf())
        }
    }

    #[test]
    fn a_move_records_a_picked_port_and_keeps_other_settings() {
        let home = tempfile::tempdir().unwrap();
        NetworkSettings {
            p2p_port: Some(4455),
            ..NetworkSettings::default()
        }
        .persist(home.path())
        .unwrap();
        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let taken = held.local_addr().unwrap().port();

        let port = move_http_port(&server_at(home.path(), taken)).unwrap();

        assert_ne!(port, taken);
        let settings = NetworkSettings::read(home.path()).unwrap();
        assert_eq!(settings.http_port, Some(port));
        assert_eq!(settings.p2p_port, Some(4455));
        assert_eq!(port_source(home.path()).unwrap(), PortSource::Picked);
    }

    #[test]
    fn a_port_a_person_set_is_chosen() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(port_source(home.path()).unwrap(), PortSource::Unset);
        NetworkSettings {
            http_port: Some(9393),
            ..NetworkSettings::default()
        }
        .persist(home.path())
        .unwrap();
        assert_eq!(port_source(home.path()).unwrap(), PortSource::Chosen);
    }

    /// A reusing loopback bind succeeds beside these on macOS, so the probe
    /// must not count them free.
    #[test]
    fn a_port_another_program_holds_on_a_wildcard_address_is_not_free() {
        for wildcard in [
            IpAddr::from(Ipv6Addr::UNSPECIFIED),
            Ipv4Addr::UNSPECIFIED.into(),
        ] {
            let held = TcpListener::bind((wildcard, 0)).unwrap();
            let port = held.local_addr().unwrap().port();
            assert!(
                !port_is_free(Ipv4Addr::LOCALHOST.into(), port),
                "{wildcard}:{port}"
            );
        }
    }
}
