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

impl UserWriteRefusal {
    /// Whether a refused write for a User, or a refused fresh delegation,
    /// is in `error`'s chain; a person is then told `REFUSED_FOR_A_PERSON`
    /// and the chain stays in the logs.
    pub fn in_chain(error: &(dyn std::error::Error + 'static)) -> bool {
        gents_protocol::error_chain::find_source::<Self>(error).is_some()
            || gents_protocol::error_chain::find_source::<RefreshRefused>(error).is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Unchanged,
    Forwarded,
    ProviderDecides,
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
            Admission::ProviderDecides
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
/// Clones share the delegation, so one a long-lived connection adopts
/// (`Identity.OwedRegistration.adopt`) is what its later writes carry.
#[derive(Clone)]
pub struct PresentedDelegation {
    caller: String,
    token: std::sync::Arc<std::sync::RwLock<String>>,
}

/// A delegation presented again on a connection that its caller did not
/// issue as a current general delegation for this node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "the delegation presented again is not a current delegation of the connection's own caller"
)]
pub struct RefreshRefused;

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
            token: std::sync::Arc::new(std::sync::RwLock::new(token)),
        })
    }

    pub fn caller(&self) -> &str {
        &self.caller
    }

    pub(crate) fn token(&self) -> String {
        self.token
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Adopt `authorization`, a bearer the connection's client presented
    /// again, for every clone of this delegation. External premise
    /// (`DocumentACP::caller_of_delegation`): DefraDB verifies a general
    /// delegation only, so a verified one is general.
    pub fn refresh(
        &self,
        node: &defra_node::Node,
        authorization: &str,
    ) -> Result<(), RefreshRefused> {
        let token = authorization
            .strip_prefix("Bearer ")
            .filter(|token| !token.is_empty())
            .ok_or(RefreshRefused)?;
        let verdict = verdict(node, &self.caller, token);
        match super::owed::adopt(true, verdict) {
            super::owed::Refresh::Adopted => {
                *self
                    .token
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = token.to_owned();
                Ok(())
            }
            super::owed::Refresh::Refused => Err(RefreshRefused),
        }
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
        Some(presented) => (Carried::Bound, verdict(node, signer, &presented.token())),
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
        (Admission::Unchanged | Admission::ProviderDecides, _) => {
            Ok(defra_node::SignerCredential::None)
        }
        (Admission::Forwarded, Some(presented)) => {
            Ok(defra_node::SignerCredential::Delegation(presented.token()))
        }
        (Admission::Forwarded, None) => Err(UserWriteRefusal::DelegationRequired),
        (Admission::Refused(refusal), _) => Err(refusal),
    }
}

/// A create a stored command carries for its User, with the request-bound
/// credentials it carries for the policy commands it sends.
pub struct StoredCreate {
    commands: super::StoredCreateCommands,
    credentials: Vec<String>,
}

impl StoredCreate {
    pub fn new(commands: super::StoredCreateCommands, credentials: Vec<String>) -> Self {
        Self {
            commands,
            credentials,
        }
    }

    /// The credential the relationship at `index` of the create's
    /// relationships is sent under, once the create was admitted forwarded.
    pub(crate) fn relationship_credential(&self, index: usize) -> Option<&str> {
        self.credentials.get(index + 1).map(String::as_str)
    }
}

/// The credential a stored create for `signer` carries to DefraDB, or the
/// refusal that stops it before anything is written
/// (`Identity.UserDelegation.admit`).
pub(crate) fn stored_create_credential(
    node: &defra_node::Node,
    signer: &str,
    create: &StoredCreate,
) -> Result<defra_node::SignerCredential, UserWriteRefusal> {
    let author = author_of(node, signer, false);
    let provider = crate::local::document_acp::provider_of(node);
    let (carried, scope, verdict) = match (provider, author) {
        (DocumentAcp::Vera, Author::User) => {
            create.commands.judge(node, signer, &create.credentials)
        }
        _ => (Carried::Absent, Scope::General, Verdict::Refused),
    };
    match admit(
        provider,
        author,
        Carrier::Stored,
        WriteKind::Create,
        carried,
        scope,
        verdict,
    ) {
        Admission::Unchanged | Admission::ProviderDecides => Ok(defra_node::SignerCredential::None),
        Admission::Forwarded => create
            .credentials
            .first()
            .map(|token| defra_node::SignerCredential::RequestBound(token.clone()))
            .ok_or(UserWriteRefusal::DelegationRequired),
        Admission::Refused(refusal) => Err(refusal),
    }
}

pub(crate) fn verdict(node: &defra_node::Node, signer: &str, token: &str) -> Verdict {
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
