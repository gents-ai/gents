mod context;
mod control_watcher;
mod router;
mod startup;

pub(super) use context::StartupBarrier;
pub(in crate::agent) use router::default_hostname;
pub(super) use startup::run_agent;

#[cfg(test)]
use crate::request::watcher::Watcher;
#[cfg(test)]
use control_watcher::{
    run_control_watcher_with_timing, subscribe_control_changes, ControlWatcherInputs,
    ControlWatcherTiming,
};
#[cfg(test)]
use router::{resolve_agent_for_request, wait_for_next_request_with_latest_snapshot};
#[cfg(test)]
pub(in crate::agent) use router::{run_router_with_watcher, RouterInputs, RuntimeAdmissionGate};
#[cfg(test)]
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use tokio::sync::{mpsc, watch};

#[cfg(test)]
mod tests;
