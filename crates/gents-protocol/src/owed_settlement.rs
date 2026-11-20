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

/// Where a client reads the Node administration removals its home owes that
/// the caller may complete (Lean `Identity.EnrollmentGrant.settlementStep`).
pub const OWED_REMOVALS_PATH: &str = "/owed/removals";

/// One member whose Node administration removal waits for a client of the
/// Node owner or a Node administrator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwedRemoval {
    pub member_did: String,
    /// The member still holds the `administrator` relation.
    pub administrator_held: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwedRemovalsReport {
    pub removals: Vec<OwedRemoval>,
}

/// Where a client waits, after a commit, for the access changes its own
/// transaction owes (Lean `Identity.OwedRegistration.dependentStep`).
pub const OWED_AWAIT_PATH: &str = "/owed/await";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwedAwaitRequest {
    pub node_did: String,
    pub transaction: String,
}

/// What a committed write's own access changes came to
/// (`Identity.OwedRegistration.GrantWait`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantWait {
    Settled,
    Parked,
    Pending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwedAwaitReport {
    pub wait: GrantWait,
}
