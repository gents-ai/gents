use serde::{Deserialize, Serialize};

use super::{
    clock_moved, general_lifetime, issue_at, resend, CredentialAnswer, CredentialRefusal,
    DelegationAudience, HeldAudienceChange, HomeClock, PresentedCredential,
    ReportedDelegationAudience,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Held {
    Unknown,
    Known,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Reported {
    Absent,
    Named,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Change {
    Unchanged,
    Learned,
    Changed,
    Reclocked,
    Forgotten,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Refusal {
    DelegationRequired,
    DelegationRefused,
    OutsideWindow,
    Other,
}

fn audience(deployment: u64) -> DelegationAudience {
    DelegationAudience {
        worker: "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".into(),
        deployment,
        genesis_id: None,
    }
}

fn token(change: &HeldAudienceChange) -> Change {
    match change {
        HeldAudienceChange::Unchanged => Change::Unchanged,
        HeldAudienceChange::Learned(_) => Change::Learned,
        HeldAudienceChange::Changed(_) => Change::Changed,
        HeldAudienceChange::Reclocked => Change::Reclocked,
        HeldAudienceChange::Forgotten => Change::Forgotten,
    }
}

#[test]
fn a_status_read_changes_the_held_audience_as_the_lean_model_does() {
    gents_lean_vocab::families::identity_held_audience_change::replay::<Change, Held, Reported>(
        |input| {
            let kept = audience(1);
            let held = match input.held {
                Held::Unknown => None,
                Held::Known => Some(kept.clone()),
            };
            let named = if input.same_audience {
                kept
            } else {
                audience(2)
            };
            let reported = match input.reported {
                Reported::Absent => ReportedDelegationAudience::Absent,
                Reported::Named => ReportedDelegationAudience::Named(named),
                Reported::Unavailable => ReportedDelegationAudience::Unavailable,
            };
            let change = reported.change_with_clock(held.as_ref(), input.clock_moved);
            let after = change.clone().held_after(held.clone());
            assert_eq!(
                after == held,
                matches!(token(&change), Change::Unchanged | Change::Reclocked),
                "the audience held changes exactly when the change names another"
            );
            token(&change)
        },
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RecoveryOut {
    Resend,
    Tell,
    Unauthenticated,
}

#[test]
fn a_refused_request_recovers_as_the_lean_model_says() {
    gents_lean_vocab::families::identity_held_audience_recovery::replay::<
        Change,
        RecoveryOut,
        Refusal,
    >(|input| {
        let refusal = match input.refusal {
            Refusal::DelegationRequired => CredentialRefusal::DelegationRequired,
            Refusal::DelegationRefused => CredentialRefusal::DelegationRefused,
            Refusal::OutsideWindow => CredentialRefusal::OutsideWindow,
            Refusal::Other => CredentialRefusal::Other,
        };
        let record = match input.record {
            Change::Unchanged => HeldAudienceChange::Unchanged,
            Change::Learned => HeldAudienceChange::Learned(audience(1)),
            Change::Changed => HeldAudienceChange::Changed(audience(2)),
            Change::Reclocked => HeldAudienceChange::Reclocked,
            Change::Forgotten => HeldAudienceChange::Forgotten,
        };
        match super::recovery(input.resent, refusal, input.authenticated, &record) {
            super::Recovery::Resend => RecoveryOut::Resend,
            super::Recovery::Tell => RecoveryOut::Tell,
            super::Recovery::Unauthenticated => RecoveryOut::Unauthenticated,
        }
    });
}

#[test]
fn a_client_reads_the_status_again_before_a_request_bound_mint_as_the_lean_model_says() {
    gents_lean_vocab::families::identity_held_audience_reread_before_request_bound::replay::<Held>(
        |input| {
            let named = DelegationAudience {
                genesis_id: input
                    .genesis_known
                    .then(|| format!("0x{}", "ab".repeat(32))),
                ..audience(1)
            };
            let held = match input.held {
                Held::Unknown => None,
                Held::Known => Some(named),
            };
            super::reread_before_request_bound(held.as_ref(), input.clock_known)
        },
    );
}

#[test]
fn a_refused_request_is_sent_again_as_the_lean_model_says() {
    gents_lean_vocab::families::identity_held_audience_resend::replay::<Change, Refusal>(|input| {
        let refusal = match input.refusal {
            Refusal::DelegationRequired => CredentialRefusal::DelegationRequired,
            Refusal::DelegationRefused => CredentialRefusal::DelegationRefused,
            Refusal::OutsideWindow => CredentialRefusal::OutsideWindow,
            Refusal::Other => CredentialRefusal::Other,
        };
        let record = match input.record {
            Change::Unchanged => HeldAudienceChange::Unchanged,
            Change::Learned => HeldAudienceChange::Learned(audience(1)),
            Change::Changed => HeldAudienceChange::Changed(audience(2)),
            Change::Reclocked => HeldAudienceChange::Reclocked,
            Change::Forgotten => HeldAudienceChange::Forgotten,
        };
        resend(input.resent, refusal, &record)
    });
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Answer {
    Unauthorized,
    Forbidden,
    OutsideWindow,
    Other,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Credential {
    Anonymous,
    CallerBearer,
    SignerRequired,
    IdentityToken,
    Delegation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RefusalOut {
    DelegationRequired,
    DelegationRefused,
    OutsideWindow,
    Other,
}

#[test]
fn a_refused_request_is_read_as_the_lean_model_reads_it() {
    gents_lean_vocab::families::identity_held_audience_refusal_of::replay::<
        Answer,
        Credential,
        RefusalOut,
    >(|input| {
        let answer = match input.answer {
            Answer::Unauthorized => CredentialAnswer::of_status(401),
            Answer::Forbidden => CredentialAnswer::of_status(403),
            Answer::OutsideWindow => CredentialAnswer::OutsideWindow,
            Answer::Other => CredentialAnswer::of_status(500),
        };
        let presented = match input.presented {
            Credential::IdentityToken => PresentedCredential::IdentityToken,
            Credential::Delegation => PresentedCredential::Delegation,
            Credential::Anonymous | Credential::CallerBearer | Credential::SignerRequired => {
                PresentedCredential::None
            }
        };
        match CredentialRefusal::of(answer, presented) {
            CredentialRefusal::DelegationRequired => RefusalOut::DelegationRequired,
            CredentialRefusal::DelegationRefused => RefusalOut::DelegationRefused,
            CredentialRefusal::OutsideWindow => RefusalOut::OutsideWindow,
            CredentialRefusal::Other => RefusalOut::Other,
        }
    });
}

#[test]
fn a_client_mints_at_the_home_clock_as_the_lean_model_says() {
    gents_lean_vocab::families::identity_user_delegation_mint_times::replay(|input| {
        let home = input.provider_at_read.map(|provider_at_read| HomeClock {
            provider_at_read,
            clock_at_read: input.clock_at_read,
        });
        gents_lean_vocab::families::identity_user_delegation_mint_times::MintTimeOutput {
            issue_at: issue_at(home.as_ref(), input.clock),
            general_lifetime: general_lifetime(home.as_ref(), input.clock),
        }
    });
}

#[test]
fn a_home_clock_moves_as_the_lean_model_says() {
    gents_lean_vocab::families::identity_held_audience_clock_moves::replay(|input| {
        let clock = |reading: &Option<
            gents_lean_vocab::families::identity_held_audience_clock_moves::ClockReading,
        >| {
            reading.as_ref().map(|reading| HomeClock {
                provider_at_read: reading.provider_at_read,
                clock_at_read: reading.clock_at_read,
            })
        };
        clock_moved(clock(&input.held).as_ref(), clock(&input.read).as_ref())
    });
}

fn record(known: bool, deployment: u64, provider_at_read: u64) -> super::HeldRecord {
    super::HeldRecord {
        audience: known.then(|| audience(deployment)),
        clock: known.then_some(HomeClock {
            provider_at_read,
            clock_at_read: 900,
        }),
    }
}

#[test]
fn a_record_changes_since_a_refused_credential_as_the_lean_model_says() {
    gents_lean_vocab::families::identity_held_audience_record_change::replay::<Change, Held>(
        |input| {
            let before = record(matches!(input.before, Held::Known), 1, 1_000);
            let after = record(
                matches!(input.after, Held::Known),
                if input.same_audience { 1 } else { 2 },
                if input.same_clock { 1_000 } else { 2_000 },
            );
            token(&super::change_since(&before, &after))
        },
    );
}

#[test]
fn a_kept_record_changes_since_the_presented_one_as_the_lean_model_says() {
    use gents_lean_vocab::families::identity_held_audience_changes_since as family;
    family::replay::<Change>(|input| {
        let record = |case: &Option<family::RecordCase>| match case {
            None => super::HeldRecord::default(),
            Some(case) => super::HeldRecord {
                audience: Some(audience(case.deployment)),
                clock: case.clock.as_ref().map(|reading| HomeClock {
                    provider_at_read: reading.provider_at_read,
                    clock_at_read: reading.clock_at_read,
                }),
            },
        };
        token(&super::change_since(
            &record(&input.presented),
            &record(&input.kept),
        ))
    });
}
