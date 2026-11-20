use serde::{Deserialize, Serialize};

use super::user_write::{
    admit, Admission, Author, Carried, Carrier, Scope, UserWriteRefusal, Verdict, WriteKind,
};
use crate::local::document_acp::DocumentAcp;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ModelProvider {
    NodeStore,
    EmbeddedVera,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ModelAuthor {
    User,
    Node,
    Agent,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ModelCarrier {
    Direct,
    Stored,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ModelWrite {
    Create,
    Update,
    Delete,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ModelCarried {
    Absent,
    Bound,
    Lifted,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ModelScope {
    General,
    ThisWrite,
    AnotherWrite,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ModelVerdict {
    IssuedBySigner,
    IssuedByAnother,
    Refused,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ModelAdmission {
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

#[test]
fn the_admission_of_a_user_write_matches_the_lean_model() {
    gents_lean_vocab::families::identity_user_delegation_admit::replay::<
        ModelAdmission,
        ModelAuthor,
        ModelCarried,
        ModelCarrier,
        ModelProvider,
        ModelScope,
        ModelVerdict,
        ModelWrite,
    >(|input| {
        let admission = admit(
            match input.provider {
                ModelProvider::NodeStore => DocumentAcp::Local,
                ModelProvider::EmbeddedVera => DocumentAcp::Vera,
            },
            match input.author {
                ModelAuthor::User => Author::User,
                ModelAuthor::Node => Author::Node,
                ModelAuthor::Agent => Author::Agent,
            },
            match input.carrier {
                ModelCarrier::Direct => Carrier::Direct,
                ModelCarrier::Stored => Carrier::Stored,
            },
            match input.write {
                ModelWrite::Create => WriteKind::Create,
                ModelWrite::Update => WriteKind::Update,
                ModelWrite::Delete => WriteKind::Delete,
            },
            match input.carried {
                ModelCarried::Absent => Carried::Absent,
                ModelCarried::Bound => Carried::Bound,
                ModelCarried::Lifted => Carried::Lifted,
            },
            match input.scope {
                ModelScope::General => Scope::General,
                ModelScope::ThisWrite => Scope::ThisWrite,
                ModelScope::AnotherWrite => Scope::AnotherWrite,
            },
            match input.verdict {
                ModelVerdict::IssuedBySigner => Verdict::IssuedBySigner,
                ModelVerdict::IssuedByAnother => Verdict::IssuedByAnother,
                ModelVerdict::Refused => Verdict::Refused,
            },
        );
        match admission {
            Admission::Unchanged => ModelAdmission::Unchanged,
            Admission::Forwarded => ModelAdmission::Forwarded,
            Admission::ProviderDecides => ModelAdmission::ProviderDecides,
            Admission::Refused(refusal) => match refusal {
                UserWriteRefusal::DelegationRequired => ModelAdmission::DelegationRequired,
                UserWriteRefusal::DelegationNotBound => ModelAdmission::DelegationNotBound,
                UserWriteRefusal::DelegationNotRequestBound => {
                    ModelAdmission::DelegationNotRequestBound
                }
                UserWriteRefusal::DelegationForAnotherWrite => {
                    ModelAdmission::DelegationForAnotherWrite
                }
                UserWriteRefusal::RequestBoundTakesNoUpdate => {
                    ModelAdmission::RequestBoundTakesNoUpdate
                }
                UserWriteRefusal::DelegationRefused => ModelAdmission::DelegationRefused,
                UserWriteRefusal::IssuerNotSigner => ModelAdmission::IssuerNotSigner,
            },
        }
    });
}
