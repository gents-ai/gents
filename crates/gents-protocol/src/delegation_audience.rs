use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const STATUS_DELEGATION_AUDIENCE_FIELD: &str = "delegation_audience";

/// The code the edge of a Vera home answers a User's identity token with
/// (`Identity.UserDelegation.edge`).
pub const DELEGATION_REQUIRED_CODE: &str = "delegationRequired";

/// The worker and deployment a Vera delegation must name for a node to act
/// under it, with the deployment's genesis identity once the node has proven
/// it (`Identity.UserDelegation.Audience`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationAudience {
    pub worker: String,
    pub deployment: u64,
    /// `0x` and 64 hexadecimal digits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genesis_id: Option<String>,
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

/// Whether a client reads its home's status again before it mints a
/// request-bound delegation (`Identity.HeldAudience.rereadForGenesis`).
pub fn reread_for_genesis(held: Option<&DelegationAudience>) -> bool {
    held.is_some_and(|audience| audience.genesis_id.is_none())
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

/// What a node answered a request with, as far as its credential goes
/// (`Identity.HeldAudience.Answer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialAnswer {
    Unauthorized,
    Forbidden,
    Other,
}

impl CredentialAnswer {
    pub fn of_status(status: u16) -> Self {
        match status {
            401 => Self::Unauthorized,
            403 => Self::Forbidden,
            _ => Self::Other,
        }
    }
}

/// The credential a request presented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentedCredential {
    None,
    IdentityToken,
    Delegation,
}

impl CredentialRefusal {
    /// `Identity.HeldAudience.refusalOf`.
    pub fn of(answer: CredentialAnswer, presented: PresentedCredential) -> Self {
        match (answer, presented) {
            (CredentialAnswer::Other, _) | (_, PresentedCredential::None) => Self::Other,
            (_, PresentedCredential::IdentityToken) => Self::DelegationRequired,
            (_, PresentedCredential::Delegation) => Self::DelegationRefused,
        }
    }
}

#[cfg(test)]
mod conformance;
