//! Moving a profile, and the profiles that share its account, to another
//! account of the same provider. An operator write: nothing in `self_config`
//! reaches it.

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;

use super::{
    apply_desired_state_plan, ConfigAccess, ConfigApplyTxn, DesiredStateApplyDocument,
    DesiredStateApplyPlan,
};
use crate::config::advertised_model_for_profile;
use crate::document_config::{ConfigReferences, InferenceBackendObservation, InferenceProfile};
use crate::oauth_credential::{
    backend_account, enabled_accounts_note, list_accounts, no_switch_candidate_note, provider_name,
    serving_account, AccountState, AccountSummary, ServingAccount,
};
use crate::usage_observation::{usage_for_backend, usage_view, UsageView};
use crate::{Collection, InferenceBackend};

/// What a move costs, shown with every plan and receipt.
pub const SWITCH_COST: &str = "After the move the provider's prompt cache starts empty, so the \
                               next turns are slower and use more of the new account's quota.";

/// An account a profile can move to: an enabled backend of the same provider
/// on another account, with its stored usage.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SwitchCandidate {
    pub label: String,
    pub backend_id: String,
    /// `offered`, or `not read yet` when the backend's catalog was never read.
    pub models: &'static str,
    pub usage: UsageView,
}

/// Where `profile` can move, who uses it and what the move costs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SwitchPlan {
    pub profile: String,
    /// The account the profile runs on.
    pub account: ServingAccount,
    pub behaviors: Vec<String>,
    /// Plugins whose model slot is bound to the profile.
    pub plugin_slots: Vec<String>,
    pub candidates: Vec<SwitchCandidate>,
    /// The other profiles of `behaviors` on the same account.
    pub companions: Vec<String>,
    pub cost: &'static str,
}

/// A committed move.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SwitchReceipt {
    pub headline: String,
    pub profile: String,
    /// The account the profile now runs on.
    pub account: ServingAccount,
    pub backend_id: String,
    pub behaviors: Vec<String>,
    pub plugin_slots: Vec<String>,
    pub cost: &'static str,
    /// Companions the target offers that stayed on the old account.
    pub companions_offered: Vec<String>,
    pub companions_moved: Vec<String>,
}

/// The accounts `profile_id` can move to. `plugin_slots` are the plugins
/// bound to the profile on the host, which only the caller can read. With no
/// candidate, the error says how to add an account.
pub async fn switch_candidates(
    access: &ConfigAccess,
    agent_did: &str,
    profile_id: &str,
    plugin_slots: &[String],
    now: DateTime<Utc>,
) -> Result<SwitchPlan> {
    let accounts = list_accounts(access, agent_did).await?;
    let snapshot = access
        .transact_readonly("config.profile_switch.candidates", |txn| {
            Box::pin(async move { Snapshot::load_in_txn(txn, agent_did, profile_id).await })
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
        let stored = usage_for_backend(access, agent_did, backend).await?;
        candidates.push(SwitchCandidate {
            label: serving.label,
            backend_id: backend.backend_id.clone(),
            models,
            usage: usage_view(stored.as_ref(), backend.provider_kind, now),
        });
    }
    anyhow::ensure!(
        !candidates.is_empty(),
        "{}",
        no_switch_candidate_note(&own_provider, &snapshot.profile.model_name)
    );
    candidates.sort_by(|a, b| a.label.cmp(&b.label));
    Ok(SwitchPlan {
        profile: profile_id.to_owned(),
        account: serving_account(&snapshot.backend, &accounts),
        behaviors: snapshot.references.behaviors_on_profile(profile_id),
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
/// companions the target offers, in one transaction. Refused, writing
/// nothing, for another provider's account, the same account, an account
/// that is not enabled and a model the target does not offer. Accounts are
/// read just before the transaction.
pub async fn switch_profile_account(
    access: &ConfigAccess,
    agent_did: &str,
    profile_id: &str,
    target_backend_id: &str,
    move_companions: bool,
    plugin_slots: &[String],
) -> Result<SwitchReceipt> {
    let accounts = list_accounts(access, agent_did).await?;
    let accounts = &accounts;
    access
        .transact("config.profile_switch.move", |txn| {
            Box::pin(async move {
                let snapshot = Snapshot::load_in_txn(txn, agent_did, profile_id).await?;
                let (target, observation) = snapshot
                    .backends
                    .iter()
                    .find(|(backend, _)| backend.backend_id == target_backend_id)
                    .with_context(|| format!("no backend {target_backend_id:?}"))?;
                let serving = serving_account(target, accounts);
                let own_provider = provider(&snapshot.backend, accounts);
                anyhow::ensure!(
                    provider(target, accounts) == own_provider,
                    "account {:?} belongs to another provider; profile {profile_id:?} moves \
                     only to another {} account",
                    serving.label,
                    provider_name(&own_provider)
                );
                anyhow::ensure!(
                    account_key(target, accounts) != account_key(&snapshot.backend, accounts),
                    "profile {profile_id:?} is already on account {:?}",
                    serving.label
                );
                let state = if target.enabled {
                    serving.state
                } else {
                    AccountState::Disabled
                };
                anyhow::ensure!(
                    state == AccountState::Enabled,
                    "account {:?} is {}{}",
                    serving.label,
                    state.as_str(),
                    serving
                        .provider
                        .map(|provider| format!("; {}", enabled_accounts_note(provider, accounts)))
                        .unwrap_or_default()
                );
                advertised_model_for_profile(target, &snapshot.profile, observation.as_ref())
                    .with_context(|| {
                        format!(
                            "account {:?} does not offer {}",
                            serving.label, snapshot.profile.model_name
                        )
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
                let behaviors = snapshot.references.behaviors_on_profile(profile_id);
                Ok(SwitchReceipt {
                    headline: headline(
                        profile_id,
                        &serving.label,
                        behaviors.len(),
                        plugin_slots.len(),
                    ),
                    profile: profile_id.to_owned(),
                    account: serving,
                    backend_id: target.backend_id.clone(),
                    behaviors,
                    plugin_slots: plugin_slots.to_vec(),
                    cost: SWITCH_COST,
                    companions_offered: offered.into_iter().map(|p| p.profile_id).collect(),
                    companions_moved,
                })
            })
        })
        .await
}

/// `Move profile <P> to <label> (used by N behaviors and M plugin slots)`.
fn headline(profile: &str, label: &str, behaviors: usize, slots: usize) -> String {
    let count = |n: usize, noun: &str| format!("{n} {noun}{}", if n == 1 { "" } else { "s" });
    let mut users = Vec::new();
    if behaviors > 0 || slots == 0 {
        users.push(count(behaviors, "behavior"));
    }
    if slots > 0 {
        users.push(count(slots, "plugin slot"));
    }
    format!(
        "Move profile {profile} to {label} (used by {})",
        users.join(" and ")
    )
}

/// The provider an account belongs to: the sign-in provider, else the
/// backend's kind (an API-key backend is its own account).
fn provider(backend: &InferenceBackend, accounts: &[AccountSummary]) -> String {
    serving_account(backend, accounts)
        .provider
        .unwrap_or(backend.provider_kind.as_str())
        .to_owned()
}

/// One key per account: the sign-in a backend runs on, else the backend.
fn account_key(backend: &InferenceBackend, accounts: &[AccountSummary]) -> String {
    match backend_account(backend, accounts) {
        Some(Some(account)) => account.credential_id.clone(),
        _ => backend.backend_id.clone(),
    }
}

/// The profile, its backend and every backend with its observation, from
/// one transaction.
struct Snapshot {
    references: ConfigReferences,
    profile: InferenceProfile,
    backend: InferenceBackend,
    backends: Vec<(InferenceBackend, Option<InferenceBackendObservation>)>,
}

impl Snapshot {
    async fn load_in_txn(
        txn: &ConfigApplyTxn<'_>,
        agent_did: &str,
        profile_id: &str,
    ) -> Result<Self> {
        let references = ConfigReferences::load_in_txn(txn, agent_did).await?;
        let (profile, backend) =
            references
                .profile_with_backend(profile_id)?
                .with_context(|| {
                    format!("no profile {profile_id:?}; `gents config profile list` shows them")
                })?;
        let mut backends = Vec::new();
        for backend in super::list_inference_backends_in_txn(txn, agent_did).await? {
            let observation = crate::backend_registry::lookup_backend_observation_in_txn(
                txn,
                agent_did,
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

    /// The other profiles (own or compaction) of the behaviors on the
    /// profile that run on the same account.
    fn companions(&self, accounts: &[AccountSummary]) -> Vec<InferenceProfile> {
        let own = account_key(&self.backend, accounts);
        let ids: BTreeSet<String> = self
            .references
            .behaviors_on_profile(&self.profile.profile_id)
            .iter()
            .flat_map(|behavior| self.references.behavior_profiles(behavior))
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
