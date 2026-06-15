//! Classify `RemoteP2pAdminError` for per-peer pairing retry status.

use serde::{Deserialize, Serialize};

use super::trait_def::RemoteP2pAdminError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairingErrorClass {
    RpcTimeout,
    RpcError,
    RemoteNotFound,
    RemoteUnauthorized,
    LocalError,
}

pub fn classify_remote_admin_error(err: &RemoteP2pAdminError) -> PairingErrorClass {
    match err {
        RemoteP2pAdminError::RpcTimeout => PairingErrorClass::RpcTimeout,
        RemoteP2pAdminError::RpcError(_) => PairingErrorClass::RpcError,
        RemoteP2pAdminError::RemoteNotFound(_) => PairingErrorClass::RemoteNotFound,
        RemoteP2pAdminError::RemoteUnauthorized => PairingErrorClass::RemoteUnauthorized,
        RemoteP2pAdminError::LocalError(_) => PairingErrorClass::LocalError,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_variant_maps_distinctly() {
        let pairs = [
            (
                RemoteP2pAdminError::RpcTimeout,
                PairingErrorClass::RpcTimeout,
            ),
            (
                RemoteP2pAdminError::RpcError("x".into()),
                PairingErrorClass::RpcError,
            ),
            (
                RemoteP2pAdminError::RemoteNotFound("c".into()),
                PairingErrorClass::RemoteNotFound,
            ),
            (
                RemoteP2pAdminError::RemoteUnauthorized,
                PairingErrorClass::RemoteUnauthorized,
            ),
            (
                RemoteP2pAdminError::LocalError("y".into()),
                PairingErrorClass::LocalError,
            ),
        ];
        for (err, class) in pairs {
            assert_eq!(classify_remote_admin_error(&err), class);
        }
    }
}
