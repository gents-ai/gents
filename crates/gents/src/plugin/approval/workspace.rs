use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

mod command;
mod command_grants;
#[cfg(test)]
pub(super) use command_grants::command_grant;
pub use command_grants::revoke_commands;
pub(super) use command_grants::{has_command_grant, remember_command, RememberedCommandGrant};
mod context;
mod file;
mod review_task;
pub(crate) use command::guard_home as command_guard_home;
pub(crate) use command::{authorize_command, writable_root as command_writable_root};
#[cfg(target_os = "macos")]
pub(crate) use context::current_home as command_context_home;
pub(crate) use context::{carry, for_request, scope, ReviewInvocation, Reviewer};
pub(crate) use file::authorize_file_mutation;
pub use review_task::{
    default_review_task, review_task_is_eligible, review_task_matches_selection,
    review_task_policy, ReviewTaskSelection,
};

use gents_loop::tool_policy::workspace_permissions::{self as policy, Grant, Input, Review, Route};

pub use gents_loop::tool_policy::workspace_permissions::{Mode, Preference, Scope, SelectionError};

pub const CONTRACT_VERSION: u32 = 1;
pub const STATUS_SUPPORT_FIELD: &str = "workspace_permissions";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(rename = "WorkspacePermissionSupport"))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Support {
    pub contract_version: u32,
    pub workspace_sandbox: bool,
    #[serde(default)]
    pub remembered_commands: bool,
    /// The runtime keys a selection by session as well as by agent and
    /// workspace. An older runtime omits it and every session resolves to the
    /// agent and workspace default.
    #[serde(default)]
    pub session_scoped: bool,
}

pub fn support() -> Support {
    Support {
        contract_version: CONTRACT_VERSION,
        workspace_sandbox: crate::tools::native::workspace_write_sandbox_enforced(),
        remembered_commands: true,
        session_scoped: true,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceKey {
    pub node_did: String,
    pub requester_did: String,
    pub agent_did: String,
    pub path: PathBuf,
    /// Absent for the agent and workspace default entry, present for one
    /// session's own entry. A file written before sessions were keyed decodes
    /// with it absent, which is the entry a session without its own selection
    /// resolves to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

impl WorkspaceKey {
    /// These paths scope host consent; node identity and document authorization
    /// continue to use the existing DID and ACP owners.
    pub fn canonical(
        node_did: &str,
        requester_did: &str,
        agent_did: &str,
        path: &Path,
    ) -> Result<Self> {
        anyhow::ensure!(
            !node_did.trim().is_empty()
                && !requester_did.trim().is_empty()
                && !agent_did.trim().is_empty(),
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
            agent_did: agent_did.to_owned(),
            path,
            session_id: None,
        })
    }

    /// The agent and workspace default entry's key for this key's workspace.
    pub fn base(&self) -> Self {
        Self {
            session_id: None,
            ..self.clone()
        }
    }

    pub fn with_session(&self, session_id: Option<&str>) -> Result<Self> {
        let session_id = match session_id {
            None => None,
            Some(value) => {
                anyhow::ensure!(
                    !value.trim().is_empty(),
                    "Start the chat before choosing its permissions."
                );
                Some(value.to_owned())
            }
        };
        Ok(Self {
            session_id,
            ..self.clone()
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) command_grants: Vec<RememberedCommandGrant>,
}

impl Settings {
    pub fn remembered_command_count(&self) -> usize {
        self.command_grants
            .iter()
            .filter(|grant| grant.revision == self.selection.revision)
            .count()
    }

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
        policy: crate::tools::native::CommandExecutionPolicy,
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
                anyhow::ensure!(
                    !argv.is_empty() && !argv[0].trim().is_empty(),
                    "the command approval has no executable"
                );
                crate::plugin::allowed::canonical_target(root, Some(true))?;
                crate::plugin::allowed::canonical_target(cwd, Some(true))?;
                if let Some(writable) = writable_root {
                    crate::plugin::allowed::canonical_target(writable, Some(true))?;
                    anyhow::ensure!(
                        writable.starts_with(root),
                        "the writable root exceeds the tool root"
                    );
                }
                anyhow::ensure!(
                    cwd.starts_with(root),
                    "the command working directory is outside its tool root"
                );
            }
            Self::FileMutation {
                path,
                root,
                existed,
                ..
            } => {
                crate::plugin::allowed::canonical_target(root, Some(true))?;
                let context = crate::tools::native::ToolContext::new(root.clone(), false)?;
                let resolved = context.resolve_path_allow_create(
                    path.to_str()
                        .context("the file approval path is not valid UTF-8")?,
                )?;
                anyhow::ensure!(
                    resolved == *path && path.try_exists()? == *existed,
                    "The file approval target changed. Retry the file change."
                );
                anyhow::ensure!(
                    !existed || path.is_file(),
                    "The file approval target is not a regular file."
                );
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
        anyhow::ensure!(
            matches!(&self.binding.action, BindingAction::Plugin {
                plugin: expected_plugin,
                artifact_digest: expected_artifact,
                input_digest: expected_input,
                ..
            } if expected_plugin == plugin
                && expected_artifact == artifact_digest
                && expected_input == input_digest),
            "The plugin invocation changed after approval. Retry the action."
        );
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
    let session_id = crate::tools::call::lifecycle::runtime::current_tool_runtime_context()
        .and_then(|context| context.session_id);
    let binding = decision.binding.clone();
    let allowed = consent_binding(home, decision, super::interactive(), session_id).await?;
    anyhow::ensure!(allowed, "{denied}");
    Ok(binding)
}

pub(crate) async fn consent_binding(
    home: &Path,
    decision: BindingDecision,
    interactive: bool,
    session_id: Option<String>,
) -> Result<bool> {
    use super::{Consent, Request};

    let bounds = crate::tools::call::lifecycle::runtime::tool_execution_bounds(super::WAIT);
    let deadline = bounds.deadline_at.context("approval deadline is missing")?;
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
            anyhow::ensure!(
                !bounds.cancellation_token.is_cancelled(),
                "action cancelled while awaiting approval"
            );
            anyhow::ensure!(
                chrono::Utc::now() < deadline,
                "action approval exceeded the request deadline"
            );
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
        _ = bounds.cancellation_token.cancelled() => anyhow::bail!("action cancelled while awaiting approval"),
        result = tokio::time::timeout(remaining, consent) => {
            result.context("action approval exceeded the request deadline")?
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Workspace permissions changed. Retry the action with the current permissions.")]
pub(crate) struct SelectionChanged;

pub(crate) fn validate_binding(home: &Path, binding: &Binding) -> Result<Settings> {
    let settings = context::governing_settings(home, &binding.scope.workspace)?;
    let available: Vec<_> = settings.iter().map(Settings::scope).collect();
    if !policy::approval_scope_matches(&available, &binding.scope) {
        return Err(SelectionChanged.into());
    }
    crate::plugin::allowed::canonical_target(&binding.scope.workspace.path, Some(true))?;
    binding.action.validate()?;
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
    action: impl FnOnce() -> Result<BindingAction>,
    covered: bool,
    payload: impl FnOnce() -> Result<serde_json::Value>,
) -> Result<Option<BindingDecision>> {
    let Some(settings) = context::settings(home)? else {
        return Ok(None);
    };
    let action = action()?;
    let current = settings.scope();
    let binding = Grant {
        scope: current.clone(),
        action: action.clone(),
    };
    let grant = has_command_grant(&settings, &binding)?.then(|| binding.clone());
    let input = Input {
        current: current.clone(),
        configured_workspace: settings.workspace,
        mode: settings.selection.mode,
        action: action.clone(),
        ceiling_allows: true,
        crosses_boundary: !covered,
        grant,
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

pub(in crate::plugin) const FILE: &str = crate::store::local::home::WORKSPACE_APPROVALS_FILE_NAME;
pub(in crate::plugin) const LOCK: &str =
    crate::store::local::home::WORKSPACE_APPROVALS_LOCK_FILE_NAME;

pub fn preferences_file(home: &Path) -> PathBuf {
    home.join(FILE)
}

fn read(home: &Path) -> Result<Stored> {
    match std::fs::read(preferences_file(home)) {
        Ok(bytes) => {
            let stored: Stored = serde_json::from_slice(&bytes)
                .context("the workspace permissions file is unreadable")?;
            let mut keys = rapidhash::RapidHashSet::default();
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

/// The one reader that chooses which stored selection governs a request.
/// Every reader outside this module goes through it, because an absent entry
/// disengages the workspace consent gate: resolving a session that has made no
/// selection to nothing would silently drop the gate its agent and workspace
/// default configured.
pub fn resolve(
    home: &Path,
    workspace: &WorkspaceKey,
    session_id: Option<&str>,
) -> Result<Option<Settings>> {
    anyhow::ensure!(
        workspace.session_id.is_none(),
        "resolution takes the agent and workspace key, not a session entry's key"
    );
    let session = session_id
        .map(|session_id| workspace.with_session(Some(session_id)))
        .transpose()?;
    let stored = read(home)?;
    Ok(
        policy::resolve(&stored.entries, session.as_ref(), workspace, |entry| {
            &entry.workspace
        })
        .cloned(),
    )
}

/// Revalidation of an already resolved key. Choosing which entry governs an
/// action is `resolve`; this never makes that choice and is reachable only
/// from this module.
fn entry(home: &Path, key: &WorkspaceKey) -> Result<Option<Settings>> {
    Ok(read(home)?
        .entries
        .into_iter()
        .find(|stored| stored.workspace == *key))
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
    let inherited = match (previous.is_none(), key.session_id.is_some()) {
        (true, true) => stored
            .entries
            .iter()
            .find(|entry| entry.workspace == key.base()),
        _ => None,
    };
    let inherited_task = inherited.and_then(|entry| entry.selection.review_task.clone());
    let retained_dirs = previous.or(inherited).map(|entry| entry.dirs.clone());
    anyhow::ensure!(
        review_task
            .is_none_or(|selection| !selection.task_id.trim().is_empty()
                && !selection.policy_digest.is_empty()),
        "Choose a valid access reviewer task."
    );
    let selected = gents_loop::tool_policy::workspace_permissions::select_mode(
        previous.map(|entry| &entry.selection),
        expected_revision,
        mode,
        review_task.cloned().or(inherited_task),
    )?;
    if !selected.changed {
        return Ok(previous
            .context("unchanged workspace permissions are missing")?
            .clone());
    }
    let settings = Settings {
        workspace: key.clone(),
        selection: selected.next,
        dirs: retained_dirs.unwrap_or_default(),
        command_grants: Vec::new(),
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

fn lock(home: &Path) -> Result<crate::store::local::file_lock::FileLock> {
    std::fs::create_dir_all(home).context("preparing workspace permissions")?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join(LOCK))
        .context("opening the workspace permissions lock")?;
    crate::store::local::file_lock::FileLock::exclusive(lock)
        .context("locking workspace permissions")
}

fn save(home: &Path, stored: &Stored) -> Result<()> {
    crate::plugin::allowed::write_permission_file(home, &preferences_file(home), |file| {
        serde_json::to_writer(file, &stored).context("encoding workspace permissions")
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
                        command_grants: Vec::new(),
                    })
                    .into_iter()
                    .collect(),
            };
            save(&home, &stored)?;
            let task = case.requested_task.map(selection);
            let result = save_mode(&home, &key, case.expected, case.mode, task.as_ref());
            assert_eq!(result.is_ok(), case.accepted, "{raw}");
            let observed = resolve(&home, &key, None)?.map(|settings| settings.selection);
            assert_eq!(observed, case.next.map(preference), "{raw}");
            assert_eq!(observed != previous, case.changed, "{raw}");
        }
        Ok(())
    }

    #[test]
    fn modes_are_scoped_to_the_exact_node_agent_canonical_workspace_and_session() -> Result<()> {
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let first = root.path().join("first");
        let second = root.path().join("second");
        std::fs::create_dir_all(&first)?;
        std::fs::create_dir_all(&second)?;
        let key = WorkspaceKey::canonical("node", "requester", "agent", &first)?;
        let full = save_mode(&home, &key, 0, Mode::FullAccess, None)?;
        assert_eq!(
            resolve(
                &home,
                &WorkspaceKey::canonical("node", "requester", "agent", &first.join("."))?,
                None
            )?
            .unwrap()
            .scope(),
            full.scope()
        );
        let others = [
            WorkspaceKey::canonical("node", "requester", "agent", &second)?,
            WorkspaceKey::canonical("other", "requester", "agent", &first)?,
            WorkspaceKey::canonical("node", "requester", "other", &first)?,
            WorkspaceKey::canonical("node", "other", "agent", &first)?,
        ];
        for other in &others {
            assert!(resolve(&home, other, None)?.is_none());
        }
        save_mode(
            &home,
            &key.with_session(Some("chat"))?,
            0,
            Mode::FullAccess,
            None,
        )?;
        for other in &others {
            assert!(resolve(&home, other, Some("chat"))?.is_none());
        }
        assert!(key.with_session(Some("  ")).is_err());
        assert!(save_mode(&home, &key, 0, Mode::Ask, None).is_err());
        assert_eq!(
            resolve(&home, &key, None)?.unwrap().selection.mode,
            Mode::FullAccess
        );
        let ask = save_mode(&home, &key, full.selection.revision, Mode::Ask, None)?;
        assert!(ask.selection.revision > full.selection.revision);
        assert!(save_mode(&home, &key, ask.selection.revision, Mode::AutoReview, None).is_err());
        assert_eq!(
            resolve(&home, &key, None)?.unwrap().selection.mode,
            Mode::Ask
        );
        let selected = ReviewTaskSelection::for_task(&crate::documents::Task {
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

    fn consent_fixture() -> Result<(tempfile::TempDir, PathBuf, PathBuf, WorkspaceKey)> {
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        std::fs::create_dir_all(&home)?;
        let project = root.path().join("project");
        std::fs::create_dir_all(&project)?;
        let project = project.canonicalize()?;
        let key = WorkspaceKey::canonical("node", "requester", "agent", &project)?;
        Ok((root, home, project, key))
    }

    fn command_binding(settings: &Settings, project: &Path) -> Binding {
        Grant {
            scope: settings.scope(),
            action: BindingAction::Command {
                argv: vec!["printf".into(), "remembered".into()],
                cwd: project.to_owned(),
                root: project.to_owned(),
                writable_root: None,
                policy: crate::tools::native::CommandExecutionPolicy::write_capable(),
            },
        }
    }

    #[test]
    fn sessions_select_modes_independently_of_each_other_and_the_default() -> Result<()> {
        let (_root, home, _project, base) = consent_fixture()?;
        let first = save_mode(
            &home,
            &base.with_session(Some("chat-a"))?,
            0,
            Mode::FullAccess,
            None,
        )?;
        let second = save_mode(
            &home,
            &base.with_session(Some("chat-b"))?,
            0,
            Mode::Ask,
            None,
        )?;
        let mode = |session| -> Result<Option<Mode>> {
            Ok(resolve(&home, &base, session)?.map(|settings| settings.selection.mode))
        };
        assert_eq!(mode(Some("chat-a"))?, Some(Mode::FullAccess));
        assert_eq!(mode(Some("chat-b"))?, Some(Mode::Ask));
        assert_eq!(mode(Some("chat-c"))?, None);
        assert_eq!(mode(None)?, None);
        let default = save_mode(&home, &base, 0, Mode::Ask, None)?;
        assert_eq!(
            resolve(&home, &base, Some("chat-c"))?.unwrap().scope(),
            default.scope()
        );
        assert_eq!(
            resolve(&home, &base, None)?.unwrap().scope(),
            default.scope()
        );
        assert_eq!(mode(Some("chat-a"))?, Some(Mode::FullAccess));
        let changed = save_mode(
            &home,
            &base.with_session(Some("chat-a"))?,
            first.selection.revision,
            Mode::Ask,
            None,
        )?;
        assert!(changed.selection.revision > first.selection.revision);
        assert_eq!(
            resolve(&home, &base, Some("chat-b"))?
                .unwrap()
                .selection
                .revision,
            second.selection.revision
        );
        assert_eq!(
            resolve(&home, &base, None)?.unwrap().selection.revision,
            default.selection.revision
        );
        Ok(())
    }

    #[test]
    fn a_delegated_run_resolves_to_the_default_not_the_chat_that_started_it() -> Result<()> {
        let (_root, home, _project, base) = consent_fixture()?;
        save_mode(&home, &base, 0, Mode::Ask, None)?;
        save_mode(
            &home,
            &base.with_session(Some("parent"))?,
            0,
            Mode::FullAccess,
            None,
        )?;
        for session in [Some("delegated"), None] {
            assert_eq!(
                resolve(&home, &base, session)?.unwrap().selection.mode,
                Mode::Ask
            );
        }
        assert_eq!(
            resolve(&home, &base, Some("parent"))?
                .unwrap()
                .selection
                .mode,
            Mode::FullAccess
        );
        Ok(())
    }

    #[test]
    fn a_file_written_without_sessions_still_governs_a_chat_that_never_chose() -> Result<()> {
        let (_root, home, _project, base) = consent_fixture()?;
        let reviewer = ReviewTaskSelection::for_task(&default_review_task("node", "agent"));
        let configured = save_mode(&home, &base, 0, Mode::AutoReview, Some(&reviewer))?;
        let stored = std::fs::read_to_string(preferences_file(&home))?;
        assert!(!stored.contains("sessionId"), "{stored}");
        let governing = resolve(&home, &base, Some("a-chat-that-never-chose"))?.unwrap();
        assert_eq!(governing.scope(), configured.scope());
        assert_eq!(governing.selection.mode, Mode::AutoReview);
        assert_eq!(governing.selection.review_task.as_ref(), Some(&reviewer));
        Ok(())
    }

    #[test]
    fn a_chats_first_selection_keeps_the_default_reviewer_and_folders() -> Result<()> {
        let (_root, home, project, base) = consent_fixture()?;
        let reviewer = ReviewTaskSelection::for_task(&default_review_task("node", "agent"));
        let default = save_mode(&home, &base, 0, Mode::AutoReview, Some(&reviewer))?;
        let granted = crate::plugin::allowed::AllowedDir {
            path: project,
            access: crate::pack::BindAccess::ReadWrite,
        };
        save(
            &home,
            &Stored {
                entries: vec![Settings {
                    dirs: vec![granted.clone()],
                    ..default.clone()
                }],
            },
        )?;
        let chat = save_mode(
            &home,
            &base.with_session(Some("chat"))?,
            0,
            Mode::AutoReview,
            None,
        )?;
        assert_eq!(chat.selection.review_task.as_ref(), Some(&reviewer));
        assert_eq!(chat.dirs, vec![granted]);
        assert_eq!(chat.selection.revision, 1);
        assert_eq!(
            resolve(&home, &base, None)?.unwrap().selection.revision,
            default.selection.revision
        );
        Ok(())
    }

    #[test]
    fn a_command_remembered_in_one_chat_is_not_honoured_in_another() -> Result<()> {
        let (_root, home, project, base) = consent_fixture()?;
        let default = save_mode(&home, &base, 0, Mode::FullAccess, None)?;
        let chat = save_mode(
            &home,
            &base.with_session(Some("chat"))?,
            0,
            Mode::FullAccess,
            None,
        )?;
        remember_command(&home, &command_binding(&chat, &project))?;
        let stored = resolve(&home, &base, Some("chat"))?.unwrap();
        assert!(has_command_grant(
            &stored,
            &command_binding(&stored, &project)
        )?);
        for session in [Some("another-chat"), None] {
            let other = resolve(&home, &base, session)?.unwrap();
            assert_eq!(other.scope(), default.scope());
            assert!(!has_command_grant(
                &other,
                &command_binding(&other, &project)
            )?);
        }
        revoke_commands(&home, &stored.workspace, stored.selection.revision)?;
        assert_eq!(
            resolve(&home, &base, None)?.unwrap().selection.revision,
            default.selection.revision
        );
        assert_eq!(
            resolve(&home, &base, Some("chat"))?
                .unwrap()
                .command_grants
                .len(),
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_chats_own_selection_withdraws_a_binding_taken_under_the_default() -> Result<()> {
        let (_root, home, project, base) = consent_fixture()?;
        let default = save_mode(&home, &base, 0, Mode::Ask, None)?;
        let binding = command_binding(&default, &project);
        let context = for_request(
            Some(&home),
            "node",
            Some("requester"),
            "agent",
            Some(&project),
            Some("chat"),
        )?;
        let check = || {
            let home = home.clone();
            let binding = binding.clone();
            let context = context.clone();
            async move { scope(context, async move { validate_binding(&home, &binding) }).await }
        };
        check().await?;
        save_mode(
            &home,
            &base.with_session(Some("chat"))?,
            0,
            Mode::FullAccess,
            None,
        )?;
        let error = check().await.unwrap_err();
        assert!(error.is::<SelectionChanged>(), "{error}");
        validate_binding(&home, &binding)?;
        Ok(())
    }

    #[test]
    fn unreadable_settings_are_not_replaced_by_a_more_permissive_default() -> Result<()> {
        let root = tempfile::tempdir()?;
        let key = WorkspaceKey::canonical("node", "requester", "agent", root.path())?;
        std::fs::write(root.path().join(FILE), b"invalid")?;
        assert!(resolve(root.path(), &key, None).is_err());
        assert!(save_mode(root.path(), &key, 0, Mode::FullAccess, None).is_err());
        assert_eq!(std::fs::read(root.path().join(FILE))?, b"invalid");
        Ok(())
    }
}
