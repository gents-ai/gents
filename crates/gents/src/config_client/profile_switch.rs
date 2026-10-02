//! Moving a profile, and the profiles that share its account, to another
//! account of the same provider. An operator write: nothing in `self_config`
//! reaches it.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;

use super::ConfigAccess;
use crate::usage_observation::UsageView;

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
    /// The label of the account the profile runs on.
    pub account: String,
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
    /// The label of the account the profile now runs on.
    pub account: String,
    pub backend_id: String,
    pub behaviors: Vec<String>,
    pub plugin_slots: Vec<String>,
    pub cost: &'static str,
    /// Companions the target offers that stayed on the old account.
    pub companions_offered: Vec<String>,
    pub companions_moved: Vec<String>,
}

/// The accounts `profile_id` can move to. `plugin_slots` are the plugins
/// bound to the profile on the host, which only the caller can read.
pub async fn switch_candidates(
    _access: &ConfigAccess,
    _agent_did: &str,
    profile_id: &str,
    _plugin_slots: &[String],
    _now: DateTime<Utc>,
) -> Result<SwitchPlan> {
    Ok(SwitchPlan {
        profile: profile_id.to_owned(),
        account: String::new(),
        behaviors: Vec::new(),
        plugin_slots: Vec::new(),
        candidates: Vec::new(),
        companions: Vec::new(),
        cost: SWITCH_COST,
    })
}

/// Move `profile_id` to `target_backend_id`, and with `move_companions` the
/// companions the target offers, in one transaction.
pub async fn switch_profile_account(
    _access: &ConfigAccess,
    _agent_did: &str,
    _profile_id: &str,
    _target_backend_id: &str,
    _move_companions: bool,
    _plugin_slots: &[String],
) -> Result<SwitchReceipt> {
    anyhow::bail!("not implemented")
}

#[cfg(test)]
#[path = "profile_switch/tests.rs"]
mod tests;
