use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingObservation {
    Desired,
    Applied,
}

impl PairingObservation {
    pub fn collection(self) -> &'static str {
        match self {
            Self::Desired => "PeerPairingDesired",
            Self::Applied => "PeerPairingApplied",
        }
    }
}

/// An explicit native grant for one physical pairing observation. Enabling a
/// tool or choosing an Agent does not itself publish or revoke this grant.
/// Either reason grants native read access to the complete document; the
/// relation records independent revocation reasons, not field-level access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PairingReadReason {
    RouteAdmission,
    ToolView,
}

impl PairingReadReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RouteAdmission => "route-admission",
            Self::ToolView => "tool-view",
        }
    }
}
