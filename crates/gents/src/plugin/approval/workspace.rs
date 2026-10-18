use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

mod context;
pub(crate) use context::{carry, for_request, scope};

use gents_loop::tool_policy::workspace_permissions::{self as policy, Grant, Input, Review, Route};

pub use gents_loop::tool_policy::workspace_permissions::{Mode, Preference, Scope};

pub const CONTRACT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Support {
    pub contract_version: u32,
    pub workspace_sandbox: bool,
}

pub fn support() -> Support {
    Support {
        contract_version: CONTRACT_VERSION,
        workspace_sandbox: crate::toolset::workspace_write_sandbox_enforced(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceKey {
    pub node_did: String,
    pub requester_did: String,
    pub agent_id: String,
    pub path: PathBuf,
}

impl WorkspaceKey {
    /// These paths scope host consent; node identity and document authorization
    /// continue to use the existing DID and ACP owners.
    pub fn canonical(
        node_did: &str,
        requester_did: &str,
        agent_id: &str,
        path: &Path,
    ) -> Result<Self> {
        anyhow::ensure!(
            !node_did.trim().is_empty()
                && !requester_did.trim().is_empty()
                && !agent_id.trim().is_empty(),
            "Choose a runtime and agent before changing permissions."
        );
        anyhow::ensure!(path.is_absolute(), "Choose an absolute workspace folder.");
        let path = path
            .canonicalize()
            .context("resolving the workspace folder")?;
        anyhow::ensure!(path.is_dir(), "Choose a workspace folder.");
        Ok(Self {
            node_did: node_did.to_owned(),
            requester_did: requester_did.to_owned(),
            agent_id: agent_id.to_owned(),
            path,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub workspace: WorkspaceKey,
    pub selection: Preference<String>,
    #[serde(default)]
    pub dirs: Vec<crate::plugin::allowed::AllowedDir>,
}

impl Settings {
    pub fn scope(&self) -> Scope<WorkspaceKey> {
        Scope {
            workspace: self.workspace.clone(),
            revision: self.selection.revision,
        }
    }
}

/// The canonical target and its kind are frozen when consent is requested.
/// Both must still match before an answer can authorize the bound action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BindingAction {
    pub plugin: String,
    pub artifact_digest: String,
    pub input_digest: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub access: crate::pack::BindAccess,
}

pub(crate) type Binding = Grant<WorkspaceKey, BindingAction>;

pub(crate) struct BindingDecision {
    pub route: Route,
    pub binding: Binding,
}

#[derive(Debug, thiserror::Error)]
#[error("Workspace permissions changed. Retry the action with the current permissions.")]
pub(crate) struct SelectionChanged;

pub(crate) fn validate_binding(home: &Path, binding: &Binding) -> Result<Settings> {
    let settings = get(home, &binding.scope.workspace)?;
    let available: Vec<_> = settings.iter().map(Settings::scope).collect();
    if !policy::approval_scope_matches(&available, &binding.scope) {
        return Err(SelectionChanged.into());
    }
    crate::plugin::allowed::canonical_target(&binding.scope.workspace.path, Some(true))?;
    crate::plugin::allowed::canonical_target(&binding.action.path, Some(binding.action.is_dir))?;
    settings.context("workspace permissions are unavailable")
}

pub(crate) fn consume_decision(
    home: &Path,
    expected: &Binding,
    received: &Binding,
    allowed: bool,
) -> Result<bool> {
    let settings = validate_binding(home, expected)?;
    let available = [settings.scope()];
    policy::consume_approval(&available, expected, received, allowed)
        .ok_or_else(|| SelectionChanged.into())
}

pub(crate) fn allowed_scope(
    home: &Path,
    workdir: Option<&Path>,
    user_home: Option<&Path>,
) -> Result<crate::plugin::allowed::Scope> {
    let mut entries = crate::plugin::allowed::list(home)?;
    if let Some(settings) = context::settings(home)? {
        entries.extend(settings.dirs);
    }
    crate::plugin::allowed::Scope::from_entries(home, entries, workdir, user_home)
}

/// Installed plugin admission still enforces its manifold. This decision
/// controls only consent for the exact bound input under that ceiling.
pub(crate) fn binding_route(
    home: &Path,
    action: BindingAction,
    covered: bool,
) -> Result<Option<BindingDecision>> {
    let Some(settings) = context::settings(home)? else {
        return Ok(None);
    };
    let current = settings.scope();
    let input = Input {
        current: current.clone(),
        configured_workspace: settings.workspace,
        mode: settings.selection.mode,
        action: action.clone(),
        ceiling_allows: true,
        crosses_boundary: !covered,
        grant: None,
    };
    let binding = Grant {
        scope: current,
        action,
    };
    let route = policy::reviewed_route(&input, Review::Unavailable, &binding);
    Ok(Some(BindingDecision { route, binding }))
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    entries: Vec<Settings>,
}

pub(in crate::plugin) const FILE: &str = "workspace-approvals.json";
pub(in crate::plugin) const LOCK: &str = "workspace-approvals.lock";

pub fn preferences_file(home: &Path) -> PathBuf {
    home.join(FILE)
}

fn read(home: &Path) -> Result<Stored> {
    match std::fs::read(preferences_file(home)) {
        Ok(bytes) => {
            let stored: Stored = serde_json::from_slice(&bytes)
                .context("the workspace permissions file is unreadable")?;
            let mut keys = std::collections::HashSet::new();
            for entry in &stored.entries {
                anyhow::ensure!(
                    keys.insert(&entry.workspace),
                    "the workspace permissions file contains duplicate scopes"
                );
            }
            Ok(stored)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Stored::default()),
        Err(error) => Err(error).context("reading workspace permissions"),
    }
}

pub fn get(home: &Path, key: &WorkspaceKey) -> Result<Option<Settings>> {
    Ok(read(home)?
        .entries
        .into_iter()
        .find(|entry| entry.workspace == *key))
}

pub fn save_mode(
    home: &Path,
    key: &WorkspaceKey,
    expected_revision: u64,
    mode: Mode,
    review_task_id: Option<&str>,
) -> Result<Settings> {
    let _lock = lock(home)?;
    let mut stored = read(home)?;
    let previous = stored.entries.iter().find(|entry| entry.workspace == *key);
    anyhow::ensure!(
        review_task_id.is_none_or(|id| !id.trim().is_empty()),
        "Choose a valid access reviewer task."
    );
    let selected = gents_loop::tool_policy::workspace_permissions::select_mode(
        previous.map(|entry| &entry.selection),
        expected_revision,
        mode,
        review_task_id.map(str::to_owned),
    )?;
    if !selected.changed {
        return Ok(previous
            .context("unchanged workspace permissions are missing")?
            .clone());
    }
    let settings = Settings {
        workspace: key.clone(),
        selection: selected.next,
        dirs: previous.map_or_else(Vec::new, |entry| entry.dirs.clone()),
    };
    stored.entries.retain(|entry| entry.workspace != *key);
    stored.entries.push(settings.clone());
    save(home, &stored)?;
    Ok(settings)
}

pub(crate) fn grant_at_least(
    home: &Path,
    binding: &Binding,
    path: &Path,
    access: crate::pack::BindAccess,
) -> Result<()> {
    let entry = crate::plugin::allowed::prepare_answered_grant(home, path, access)?;
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
    crate::plugin::allowed::put_entry(&mut settings.dirs, entry, true);
    save(home, &stored)
}

fn lock(home: &Path) -> Result<crate::file_lock::FileLock> {
    std::fs::create_dir_all(home).context("preparing workspace permissions")?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join(LOCK))
        .context("opening the workspace permissions lock")?;
    crate::file_lock::FileLock::exclusive(lock).context("locking workspace permissions")
}

fn save(home: &Path, stored: &Stored) -> Result<()> {
    let staged = tempfile::NamedTempFile::new_in(home).context("staging workspace permissions")?;
    serde_json::to_writer(staged.as_file(), &stored).context("encoding workspace permissions")?;
    staged
        .as_file()
        .sync_all()
        .context("flushing workspace permissions")?;
    staged
        .persist(preferences_file(home))
        .context("saving workspace permissions")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_are_scoped_to_the_exact_node_agent_and_canonical_workspace() -> Result<()> {
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let first = root.path().join("first");
        let second = root.path().join("second");
        std::fs::create_dir_all(&first)?;
        std::fs::create_dir_all(&second)?;
        let key = WorkspaceKey::canonical("node", "requester", "agent", &first)?;
        let full = save_mode(&home, &key, 0, Mode::FullAccess, None)?;
        assert_eq!(
            get(
                &home,
                &WorkspaceKey::canonical("node", "requester", "agent", &first.join("."))?
            )?
            .unwrap()
            .scope(),
            full.scope()
        );
        for other in [
            WorkspaceKey::canonical("node", "requester", "agent", &second)?,
            WorkspaceKey::canonical("other", "requester", "agent", &first)?,
            WorkspaceKey::canonical("node", "requester", "other", &first)?,
            WorkspaceKey::canonical("node", "other", "agent", &first)?,
        ] {
            assert!(get(&home, &other)?.is_none());
        }
        assert!(save_mode(&home, &key, 0, Mode::Ask, None).is_err());
        assert_eq!(get(&home, &key)?.unwrap().selection.mode, Mode::FullAccess);
        let ask = save_mode(&home, &key, full.selection.revision, Mode::Ask, None)?;
        assert!(ask.selection.revision > full.selection.revision);
        assert!(save_mode(&home, &key, ask.selection.revision, Mode::AutoReview, None).is_err());
        assert_eq!(get(&home, &key)?.unwrap().selection.mode, Mode::Ask);
        let review = save_mode(
            &home,
            &key,
            ask.selection.revision,
            Mode::AutoReview,
            Some("review-task"),
        )?;
        let ask = save_mode(&home, &key, review.selection.revision, Mode::Ask, None)?;
        assert_eq!(ask.selection.review_task.as_deref(), Some("review-task"));
        Ok(())
    }

    #[test]
    fn unreadable_settings_are_not_replaced_by_a_more_permissive_default() -> Result<()> {
        let root = tempfile::tempdir()?;
        let key = WorkspaceKey::canonical("node", "requester", "agent", root.path())?;
        std::fs::write(root.path().join(FILE), b"invalid")?;
        assert!(get(root.path(), &key).is_err());
        assert!(save_mode(root.path(), &key, 0, Mode::FullAccess, None).is_err());
        assert_eq!(std::fs::read(root.path().join(FILE))?, b"invalid");
        Ok(())
    }
}
