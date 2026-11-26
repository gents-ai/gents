mod context;
pub(in crate::runtime::agent) mod control_watcher;
mod router;
pub(in crate::runtime::agent) mod slot_tools;
mod startup;
mod surface_publication;

pub(super) use context::StartupBarrier;
pub(in crate::runtime::agent) use router::default_hostname;
pub(super) use startup::run_agent;

#[cfg(test)]
use control_watcher::{
    run_control_watcher_with_timing, subscribe_control_changes, ControlWatcherInputs,
    ControlWatcherTiming,
};
#[cfg(test)]
use router::resolve_agent_for_request;
#[cfg(test)]
pub(in crate::runtime::agent) use router::session_selection::UNRESOLVED_SESSION_ATTEMPT_BOUND;
#[cfg(test)]
pub(in crate::runtime::agent) use router::{
    run_router_with_watcher, RouterInputs, RuntimeAdmissionGate,
};
#[cfg(test)]
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use tokio::sync::{mpsc, watch};

#[cfg(test)]
mod tests;
