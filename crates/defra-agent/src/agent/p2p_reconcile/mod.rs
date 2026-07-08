//! Runtime-owned P2P pairing reconcile seam.

pub mod diff;
pub mod discovery;
pub mod embedded_impl;
pub mod endpoint;
pub mod engine;
pub mod error_class;
pub mod intervals;
pub mod network;
pub mod profiles;
pub mod reciprocal;
pub mod registry;
pub mod templates;
pub mod trait_def;

pub use diff::{
    DiffOp, PairingActual, PairingApplied, PairingDesired, compute_owned_pairing_diff,
    compute_pairing_diff,
};
pub use discovery::{
    DiscoveredEntry, DiscoveryStore, DiscoveryTickOutcome, GraphqlDiscoveryStore, JoinAdmission,
    REGISTRY_STALE_AFTER, RegistryMemberRow, SOURCE_OPERATOR, SOURCE_REGISTRY,
    decide_join_admission, derive_registry_desired, heartbeat_is_fresh, reconcile_discovery_tick,
    run_discovery_reconciler,
};
pub use embedded_impl::EmbeddedRemoteP2pAdmin;
pub use endpoint::{peer_endpoint_upsert_mutation, run_endpoint_heartbeat};
pub use engine::{
    GraphqlPairingStateStore, PAIRING_SWEEP_INTERVAL, PairingStateStore, PairingTickOutcome,
    merge_layered_desired, reconcile_peer_tick, run_pairing_reconciler,
    update_applied_after_success,
};
pub use error_class::{PairingErrorClass, classify_remote_admin_error};
pub use network::{
    GraphqlNetworkStore, NetworkEndpointEntry, NetworkStore, NetworkTickOutcome, SOURCE_NETWORK,
    derive_network_desired, endpoint_is_fresh, reconcile_network_tick, run_network_reconciler,
};
pub use profiles::{P2pCollectionProfile, expand_p2p_collection_profile_ids};
pub use reciprocal::{
    GraphqlReciprocalStore, ReciprocalStore, ReciprocalTickOutcome, derive_reciprocal_desired,
    reconcile_reciprocal_tick,
};
pub use registry::{
    DEFAULT_NETWORK_ID, NETWORK_ID_ENV, REGISTRY_HEARTBEAT_INTERVAL, RegistryEntry, UpsertKind,
    registry_upsert_mutation, resolve_network_id, run_registry_heartbeat,
};
pub use templates::{
    Delivery, FilterPredicate, PairingFilters, Scope, ScopeTemplate, builtin_templates,
    resolve_template, scope_filter,
};
pub use trait_def::{
    RemoteP2pAdmin, RemoteP2pAdminError, RemoteP2pAdminResult, RemoteP2pDocument, RemoteReplicator,
};
