use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const STATUS_DELEGATION_AUDIENCE_FIELD: &str = "delegation_audience";

/// The status field naming the provider's time, in Unix seconds, that a
/// client mints its delegations at (`Identity.UserDelegation.issueAt`).
pub const STATUS_PROVIDER_TIME_FIELD: &str = "provider_time";
pub const STATUS_NODE_DID_FIELD: &str = "node_did";

/// External premise (defradb.rs#3006): the code DefraDB answers a verified
/// write with when the delegation it carries does not hold at the provider's
/// time.
pub const DELEGATION_OUTSIDE_WINDOW_CODE: &str = "DELEGATION_OUTSIDE_WINDOW";

/// `Identity.UserDelegation.backdateSeconds`.
pub const CLOCK_SKEW_ALLOWANCE_SECS: u64 = 30;

/// `Identity.UserDelegation.lifetimeSeconds`.
pub const DELEGATION_LIFETIME_SECS: u64 = 120;

/// What a client read of its home's clock (`Identity.UserDelegation.HomeClock`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HomeClock {
    pub provider_at_read: u64,
    pub clock_at_read: u64,
}

/// The provider's time a client issues at, on its own `clock`
/// (`Identity.UserDelegation.issueAt`).
pub fn issue_at(home: Option<&HomeClock>, clock: u64) -> u64 {
    match home {
        None => clock,
        Some(home) => home
            .provider_at_read
            .saturating_add(clock.saturating_sub(home.clock_at_read)),
    }
}

/// How long a general delegation minted on `clock` lasts
/// (`Identity.UserDelegation.generalLifetime`).
pub fn general_lifetime(home: Option<&HomeClock>, clock: u64) -> u64 {
    DELEGATION_LIFETIME_SECS.saturating_add(issue_at(home, clock).saturating_sub(clock))
}

/// Whether a clock read again moved from the held one
/// (`Identity.HeldAudience.clockMoved`).
pub fn clock_moved(held: Option<&HomeClock>, read: Option<&HomeClock>) -> bool {
    match (held, read) {
        (None, Some(_)) => true,
        (Some(kept), Some(now)) => {
            let expected = issue_at(Some(kept), now.clock_at_read);
            expected.saturating_add(CLOCK_SKEW_ALLOWANCE_SECS) < now.provider_at_read
                || now
                    .provider_at_read
                    .saturating_add(CLOCK_SKEW_ALLOWANCE_SECS)
                    < expected
        }
        (_, None) => false,
    }
}

/// What a home's public status reports for a client to mint at: its
/// delegation audience and the provider's time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportedHome {
    pub audience: ReportedDelegationAudience,
    pub provider_time: Option<u64>,
}

impl ReportedHome {
    pub fn observe(status: &Value) -> Self {
        Self {
            audience: ReportedDelegationAudience::observe(status),
            provider_time: status
                .get(STATUS_PROVIDER_TIME_FIELD)
                .and_then(Value::as_u64),
        }
    }
}

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
    Reclocked,
    Forgotten,
}

impl ReportedDelegationAudience {
    pub fn change_for(&self, held: Option<&DelegationAudience>) -> HeldAudienceChange {
        self.change_with_clock(held, false)
    }

    /// `Identity.HeldAudience.change`, with whether the home's clock moved.
    pub fn change_with_clock(
        &self,
        held: Option<&DelegationAudience>,
        clock_moved: bool,
    ) -> HeldAudienceChange {
        match (held, self) {
            (None, Self::Named(audience)) => HeldAudienceChange::Learned(audience.clone()),
            (Some(kept), Self::Named(audience)) if kept == audience => {
                if clock_moved {
                    HeldAudienceChange::Reclocked
                } else {
                    HeldAudienceChange::Unchanged
                }
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
            Self::Unchanged | Self::Reclocked => held,
            Self::Learned(audience) | Self::Changed(audience) => Some(audience),
            Self::Forgotten => None,
        }
    }
}

/// Whether a client reads its home's status again before it mints a
/// request-bound delegation (`Identity.HeldAudience.rereadBeforeRequestBound`).
pub fn reread_before_request_bound(held: Option<&DelegationAudience>, clock_known: bool) -> bool {
    held.is_some_and(|audience| audience.genesis_id.is_none() || !clock_known)
}

/// Why a node refused a client's request, as far as its credential goes
/// (`Identity.HeldAudience.Refusal`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialRefusal {
    DelegationRequired,
    DelegationRefused,
    OutsideWindow,
    Other,
}

/// Whether a request refused for its credential is sent again after reading
/// the status (`Identity.HeldAudience.resend`).
pub fn resend(resent: bool, refusal: CredentialRefusal, record: &HeldAudienceChange) -> bool {
    !resent
        && refusal != CredentialRefusal::Other
        && (matches!(
            record,
            HeldAudienceChange::Learned(_)
                | HeldAudienceChange::Changed(_)
                | HeldAudienceChange::Reclocked
        ) || refusal == CredentialRefusal::OutsideWindow)
}

/// What becomes of a request refused for its credential once its home's
/// status was read again (`Identity.HeldAudience.Recovery`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    Resend,
    Tell,
    Unauthenticated,
}

/// `Identity.HeldAudience.recovery`: `authenticated` is whether the status
/// read again was the home's.
pub fn recovery(
    resent: bool,
    refusal: CredentialRefusal,
    authenticated: bool,
    record: &HeldAudienceChange,
) -> Recovery {
    if refusal == CredentialRefusal::Other {
        Recovery::Tell
    } else if !authenticated {
        Recovery::Unauthenticated
    } else if resend(resent, refusal, record) {
        Recovery::Resend
    } else {
        Recovery::Tell
    }
}

/// What a node answered a request with, as far as its credential goes
/// (`Identity.HeldAudience.Answer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialAnswer {
    Unauthorized,
    Forbidden,
    OutsideWindow,
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
            (CredentialAnswer::OutsideWindow, PresentedCredential::Delegation) => {
                Self::OutsideWindow
            }
            (CredentialAnswer::OutsideWindow, _) => Self::Other,
            (_, PresentedCredential::IdentityToken) => Self::DelegationRequired,
            (_, PresentedCredential::Delegation) => Self::DelegationRefused,
        }
    }
}

#[cfg(test)]
mod conformance;
