use gents_store::config_client::user_write::UserWriteRefusal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Admission {
    Unchanged,
    Forwarded,
    DelegationRequired,
    DelegationNotBound,
    DelegationNotRequestBound,
    DelegationForAnotherWrite,
    RequestBoundTakesNoUpdate,
    ProviderDecides,
    DelegationRefused,
    IssuerNotSigner,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Pending,
    Rejected,
}

#[test]
fn a_refused_user_write_is_answered_as_the_lean_model_says() {
    gents_lean_vocab::families::identity_session_input_edit_answer::replay::<Admission, Outcome>(
        |input| {
            let admission = match input.admission {
                Admission::Unchanged | Admission::Forwarded | Admission::ProviderDecides => Ok(()),
                Admission::DelegationRequired => Err(UserWriteRefusal::DelegationRequired),
                Admission::DelegationNotBound => Err(UserWriteRefusal::DelegationNotBound),
                Admission::DelegationNotRequestBound => {
                    Err(UserWriteRefusal::DelegationNotRequestBound)
                }
                Admission::DelegationForAnotherWrite => {
                    Err(UserWriteRefusal::DelegationForAnotherWrite)
                }
                Admission::RequestBoundTakesNoUpdate => {
                    Err(UserWriteRefusal::RequestBoundTakesNoUpdate)
                }
                Admission::DelegationRefused => Err(UserWriteRefusal::DelegationRefused),
                Admission::IssuerNotSigner => Err(UserWriteRefusal::IssuerNotSigner),
            };
            match super::answer(admission) {
                None => Outcome::Pending,
                Some(reason) => {
                    assert_eq!(reason, gents_protocol::failure_text::REFUSED_FOR_A_PERSON);
                    Outcome::Rejected
                }
            }
        },
    );
}
