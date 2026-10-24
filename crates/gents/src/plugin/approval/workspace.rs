use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

mod command;
mod context;
mod file;
mod review_task;
pub(crate) use command::guard_home as command_guard_home;
pub(crate) use command::{authorize_command, writable_root as command_writable_root};
pub(crate) use context::{carry, for_request, scope, ReviewInvocation, Reviewer};
pub(crate) use file::authorize_file_mutation;
pub use review_task::{
    default_review_task, review_task_is_eligible, review_task_matches_selection,
    review_task_policy, ReviewTaskSelection,
};

use gents_loop::tool_policy::workspace_permissions::{self as policy, Grant, Input, Review, Route};

pub use gents_loop::tool_policy::workspace_permissions::{Mode, Preference, Scope, SelectionError};

use super::PluginApprovalError;

type Result<T, E = PluginApprovalError> = std::result::Result<T, E>;

pub const CONTRACT_VERSION: u32 = 1;
pub const STATUS_SUPPORT_FIELD: &str = "workspace_permissions";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(rename = "WorkspacePermissionSupport"))]
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
        if node_did.trim().is_empty()
            || requester_did.trim().is_empty()
            || agent_id.trim().is_empty()
        {
            return Err(PluginApprovalError::ChooseRuntime);
        }
        if !path.is_absolute() {
            return Err(PluginApprovalError::AbsoluteWorkspace);
        }
        let path = path
            .canonicalize()
            .map_err(PluginApprovalError::ResolveWorkspace)?;
        if !path.is_dir() {
            return Err(PluginApprovalError::NotWorkspaceFolder);
        }
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
    pub selection: Preference<ReviewTaskSelection>,
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
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(crate) enum BindingAction {
    Plugin {
        plugin: String,
        artifact_digest: String,
        input_digest: String,
        path: PathBuf,
        is_dir: bool,
        access: crate::pack::BindAccess,
    },
    Command {
        argv: Vec<String>,
        cwd: PathBuf,
        root: PathBuf,
        writable_root: Option<PathBuf>,
        policy: crate::toolset::CommandExecutionPolicy,
    },
    FileMutation {
        tool: String,
        input_digest: String,
        path: PathBuf,
        root: PathBuf,
        existed: bool,
    },
}

impl BindingAction {
    pub(super) fn filesystem(&self) -> Option<super::FilesystemApproval<'_>> {
        match self {
            Self::Plugin {
                plugin,
                path,
                is_dir,
                access,
                ..
            } => Some(super::FilesystemApproval::new(
                plugin, path, *is_dir, *access,
            )),
            Self::FileMutation { tool, path, .. } => Some(super::FilesystemApproval::new(
                match tool.as_str() {
                    "write_file" => "Write file",
                    "edit_file" => "Edit file",
                    "lsp_edit" => "Language-server edit",
                    _ => tool,
                },
                path,
                false,
                crate::pack::BindAccess::ReadWrite,
            )),
            Self::Command { .. } => None,
        }
    }

    pub(super) fn validate(&self) -> Result<()> {
        match self {
            Self::Plugin { path, is_dir, .. } => {
                crate::plugin::allowed::canonical_target(path, Some(*is_dir))?;
            }
            Self::Command {
                argv,
                cwd,
                root,
                writable_root,
                ..
            } => {
                if argv.is_empty() || argv[0].trim().is_empty() {
                    return Err(PluginApprovalError::NoExecutable);
                }
                crate::plugin::allowed::canonical_target(root, Some(true))?;
                crate::plugin::allowed::canonical_target(cwd, Some(true))?;
                if let Some(writable) = writable_root {
                    crate::plugin::allowed::canonical_target(writable, Some(true))?;
                    if !writable.starts_with(root) {
                        return Err(PluginApprovalError::WritableRootOutside);
                    }
                }
                if !cwd.starts_with(root) {
                    return Err(PluginApprovalError::CwdOutsideRoot);
                }
            }
            Self::FileMutation {
                path,
                root,
                existed,
                ..
            } => {
                crate::plugin::allowed::canonical_target(root, Some(true))?;
                let context = crate::toolset::ToolContext::new(root.clone(), false)?;
                let resolved = context.resolve_path_allow_create(
                    path.to_str().ok_or(PluginApprovalError::PathNotUtf8)?,
                )?;
                if !(resolved == *path && path.try_exists()? == *existed) {
                    return Err(PluginApprovalError::FileTargetChanged);
                }
                if *existed && !path.is_file() {
                    return Err(PluginApprovalError::FileTargetNotRegular);
                }
            }
        }
        Ok(())
    }
}

pub(crate) type Binding = Grant<WorkspaceKey, BindingAction>;

#[derive(Clone, Debug)]
pub(crate) struct ActionConsent {
    home: PathBuf,
    binding: Binding,
}

impl ActionConsent {
    /// Consent can outlive an approval wait or a plugin host round. Recheck
    /// the captured workspace revision and action before each mutation or run.
    pub(crate) fn validate(&self) -> Result<()> {
        validate_binding(&self.home, &self.binding)?;
        Ok(())
    }

    pub(crate) fn validate_plugin(
        &self,
        plugin: &str,
        artifact_digest: &str,
        input_digest: &str,
    ) -> Result<()> {
        self.validate()?;
        if !matches!(&self.binding.action, BindingAction::Plugin {
            plugin: expected_plugin,
            artifact_digest: expected_artifact,
            input_digest: expected_input,
            ..
        } if expected_plugin == plugin
            && expected_artifact == artifact_digest
            && expected_input == input_digest)
        {
            return Err(PluginApprovalError::PluginChanged);
        }
        Ok(())
    }
}

pub(crate) fn action_consent(home: &Path, binding: Binding) -> ActionConsent {
    ActionConsent {
        home: home.to_owned(),
        binding,
    }
}

pub(crate) fn within_workspace(path: &Path) -> bool {
    context::within_workspace(path)
}

pub(crate) struct BindingDecision {
    pub route: Route,
    pub binding: Binding,
    input: Input<WorkspaceKey, BindingAction>,
    review_payload: Option<serde_json::Value>,
}

async fn authorize_binding(
    home: &Path,
    decision: BindingDecision,
    denied: &str,
) -> Result<Binding> {
    let session_id = crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
        .and_then(|context| context.session_id);
    let binding = decision.binding.clone();
    let allowed = consent_binding(home, decision, super::interactive(), session_id).await?;
    if !allowed {
        return Err(PluginApprovalError::NotApproved {
            message: denied.to_owned(),
        });
    }
    Ok(binding)
}

pub(crate) async fn consent_binding(
    home: &Path,
    decision: BindingDecision,
    interactive: bool,
    session_id: Option<String>,
) -> Result<bool> {
    use super::{Consent, Request};

    let bounds = crate::tool_call_lifecycle::runtime::tool_execution_bounds(super::WAIT);
    let deadline = bounds
        .deadline_at
        .ok_or(PluginApprovalError::DeadlineMissing)?;
    let remaining = deadline
        .signed_duration_since(chrono::Utc::now())
        .to_std()
        .unwrap_or_default();
    let mut request = Request::for_binding(decision.binding.clone(), session_id);
    request.expires_at_ms = request
        .expires_at_ms
        .min(u64::try_from(deadline.timestamp_millis()).unwrap_or_default());
    let consent = async {
        let ensure_open = || -> Result<()> {
            if bounds.cancellation_token.is_cancelled() {
                return Err(PluginApprovalError::Cancelled);
            }
            if chrono::Utc::now() >= deadline {
                return Err(PluginApprovalError::RequestDeadline);
            }
            Ok(())
        };
        ensure_open()?;
        validate_binding(home, &decision.binding)?;
        let route = context::resolve_review(home, &decision, &request.id).await?;
        ensure_open()?;
        let allowed = match route {
            Route::Allow => true,
            Route::Deny => false,
            Route::Ask | Route::Review => match super::consent(interactive) {
                Consent::Deny => false,
                Consent::Ask => super::ask(home, &request, remaining).await?,
            },
        };
        ensure_open()?;
        validate_binding(home, &decision.binding)?;
        Ok(allowed)
    };
    tokio::select! {
        biased;
        _ = bounds.cancellation_token.cancelled() => Err(PluginApprovalError::Cancelled),
        result = tokio::time::timeout(remaining, consent) => {
            result.map_err(PluginApprovalError::ApprovalTimeout)?
        }
    }
}

pub(crate) fn validate_binding(home: &Path, binding: &Binding) -> Result<Settings> {
    let settings = get(home, &binding.scope.workspace)?;
    let available: Vec<_> = settings.iter().map(Settings::scope).collect();
    if !policy::approval_scope_matches(&available, &binding.scope) {
        return Err(PluginApprovalError::SelectionChanged);
    }
    crate::plugin::allowed::canonical_target(&binding.scope.workspace.path, Some(true))?;
    binding.action.validate()?;
    settings.ok_or(PluginApprovalError::WorkspaceUnavailable)
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
        .ok_or(PluginApprovalError::SelectionChanged)
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
    Ok(crate::plugin::allowed::Scope::from_entries(
        home, entries, workdir, user_home,
    )?)
}

/// Installed plugin admission still enforces its manifold. This decision
/// controls only consent for the exact bound input under that ceiling.
pub(crate) fn binding_route(
    home: &Path,
    action: impl FnOnce() -> Result<BindingAction>,
    covered: bool,
    payload: impl FnOnce() -> Result<serde_json::Value>,
) -> Result<Option<BindingDecision>> {
    let Some(settings) = context::settings(home)? else {
        return Ok(None);
    };
    let action = action()?;
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
    let route = policy::route(&input);
    let review_payload = (route == Route::Review).then(payload).transpose()?;
    Ok(Some(BindingDecision {
        route,
        binding,
        input,
        review_payload,
    }))
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    entries: Vec<Settings>,
}

pub(in crate::plugin) const FILE: &str = crate::home::WORKSPACE_APPROVALS_FILE_NAME;
pub(in crate::plugin) const LOCK: &str = crate::home::WORKSPACE_APPROVALS_LOCK_FILE_NAME;

pub fn preferences_file(home: &Path) -> PathBuf {
    home.join(FILE)
}

fn read(home: &Path) -> Result<Stored> {
    match std::fs::read(preferences_file(home)) {
        Ok(bytes) => {
            let stored: Stored = serde_json::from_slice(&bytes)
                .map_err(PluginApprovalError::UnreadableWorkspaceFile)?;
            let mut keys = rapidhash::RapidHashSet::default();
            for entry in &stored.entries {
                if !keys.insert(&entry.workspace) {
                    return Err(PluginApprovalError::DuplicateScopes);
                }
            }
            Ok(stored)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Stored::default()),
        Err(error) => Err(PluginApprovalError::ReadWorkspace(error)),
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
    review_task: Option<&ReviewTaskSelection>,
) -> Result<Settings> {
    let _lock = lock(home)?;
    let mut stored = read(home)?;
    let previous = stored.entries.iter().find(|entry| entry.workspace == *key);
    if !review_task.is_none_or(|selection| {
        !selection.task_id.trim().is_empty() && !selection.policy_digest.is_empty()
    }) {
        return Err(PluginApprovalError::InvalidReviewTask);
    }
    let selected = gents_loop::tool_policy::workspace_permissions::select_mode(
        previous.map(|entry| &entry.selection),
        expected_revision,
        mode,
        review_task.cloned(),
    )?;
    if !selected.changed {
        return previous
            .cloned()
            .ok_or(PluginApprovalError::UnchangedMissing);
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
        .ok_or(PluginApprovalError::SelectionChanged)?;
    if !policy::approval_scope_matches(&[settings.scope()], &binding.scope) {
        return Err(PluginApprovalError::SelectionChanged);
    }
    crate::plugin::allowed::put_entry(&mut settings.dirs, entry, true);
    save(home, &stored)
}

fn lock(home: &Path) -> Result<crate::file_lock::FileLock> {
    std::fs::create_dir_all(home).map_err(PluginApprovalError::PrepareWorkspace)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join(LOCK))
        .map_err(PluginApprovalError::WorkspaceLockOpen)?;
    crate::file_lock::FileLock::exclusive(lock).map_err(PluginApprovalError::WorkspaceLock)
}

fn save(home: &Path, stored: &Stored) -> Result<()> {
    crate::plugin::allowed::write_permission_file(home, &preferences_file(home), |file| {
        serde_json::to_writer(file, &stored).map_err(PluginApprovalError::EncodeWorkspace)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_workspace_permission_writes_preserve_selected_policy_versions() -> Result<()> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Case {
            previous: Option<Preference<u64>>,
            expected: u64,
            mode: Mode,
            requested_task: Option<u64>,
            accepted: bool,
            changed: bool,
            next: Option<Preference<u64>>,
        }
        fn selection(id: u64) -> ReviewTaskSelection {
            let mut task = default_review_task("node", "agent");
            task.prompt_template = format!("Review policy {id}.");
            ReviewTaskSelection::for_task(&task)
        }
        fn preference(value: Preference<u64>) -> Preference<ReviewTaskSelection> {
            Preference {
                mode: value.mode,
                revision: value.revision,
                review_task: value.review_task.map(selection),
            }
        }
        let directory = tempfile::tempdir()?;
        let home = directory.path().join("home");
        std::fs::create_dir(&home)?;
        let key = WorkspaceKey::canonical("node", "requester", "agent", directory.path())?;
        let cases =
            &crate::lean_vocab_test::lean_contract_snapshot().workspace_permission_write_cases;
        assert!(!cases.is_empty());
        for raw in cases {
            let case: Case = serde_json::from_value(raw.clone())?;
            let previous = case.previous.map(preference);
            let stored = Stored {
                entries: previous
                    .clone()
                    .map(|selection| Settings {
                        workspace: key.clone(),
                        selection,
                        dirs: Vec::new(),
                    })
                    .into_iter()
                    .collect(),
            };
            save(&home, &stored)?;
            let task = case.requested_task.map(selection);
            let result = save_mode(&home, &key, case.expected, case.mode, task.as_ref());
            assert_eq!(result.is_ok(), case.accepted, "{raw}");
            let observed = get(&home, &key)?.map(|settings| settings.selection);
            assert_eq!(observed, case.next.map(preference), "{raw}");
            assert_eq!(observed != previous, case.changed, "{raw}");
        }
        Ok(())
    }

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
        let selected = ReviewTaskSelection::for_task(&crate::document_config::Task {
            task_id: "review-task".into(),
            ..default_review_task("node", "agent")
        });
        let review = save_mode(
            &home,
            &key,
            ask.selection.revision,
            Mode::AutoReview,
            Some(&selected),
        )?;
        let ask = save_mode(&home, &key, review.selection.revision, Mode::Ask, None)?;
        assert_eq!(ask.selection.review_task.as_ref(), Some(&selected));
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
