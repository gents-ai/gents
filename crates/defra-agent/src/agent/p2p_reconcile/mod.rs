//! Runtime-owned P2P pairing reconcile seam.

pub mod diff;
pub mod discovery;
pub mod embedded_impl;
pub mod engine;
pub mod error_class;
pub mod profiles;
pub mod registry;
pub mod trait_def;

pub use diff::{
    compute_owned_pairing_diff, compute_pairing_diff, DiffOp, PairingActual, PairingApplied,
    PairingDesired,
};
pub use discovery::{
    derive_registry_desired, reconcile_discovery_tick, run_discovery_reconciler, DiscoveredEntry,
    DiscoveryStore, DiscoveryTickOutcome, GraphqlDiscoveryStore, REGISTRY_STALE_AFTER,
    SOURCE_OPERATOR, SOURCE_REGISTRY,
};
pub use embedded_impl::EmbeddedRemoteP2pAdmin;
pub use engine::{
    reconcile_peer_tick, run_pairing_reconciler, update_applied_after_success,
    GraphqlPairingStateStore, PairingStateStore, PairingTickOutcome, PAIRING_SWEEP_INTERVAL,
};
pub use error_class::{classify_remote_admin_error, PairingErrorClass};
pub use profiles::{expand_p2p_collection_profile_ids, P2pCollectionProfile};
pub use registry::{
    registry_upsert_mutation, run_registry_heartbeat, RegistryEntry, REGISTRY_HEARTBEAT_INTERVAL,
};
pub use trait_def::{
    RemoteP2pAdmin, RemoteP2pAdminError, RemoteP2pAdminResult, RemoteP2pDocument, RemoteReplicator,
};
