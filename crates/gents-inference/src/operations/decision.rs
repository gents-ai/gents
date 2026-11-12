use gents_documents::{BackendAuth, InferenceBackend};
use gents_protocol::operations::inference::{
    AccountOperation, BackendDecision, ChoiceDecision, DiscoveryAnswer, InferenceActor,
};
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};
use serde::{Deserialize, Serialize};

use crate::backends::provider::BackendProviderKind;

/// Lean `InferenceOperations.Signer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signer {
    ClientHeld,
    ManagedAgent,
    None,
}

/// Lean `InferenceOperations.ActingDid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActingDid {
    Absent,
    Node,
    AgentOfNode,
    Other,
}

/// Lean `InferenceOperations.actorKind`: the person rules need a client-held
/// signer for a DID that is neither the Node nor one of its Agents; anything
/// else, and anything that cannot be told, gets the Agent rules.
pub fn actor_kind(signer: Signer, did: ActingDid) -> InferenceActor {
    match (signer, did) {
        (Signer::ClientHeld, ActingDid::Other) => InferenceActor::Person,
        _ => InferenceActor::Agent,
    }
}

fn signer(managed_agent: bool, client_held: Option<&str>, actor: Option<&str>) -> Signer {
    if managed_agent {
        Signer::ManagedAgent
    } else if actor.is_some() && client_held == actor {
        Signer::ClientHeld
    } else {
        Signer::None
    }
}

async fn acting_did(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    actor: Option<&str>,
) -> anyhow::Result<ActingDid> {
    use gents_protocol::document_query::{self as query, fields::agent, Filter, Query, Selection};
    let Some(actor) = actor else {
        return Ok(ActingDid::Absent);
    };
    if actor == node_did {
        return Ok(ActingDid::Node);
    }
    let response = txn
        .run(
            &Query::select(Selection::doc_id())
                .filter(
                    Filter::eq(agent::NODE_DID, node_did).and(Filter::eq(agent::AGENT_DID, actor)),
                )
                .limit(1)
                .render(),
        )
        .await?;
    Ok(if query::rows(&response, "Agent").is_empty() {
        ActingDid::Other
    } else {
        ActingDid::AgentOfNode
    })
}

/// The actor kind of `txn` from what the transaction itself carries. An
/// embedded transaction carries no client key, so it never proves a person.
pub async fn acting(txn: &ConfigApplyTxn<'_>, node_did: &str) -> anyhow::Result<InferenceActor> {
    let actor = txn.actor_did().map(str::to_owned);
    let signer = signer(
        txn.managed_agent_signer().is_some(),
        txn.client_held_signer_did(),
        actor.as_deref(),
    );
    if signer != Signer::ClientHeld {
        return Ok(InferenceActor::Agent);
    }
    Ok(actor_kind(
        signer,
        acting_did(txn, node_did, actor.as_deref()).await?,
    ))
}

async fn acting_with(
    access: &ConfigAccess,
    node_did: &str,
    signer: Signer,
) -> anyhow::Result<InferenceActor> {
    if signer != Signer::ClientHeld {
        return Ok(InferenceActor::Agent);
    }
    let actor = access.actor_did();
    let did = access
        .transact_readonly("inference.operation.actor", |txn| {
            let actor = actor.as_deref();
            Box::pin(acting_did(txn, node_did, actor))
        })
        .await?;
    Ok(actor_kind(signer, did))
}

/// The actor kind of an access: a person only when its client holds the
/// signing key of the DID it acts as.
pub async fn acting_on(access: &ConfigAccess, node_did: &str) -> anyhow::Result<InferenceActor> {
    let actor = access.actor_did();
    let signer = signer(
        access.managed_agent_signer().is_some(),
        access.client_held_signer_did(),
        actor.as_deref(),
    );
    acting_with(access, node_did, signer).await
}

/// The actor kind of `did` whose signature the caller verified itself, as a
/// signed command's author: the client-held evidence the transaction cannot
/// carry for it.
pub async fn acting_signed_by(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    did: &str,
) -> anyhow::Result<InferenceActor> {
    Ok(actor_kind(
        Signer::ClientHeld,
        acting_did(txn, node_did, Some(did)).await?,
    ))
}

/// The actor kind of an embedded access a runtime route opened for the
/// caller of an HTTP request. The caller must have obtained the access's
/// actor from DefraDB's authenticated request extractor: the request's bearer
/// was signed by a key its sender holds, which is the client-held evidence an
/// embedded access cannot carry itself.
pub async fn acting_for_authenticated_request(
    access: &ConfigAccess,
    node_did: &str,
) -> anyhow::Result<InferenceActor> {
    let signer = if access.managed_agent_signer().is_some() {
        Signer::ManagedAgent
    } else if access.actor_did().is_some() {
        Signer::ClientHeld
    } else {
        Signer::None
    };
    acting_with(access, node_did, signer).await
}

/// Lean `InferenceOperations.AuthClass`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthClass {
    AccountFree,
    RawSecret,
    OauthAccount,
    OauthPool,
}

impl AuthClass {
    pub fn of(auth: &BackendAuth) -> Self {
        match auth {
            BackendAuth::Unauthenticated | BackendAuth::Environment { .. } => Self::AccountFree,
            BackendAuth::ApiKey { .. }
            | BackendAuth::Credential { .. }
            | BackendAuth::AwsCredentials { .. }
            | BackendAuth::GoogleCredentials { .. } => Self::RawSecret,
            BackendAuth::NodeOAuth { .. } => Self::OauthAccount,
            BackendAuth::NodeOAuthPool { .. } => Self::OauthPool,
        }
    }
}

/// Lean `InferenceOperations.keepsAuth`.
pub fn keeps_auth(stored: &BackendAuth, candidate: &BackendAuth) -> bool {
    match candidate {
        BackendAuth::NodeOAuth { account_ref } => {
            account_ref.as_deref() == stored.oauth_account_ref()
        }
        _ => candidate == stored,
    }
}

/// Lean `InferenceOperations.backendUpdateDecision`.
pub fn backend_update_decision(
    actor: InferenceActor,
    stored: &BackendAuth,
    candidate: &BackendAuth,
) -> BackendDecision {
    if actor == InferenceActor::Person || keeps_auth(stored, candidate) {
        return BackendDecision::Admitted;
    }
    match AuthClass::of(candidate) {
        AuthClass::AccountFree => BackendDecision::Admitted,
        AuthClass::RawSecret => BackendDecision::RawCredential,
        AuthClass::OauthAccount => BackendDecision::OauthAccount,
        AuthClass::OauthPool => BackendDecision::OauthPool,
    }
}

fn is_local_server(backend: &InferenceBackend) -> bool {
    backend.provider_kind == BackendProviderKind::OpenAiCompatible
        && matches!(backend.auth, BackendAuth::Unauthenticated)
}

/// Lean `InferenceOperations.backendCreateDecision`.
pub fn backend_create_decision(
    actor: InferenceActor,
    candidate: &InferenceBackend,
) -> BackendDecision {
    if actor == InferenceActor::Person {
        BackendDecision::Admitted
    } else if !is_local_server(candidate) {
        BackendDecision::LocalServerOnly
    } else if !candidate.enabled {
        BackendDecision::StartsEnabled
    } else {
        BackendDecision::Admitted
    }
}

/// Lean `InferenceOperations.CurrentBackend`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurrentBackend {
    Absent,
    Same,
    SameProvider,
    OtherProvider,
}

impl CurrentBackend {
    pub fn of(current: Option<&InferenceBackend>, next: &InferenceBackend) -> Self {
        match current {
            None => Self::Absent,
            Some(current) if current.provider_kind != next.provider_kind => Self::OtherProvider,
            Some(current) if current.auth == next.auth => Self::Same,
            Some(_) => Self::SameProvider,
        }
    }
}

/// Lean `InferenceOperations.profileChoiceDecision`; `default_account` is
/// the default account of `next`'s provider (Lean `namesDefault`).
pub fn profile_choice_decision(
    actor: InferenceActor,
    current: Option<&InferenceBackend>,
    next: &InferenceBackend,
    default_account: Option<&str>,
) -> ChoiceDecision {
    if actor == InferenceActor::Person {
        return ChoiceDecision::Admitted;
    }
    let relation = CurrentBackend::of(current, next);
    match &next.auth {
        BackendAuth::NodeOAuth { account_ref } => match relation {
            CurrentBackend::Same => ChoiceDecision::Admitted,
            CurrentBackend::SameProvider => ChoiceDecision::OtherAccount,
            CurrentBackend::Absent | CurrentBackend::OtherProvider => {
                if account_ref.as_deref() == default_account {
                    ChoiceDecision::Admitted
                } else {
                    ChoiceDecision::OtherAccount
                }
            }
        },
        BackendAuth::NodeOAuthPool { .. } => {
            if relation == CurrentBackend::Same {
                ChoiceDecision::Admitted
            } else {
                ChoiceDecision::OtherPool
            }
        }
        _ => ChoiceDecision::Admitted,
    }
}

/// Lean `InferenceOperations.discoveryAnswer`.
pub fn discovery_answer(actor: InferenceActor, backend: &InferenceBackend) -> DiscoveryAnswer {
    if actor == InferenceActor::Agent && !is_local_server(backend) {
        DiscoveryAnswer::StoredObservation
    } else {
        DiscoveryAnswer::ContactsProvider
    }
}

/// Lean `InferenceOperations.accountOperationAdmitted`.
pub fn account_operation_admitted(actor: InferenceActor, operation: AccountOperation) -> bool {
    actor == InferenceActor::Person || operation == AccountOperation::List
}

#[cfg(test)]
mod conformance {
    use super::*;
    use gents_lean_vocab::families;

    fn backend(kind: BackendProviderKind, auth: BackendAuth, enabled: bool) -> InferenceBackend {
        let mut backend: InferenceBackend = serde_json::from_value(serde_json::json!({
            "node_did": "did:key:node",
            "backend_id": "backend",
            "name": "Backend",
            "provider_kind": kind,
            "endpoint": "http://localhost:8000/v1",
            "auth": {"kind": "unauthenticated"},
        }))
        .unwrap();
        backend.auth = auth;
        backend.enabled = enabled;
        backend
    }

    fn account(reference: Option<&str>) -> BackendAuth {
        BackendAuth::NodeOAuth {
            account_ref: reference.map(str::to_owned),
        }
    }

    /// Stored and candidate auth for each class, kept and not kept.
    fn auth_pairs(class: AuthClass, keeps: bool) -> Vec<(BackendAuth, BackendAuth)> {
        let key = |key: &str| BackendAuth::ApiKey { key: key.into() };
        let credential = |version: &str| BackendAuth::Credential {
            id: "credential".into(),
            version: version.into(),
        };
        let variable = |variable: &str| BackendAuth::Environment {
            variable: variable.into(),
        };
        let pool = |allow_overage| BackendAuth::NodeOAuthPool { allow_overage };
        let aws = |secret: &str| BackendAuth::AwsCredentials {
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: secret.into(),
            session_token: None,
        };
        let google = BackendAuth::GoogleCredentials {
            credentials_json: "{}".into(),
        };
        match (class, keeps) {
            (AuthClass::AccountFree, true) => vec![
                (BackendAuth::Unauthenticated, BackendAuth::Unauthenticated),
                (variable("KEY"), variable("KEY")),
            ],
            (AuthClass::AccountFree, false) => vec![
                (key("stored"), BackendAuth::Unauthenticated),
                (BackendAuth::Unauthenticated, variable("KEY")),
                (account(Some("a")), variable("KEY")),
            ],
            (AuthClass::RawSecret, true) => vec![
                (key("stored"), key("stored")),
                (credential("1"), credential("1")),
                (aws("stored"), aws("stored")),
                (google.clone(), google.clone()),
            ],
            (AuthClass::RawSecret, false) => vec![
                (BackendAuth::Unauthenticated, key("new")),
                (key("stored"), key("new")),
                (credential("1"), credential("2")),
                (credential("1"), key("new")),
                (aws("stored"), aws("new")),
                (variable("KEY"), aws("new")),
                (variable("KEY"), google),
            ],
            (AuthClass::OauthAccount, true) => vec![
                (account(Some("a")), account(Some("a"))),
                (account(None), account(None)),
                (BackendAuth::Unauthenticated, account(None)),
                (pool(false), account(None)),
            ],
            (AuthClass::OauthAccount, false) => vec![
                (account(Some("a")), account(Some("b"))),
                (account(Some("a")), account(None)),
                (BackendAuth::Unauthenticated, account(Some("a"))),
            ],
            (AuthClass::OauthPool, true) => vec![(pool(true), pool(true))],
            (AuthClass::OauthPool, false) => vec![
                (pool(false), pool(true)),
                (account(None), pool(false)),
                (BackendAuth::Unauthenticated, pool(false)),
            ],
        }
    }

    #[test]
    fn the_actor_kind_is_decided_as_the_model_decides_it() {
        families::inference_operations_actor_kind::replay(
            |i: &families::inference_operations_actor_kind::Input<ActingDid, Signer>| {
                actor_kind(i.signer, i.did)
            },
        );
    }

    #[test]
    fn a_backend_update_is_decided_as_the_model_decides_it() {
        families::inference_operations_backend_update_decision::replay(
            |i: &families::inference_operations_backend_update_decision::Input<
                InferenceActor,
                AuthClass,
            >| {
                let decisions: Vec<BackendDecision> = auth_pairs(i.auth, i.keeps)
                    .iter()
                    .map(|(stored, candidate)| {
                        assert_eq!(AuthClass::of(candidate), i.auth);
                        assert_eq!(keeps_auth(stored, candidate), i.keeps);
                        backend_update_decision(i.actor, stored, candidate)
                    })
                    .collect();
                assert!(decisions.windows(2).all(|pair| pair[0] == pair[1]));
                decisions[0]
            },
        );
    }

    #[test]
    fn a_backend_create_is_decided_as_the_model_decides_it() {
        families::inference_operations_backend_create_decision::replay(
            |i: &families::inference_operations_backend_create_decision::Input<InferenceActor>| {
                let candidates = if i.local_server {
                    vec![backend(
                        BackendProviderKind::OpenAiCompatible,
                        BackendAuth::Unauthenticated,
                        i.enabled,
                    )]
                } else {
                    vec![
                        backend(
                            BackendProviderKind::OpenAiCompatible,
                            BackendAuth::Environment {
                                variable: "KEY".into(),
                            },
                            i.enabled,
                        ),
                        backend(
                            BackendProviderKind::OpenRouter,
                            BackendAuth::Unauthenticated,
                            i.enabled,
                        ),
                        backend(BackendProviderKind::ChatGptCodex, account(None), i.enabled),
                    ]
                };
                let decisions: Vec<BackendDecision> = candidates
                    .iter()
                    .map(|candidate| backend_create_decision(i.actor, candidate))
                    .collect();
                assert!(decisions.windows(2).all(|pair| pair[0] == pair[1]));
                decisions[0]
            },
        );
    }

    #[derive(Debug, Clone, Copy, Deserialize, Serialize)]
    #[serde(rename_all = "snake_case")]
    enum ProfileRoute {
        Anchored,
        ExactId,
    }

    /// The backend a profile uses and the one it would move to, with the
    /// default account of the second one's provider.
    fn choice(
        next: AuthClass,
        current: CurrentBackend,
        is_default: bool,
    ) -> (
        Option<InferenceBackend>,
        InferenceBackend,
        Option<&'static str>,
    ) {
        use BackendProviderKind::{ChatGptCodex, OpenAiCompatible, XaiGrokOAuth};
        let (kind, auth, default) = match next {
            AuthClass::AccountFree => (OpenAiCompatible, BackendAuth::Unauthenticated, None),
            AuthClass::RawSecret => (
                OpenAiCompatible,
                BackendAuth::Credential {
                    id: "credential".into(),
                    version: "1".into(),
                },
                None,
            ),
            AuthClass::OauthAccount if is_default => (ChatGptCodex, account(Some("a")), Some("a")),
            AuthClass::OauthAccount => (ChatGptCodex, account(Some("a")), Some("b")),
            AuthClass::OauthPool => (
                BackendProviderKind::ClaudeCliSubscription,
                BackendAuth::NodeOAuthPool {
                    allow_overage: false,
                },
                None,
            ),
        };
        let next_backend = backend(kind, auth.clone(), true);
        let current_backend = match current {
            CurrentBackend::Absent => None,
            CurrentBackend::Same => Some(next_backend.clone()),
            CurrentBackend::SameProvider => Some(backend(
                kind,
                match next {
                    AuthClass::OauthPool => account(None),
                    AuthClass::OauthAccount => account(Some("other")),
                    _ => BackendAuth::Environment {
                        variable: "KEY".into(),
                    },
                },
                true,
            )),
            CurrentBackend::OtherProvider => Some(backend(XaiGrokOAuth, account(Some("x")), true)),
        };
        (current_backend, next_backend, default)
    }

    #[test]
    fn a_profile_choice_is_decided_as_the_model_decides_it() {
        families::inference_operations_profile_write_decision::replay(
            |i: &families::inference_operations_profile_write_decision::Input<
                InferenceActor,
                AuthClass,
                CurrentBackend,
                ProfileRoute,
            >| {
                let (current, next, default) = choice(i.next, i.current, i.is_default);
                assert_eq!(AuthClass::of(&next.auth), i.next);
                assert_eq!(CurrentBackend::of(current.as_ref(), &next), i.current);
                profile_choice_decision(i.actor, current.as_ref(), &next, default)
            },
        );
    }

    #[test]
    fn discovery_answers_as_the_model_answers() {
        families::inference_operations_discovery_answer::replay(
            |i: &families::inference_operations_discovery_answer::Input<InferenceActor>| {
                let candidates = if i.credentialed {
                    vec![
                        backend(
                            BackendProviderKind::OpenAiCompatible,
                            BackendAuth::Environment {
                                variable: "KEY".into(),
                            },
                            true,
                        ),
                        backend(BackendProviderKind::ChatGptCodex, account(None), true),
                        backend(
                            BackendProviderKind::OpenRouter,
                            BackendAuth::Unauthenticated,
                            true,
                        ),
                    ]
                } else {
                    vec![backend(
                        BackendProviderKind::OpenAiCompatible,
                        BackendAuth::Unauthenticated,
                        true,
                    )]
                };
                let answers: Vec<DiscoveryAnswer> = candidates
                    .iter()
                    .map(|candidate| discovery_answer(i.actor, candidate))
                    .collect();
                assert!(answers.windows(2).all(|pair| pair[0] == pair[1]));
                answers[0]
            },
        );
    }

    #[test]
    fn account_operations_are_admitted_as_the_model_admits_them() {
        families::inference_operations_account_operation_admitted::replay(
            |i: &families::inference_operations_account_operation_admitted::Input<
                AccountOperation,
                InferenceActor,
            >| account_operation_admitted(i.actor, i.operation),
        );
    }
}
