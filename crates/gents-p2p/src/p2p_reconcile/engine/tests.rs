use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::anyhow;
use defra_node::Node as EmbeddedNode;
use events::Bus;
use tokio_util::sync::CancellationToken;

use super::reconcile::reconcile_peer_tick_with_replay;
use super::remote_topology::{canonical_replicator_address, read_actual};
use super::rows::{graphql_nullable_string_array, AppliedStateRow};
use super::sweep::{
    run_pairing_reconciler_loop, sweep_pairings, sweep_pairings_logged_until_cancelled,
};
use super::*;
use crate::p2p_reconcile::templates::PairingFilters;
use crate::p2p_reconcile::{
    to_replication_filters, RemoteP2pAdmin, RemoteP2pAdminError, RemoteP2pAdminResult,
    RemoteReplicator,
};
use gents_store::graphql::rows;

mod data_plane;

include!("tests/support.rs");
include!("tests/admin.rs");
include!("tests/enrollment_routes.rs");
include!("tests/sweep.rs");
include!("tests/reconnect.rs");
include!("tests/topology.rs");
include!("tests/applied_state.rs");
include!("tests/convergence.rs");
include!("tests/tick.rs");
include!("tests/overlay_acp.rs");
