use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use gents_protocol::job::PluginJobSettings;
use serde_json::{Map, Value};

use super::{read_owned_doc, SelfConfigCore};
use crate::config_client::patch::SelfConfigTarget;
use crate::config_client::ConfigApplyTxn;
use crate::document_config::{IntegrationTools, SelfConfigTools};

/// Operator-managed grants carried by a Tools document (Lean
/// `SelfConfig.Grants`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OperatorGrants {
    /// `self_config.enable_pack_install`.
    pub pack_install: bool,
    /// `integrations.plugins[].job` by plugin coordinate. A plugin with no
    /// entry has the empty ceiling.
    pub plugin_jobs: BTreeMap<String, PluginJobSettings>,
}

impl OperatorGrants {
    pub fn from_tools_json(tools: &Map<String, Value>) -> Result<Self> {
        let config = match tools.get("self_config") {
            None | Some(Value::Null) => SelfConfigTools::default(),
            Some(group) => serde_json::from_value::<SelfConfigTools>(group.clone())
                .context("Tools.self_config does not decode; its operator grants cannot be read")?,
        };
        let integrations = match tools.get("integrations") {
            None | Some(Value::Null) => IntegrationTools::default(),
            Some(group) => serde_json::from_value::<IntegrationTools>(group.clone()).context(
                "Tools.integrations does not decode; its operator grants cannot be read",
            )?,
        };
        let mut plugin_jobs = BTreeMap::new();
        for plugin in integrations.plugins.into_iter().flatten() {
            let Some(job) = plugin.job else { continue };
            anyhow::ensure!(
                plugin_jobs.insert(plugin.plugin.clone(), job).is_none(),
                "Tools.integrations.plugins names {:?} twice; its job grant cannot be read",
                plugin.plugin
            );
        }
        Ok(Self {
            pack_install: config.enable_pack_install.unwrap_or(false),
            plugin_jobs,
        })
    }

    /// Lean `Grants.boundedBy` and `keepsJobGrants`: pack installation is
    /// held-bounded: carried by `stored` or held by `held`. Job blocks are
    /// bounded by `stored` alone ([`Self::job_refusal`]).
    pub fn bounded_by(&self, stored: &Self, held: &Self) -> bool {
        (!self.pack_install || stored.pack_install || held.pack_install)
            && self.job_refusal(stored).is_none()
    }

    /// Lean `JobGrant.narrows` over every job block `self` carries: the first
    /// block that adds to, widens or edits the policy of the block `stored`
    /// carries for the same plugin, as a refusal naming the field. An absent
    /// block is the empty ceiling, so a block with no stored counterpart is
    /// an addition.
    fn job_refusal(&self, stored: &Self) -> Option<String> {
        self.plugin_jobs.iter().find_map(|(plugin, job)| {
            let Some(base) = stored.plugin_jobs.get(plugin) else {
                return Some(format!(
                    "integrations.plugins[{plugin}].job is not in the stored Tools; only the operator may add a job block"
                ));
            };
            let field = if !job.agent_targets.iter().all(|t| base.agent_targets.contains(t)) {
                "agent_targets"
            } else if job.max_live > base.max_live {
                "max_live"
            } else if job.max_spawns > base.max_spawns {
                "max_spawns"
            } else if job.max_messages_per_slot > base.max_messages_per_slot {
                "max_messages_per_slot"
            } else if job.result_bytes > base.result_bytes {
                "result_bytes"
            } else if job.max_wall_secs > base.max_wall_secs {
                "max_wall_secs"
            } else if job.policy != base.policy {
                "policy"
            } else {
                return None;
            };
            Some(format!(
                "integrations.plugins[{plugin}].job.{field} differs from the stored block beyond narrowing; only the operator may widen a job grant or edit its policy"
            ))
        })
    }
}

/// Lean `SelfConfig.keepsGrants`; `stored` is `None` when no document is
/// replaced. Unlike [`super::guard_tools_keep_control`] this always runs, from the
/// validate slot.
pub fn guard_tools_keep_grants(
    held: &OperatorGrants,
    stored: Option<&Map<String, Value>>,
    candidate: &Map<String, Value>,
) -> Result<()> {
    let candidate = OperatorGrants::from_tools_json(candidate)?;
    let stored = stored
        .map(OperatorGrants::from_tools_json)
        .transpose()?
        .unwrap_or_default();
    if let Some(refusal) = candidate.job_refusal(&stored) {
        bail!(refusal);
    }
    anyhow::ensure!(
        candidate.bounded_by(&stored, held),
        "pack installation is operator-managed and cannot be self-granted"
    );
    Ok(())
}

/// Lean `SelfConfig.reselectionKeepsGrants`.
pub fn reselection_keeps_grants(
    held: &OperatorGrants,
    before: Option<&Map<String, Value>>,
    after: Option<&Map<String, Value>>,
) -> Result<()> {
    match after {
        None => Ok(()),
        Some(after) => guard_tools_keep_grants(held, before, after),
    }
}

/// The native side of Lean `SelfConfig.chainKeepsGrants`. An unchanged
/// selection passes without reads (Lean
/// `chain_unchanged_selection_keeps_grants`).
pub(crate) async fn guard_reselection_keeps_grants_in_txn(
    txn: &ConfigApplyTxn<'_>,
    core: &SelfConfigCore,
    target: SelfConfigTarget,
    stored: Option<&Map<String, Value>>,
    candidate: &Map<String, Value>,
) -> Result<()> {
    let field = match target {
        SelfConfigTarget::AgentContext => "tools_id",
        SelfConfigTarget::Agent => "context_id",
        _ => return Ok(()),
    };
    let selected = |doc: &Map<String, Value>| {
        doc.get(field)
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(ToOwned::to_owned)
    };
    let before = stored.and_then(selected);
    let after = selected(candidate);
    if before == after {
        return Ok(());
    }
    let owner = core.node_did();
    let before_tools = selected_tools_in_txn(txn, owner, target, before.as_deref()).await?;
    let after_tools = selected_tools_in_txn(txn, owner, target, after.as_deref()).await?;
    reselection_keeps_grants(core.held_grants(), before_tools.as_ref(), after_tools.as_ref())
        .with_context(|| {
            format!(
                "{field} {:?} selects Tools carrying an operator grant this agent does not hold; select Tools without it or ask the operator to grant it",
                after.as_deref().unwrap_or_default()
            )
        })
}

async fn selected_tools_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    target: SelfConfigTarget,
    id: Option<&str>,
) -> Result<Option<Map<String, Value>>> {
    let Some(id) = id else {
        return Ok(None);
    };
    match target {
        SelfConfigTarget::AgentContext => tools_by_id_in_txn(txn, owner, id).await,
        _ => context_tools_in_txn(txn, owner, id).await,
    }
}

async fn tools_by_id_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    tools_id: &str,
) -> Result<Option<Map<String, Value>>> {
    Ok(
        read_owned_doc(txn, SelfConfigTarget::Tools, owner, tools_id)
            .await?
            .map(|(_, doc)| doc),
    )
}

async fn context_tools_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    context_id: &str,
) -> Result<Option<Map<String, Value>>> {
    let Some((_, context)) =
        read_owned_doc(txn, SelfConfigTarget::AgentContext, owner, context_id).await?
    else {
        return Ok(None);
    };
    match context.get("tools_id").and_then(Value::as_str) {
        Some(tools_id) if !tools_id.is_empty() => tools_by_id_in_txn(txn, owner, tools_id).await,
        _ => Ok(None),
    }
}
