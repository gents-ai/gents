use serde::{Deserialize, Serialize};

/// Where a client asks its runtime to settle what its own User's writes
/// still owe the home's document access control.
pub const OWED_SETTLEMENT_PATH: &str = "/owed/settle";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwedSettlementRequest {
    pub node_did: String,
}

/// What a client observes of one entry its User owes
/// (`Identity.OwedRegistration.Standing`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwedStanding {
    Settled,
    Sending,
    HeldBehind,
    AwaitingAuthority,
    Failed,
}

/// The standing of each entry of the caller's own after a settlement pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwedSettlementReport {
    pub standings: Vec<OwedStanding>,
}
