pub(crate) mod fleet_slots;
pub(crate) mod healthz;
pub(crate) mod liveness;
pub(crate) mod prometheus;
pub(crate) mod router;
pub(crate) mod version;

pub(crate) use router::runtime_contract_router;
