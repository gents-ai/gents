use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

use super::{
    lock, policy, read, save, Binding, BindingAction, Grant, Scope, SelectionChanged, Settings,
    WorkspaceKey,
};

/// Only the invocation digest is retained: command arguments can contain credentials.
/// The enclosing settings bind node, requester, agent and canonical workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::plugin::approval) struct RememberedCommandGrant {
    pub(super) revision: u64,
    action_digest: String,
}

pub(in crate::plugin::approval) fn command_grant(
    binding: &Binding,
) -> Result<RememberedCommandGrant> {
    anyhow::ensure!(
        matches!(binding.action, BindingAction::Command { .. }),
        "Only command invocations can be remembered."
    );
    Ok(RememberedCommandGrant {
        revision: binding.scope.revision,
        action_digest: format!("{:x}", Sha256::digest(serde_json::to_vec(&binding.action)?)),
    })
}

pub(in crate::plugin::approval) fn has_command_grant(
    settings: &Settings,
    binding: &Binding,
) -> Result<bool> {
    if !matches!(binding.action, BindingAction::Command { .. }) {
        return Ok(false);
    }
    let expected = command_grant(binding)?;
    let grants: Vec<_> = settings
        .command_grants
        .iter()
        .map(|entry| Grant {
            scope: Scope {
                workspace: settings.workspace.clone(),
                revision: entry.revision,
            },
            action: entry.action_digest.clone(),
        })
        .collect();
    Ok(policy::has_command_grant(
        &grants,
        &Grant {
            scope: binding.scope.clone(),
            action: expected.action_digest,
        },
    ))
}

pub fn revoke_commands(
    home: &Path,
    key: &WorkspaceKey,
    expected_revision: u64,
) -> Result<Settings> {
    let _lock = lock(home)?;
    let mut stored = read(home)?;
    let settings = stored
        .entries
        .iter_mut()
        .find(|entry| entry.workspace == *key)
        .ok_or(SelectionChanged)?;
    let selected = policy::revoke_command_grants(&settings.selection, expected_revision)?;
    settings.selection = selected.next;
    settings.command_grants.clear();
    let result = settings.clone();
    save(home, &stored)?;
    Ok(result)
}

pub(in crate::plugin::approval) fn remember_command(home: &Path, binding: &Binding) -> Result<()> {
    let grant = command_grant(binding)?;
    let _lock = lock(home)?;
    let mut stored = read(home)?;
    let settings = stored
        .entries
        .iter_mut()
        .find(|entry| entry.workspace == binding.scope.workspace)
        .ok_or(SelectionChanged)?;
    if !policy::approval_scope_matches(&[settings.scope()], &binding.scope) {
        return Err(SelectionChanged.into());
    }
    binding.action.validate()?;
    if !has_command_grant(settings, binding)? {
        settings.command_grants.push(grant);
    }
    save(home, &stored)
}
