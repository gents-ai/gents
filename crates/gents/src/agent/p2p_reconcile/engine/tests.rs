use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use defra_node::EmbeddedNode;
use events::Bus;
use tokio_util::sync::CancellationToken;

use super::enrollment::{enrollment_base_row, local_data_plane_row, materialized_enrollment_entry};
use super::reconcile::reconcile_peer_tick_with_replay;
use super::remote_topology::{canonical_replicator_address, read_actual};
use super::rows::{graphql_nullable_string_array, AppliedStateRow};
use super::sweep::{
    run_pairing_reconciler_loop, sweep_pairings, sweep_pairings_logged_until_cancelled,
};
use super::*;
use crate::agent::p2p_reconcile::graphql_helpers::rows;
use crate::agent::p2p_reconcile::templates::PairingFilters;
use crate::agent::p2p_reconcile::{
    single_string_eq, to_replication_filters, RemoteP2pAdmin, RemoteP2pAdminError,
    RemoteP2pAdminResult, RemoteReplicator,
};

mod desired_state;
use desired_state::desired_row;

include!("tests/support.rs");
include!("tests/admin.rs");
include!("tests/desired_layers.rs");
include!("tests/enrollment_routes.rs");
include!("tests/sweep.rs");
include!("tests/reconnect.rs");
include!("tests/topology.rs");
include!("tests/applied_state.rs");
include!("tests/convergence.rs");
include!("tests/delivery.rs");
