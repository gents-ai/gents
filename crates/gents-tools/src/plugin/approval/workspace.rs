use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
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
pub use command::{authorize_command, writable_root as command_writable_root};
#[cfg(target_os = "macos")]
pub(crate) use context::current_home as command_context_home;
pub(crate) use context::{carry, current};
pub use context::{for_request, scope, Context, ReviewInvocation, Reviewer};
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
    #[serde(default)]
    pub session_scoped: bool,
}

pub fn support() -> Support {
    Support {
        contract_version: CONTRACT_VERSION,
        workspace_sandbox: gents_session::workspace::sandbox::workspace_write_sandbox_enforced(),
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
    pub dirs: Vec<gents_pack::plugin::allowed::AllowedDir>,
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
pub enum BindingAction {
    Plugin {
        plugin: String,
        artifact_digest: String,
        input_digest: String,
        path: PathBuf,
        is_dir: bool,
        access: gents_pack::pack::BindAccess,
    },
    Command {
        argv: Vec<String>,
        cwd: PathBuf,
        root: PathBuf,
        writable_root: Option<PathBuf>,
        policy: crate::native::CommandExecutionPolicy,
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
                gents_pack::pack::BindAccess::ReadWrite,
            )),
            Self::Command { .. } => None,
        }
    }

    pub(super) fn validate(&self) -> Result<()> {
        match self {
            Self::Plugin { path, is_dir, .. } => {
                gents_pack::plugin::allowed::canonical_target(path, Some(*is_dir))?;
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
                gents_pack::plugin::allowed::canonical_target(root, Some(true))?;
                gents_pack::plugin::allowed::canonical_target(cwd, Some(true))?;
                if let Some(writable) = writable_root {
                    gents_pack::plugin::allowed::canonical_target(writable, Some(true))?;
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
                gents_pack::plugin::allowed::canonical_target(root, Some(true))?;
                let context = crate::native::ToolContext::new(root.clone(), false)?;
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
pub struct ActionConsent {
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

impl gents_pack::plugin::InvocationConsent for ActionConsent {
    fn validate(&self) -> Result<()> {
        ActionConsent::validate(self)
    }

    fn validate_plugin(
        &self,
        plugin: &str,
        artifact_digest: &str,
        input_digest: &str,
    ) -> Result<()> {
        ActionConsent::validate_plugin(self, plugin, artifact_digest, input_digest)
    }
}

pub fn action_consent(home: &Path, binding: Binding) -> ActionConsent {
    ActionConsent {
        home: home.to_owned(),
        binding,
    }
}

pub fn within_workspace(path: &Path) -> bool {
    context::within_workspace(path)
}

pub struct BindingDecision {
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
    let session_id = crate::runtime_context::current_tool_runtime_context()
        .and_then(|context| context.session_id);
    let binding = decision.binding.clone();
    let allowed = consent_binding(home, decision, super::interactive(), session_id).await?;
    anyhow::ensure!(allowed, "{denied}");
    Ok(binding)
}

pub async fn consent_binding(
    home: &Path,
    decision: BindingDecision,
    interactive: bool,
    session_id: Option<String>,
) -> Result<bool> {
    use super::{Consent, Request};

    let bounds = crate::runtime_context::tool_execution_bounds(super::WAIT);
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
    gents_pack::plugin::allowed::canonical_target(&binding.scope.workspace.path, Some(true))?;
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

pub fn allowed_scope(
    home: &Path,
    workdir: Option<&Path>,
    user_home: Option<&Path>,
) -> Result<gents_pack::plugin::allowed::Scope> {
    let mut entries = gents_pack::plugin::allowed::list(home)?;
    if let Some(settings) = context::settings(home)? {
        entries.extend(settings.dirs);
    }
    gents_pack::plugin::allowed::Scope::from_entries(home, entries, workdir, user_home)
}

/// Installed plugin admission still enforces its manifold. This decision
/// controls only consent for the exact bound input under that ceiling.
pub fn binding_route(
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

pub(crate) const FILE: &str = gents_store::local::home::WORKSPACE_APPROVALS_FILE_NAME;
pub(crate) const LOCK: &str = gents_store::local::home::WORKSPACE_APPROVALS_LOCK_FILE_NAME;

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
    access: gents_pack::pack::BindAccess,
) -> Result<()> {
    let entry = gents_pack::plugin::allowed::prepare_answered_grant(home, path, access)?;
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
    gents_pack::plugin::allowed::put_entry(&mut settings.dirs, entry, true);
    save(home, &stored)
}

fn lock(home: &Path) -> Result<gents_store::local::file_lock::FileLock> {
    std::fs::create_dir_all(home).context("preparing workspace permissions")?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join(LOCK))
        .context("opening the workspace permissions lock")?;
    gents_store::local::file_lock::FileLock::exclusive(lock)
        .context("locking workspace permissions")
}

fn save(home: &Path, stored: &Stored) -> Result<()> {
    gents_pack::plugin::allowed::write_permission_file(home, &preferences_file(home), |file| {
        serde_json::to_writer(file, &stored).context("encoding workspace permissions")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_workspace_mode_selection_follows_the_lean_model() -> Result<()> {
        use gents_lean_vocab::families::workspace_permissions_persisted_selection::{
            Preference as Wire, SelectionInput, WriteResult,
        };
        fn mode(value: &str) -> Mode {
            match value {
                "ask" => Mode::Ask,
                "autoReview" => Mode::AutoReview,
                "fullAccess" => Mode::FullAccess,
                other => panic!("unknown mode {other}"),
            }
        }
        fn mode_name(value: Mode) -> String {
            match value {
                Mode::Ask => "ask",
                Mode::AutoReview => "autoReview",
                Mode::FullAccess => "fullAccess",
            }
            .into()
        }
        fn selection(id: u64) -> ReviewTaskSelection {
            let mut task = default_review_task("node", "agent");
            task.prompt_template = format!("Review policy {id}.");
            ReviewTaskSelection::for_task(&task)
        }
        let directory = tempfile::tempdir()?;
        let home = directory.path().join("home");
        std::fs::create_dir(&home)?;
        let key = WorkspaceKey::canonical("node", "requester", "agent", directory.path())?;
        let cases = gents_lean_vocab::families::workspace_permissions_persisted_selection::cases::<
            String,
        >();
        for case in &cases {
            let input: &SelectionInput<String> = &case.input;
            let previous = input.previous.as_ref().map(|value| Preference {
                mode: mode(&value.mode),
                revision: value.revision,
                review_task: value.review_task.map(selection),
            });
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
            let task = input.requested_task.map(selection);
            let accepted = save_mode(
                &home,
                &key,
                input.expected,
                mode(&input.mode),
                task.as_ref(),
            )
            .is_ok();
            let observed = resolve(&home, &key, None)?.map(|settings| settings.selection);
            let task_id = |stored: &ReviewTaskSelection| {
                input
                    .requested_task
                    .into_iter()
                    .chain(input.previous.as_ref().and_then(|value| value.review_task))
                    .find(|id| &selection(*id) == stored)
                    .unwrap_or_else(|| panic!("{input:?}: an unknown review task was stored"))
            };
            case.check(WriteResult {
                accepted,
                changed: observed != previous,
                next: observed.as_ref().map(|value| Wire {
                    revision: value.revision,
                    mode: mode_name(value.mode),
                    review_task: value.review_task.as_ref().map(task_id),
                }),
            });
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
        let selected = ReviewTaskSelection::for_task(&gents_documents::Task {
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
                policy: crate::native::CommandExecutionPolicy::write_capable(),
            },
        }
    }

    #[test]
    fn generated_session_selections_match_persisted_workspace_entries() -> Result<()> {
        use gents_lean_vocab::families::workspace_permission_resolution_selection as family;
        let (_root, home, project, _) = consent_fixture()?;
        let key = |workspace: u64, session: Option<u64>| -> Result<WorkspaceKey> {
            WorkspaceKey::canonical("node", "requester", &format!("agent-{workspace}"), &project)?
                .with_session(session.map(|id| format!("session-{id}")).as_deref())
        };
        let settings = |entry: &family::Entry<Mode>| -> Result<Settings> {
            Ok(Settings {
                workspace: key(entry.key.workspace, entry.key.session)?,
                selection: Preference {
                    mode: entry.preference.mode,
                    revision: entry.preference.revision,
                    review_task: entry.preference.review_task.map(|id| {
                        let mut task = default_review_task("node", "agent");
                        task.prompt_template = format!("Review policy {id}.");
                        ReviewTaskSelection::for_task(&task)
                    }),
                },
                dirs: Vec::new(),
                command_grants: Vec::new(),
            })
        };
        for case in family::cases::<Mode>() {
            let entries = case
                .input
                .entries
                .iter()
                .map(&settings)
                .collect::<Result<Vec<_>>>()?;
            save(&home, &Stored { entries })?;
            let base = key(case.input.workspace, None)?;
            let session = case.input.session.map(|id| format!("session-{id}"));
            let observed = resolve(&home, &base, session.as_deref())?;
            let resolved = observed.as_ref().map(|saved| {
                let entry = case
                    .input
                    .entries
                    .iter()
                    .find(|entry| {
                        key(entry.key.workspace, entry.key.session).unwrap() == saved.workspace
                    })
                    .expect("resolution returns a stored entry");
                assert_eq!(
                    serde_json::to_value(saved).unwrap(),
                    serde_json::to_value(settings(entry).unwrap()).unwrap()
                );
                entry.clone()
            });
            let bound = key(case.input.bound.workspace, case.input.bound.session)?;
            let admits = observed.as_ref().map(|entry| &entry.workspace) == Some(&bound);
            case.check(family::Result { resolved, admits });
        }
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
        let granted = gents_pack::plugin::allowed::AllowedDir {
            path: project,
            access: gents_pack::pack::BindAccess::ReadWrite,
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
