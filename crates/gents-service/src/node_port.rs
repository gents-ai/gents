use std::net::{Ipv4Addr, TcpListener};
use std::path::Path;

use crate::network_settings::NetworkSettings;

/// Who answers on the port a home would listen on (Lean `NodePort.Holder`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortOccupant {
    Nobody,
    ThisHome,
    AnotherNode,
    AnotherApp,
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
pub fn settle(recorded: bool, fresh: bool, occupant: PortOccupant) -> PortSettlement {
    let movable = fresh && !recorded;
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

/// The first candidate after `taken` that nothing listens on now.
pub fn free_port_after(taken: u16) -> Option<u16> {
    candidate_ports(taken).find(|port| TcpListener::bind((Ipv4Addr::LOCALHOST, *port)).is_ok())
}

#[derive(Debug, thiserror::Error)]
pub enum MoveError {
    #[error("port {taken} is in use and none of the next hundred ports is free")]
    NoFreePort { taken: u16 },
    #[error("recording the new HTTP port in the home's network settings")]
    Record(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Whether the home's network settings name an HTTP port.
pub fn port_recorded(home: &Path) -> Result<bool, MoveError> {
    NetworkSettings::read(home)
        .map(|settings| settings.http_port.is_some())
        .map_err(|error| MoveError::Record(error.into()))
}

/// Records a free port after `taken` as the home's HTTP port and returns it.
pub fn move_http_port(home: &Path, taken: u16) -> Result<u16, MoveError> {
    let port = free_port_after(taken).ok_or(MoveError::NoFreePort { taken })?;
    let settings = NetworkSettings::read(home).map_err(|error| MoveError::Record(error.into()))?;
    NetworkSettings {
        http_port: Some(port),
        ..settings
    }
    .persist(home)
    .map_err(|error| MoveError::Record(error.into()))?;
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_lean_vocab::families::{node_port_candidate_ports, node_port_settle};

    #[test]
    fn settlement_matches_lean() {
        node_port_settle::replay::<PortOccupant, PortSettlement>(|i| {
            settle(i.recorded, i.fresh, i.holder)
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

    #[test]
    fn a_move_records_a_free_port_and_keeps_other_settings() {
        let home = tempfile::tempdir().unwrap();
        NetworkSettings {
            p2p_port: Some(4455),
            ..NetworkSettings::default()
        }
        .persist(home.path())
        .unwrap();
        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let taken = held.local_addr().unwrap().port();

        let port = move_http_port(home.path(), taken).unwrap();

        assert_ne!(port, taken);
        let settings = NetworkSettings::read(home.path()).unwrap();
        assert_eq!(settings.http_port, Some(port));
        assert_eq!(settings.p2p_port, Some(4455));
        assert!(port_recorded(home.path()).unwrap());
    }
}
