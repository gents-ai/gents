use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const STATUS_DELEGATION_AUDIENCE_FIELD: &str = "delegation_audience";

/// The worker and deployment a Vera delegation must name for a node to act
/// under it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationAudience {
    pub worker: String,
    pub deployment: u64,
}

/// What a node's public status says of its delegation audience
/// (`Identity.UserDelegation.Reported`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportedDelegationAudience {
    Absent,
    Named(DelegationAudience),
    Unavailable,
}

impl ReportedDelegationAudience {
    pub fn observe(status: &Value) -> Self {
        match status.get(STATUS_DELEGATION_AUDIENCE_FIELD) {
            None => Self::Absent,
            Some(reported) => {
                serde_json::from_value(reported.clone()).map_or(Self::Unavailable, Self::Named)
            }
        }
    }

    /// The status field's value; a status without the field reports `Absent`.
    pub fn status_value(&self) -> Option<Value> {
        match self {
            Self::Absent => None,
            Self::Named(audience) => Some(serde_json::json!(audience)),
            Self::Unavailable => Some(Value::Null),
        }
    }
}
