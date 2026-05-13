//! Transport-agnostic admin client for talking to a remote peer's P2P
//! management surface.
//!
//! This is a sibling to `defra_p2p_adapter::P2POperations`, which stays
//! local-only. `RemoteP2pAdmin` is the consumer-side trait used by pairing
//! reconcile; implementations dispatch over HTTP today and can dispatch over
//! a future DefraDB admin channel later.

pub mod diff;
pub mod error_class;
pub mod http_impl;
pub mod trait_def;

pub use diff::{compute_pairing_diff, DiffOp, PairingActual, PairingDesired};
pub use error_class::{classify_remote_admin_error, PairingErrorClass};
pub use http_impl::HttpRemoteP2pAdmin;
pub use trait_def::{
    RemoteP2pAdmin, RemoteP2pAdminError, RemoteP2pAdminResult, RemoteP2pDocument, RemoteReplicator,
};
