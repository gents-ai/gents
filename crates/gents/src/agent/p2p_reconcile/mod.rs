//! Runtime-owned P2P pairing reconcile seam.

pub mod diff;
pub mod embedded_impl;
pub mod endpoint;
pub mod engine;
pub mod enrollment;
pub mod enrollment_reconcile;
pub mod enrollment_store;
pub mod error_class;
mod graphql_helpers;
pub mod intervals;
pub mod persona_requests;
pub mod policy;
pub mod profiles;
pub mod registry;
pub mod session_hydration;
pub mod session_hydration_reconcile;
pub mod templates;
pub mod trait_def;

pub use diff::{
    compute_owned_pairing_diff, compute_pairing_diff, owned_pairing_live_matches, DiffOp,
    PairingActual, PairingApplied, PairingDesired,
};
pub use embedded_impl::EmbeddedRemoteP2pAdmin;
pub use endpoint::{peer_endpoint_upsert_mutation, run_endpoint_heartbeat};
pub use engine::{
    merge_layered_desired, observe_owned_pairing_live_matches, reconcile_peer_tick,
    run_pairing_reconciler, teardown_owned_replicators_at_endpoint, update_applied_after_success,
    EnrollmentEndpointEntry, GraphqlPairingStateStore, LoadedPairingApplied, PairingStateStore,
    PairingTickOutcome, MAX_CONCURRENT_PEER_PREPARATIONS, PAIRING_SWEEP_INTERVAL,
};
pub use enrollment_reconcile::{
    enrollment_authority_channel, run_enrollment_reconciler, EnrollmentAuthorityHandle,
    EnrollmentAuthorityOwner, EnrollmentAuthorizationFence, PeerAdmissionAuthority,
};
pub use enrollment_store::{
    ActiveEnrollment, DeniedEnrollment, EnrollmentDecisionOutcome, EnrollmentProjection,
    GraphqlEnrollmentStore, PendingEnrollment,
};
pub use error_class::{classify_remote_admin_error, PairingErrorClass};
pub use persona_requests::{
    reconcile_persona_tick, run_persona_request_reconciler, GraphqlPersonaRequestStore,
    PersonaRequestStore, PersonaTickOutcome,
};
pub use policy::{
    client_route_collections, client_route_direction, client_route_id, desired_route_is_applied,
    resolve_template_filters, ClientRouteIdentity, PairingDirection, TransportEndpoint,
    CLIENT_TO_RUNTIME_SUFFIX, RUNTIME_TO_CLIENT_SUFFIX,
};
pub use profiles::{expand_p2p_collection_profile_ids, P2pCollectionProfile};
pub use registry::{
    heartbeat_is_fresh, registry_upsert_mutation, resolve_network_id, run_registry_heartbeat,
    RegistryEntry, UpsertKind, DEFAULT_NETWORK_ID, NETWORK_ID_ENV, REGISTRY_HEARTBEAT_INTERVAL,
    REGISTRY_STALE_AFTER,
};
pub use session_hydration_reconcile::run_session_hydration_reconciler;
pub use templates::{
    builtin_templates, combine_filters, decode_pairing_filters, equality_filter, filter_conditions,
    resolve_template, scope_filter, single_string_eq, to_replication_filters, Delivery, DidSource,
    FilterPredicate, PairingFilters, Scope, ScopeTemplate, AGENT_DIRECTORY_COLLECTION,
    CLIENT_COLLECTIONS, CLIENT_TEMPLATE, CLIENT_TO_RUNTIME_COLLECTIONS,
};
pub use trait_def::{
    RemoteP2pAdmin, RemoteP2pAdminError, RemoteP2pAdminResult, RemoteP2pDocument, RemoteReplicator,
};
