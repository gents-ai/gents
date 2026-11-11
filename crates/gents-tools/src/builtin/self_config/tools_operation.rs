//! Saving a Tools document: the one admission every client's save passes
//! (Lean `SelfConfig.ToolsSave`). A User's save runs here whole; an Agent's
//! runs inside its anchored patch and calls the same admission.

use std::collections::BTreeSet;

use gents_documents::operations::write::{candidate_in_txn, publish_in_txn, DocumentWrite};
use gents_documents::operations::OperationFailure;
use gents_documents::patch::SelfConfigTarget;
use gents_protocol::operations::document::{
    DocumentOperationError, DocumentReceipt, OperationActor,
};
use gents_protocol::operations::tools::{
    SettingChange, SharedOutcome, ToolsOperationError, ToolsReach, ToolsSaveInput, ToolsSaveOutcome,
};
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};
use serde_json::{Map, Value};

use super::ops::{shared_referrers, validate_merged_selection, OperatorGrants};

pub type ToolsFailure = OperationFailure<ToolsOperationError>;

/// Lean `SelfConfig.ToolsSave.holdsOperatorGrant`.
pub fn holds_operator_grant(actor: OperationActor, agent_holds: bool) -> bool {
    actor == OperationActor::User || agent_holds
}

/// Lean `SelfConfig.ToolsSave.ownControlBinds`.
pub fn own_control_binds(actor: OperationActor, own_tools: bool, no_lockout: bool) -> bool {
    actor == OperationActor::Agent && own_tools && no_lockout
}

/// Lean `SelfConfig.ToolsSave.sharedOutcome`.
pub fn decide_shared(reach: ToolsReach, acknowledged: bool, holds_catalog: bool) -> SharedOutcome {
    match reach {
        ToolsReach::Several if !acknowledged => SharedOutcome::RefusedUnacknowledged,
        ToolsReach::Several if !holds_catalog => SharedOutcome::RefusedWithoutCatalog,
        _ => SharedOutcome::Proceed,
    }
}

/// Lean `SelfConfig.ToolsSave.outcome`.
pub fn decide_tools_save(
    shared: SharedOutcome,
    grants_held: bool,
    drops_allowed: bool,
    plugins_installed: bool,
    keeps_own_control: bool,
) -> ToolsSaveOutcome {
    if shared != SharedOutcome::Proceed {
        ToolsSaveOutcome::RefusedShared
    } else if !grants_held {
        ToolsSaveOutcome::RefusedGrantNotHeld
    } else if !drops_allowed {
        ToolsSaveOutcome::RefusedDropsSettings
    } else if !plugins_installed {
        ToolsSaveOutcome::RefusedPluginNotInstalled
    } else if !keeps_own_control {
        ToolsSaveOutcome::RefusedLosesOwnControl
    } else {
        ToolsSaveOutcome::Saved
    }
}

/// Lean `SelfConfig.ToolsSave.dropRefused`.
pub fn drop_refused(change: SettingChange, group_allowed: bool) -> bool {
    change == SettingChange::Removed && !group_allowed
}

fn removed_settings(path: &str, before: &Value, after: &Value, out: &mut Vec<String>) {
    match (before, after) {
        (Value::Null, _) => {}
        (Value::Object(before), Value::Object(after)) => {
            for (key, value) in before {
                removed_settings(
                    &format!("{path}.{key}"),
                    value,
                    after.get(key).unwrap_or(&Value::Null),
                    out,
                );
            }
        }
        (Value::Array(before), Value::Array(after)) => {
            for item in before.iter().filter(|item| !after.contains(item)) {
                let moved = (path == "integrations.plugins")
                    .then(|| item.get("plugin").filter(|plugin| plugin.is_string()))
                    .flatten()
                    .and_then(|plugin| {
                        after
                            .iter()
                            .find(|candidate| candidate.get("plugin") == Some(plugin))
                            .map(|candidate| (plugin, candidate))
                    });
                match moved {
                    Some((plugin, candidate)) => {
                        removed_settings(&format!("{path} item {plugin}"), item, candidate, out)
                    }
                    None => out.push(format!("{path} item {item}")),
                }
            }
        }
        (_, Value::Null) => out.push(path.to_owned()),
        _ => {}
    }
}

/// The stored settings a save removes from the groups it rewrites without the
/// call allowing the drop for that group.
pub fn refused_drops(
    groups: &[String],
    allow_drop: &BTreeSet<String>,
    stored: &Map<String, Value>,
    merged: &Map<String, Value>,
) -> Vec<String> {
    let mut dropped = Vec::new();
    for group in groups {
        let mut removed = Vec::new();
        removed_settings(
            group,
            stored.get(group).unwrap_or(&Value::Null),
            merged.get(group).unwrap_or(&Value::Null),
            &mut removed,
        );
        let allowed = allow_drop.contains(group);
        dropped.extend(
            removed
                .into_iter()
                .filter(|_| drop_refused(SettingChange::Removed, allowed)),
        );
    }
    dropped
}

fn selected_plugins(document: &Map<String, Value>) -> BTreeSet<&str> {
    document
        .get("integrations")
        .and_then(|integrations| integrations.get("plugins"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("plugin")?.as_str())
        .collect()
}

/// Who saves, as the admission reads it.
pub struct ToolsActor<'a> {
    pub actor: OperationActor,
    /// The operator grants an Agent holds. A User holds every one.
    pub agent_held: OperatorGrants,
    /// `nodes manage_agents` on the Node, as ACP answers for the actor.
    pub holds_catalog: bool,
    /// Whether this node has `plugin` installed. `None` when the caller
    /// cannot observe the node's plugin home.
    pub installed: Option<&'a (dyn Fn(&str) -> anyhow::Result<()> + Send + Sync)>,
}

impl ToolsActor<'_> {
    fn held(&self) -> OperatorGrants {
        OperatorGrants {
            pack_install: holds_operator_grant(self.actor, self.agent_held.pack_install),
            policy: holds_operator_grant(self.actor, self.agent_held.policy),
            pack_authority: holds_operator_grant(self.actor, self.agent_held.pack_authority),
        }
    }
}

/// What the admission reads about one save.
pub struct ToolsCandidate<'a> {
    /// The groups the save's patch names.
    pub groups: &'a [String],
    pub allow_drop: &'a BTreeSet<String>,
    pub shared_acknowledged: bool,
    pub stored: Option<&'a Map<String, Value>>,
    pub merged: &'a Map<String, Value>,
    /// Every Agent that reaches these Tools.
    pub agents: &'a [String],
    /// The no-lockout rule's answer where `own_control_binds`, else `Ok`.
    pub own_control: anyhow::Result<()>,
}

/// Lean `SelfConfig.ToolsSave.outcome` over the facts of one save: the first
/// refusal, or admission.
pub fn admit_tools_save(
    actor: &ToolsActor<'_>,
    candidate: ToolsCandidate<'_>,
) -> anyhow::Result<Result<(), ToolsOperationError>> {
    let reach = match candidate.agents.len() {
        0 => ToolsReach::Unselected,
        1 => ToolsReach::One,
        _ => ToolsReach::Several,
    };
    let shared = decide_shared(reach, candidate.shared_acknowledged, actor.holds_catalog);
    let empty = Map::new();
    let stored = candidate.stored.unwrap_or(&empty);
    let unheld = OperatorGrants::from_tools_json(candidate.merged)?
        .unheld_by(&OperatorGrants::from_tools_json(stored)?, &actor.held());
    let dropped = refused_drops(
        candidate.groups,
        candidate.allow_drop,
        stored,
        candidate.merged,
    );
    let before = selected_plugins(stored);
    let uninstalled = actor.installed.and_then(|installed| {
        selected_plugins(candidate.merged)
            .difference(&before)
            .find_map(|plugin| {
                installed(plugin)
                    .err()
                    .map(|error| ((*plugin).to_owned(), format!("{error:#}")))
            })
    });
    let outcome = decide_tools_save(
        shared,
        unheld.is_empty(),
        dropped.is_empty(),
        uninstalled.is_none(),
        candidate.own_control.is_ok(),
    );
    Ok(match outcome {
        ToolsSaveOutcome::Saved => Ok(()),
        ToolsSaveOutcome::RefusedShared => Err(match shared {
            SharedOutcome::RefusedWithoutCatalog => ToolsOperationError::SharedWithoutCatalog {
                agents: candidate.agents.to_vec(),
            },
            _ => ToolsOperationError::SharedUnacknowledged {
                agents: candidate.agents.to_vec(),
            },
        }),
        ToolsSaveOutcome::RefusedGrantNotHeld => Err(ToolsOperationError::GrantNotHeld {
            grants: unheld.into_iter().map(ToOwned::to_owned).collect(),
        }),
        ToolsSaveOutcome::RefusedLosesOwnControl => Err(ToolsOperationError::LosesOwnControl {
            source: candidate
                .own_control
                .err()
                .map(anyhow::Error::reallocate_into_boxed_dyn_error_without_backtrace)
                .unwrap_or_else(|| "the save would remove this agent's config access".into()),
        }),
        ToolsSaveOutcome::RefusedDropsSettings => {
            let mut groups = dropped
                .iter()
                .map(|path| path.split(['.', ' ']).next().unwrap_or_default().to_owned())
                .collect::<Vec<_>>();
            groups.dedup();
            Err(ToolsOperationError::DropsSettings { dropped, groups })
        }
        ToolsSaveOutcome::RefusedPluginNotInstalled => {
            let (plugin, reason) = uninstalled.unwrap_or_default();
            Err(ToolsOperationError::PluginNotInstalled { plugin, reason })
        }
    })
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ToolsReceipt {
    #[serde(flatten)]
    pub document: DocumentReceipt,
    /// Every Agent the save reaches, when more than one does.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub shared_with: Vec<String>,
}

fn invalid(error: anyhow::Error) -> ToolsFailure {
    OperationFailure::Operation(ToolsOperationError::Document(
        DocumentOperationError::Invalid {
            collection: SelfConfigTarget::Tools.collection_name(),
            source: error.reallocate_into_boxed_dyn_error_without_backtrace(),
        },
    ))
}

async fn save_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    input: &ToolsSaveInput,
    actor: &ToolsActor<'_>,
    commit: bool,
) -> Result<ToolsReceipt, ToolsFailure> {
    let write = DocumentWrite {
        target: SelfConfigTarget::Tools,
        owner,
        id: &input.tools_id,
        mode: input.mode,
        patch: &input.patch,
        node_binding: None,
    };
    let candidate = candidate_in_txn(txn, &write)
        .await
        .map_err(|failure| failure.map(ToolsOperationError::Document))?;
    validate_merged_selection(&candidate.merged).map_err(invalid)?;
    let agents = if candidate.created {
        Vec::new()
    } else {
        shared_referrers(txn, SelfConfigTarget::Tools, owner, &input.tools_id).await?
    };
    let groups = input
        .patch
        .iter()
        .map(|(field, _)| field.clone())
        .collect::<Vec<_>>();
    admit_tools_save(
        actor,
        ToolsCandidate {
            groups: &groups,
            allow_drop: &input.allow_drop,
            shared_acknowledged: input.shared_acknowledged,
            stored: (!candidate.created).then_some(&candidate.stored),
            merged: &candidate.merged,
            agents: &agents,
            own_control: Ok(()),
        },
    )?
    .map_err(OperationFailure::Operation)?;
    let document = publish_in_txn(
        txn,
        &write,
        &candidate,
        &gents_inference::resolved::ProviderProfilePolicy,
        commit,
    )
    .await
    .map_err(|failure| failure.map(ToolsOperationError::Document))?;
    Ok(ToolsReceipt {
        document,
        shared_with: if agents.len() > 1 { agents } else { Vec::new() },
    })
}

/// Saves the Tools `input` names for `owner` as whoever `access` signs for,
/// or with `commit` false reports what the save would do.
pub async fn save_tools(
    access: &ConfigAccess,
    owner: &str,
    input: &ToolsSaveInput,
    actor: &ToolsActor<'_>,
    commit: bool,
) -> Result<ToolsReceipt, ToolsFailure> {
    if commit {
        access
            .transact_write("operations.tools.save", |txn| {
                Box::pin(save_in_txn(txn, owner, input, actor, commit))
            })
            .await
    } else {
        access
            .transact_readonly("operations.tools.preview", |txn| {
                Box::pin(save_in_txn(txn, owner, input, actor, commit))
            })
            .await
    }
}

pub fn finish(failure: ToolsFailure) -> ToolsOperationError {
    failure.finish(|error| DocumentOperationError::Passthrough(error).into())
}

#[cfg(test)]
mod conformance {
    use gents_lean_vocab::families;

    use super::*;

    #[test]
    fn tools_save_admission_matches_lean() {
        families::self_config_tools_save_holds_operator_grant::replay(|i| {
            holds_operator_grant(i.actor, i.agent_holds)
        });
        families::self_config_tools_save_own_control_binds::replay(|i| {
            own_control_binds(i.actor, i.own_tools, i.no_lockout)
        });
        families::self_config_tools_save_shared_outcome::replay(|i| {
            decide_shared(i.reach, i.acknowledged, i.holds_catalog)
        });
        families::self_config_tools_save_outcome::replay(|i| {
            decide_tools_save(
                i.shared,
                i.grants_held,
                i.drops_allowed,
                i.plugins_installed,
                i.keeps_own_control,
            )
        });
        families::self_config_tools_save_drop_refused::replay(|i| {
            drop_refused(i.change, i.group_allowed)
        });
    }

    #[test]
    fn a_moved_plugin_pin_is_a_change_and_a_removed_selection_is_a_drop() {
        use serde_json::json;
        let dropped = |before: Value, after: Value| {
            let stored = json!({"integrations": before});
            let merged = json!({"integrations": after});
            refused_drops(
                &["integrations".to_owned()],
                &BTreeSet::new(),
                stored.as_object().unwrap(),
                merged.as_object().unwrap(),
            )
        };
        let pinned = |digest: &str| json!({"plugin": "node/engine", "digest": digest});
        let other = json!({"plugin": "node/other"});
        assert_eq!(
            dropped(
                json!({"plugins": [pinned("sha256:a"), other]}),
                json!({"plugins": [other, pinned("sha256:b")]}),
            ),
            Vec::<String>::new()
        );
        assert_eq!(
            dropped(
                json!({"plugins": [pinned("sha256:a"), other]}),
                json!({"plugins": [pinned("sha256:b")]}),
            ),
            ["integrations.plugins item {\"plugin\":\"node/other\"}"]
        );
        assert_eq!(
            dropped(
                json!({"plugins": [{"plugin": "node/engine", "digest": "sha256:a", "job": {}}]}),
                json!({"plugins": [pinned("sha256:b")]}),
            ),
            ["integrations.plugins item \"node/engine\".job"]
        );
    }
}
