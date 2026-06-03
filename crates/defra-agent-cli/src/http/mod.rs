pub(crate) mod fleet;
pub(crate) mod fleet_slots;
pub(crate) mod healthz;
pub(crate) mod identity_decide;
pub(crate) mod liveness;
pub(crate) mod mcp_server;
pub(crate) mod prometheus;
pub(crate) mod r5_dispatch;
pub(crate) mod router;
pub(crate) mod self_view;
pub(crate) mod subagent_tree;
pub(crate) mod version;

pub(crate) use router::runtime_contract_router;
