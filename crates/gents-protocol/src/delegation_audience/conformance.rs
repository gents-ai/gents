use serde::{Deserialize, Serialize};

use super::{
    resend, CredentialAnswer, CredentialRefusal, DelegationAudience, HeldAudienceChange,
    PresentedCredential, ReportedDelegationAudience,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Answer {
    Unauthorized,
    Forbidden,
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
    Other,
}

#[test]
fn a_refused_request_is_read_as_the_lean_model_reads_it() {
    gents_lean_vocab::families::identity_held_audience_refusal_of::replay::<
        Answer,
        Credential,
        RefusalOut,
    >(|input| {
        let answer = CredentialAnswer::of_status(match input.answer {
            Answer::Unauthorized => 401,
            Answer::Forbidden => 403,
            Answer::Other => 500,
        });
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
            CredentialRefusal::Other => RefusalOut::Other,
        }
    });
}
