use std::time::Duration;

pub use gents_protocol::owed_settlement::OwedStanding;

use super::user_write::Verdict;

mod access;
mod client;
mod pass;

pub use access::{
    dependent_step, observed_grant, serve_await, DependentStep, GrantWait, OwedAccessTicket,
    OwnGrantError, OWN_GRANT_DEADLINE,
};

pub use client::{
    settle_owed_while_online, spawn_settlement, EndpointPass, OwedPass, OwedSettlementFailed,
};
pub use pass::{settle_pass, OwedPassError};

/// `Identity.OwedRegistration.retrySeconds`.
pub const OWED_RETRY: Duration = Duration::from_secs(30);

/// `Identity.OwedRegistration.refreshAfterSeconds`.
pub const DELEGATION_REFRESH_AFTER: Duration = Duration::from_secs(60);

/// What a person reads when a change of theirs was not kept on the home.
pub const OWED_FAILED_FOR_A_PERSON: &str =
    "A recent change could not be saved on this home. Make the change again.";

/// Where DefraDB records an owed entry (`Identity.OwedRegistration.Recorded`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorded {
    Absent,
    Staged,
    AwaitingAuthority,
    Refused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handling {
    HandIn,
    Leave,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryStep {
    Done,
    AskAgain,
    Report,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    Adopted,
    Refused,
}

/// `Identity.OwedRegistration.standing`.
pub fn standing(recorded: Recorded, sendable: bool, held: bool, discarded: bool) -> OwedStanding {
    match recorded {
        Recorded::Absent => OwedStanding::Settled,
        Recorded::Refused => OwedStanding::Failed,
        Recorded::Staged | Recorded::AwaitingAuthority if discarded => OwedStanding::Failed,
        Recorded::Staged | Recorded::AwaitingAuthority if sendable => OwedStanding::Sending,
        Recorded::Staged | Recorded::AwaitingAuthority if held => OwedStanding::HeldBehind,
        Recorded::AwaitingAuthority => OwedStanding::AwaitingAuthority,
        Recorded::Staged => OwedStanding::Sending,
    }
}

/// `Identity.OwedRegistration.handling`.
pub fn handling(standing: OwedStanding) -> Handling {
    match standing {
        OwedStanding::AwaitingAuthority => Handling::HandIn,
        OwedStanding::Settled
        | OwedStanding::Sending
        | OwedStanding::HeldBehind
        | OwedStanding::Failed => Handling::Leave,
    }
}

/// `Identity.OwedRegistration.servesPass`.
pub fn serves_pass(presented: bool, verdict: Verdict) -> bool {
    presented && verdict == Verdict::IssuedBySigner
}

/// `Identity.OwedRegistration.entryStep`.
pub fn entry_step(standing: OwedStanding) -> EntryStep {
    match standing {
        OwedStanding::Settled => EntryStep::Done,
        OwedStanding::Failed => EntryStep::Report,
        OwedStanding::Sending | OwedStanding::HeldBehind | OwedStanding::AwaitingAuthority => {
            EntryStep::AskAgain
        }
    }
}

/// `Identity.OwedRegistration.continues`.
pub fn continues(online: bool, standings: &[OwedStanding]) -> bool {
    online
        && standings
            .iter()
            .any(|standing| entry_step(*standing) == EntryStep::AskAgain)
}

/// `Identity.OwedRegistration.adopt`.
pub fn adopt(general: bool, verdict: Verdict) -> Refresh {
    if general && verdict == Verdict::IssuedBySigner {
        Refresh::Adopted
    } else {
        Refresh::Refused
    }
}

/// `Identity.OwedRegistration.refreshDue`.
pub fn refresh_due(age: Duration) -> bool {
    age >= DELEGATION_REFRESH_AFTER
}
