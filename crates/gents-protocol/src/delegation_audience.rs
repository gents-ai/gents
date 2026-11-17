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

/// What reading a status does to the audience a client keeps with its record
/// of a home (`Identity.HeldAudience.change`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeldAudienceChange {
    Unchanged,
    Learned(DelegationAudience),
    Changed(DelegationAudience),
    Forgotten,
}

impl ReportedDelegationAudience {
    pub fn change_for(&self, held: Option<&DelegationAudience>) -> HeldAudienceChange {
        match (held, self) {
            (None, Self::Named(audience)) => HeldAudienceChange::Learned(audience.clone()),
            (Some(kept), Self::Named(audience)) if kept == audience => {
                HeldAudienceChange::Unchanged
            }
            (Some(_), Self::Named(audience)) => HeldAudienceChange::Changed(audience.clone()),
            (Some(_), Self::Absent) => HeldAudienceChange::Forgotten,
            (None, Self::Absent | Self::Unavailable) | (Some(_), Self::Unavailable) => {
                HeldAudienceChange::Unchanged
            }
        }
    }
}

impl HeldAudienceChange {
    /// The record after the read (`Identity.HeldAudience.observe`).
    pub fn held_after(self, held: Option<DelegationAudience>) -> Option<DelegationAudience> {
        match self {
            Self::Unchanged => held,
            Self::Learned(audience) | Self::Changed(audience) => Some(audience),
            Self::Forgotten => None,
        }
    }
}

/// Why a node refused a client's request, as far as its credential goes
/// (`Identity.HeldAudience.Refusal`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialRefusal {
    DelegationRequired,
    DelegationRefused,
    Other,
}

/// Whether a request refused for its credential is sent again after reading
/// the status (`Identity.HeldAudience.resend`).
pub fn resend(resent: bool, refusal: CredentialRefusal, record: &HeldAudienceChange) -> bool {
    !resent
        && refusal != CredentialRefusal::Other
        && matches!(
            record,
            HeldAudienceChange::Learned(_) | HeldAudienceChange::Changed(_)
        )
}

#[cfg(test)]
mod conformance;
