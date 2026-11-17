use crate::local::document_acp::DocumentAcp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Author {
    User,
    Node,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carrier {
    Direct,
    Stored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteKind {
    Create,
    Update,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carried {
    Absent,
    Bound,
    Lifted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    General,
    ThisWrite,
    AnotherWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    IssuedBySigner,
    IssuedByAnother,
    Refused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UserWriteRefusal {
    #[error(
        "this home decides document access through Vera, where a write for a User runs only under \
         a Vera delegation that User's client minted for this runtime, and the write carried \
         none; mint one for the delegation audience the runtime's public status reports"
    )]
    DelegationRequired,
    #[error(
        "the Vera delegation this write carried is not the one its signed command names; sign \
         the command again with the delegation attached to it"
    )]
    DelegationNotBound,
    #[error(
        "a stored command carries a Vera delegation bound to its own write, and this one is \
         general; sign the command again with a request-bound delegation"
    )]
    DelegationNotRequestBound,
    #[error(
        "the Vera delegation this write carried is bound to another write; sign the command \
         again with a delegation for this one"
    )]
    DelegationForAnotherWrite,
    #[error(
        "a Vera delegation bound to one request covers only a create; an update or a delete \
         carries a general delegation"
    )]
    RequestBoundTakesNoUpdate,
    #[error(
        "this home decides document access through Vera, which does not yet admit an update or \
         a delete a User signed into a stored command; make the change directly from the client"
    )]
    StoredEditAwaitsVerifiedPermission,
    #[error(
        "the Vera delegation this write carried was refused: it is expired, not yet valid, or \
         minted for another runtime; mint a new one for the audience the runtime's public status \
         reports"
    )]
    DelegationRefused,
    #[error(
        "the Vera delegation this write carried was issued by another identity than its signer"
    )]
    IssuerNotSigner,
}

/// What a person reads where a refused write for them is answered; the
/// refusal itself stays in the logs.
pub const REFUSED_FOR_A_PERSON: &str =
    "This change could not be applied on this home. Update Gents on this device and try again.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Unchanged,
    Forwarded,
    Refused(UserWriteRefusal),
}

/// What the runtime does with a write it makes in its own process for the
/// author of a command it verified (`Identity.UserDelegation.admit`).
pub fn admit(
    provider: DocumentAcp,
    author: Author,
    carrier: Carrier,
    write: WriteKind,
    carried: Carried,
    scope: Scope,
    verdict: Verdict,
) -> Admission {
    match (provider, author) {
        (DocumentAcp::Local, _) | (DocumentAcp::Vera, Author::Node | Author::Agent) => {
            Admission::Unchanged
        }
        (DocumentAcp::Vera, Author::User)
            if carrier == Carrier::Stored && write != WriteKind::Create =>
        {
            Admission::Refused(UserWriteRefusal::StoredEditAwaitsVerifiedPermission)
        }
        (DocumentAcp::Vera, Author::User) => admit_carried(carrier, write, carried, scope, verdict),
    }
}

fn admit_carried(
    carrier: Carrier,
    write: WriteKind,
    carried: Carried,
    scope: Scope,
    verdict: Verdict,
) -> Admission {
    let refused = match (carried, scope) {
        (Carried::Absent, _) => UserWriteRefusal::DelegationRequired,
        (Carried::Lifted, _) => UserWriteRefusal::DelegationNotBound,
        (Carried::Bound, Scope::AnotherWrite) => UserWriteRefusal::DelegationForAnotherWrite,
        (Carried::Bound, Scope::General) if carrier == Carrier::Stored => {
            UserWriteRefusal::DelegationNotRequestBound
        }
        (Carried::Bound, Scope::ThisWrite) if write != WriteKind::Create => {
            UserWriteRefusal::RequestBoundTakesNoUpdate
        }
        (Carried::Bound, _) => match verdict {
            Verdict::IssuedBySigner => return Admission::Forwarded,
            Verdict::IssuedByAnother => UserWriteRefusal::IssuerNotSigner,
            Verdict::Refused => UserWriteRefusal::DelegationRefused,
        },
    };
    Admission::Refused(refused)
}

/// The Vera delegation a request served in the runtime's own process
/// presented as its bearer, for its authenticated caller.
#[derive(Clone)]
pub struct PresentedDelegation {
    caller: String,
    token: String,
}

impl std::fmt::Debug for PresentedDelegation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("PresentedDelegation")
            .field(&self.caller)
            .finish()
    }
}

impl PresentedDelegation {
    /// The bearer DefraDB authenticated `caller`'s request by, when that
    /// bearer is a Vera delegation; an identity token is none.
    pub fn of_request(
        node: &defra_node::Node,
        context: &defra_core::RequestContext,
        caller: &str,
    ) -> Option<Self> {
        let token = context.bearer_token_for(caller)?;
        let _verdict = node.document_acp().caller_of_delegation(&token)?;
        Some(Self {
            caller: caller.to_owned(),
            token,
        })
    }
}

/// The credential a write the runtime makes in its own process for `signer`
/// carries to DefraDB, or the refusal that stops it before anything is
/// written. `presented` is the delegation of the direct request the write
/// serves; a stored command carries none yet.
pub(crate) fn signer_credential(
    node: &defra_node::Node,
    author: Author,
    carrier: Carrier,
    write: WriteKind,
    signer: &str,
    presented: Option<&PresentedDelegation>,
) -> Result<defra_node::SignerCredential, UserWriteRefusal> {
    let presented = presented.filter(|presented| presented.caller == signer);
    let (carried, verdict) = match presented {
        None => (Carried::Absent, Verdict::Refused),
        Some(presented) => (Carried::Bound, verdict(node, signer, &presented.token)),
    };
    let admission = admit(
        crate::local::document_acp::provider_of(node),
        author,
        carrier,
        write,
        carried,
        Scope::General,
        verdict,
    );
    match (admission, presented) {
        (Admission::Unchanged, _) => Ok(defra_node::SignerCredential::None),
        (Admission::Forwarded, Some(presented)) => Ok(defra_node::SignerCredential::Delegation(
            presented.token.clone(),
        )),
        (Admission::Forwarded, None) => Err(UserWriteRefusal::DelegationRequired),
        (Admission::Refused(refusal), _) => Err(refusal),
    }
}

fn verdict(node: &defra_node::Node, signer: &str, token: &str) -> Verdict {
    let acp = node.document_acp();
    match acp.caller_of_delegation(token) {
        Some(Ok(caller))
            if identity::Did::new(signer).is_ok_and(|signer| {
                acp.canonical_actor(&caller.did) == acp.canonical_actor(&signer)
            }) =>
        {
            Verdict::IssuedBySigner
        }
        Some(Ok(_)) => Verdict::IssuedByAnother,
        Some(Err(_)) | None => Verdict::Refused,
    }
}

/// Who authors a write made in an embedded transaction acting as `actor`.
pub(crate) fn author_of(node: &defra_node::Node, actor: &str, managed_agent: bool) -> Author {
    if node.node_identity_did() == Some(actor) {
        Author::Node
    } else if managed_agent {
        Author::Agent
    } else {
        Author::User
    }
}
