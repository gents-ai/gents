use gents_protocol::request_admission::AgentRequestCreate;

use super::user_write::{Carried, Scope, Verdict};
use super::{ClientSigningCapability, ConfigClientError, DelegationError, RequestBoundAudience};

/// The relationships a User's request create writes on the request it
/// creates, as that User: what a stored create sends besides its registration.
pub(crate) fn request_relations(
    create: &AgentRequestCreate,
    session_doc: &str,
    agent_doc: &str,
) -> Result<[(&'static str, String); 3], ConfigClientError> {
    Ok([
        (
            "parent",
            super::relationships::resource_target("agents", agent_doc)?,
        ),
        (
            "session",
            super::relationships::resource_target("sessions", session_doc)?,
        ),
        ("working_agent", create.agent_did.clone()),
    ])
}

enum Subject {
    Actor(vera_delegation::SubjectRef),
    Object { resource: String, object_id: String },
}

enum Action {
    Register,
    Relate { relation: String, subject: Subject },
}

struct Command {
    policy_id: String,
    resource: String,
    object_id: String,
    action: Action,
}

impl Command {
    fn sent(&self) -> vera_delegation::PolicyCommand<'_> {
        let action = match &self.action {
            Action::Register => vera_delegation::PolicyAction::RegisterObject,
            Action::Relate { relation, subject } => {
                vera_delegation::PolicyAction::SetRelationship {
                    relation,
                    subject: match subject {
                        Subject::Actor(actor) => vera_delegation::PolicySubject::Actor(actor),
                        Subject::Object {
                            resource,
                            object_id,
                        } => vera_delegation::PolicySubject::Structured {
                            kind: OBJECT_SUBJECT,
                            resource,
                            object_id,
                            relation: "",
                        },
                    },
                }
            }
        };
        vera_delegation::PolicyCommand {
            policy_id: &self.policy_id,
            resource: &self.resource,
            object_id: &self.object_id,
            action,
        }
    }
}

/// External premise (`zanzibar::encode_subject`): an object named as a
/// relationship's subject is sent as kind 2.
const OBJECT_SUBJECT: u8 = 2;

/// The policy commands a User's request create sends for its User, in the
/// order its request-bound credentials name them
/// (`Identity.UserDelegation.storedCreateCommands`): the registration of
/// `doc_id`, then each of `request_relations`.
pub struct StoredCreateCommands(Vec<Command>);

impl StoredCreateCommands {
    pub fn of_request(
        create: &AgentRequestCreate,
        doc_id: &str,
        ids: &gents_protocol::schemas::HomePolicyIds,
    ) -> Result<Self, ConfigClientError> {
        let schema = super::client_signed_agent::signing_schema(
            gents_protocol::schemas::AGENT_REQUEST,
            "AgentRequest",
            ids,
        )?;
        let policy = schema
            .policy
            .ok_or(ConfigClientError::ClientConfigurationUpdateInvalid)?;
        let session_doc = create
            .session_doc_id
            .as_deref()
            .ok_or(ConfigClientError::ClientConfigurationUpdateInvalid)?;
        let agent_doc = create
            .agent_doc_id
            .as_deref()
            .ok_or(ConfigClientError::ClientConfigurationUpdateInvalid)?;
        let command = |action| Command {
            policy_id: policy.id.clone(),
            resource: policy.resource_name.clone(),
            object_id: doc_id.to_owned(),
            action,
        };
        let mut commands = vec![command(Action::Register)];
        for (relation, target) in request_relations(create, session_doc, agent_doc)? {
            let subject = match acp::parse_target_subject(&target) {
                Ok(acp::Subject::EntitySet {
                    resource,
                    object_id,
                    relation,
                }) if relation.is_empty() => Subject::Object {
                    resource,
                    object_id,
                },
                Ok(acp::Subject::Entity(did)) => {
                    Subject::Actor(vera_delegation::SubjectRef::Actor(did.to_string()))
                }
                _ => return Err(ConfigClientError::InvalidResourceRelationshipTarget),
            };
            commands.push(command(Action::Relate {
                relation: relation.to_owned(),
                subject,
            }));
        }
        Ok(Self(commands))
    }

    /// One credential for each command, bound to it, minted by
    /// `capability`'s User for `audience` at the one minting point.
    pub fn mint(
        &self,
        capability: &ClientSigningCapability,
        audience: &RequestBoundAudience,
    ) -> Result<Vec<String>, DelegationError> {
        self.mint_for(capability, audience, super::VERA_REQUEST_BOUND_LIFETIME)
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    #[doc(hidden)]
    pub fn mint_with_lifetime_for_test(
        &self,
        capability: &ClientSigningCapability,
        audience: &RequestBoundAudience,
        lifetime: std::time::Duration,
    ) -> Result<Vec<String>, DelegationError> {
        self.mint_for(capability, audience, lifetime)
    }

    fn mint_for(
        &self,
        capability: &ClientSigningCapability,
        audience: &RequestBoundAudience,
        lifetime: std::time::Duration,
    ) -> Result<Vec<String>, DelegationError> {
        self.0
            .iter()
            .map(|command| {
                super::delegation::mint_request_bound(
                    capability,
                    audience,
                    &command.sent(),
                    lifetime,
                )
            })
            .collect()
    }

    /// What `credentials` carried with a stored create for `signer` come to
    /// against these commands at `node`, now: the `carried`, `scope` and
    /// `verdict` of `Identity.UserDelegation.admit`. The credentials are
    /// covered by the command's verified signature, so attached ones are
    /// bound.
    pub(crate) fn judge(
        &self,
        node: &defra_node::Node,
        signer: &str,
        credentials: &[String],
    ) -> (Carried, Scope, Verdict) {
        if credentials.is_empty() {
            return (Carried::Absent, Scope::General, Verdict::Refused);
        }
        if credentials.len() != self.0.len() {
            return (Carried::Bound, Scope::AnotherWrite, Verdict::Refused);
        }
        let audience = crate::local::document_acp::node_delegation_audience(node)
            .and_then(|audience| super::delegation::provider_audience(&audience).ok());
        let acp = node.document_acp();
        let mut judged = (Carried::Bound, Scope::ThisWrite, Verdict::IssuedBySigner);
        for (command, token) in self.0.iter().zip(credentials) {
            let (scope, verdict) = judge_one(&command.sent(), token, audience.as_ref(), |issuer| {
                identity::Did::new(signer).is_ok_and(|signer| {
                    identity::Did::new(issuer).is_ok_and(|issuer| {
                        acp.canonical_actor(&issuer) == acp.canonical_actor(&signer)
                    })
                })
            });
            judged.2 = worse(judged.2, verdict);
            judged.1 = match (judged.1, scope) {
                (Scope::AnotherWrite, _) | (_, Scope::AnotherWrite) => Scope::AnotherWrite,
                (Scope::General, _) | (_, Scope::General) => Scope::General,
                (Scope::ThisWrite, Scope::ThisWrite) => Scope::ThisWrite,
            };
        }
        judged
    }
}

fn worse(left: Verdict, right: Verdict) -> Verdict {
    match (left, right) {
        (Verdict::Refused, _) | (_, Verdict::Refused) => Verdict::Refused,
        (Verdict::IssuedByAnother, _) | (_, Verdict::IssuedByAnother) => Verdict::IssuedByAnother,
        (Verdict::IssuedBySigner, Verdict::IssuedBySigner) => Verdict::IssuedBySigner,
    }
}

/// What one request-bound credential comes to for `command`.
/// External premise (`vera_delegation::bearer::request`): DefraDB sends
/// `command` under `token` only when the token verifies, names the provider's
/// worker and deployment, is current with an operation identity still open
/// and due within `DEFAULT_OPERATION_LEAD`, names the deployment's genesis
/// identity and the digest of `command`; it offers no way to ask before the
/// write, so the runtime asks the same of the token here and DefraDB asks
/// again when it sends.
fn judge_one<F>(
    command: &vera_delegation::PolicyCommand<'_>,
    token: &str,
    audience: Option<&acp::DelegationAudience>,
    issued_by_signer: F,
) -> (Scope, Verdict)
where
    F: FnOnce(&str) -> bool,
{
    let Ok(claims) = vera_crypto::jwt::verify_bearer_token(token) else {
        return (Scope::ThisWrite, Verdict::Refused);
    };
    let Some(request) = claims.request.as_ref() else {
        return (Scope::General, Verdict::Refused);
    };
    let Ok(digest) = vera_delegation::request_digest(command) else {
        return (Scope::AnotherWrite, Verdict::Refused);
    };
    if request.digest != digest {
        return (Scope::AnotherWrite, Verdict::Refused);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let current = audience.is_some_and(|audience| {
        claims.relay.is_none()
            && claims.scope == vera_crypto::jwt::DelegationScope::PolicyCommands
            && claims
                .authorize(&audience.worker, audience.deployment, now)
                .is_ok()
            && request
                .id
                .validate(now, vera_crypto::operation::DEFAULT_OPERATION_LEAD)
                .is_ok()
            && audience.genesis_id == Some(request.genesis_id)
            && claims.exp <= request.id.expires_at()
    });
    let verdict = match (current, issued_by_signer(&claims.iss)) {
        (false, _) => Verdict::Refused,
        (true, true) => Verdict::IssuedBySigner,
        (true, false) => Verdict::IssuedByAnother,
    };
    (Scope::ThisWrite, verdict)
}

impl super::ConfigAccess {
    /// The audience a stored create made through this access carries
    /// request-bound credentials for; `None` on a home that decides document
    /// access in its own store.
    pub async fn request_bound_audience(
        &self,
    ) -> Result<Option<RequestBoundAudience>, ConfigClientError> {
        match self {
            Self::Graphql(endpoint) => endpoint.request_bound_audience().await,
            Self::Local(local) => match &local.client_endpoint {
                Some(endpoint) => endpoint.request_bound_audience().await,
                None => match crate::local::document_acp::provider_of(&local.node) {
                    crate::local::document_acp::DocumentAcp::Local => Ok(None),
                    crate::local::document_acp::DocumentAcp::Vera => {
                        crate::local::document_acp::node_delegation_audience(&local.node)
                            .filter(|audience| audience.genesis_id.is_some())
                            .map(|audience| {
                                Some(RequestBoundAudience {
                                    audience,
                                    issued_at: std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map_or(0, |elapsed| elapsed.as_secs()),
                                })
                            })
                            .ok_or(DelegationError::AudienceUnavailable.into())
                    }
                },
            },
        }
    }

    /// The time a stored command made through this access is dated at: the
    /// provider's time as its client reads it on a Vera home, the device's
    /// clock elsewhere (`Identity.UserDelegation.storedCommandWindow`).
    pub async fn home_now(&self) -> chrono::DateTime<chrono::Utc> {
        let endpoint = match self {
            Self::Graphql(endpoint) => Some(endpoint),
            Self::Local(local) => local.client_endpoint.as_ref(),
        };
        match endpoint {
            Some(endpoint) => i64::try_from(endpoint.home_time().await)
                .ok()
                .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
                .unwrap_or_else(chrono::Utc::now),
            None => chrono::Utc::now(),
        }
    }

    /// The request-bound credentials the request create `create`, signed by
    /// `signer` with `evidence`, carries in a stored command made through
    /// this access: one for each policy command it sends, none on a home
    /// that decides document access in its own store.
    pub async fn stored_create_credentials(
        &self,
        signer: &dyn crate::identity::NodeIdentity,
        create: &AgentRequestCreate,
        evidence: &serde_json::Value,
    ) -> Result<Vec<String>, ConfigClientError> {
        let Some(audience) = self.request_bound_audience().await? else {
            return Ok(Vec::new());
        };
        let ids = self.home_policy_ids().await?;
        let graph = super::verify_request_evidence(
            super::client_request_input(create)
                .map_err(|_| ConfigClientError::ClientConfigurationUpdateInvalid)?,
            evidence.clone(),
            ids,
        )
        .await?;
        let capability = signer.client_signing_capability()?;
        Ok(
            StoredCreateCommands::of_request(create, graph.doc_id(), ids)?
                .mint(&capability, &audience)?,
        )
    }
}
