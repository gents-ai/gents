use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanSpawnFenceStep {
    pub(crate) action: String,
    pub(crate) enabled: bool,
    pub(crate) stale_host_view: bool,
    pub(crate) bridge: String,
    pub(crate) child: String,
    pub(crate) cancel_intent: bool,
    pub(crate) host_sees_intent: bool,
    pub(crate) interrupt_latched: bool,
    pub(crate) ack_pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanSpawnFenceCase {
    pub(crate) name: String,
    pub(crate) route: String,
    pub(crate) unclaimed_deadline_set: bool,
    pub(crate) single_node_replayable: bool,
    pub(crate) steps: Vec<LeanSpawnFenceStep>,
}
