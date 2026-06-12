//! Runtime-owned P2P pairing reconcile seam.

pub mod diff;
pub mod error_class;
pub mod trait_def;

pub use diff::{compute_pairing_diff, DiffOp, PairingActual, PairingDesired};
pub use error_class::{classify_remote_admin_error, PairingErrorClass};
pub use trait_def::{
    RemoteP2pAdmin, RemoteP2pAdminError, RemoteP2pAdminResult, RemoteP2pDocument, RemoteReplicator,
};
