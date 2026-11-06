use std::collections::BTreeSet;

use anyhow::{Context, Result};
use serde_json::{Map, Value};

use super::{read_owned_doc, SelfConfigCore};
use crate::documents::{IntegrationTools, SelfConfigTools};
use crate::store::config_client::patch::SelfConfigTarget;
use crate::store::config_client::ConfigApplyTxn;

/// Operator-managed grants carried by a Tools document (Lean
/// `SelfConfig.Grants`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OperatorGrants {
    /// `self_config.enable_pack_install`.
    pub pack_install: bool,
    /// The `policy` self-config category.
    pub policy: bool,
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
        let mut plugin_jobs = BTreeSet::new();
        for plugin in integrations.plugins.into_iter().flatten() {
            if plugin.job.is_none() {
                continue;
            }
            anyhow::ensure!(
                plugin_jobs.insert(plugin.plugin.clone()),
                "Tools.integrations.plugins names {:?} twice; its job grant cannot be read",
                plugin.plugin
            );
        }
        Ok(Self {
            pack_install: config.enable_pack_install.unwrap_or(false),
            policy: config
                .self_config_categories
                .iter()
                .flatten()
                .any(|category| category == "policy"),
        })
    }

    /// The operator grants this candidate carries that neither the stored Tools
    /// nor the holder has. Each grant is bounded on its own.
    pub fn unheld_by(&self, stored: &Self, held: &Self) -> Vec<&'static str> {
        let mut unheld = Vec::new();
        if self.pack_install && !stored.pack_install && !held.pack_install {
            unheld.push("enable_pack_install");
        }
        if self.policy && !stored.policy && !held.policy {
            unheld.push("the policy category");
        }
        unheld
    }

    /// Lean `Grants.boundedBy`: every grant is kept from the stored Tools or the holder.
    pub fn bounded_by(&self, stored: &Self, held: &Self) -> bool {
        self.unheld_by(stored, held).is_empty()
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
    let unheld = candidate.unheld_by(&stored, held);
    anyhow::ensure!(
        unheld.is_empty(),
        "operator-managed {} cannot be self-granted; this agent does not hold {}. Select Tools without it or ask the operator to grant it",
        unheld.join(", "),
        if unheld.len() == 1 { "it" } else { "them" }
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

#[cfg(test)]
mod lean_bound_tests {
    use super::*;

    #[test]
    fn modeled_grant_bound_matches_the_owner() {
        for case in &crate::lean_vocab_test::lean_contract_snapshot().policy_grant_bound_cases {
            let grants = |field: &str| OperatorGrants {
                pack_install: false,
                policy: case[field].as_bool().unwrap(),
            };
            assert_eq!(
                grants("candidate_policy").bounded_by(&grants("stored_policy"), &grants("held_policy")),
                case["accepted"].as_bool().unwrap(),
                "{case}"
            );
        }
    }

    #[test]
    fn a_non_holder_cannot_select_the_policy_category() {
        let tools = serde_json::json!({"self_config": {
            "enable_self_config": true,
            "self_config_categories": ["agent", "tools", "profile", "policy"],
        }});
        let tools = tools.as_object().unwrap();
        let refused = guard_tools_keep_grants(&OperatorGrants::default(), None, tools)
            .unwrap_err()
            .to_string();
        assert!(refused.contains("the policy category"), "{refused}");
        let holder = OperatorGrants {
            pack_install: false,
            policy: true,
        };
        guard_tools_keep_grants(&holder, None, tools).unwrap();
    }
}
