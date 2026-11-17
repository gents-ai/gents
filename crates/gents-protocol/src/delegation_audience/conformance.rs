use serde::{Deserialize, Serialize};

use super::{
    resend, CredentialRefusal, DelegationAudience, HeldAudienceChange, ReportedDelegationAudience,
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
    Forgotten,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Refusal {
    DelegationRequired,
    DelegationRefused,
    Other,
}

fn audience(deployment: u64) -> DelegationAudience {
    DelegationAudience {
        worker: "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".into(),
        deployment,
    }
}

fn token(change: &HeldAudienceChange) -> Change {
    match change {
        HeldAudienceChange::Unchanged => Change::Unchanged,
        HeldAudienceChange::Learned(_) => Change::Learned,
        HeldAudienceChange::Changed(_) => Change::Changed,
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
            let change = reported.change_for(held.as_ref());
            let after = change.clone().held_after(held.clone());
            assert_eq!(
                after == held,
                token(&change) == Change::Unchanged,
                "the record changes exactly when the change says so"
            );
            token(&change)
        },
    );
}

#[test]
fn a_refused_request_is_sent_again_as_the_lean_model_says() {
    gents_lean_vocab::families::identity_held_audience_resend::replay::<Change, Refusal>(|input| {
        let refusal = match input.refusal {
            Refusal::DelegationRequired => CredentialRefusal::DelegationRequired,
            Refusal::DelegationRefused => CredentialRefusal::DelegationRefused,
            Refusal::Other => CredentialRefusal::Other,
        };
        let record = match input.record {
            Change::Unchanged => HeldAudienceChange::Unchanged,
            Change::Learned => HeldAudienceChange::Learned(audience(1)),
            Change::Changed => HeldAudienceChange::Changed(audience(2)),
            Change::Forgotten => HeldAudienceChange::Forgotten,
        };
        resend(input.resent, refusal, &record)
    });
}
