use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::desired_state::DesiredStateError;
use super::inference_backend::InferenceBackendError;
use super::{
    apply_desired_state_plan, ConfigAccess, ConfigApplyTxn, ConfigClientError,
    DesiredStateApplyDocument, DesiredStateApplyPlan, TransactionFailure,
};
use crate::backend_registry::BackendRegistryError;
use crate::config::{advertised_model_for_profile, ConfigError};
use crate::document_config::{
    ConfigReferences, DocumentConfigError, InferenceBackendObservation, InferenceProfile,
};
use crate::oauth_credential::{
    backend_account, enabled_accounts_note, list_accounts, no_switch_candidate_note, provider_name,
    serving_account, AccountState, AccountSummary, OAuthCredentialError, ServingAccount,
};
use crate::usage_observation::{usage_for_backend, usage_view, UsageObservationError, UsageView};
use crate::{Collection, InferenceBackend};

#[derive(Debug, thiserror::Error)]
pub enum ProfileSwitchError {
    #[error("{note}")]
    NoCandidate { note: String },
    #[error("no backend {backend_id:?}")]
    NoBackend { backend_id: String },
    #[error(
        "account {label:?} belongs to another provider; profile {profile_id:?} moves only to another {provider} account"
    )]
    OtherProvider {
        label: String,
        profile_id: String,
        provider: String,
    },
    #[error("profile {profile_id:?} is already on account {label:?}")]
    SameAccount { profile_id: String, label: String },
    #[error("account {label:?} is {state}{note}")]
    AccountUnavailable {
        label: String,
        state: &'static str,
        note: String,
    },
    #[error("account {label:?} does not offer {model_name}")]
    ModelNotOffered {
        label: String,
        model_name: String,
        #[source]
        source: ConfigError,
    },
    #[error("no profile {profile_id:?}; `gents config profile list` shows them")]
    NoProfile { profile_id: String },
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    DocumentConfig(#[from] DocumentConfigError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    DesiredState(#[from] DesiredStateError),
    #[error(transparent)]
    InferenceBackend(#[from] InferenceBackendError),
    #[error(transparent)]
    BackendRegistry(#[from] BackendRegistryError),
    #[error(transparent)]
    OAuthCredential(#[from] OAuthCredentialError),
    #[error(transparent)]
    UsageObservation(#[from] UsageObservationError),
    #[error(transparent)]
    Access(#[from] ConfigClientError),
}

impl TransactionFailure for ProfileSwitchError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Access(error) => Some(error),
            Self::DocumentConfig(error) => error.config_client_error(),
            Self::DesiredState(error) => error.config_client_error(),
            Self::InferenceBackend(error) => error.config_client_error(),
            Self::BackendRegistry(error) => error.config_client_error(),
            Self::OAuthCredential(error) => error.config_client_error(),
            Self::UsageObservation(error) => error.config_client_error(),
            Self::NoCandidate { .. }
            | Self::NoBackend { .. }
            | Self::OtherProvider { .. }
            | Self::SameAccount { .. }
            | Self::AccountUnavailable { .. }
            | Self::ModelNotOffered { .. }
            | Self::NoProfile { .. }
            | Self::Config(_)
            | Self::Json(_) => None,
        }
    }
}

type Result<T, E = ProfileSwitchError> = std::result::Result<T, E>;

/// What a move costs, shown with every plan and receipt.
pub const SWITCH_COST: &str = "After the move the provider's prompt cache starts empty, so the \
                               next turns are slower and use more of the new account's quota.";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SwitchCandidate {
    pub label: String,
    pub backend_id: String,
    pub models: &'static str,
    pub usage: UsageView,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SwitchPlan {
    pub profile: String,
    pub account: ServingAccount,
    pub agents: Vec<String>,
    pub plugin_slots: Vec<String>,
    pub candidates: Vec<SwitchCandidate>,
    pub companions: Vec<String>,
    pub cost: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SwitchReceipt {
    pub headline: String,
    pub profile: String,
    pub account: ServingAccount,
    pub backend_id: String,
    pub agents: Vec<String>,
    pub plugin_slots: Vec<String>,
    pub cost: &'static str,
    pub companions_offered: Vec<String>,
    pub companions_moved: Vec<String>,
}

pub async fn switch_candidates(
    access: &ConfigAccess,
    node_did: &str,
    profile_id: &str,
    plugin_slots: &[String],
    now: DateTime<Utc>,
) -> Result<SwitchPlan> {
    let accounts = list_accounts(access, node_did).await?;
    let snapshot = access
        .transact_readonly("config.profile_switch.candidates", |txn| {
            Box::pin(async move { Snapshot::load_in_txn(txn, node_did, profile_id).await })
        })
        .await?;
    let own_provider = provider(&snapshot.backend, &accounts);
    let own = account_key(&snapshot.backend, &accounts);
    let mut candidates = Vec::new();
    for (backend, observation) in &snapshot.backends {
        let serving = serving_account(backend, &accounts);
        if !backend.enabled
            || serving.state != AccountState::Enabled
            || provider(backend, &accounts) != own_provider
            || account_key(backend, &accounts) == own
        {
            continue;
        }
        let models =
            match advertised_model_for_profile(backend, &snapshot.profile, observation.as_ref()) {
                Ok(Some(_)) => "offered",
                Ok(None) => "not read yet",
                Err(_) => continue,
            };
        let stored = usage_for_backend(access, node_did, backend).await?;
        candidates.push(SwitchCandidate {
            label: serving.label,
            backend_id: backend.backend_id.clone(),
            models,
            usage: usage_view(stored.as_ref(), backend.provider_kind, now),
        });
    }
    if candidates.is_empty() {
        return Err(ProfileSwitchError::NoCandidate {
            note: no_switch_candidate_note(&own_provider, &snapshot.profile.model_name),
        });
    }
    candidates.sort_by(|a, b| a.label.cmp(&b.label));
    Ok(SwitchPlan {
        profile: profile_id.to_owned(),
        account: serving_account(&snapshot.backend, &accounts),
        agents: snapshot.references.agents_on_profile(profile_id),
        plugin_slots: plugin_slots.to_vec(),
        candidates,
        companions: snapshot
            .companions(&accounts)
            .into_iter()
            .map(|profile| profile.profile_id)
            .collect(),
        cost: SWITCH_COST,
    })
}

/// Move `profile_id` to `target_backend_id`, and with `move_companions` the
/// companions the target offers, in one transaction. Refused, writing nothing,
/// for another provider's account, the same account, a disabled account, or a
/// model the target does not offer. Accounts are read just before the
/// transaction.
pub async fn switch_profile_account(
    access: &ConfigAccess,
    node_did: &str,
    profile_id: &str,
    target_backend_id: &str,
    move_companions: bool,
    plugin_slots: &[String],
) -> Result<SwitchReceipt> {
    let accounts = list_accounts(access, node_did).await?;
    let accounts = &accounts;
    access
        .transact_write("config.profile_switch.move", |txn| {
            Box::pin(async move {
                let snapshot = Snapshot::load_in_txn(txn, node_did, profile_id).await?;
                let (target, observation) = snapshot
                    .backends
                    .iter()
                    .find(|(backend, _)| backend.backend_id == target_backend_id)
                    .ok_or_else(|| ProfileSwitchError::NoBackend {
                        backend_id: target_backend_id.to_owned(),
                    })?;
                let serving = serving_account(target, accounts);
                let own_provider = provider(&snapshot.backend, accounts);
                if provider(target, accounts) != own_provider {
                    return Err(ProfileSwitchError::OtherProvider {
                        label: serving.label.clone(),
                        profile_id: profile_id.to_owned(),
                        provider: provider_name(&own_provider).to_string(),
                    });
                }
                if account_key(target, accounts) == account_key(&snapshot.backend, accounts) {
                    return Err(ProfileSwitchError::SameAccount {
                        profile_id: profile_id.to_owned(),
                        label: serving.label.clone(),
                    });
                }
                let state = if target.enabled {
                    serving.state
                } else {
                    AccountState::Disabled
                };
                if state != AccountState::Enabled {
                    return Err(ProfileSwitchError::AccountUnavailable {
                        label: serving.label.clone(),
                        state: state.as_str(),
                        note: serving
                            .provider
                            .map(|provider| {
                                format!("; {}", enabled_accounts_note(provider, accounts))
                            })
                            .unwrap_or_default(),
                    });
                }
                advertised_model_for_profile(target, &snapshot.profile, observation.as_ref())
                    .map_err(|source| ProfileSwitchError::ModelNotOffered {
                        label: serving.label.clone(),
                        model_name: snapshot.profile.model_name.clone(),
                        source,
                    })?;
                let (mut moved, mut offered) = (vec![snapshot.profile.clone()], Vec::new());
                for companion in snapshot.companions(accounts) {
                    if advertised_model_for_profile(target, &companion, observation.as_ref())
                        .is_ok()
                    {
                        offered.push(companion);
                    }
                }
                let companions_moved = if move_companions {
                    moved.append(&mut offered);
                    moved[1..].iter().map(|p| p.profile_id.clone()).collect()
                } else {
                    Vec::new()
                };
                let documents = moved
                    .into_iter()
                    .map(|mut profile| {
                        profile.backend_id = target.backend_id.clone();
                        profile.validate()?;
                        let value = serde_json::to_value(&profile)?;
                        Ok(DesiredStateApplyDocument {
                            collection: Collection::InferenceProfile,
                            add: value.clone(),
                            update: value,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                apply_desired_state_plan(txn, &DesiredStateApplyPlan::new(documents)?).await?;
                let agents = snapshot.references.agents_on_profile(profile_id);
                Ok(SwitchReceipt {
                    headline: headline(
                        profile_id,
                        &serving.label,
                        agents.len(),
                        plugin_slots.len(),
                    ),
                    profile: profile_id.to_owned(),
                    account: serving,
                    backend_id: target.backend_id.clone(),
                    agents,
                    plugin_slots: plugin_slots.to_vec(),
                    cost: SWITCH_COST,
                    companions_offered: offered.into_iter().map(|p| p.profile_id).collect(),
                    companions_moved,
                })
            })
        })
        .await
}

fn headline(profile: &str, label: &str, agents: usize, slots: usize) -> String {
    let count = |n: usize, noun: &str| format!("{n} {noun}{}", if n == 1 { "" } else { "s" });
    let mut users = Vec::new();
    if agents > 0 || slots == 0 {
        users.push(count(agents, "agent"));
    }
    if slots > 0 {
        users.push(count(slots, "plugin slot"));
    }
    format!(
        "Move profile {profile} to {label} (used by {})",
        users.join(" and ")
    )
}

fn provider(backend: &InferenceBackend, accounts: &[AccountSummary]) -> String {
    serving_account(backend, accounts)
        .provider
        .unwrap_or(backend.provider_kind.as_str())
        .to_owned()
}

fn account_key(backend: &InferenceBackend, accounts: &[AccountSummary]) -> String {
    match backend_account(backend, accounts) {
        Some(Some(account)) => account.credential_id.clone(),
        _ => backend.backend_id.clone(),
    }
}

struct Snapshot {
    references: ConfigReferences,
    profile: InferenceProfile,
    backend: InferenceBackend,
    backends: Vec<(InferenceBackend, Option<InferenceBackendObservation>)>,
}

impl Snapshot {
    async fn load_in_txn(
        txn: &ConfigApplyTxn<'_>,
        node_did: &str,
        profile_id: &str,
    ) -> Result<Self> {
        let references = ConfigReferences::load_in_txn(txn, node_did).await?;
        let (profile, backend) = references
            .profile_with_backend(profile_id)?
            .ok_or_else(|| ProfileSwitchError::NoProfile {
                profile_id: profile_id.to_owned(),
            })?;
        let mut backends = Vec::new();
        for backend in super::list_inference_backends_in_txn(txn, node_did).await? {
            let observation = crate::backend_registry::lookup_backend_observation_in_txn(
                txn,
                node_did,
                &backend.backend_id,
            )
            .await?;
            backends.push((backend, observation));
        }
        Ok(Self {
            references,
            profile,
            backend,
            backends,
        })
    }

    fn companions(&self, accounts: &[AccountSummary]) -> Vec<InferenceProfile> {
        let own = account_key(&self.backend, accounts);
        let ids: BTreeSet<String> = self
            .references
            .agents_on_profile(&self.profile.profile_id)
            .iter()
            .flat_map(|behavior| self.references.agent_profiles(behavior))
            .filter(|id| *id != self.profile.profile_id)
            .collect();
        ids.iter()
            .filter_map(|id| self.references.profile_with_backend(id).ok().flatten())
            .filter(|(_, backend)| account_key(backend, accounts) == own)
            .map(|(profile, _)| profile)
            .collect()
    }
}

#[cfg(test)]
#[path = "profile_switch/tests.rs"]
mod tests;
