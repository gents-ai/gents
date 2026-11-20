use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};

use super::WorkspaceKey;
use gents_loop::tool_policy::access_review::History;

pub struct ReviewInvocation {
    pub action_id: String,
    pub task: super::ReviewTaskSelection,
    pub binding: super::Binding,
    pub payload: serde_json::Value,
}

pub type Reviewer = Arc<
    dyn Fn(ReviewInvocation) -> Pin<Box<dyn Future<Output = Result<Option<String>>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct Context {
    home: PathBuf,
    workspace: WorkspaceKey,
    session_id: Option<String>,
    review_history: Arc<Mutex<History>>,
    review_gate: Arc<tokio::sync::Mutex<()>>,
    reviewer: Option<Reviewer>,
}

impl Context {
    pub fn with_reviewer(mut self, reviewer: Reviewer) -> Self {
        self.reviewer = Some(reviewer);
        self
    }

    fn governs(&self, key: &WorkspaceKey) -> bool {
        key.base() == self.workspace
            && (key.session_id.is_none() || key.session_id == self.session_id)
    }
}

/// The home comes from the host's runtime options. The request's original
/// workspace remains fixed even when a tool changes its working directory.
pub fn for_request(
    home: Option<&Path>,
    node_did: &str,
    requester_did: Option<&str>,
    agent_did: &str,
    workspace: Option<&Path>,
    session_id: Option<&str>,
) -> Result<Option<Context>> {
    let (Some(home), Some(requester), Some(workspace)) = (home, requester_did, workspace) else {
        return Ok(None);
    };
    let home = home.canonicalize().context("resolving the approval home")?;
    Ok(Some(Context {
        home,
        workspace: WorkspaceKey::canonical(node_did, requester, agent_did, workspace)?,
        session_id: session_id
            .map(str::trim)
            .filter(|session_id| !session_id.is_empty())
            .map(ToOwned::to_owned),
        review_history: Arc::default(),
        review_gate: Arc::default(),
        reviewer: None,
    }))
}

pub(super) async fn resolve_review(
    context: Option<&Context>,
    home: &Path,
    decision: &super::BindingDecision,
    action_id: &str,
) -> Result<super::Route> {
    use super::{Review, Route};

    let Some(context) = context else {
        return Ok(Route::Deny);
    };
    if !context.governs(&decision.binding.scope.workspace) {
        return Ok(Route::Deny);
    }
    if decision.route != Route::Review {
        return Ok(decision.route);
    }
    let _gate = context.review_gate.lock().await;
    let settings = super::validate_binding(Some(context), home, &decision.binding)
        .map_err(super::BindingError::into_anyhow)?;
    if context
        .review_history
        .lock()
        .expect("review history lock poisoned")
        .blocked()
    {
        return Ok(Route::Ask);
    }
    let verdict = match (&context.reviewer, settings.selection.review_task) {
        (Some(reviewer), Some(task)) => {
            let invocation = ReviewInvocation {
                action_id: action_id.to_owned(),
                task,
                binding: decision.binding.clone(),
                payload: decision
                    .review_payload
                    .clone()
                    .context("access review input is missing")?,
            };
            match reviewer(invocation).await {
                Ok(Some(reply)) => {
                    gents_loop::tool_policy::access_review::decode_reply(action_id, &reply)
                }
                Ok(None) => Review::Unavailable,
                Err(error) => {
                    tracing::warn!(%error, "workspace access review was unavailable");
                    Review::Unavailable
                }
            }
        }
        _ => Review::Unavailable,
    };
    super::validate_binding(Some(context), home, &decision.binding)
        .map_err(super::BindingError::into_anyhow)?;
    Ok(review_route(
        Some(context),
        &decision.input,
        verdict,
        &decision.binding,
    ))
}

pub(super) fn review_route(
    context: Option<&Context>,
    input: &super::Input<WorkspaceKey, super::BindingAction>,
    verdict: super::Review,
    grant: &super::Binding,
) -> super::Route {
    let Some(context) = context else {
        return super::Route::Deny;
    };
    if !context.governs(&input.current.workspace) {
        return super::Route::Deny;
    }
    let mut history = context
        .review_history
        .lock()
        .expect("review history lock poisoned");
    gents_loop::tool_policy::access_review::apply(input, &mut history, verdict, grant)
}

pub(crate) fn current_home(context: Option<&Context>) -> Option<PathBuf> {
    context.map(|context| context.home.clone())
}

pub(super) fn within_workspace(context: Option<&Context>, path: &Path) -> bool {
    context.is_some_and(|context| path.starts_with(&context.workspace.path))
}

pub(super) fn settings(context: Option<&Context>, home: &Path) -> Result<Option<super::Settings>> {
    request_settings(context, home).map_err(super::BindingError::into_anyhow)
}

fn request_settings(
    context: Option<&Context>,
    home: &Path,
) -> Result<Option<super::Settings>, super::BindingError> {
    let Some(context) = context else {
        return Ok(None);
    };
    if home.canonicalize().context("resolving the approval home")? != context.home {
        return Err(anyhow::anyhow!("the plugin and request have different approval homes").into());
    }
    let settings = super::resolve(
        &context.home,
        &context.workspace,
        context.session_id.as_deref(),
    )?;
    if settings.is_some() {
        gents_pack::plugin::allowed::canonical_target(&context.workspace.path, Some(true))?;
    }
    Ok(settings)
}

/// The settings a consent check validates `key` against (Lean
/// `WorkspacePermissions.governingSettings`): the request's, when the check
/// carries the request's approval context, wherever it runs; the entry stored
/// under `key` only for a check outside any request, the operator answering or
/// listing questions.
pub(super) fn governing_settings(
    context: Option<&Context>,
    home: &Path,
    key: &WorkspaceKey,
) -> Result<Option<super::Settings>, super::BindingError> {
    match context {
        Some(context) => request_settings(Some(context), home),
        None => Ok(super::entry(home, key)?),
    }
}

#[cfg(test)]
mod review_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_request_context_moved_into_a_spawned_task_is_the_same_request() -> Result<()> {
        let home = tempfile::tempdir()?;
        let first = tempfile::tempdir()?;
        let second = tempfile::tempdir()?;
        let context = for_request(
            Some(home.path()),
            "node",
            Some("requester"),
            "agent",
            Some(first.path()),
            Some("chat"),
        )?
        .unwrap();
        let moved = context.clone();
        let observed = tokio::spawn(async move { moved }).await?;
        assert_eq!(observed.home, context.home);
        assert_eq!(observed.workspace, context.workspace);
        assert_eq!(observed.session_id, context.session_id);
        assert!(observed.governs(&observed.workspace));
        assert!(observed.governs(&observed.workspace.with_session(Some("chat")).unwrap()));
        assert!(!observed.governs(
            &observed
                .workspace
                .with_session(Some("another-chat"))
                .unwrap()
        ));
        assert!(Arc::ptr_eq(
            &observed.review_history,
            &context.review_history
        ));
        let other = for_request(
            Some(home.path()),
            "node",
            Some("requester"),
            "agent",
            Some(second.path()),
            None,
        )?
        .unwrap();
        assert_eq!(other.workspace.path, second.path().canonicalize()?);
        assert!(!Arc::ptr_eq(&other.review_history, &context.review_history));
        assert!(for_request(
            Some(home.path()),
            "node",
            Some("requester"),
            "agent",
            None,
            None,
        )?
        .is_none());
        assert!(for_request(
            Some(&home.path().join("uninitialized")),
            "node",
            Some("requester"),
            "agent",
            Some(first.path()),
            None,
        )
        .is_err());
        Ok(())
    }

    #[tokio::test]
    async fn review_history_is_shared_with_background_calls_but_not_a_new_request() -> Result<()> {
        use super::super::{BindingAction, Grant, Input, Mode, Review, Route, Scope};

        let home = tempfile::tempdir()?;
        let workspace = tempfile::tempdir()?;
        let context = || {
            for_request(
                Some(home.path()),
                "node",
                Some("requester"),
                "agent",
                Some(workspace.path()),
                None,
            )
        };
        let first = context()?.unwrap();
        let second = context()?.unwrap();
        let input = Input {
            current: Scope {
                workspace: first.workspace.clone(),
                revision: 1,
            },
            configured_workspace: first.workspace.clone(),
            mode: Mode::AutoReview,
            action: BindingAction::Command {
                argv: vec!["fixture".into()],
                cwd: first.workspace.path.clone(),
                root: first.workspace.path.clone(),
                writable_root: None,
                policy: crate::native::CommandExecutionPolicy::write_capable(),
            },
            ceiling_allows: true,
            crosses_boundary: true,
            grant: None,
        };
        let grant = Grant {
            scope: input.current.clone(),
            action: input.action.clone(),
        };
        for _ in 0..3 {
            assert_eq!(
                review_route(Some(&first), &input, Review::Deny, &grant),
                Route::Deny
            );
        }
        let background = first.clone();
        let inherited = tokio::spawn({
            let input = input.clone();
            let grant = grant.clone();
            async move { review_route(Some(&background), &input, Review::Allow, &grant) }
        })
        .await?;
        assert_eq!(inherited, Route::Ask);
        assert_eq!(
            review_route(Some(&second), &input, Review::Allow, &grant),
            Route::Allow
        );
        assert_eq!(
            review_route(Some(&first), &input, Review::Allow, &grant),
            Route::Ask
        );
        assert_eq!(
            review_route(None, &input, Review::Allow, &grant),
            Route::Deny
        );
        Ok(())
    }

    #[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum CheckSite {
        RequestTask,
        BlockingThread,
    }

    #[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum GoverningSettings {
        Request,
        StoredEntry,
    }

    /// The binding's stored entry is the workspace's own preference; the
    /// request's governing settings are its session's, so the two answers are
    /// told apart by the key the returned settings carry.
    #[tokio::test]
    async fn governing_settings_conform_to_lean() -> Result<()> {
        use gents_lean_vocab::families::tool_policy_workspace_permissions_governing_settings as family;

        let home = tempfile::tempdir()?;
        let workspace = tempfile::tempdir()?;
        let request = for_request(
            Some(home.path()),
            "node",
            Some("requester"),
            "agent",
            Some(workspace.path()),
            Some("chat"),
        )?
        .unwrap();
        let stored = request.workspace.clone();
        super::super::save_mode(home.path(), &stored, 0, super::super::Mode::Ask, None)?;
        super::super::save_mode(
            home.path(),
            &stored.with_session(Some("chat"))?,
            0,
            super::super::Mode::FullAccess,
            None,
        )?;
        for case in family::cases::<CheckSite, GoverningSettings>() {
            let context = case.input.request_context.then(|| request.clone());
            let home = home.path().to_owned();
            let key = stored.clone();
            let check = move || governing_settings(context.as_ref(), &home, &key);
            let settings = match case.input.site {
                CheckSite::RequestTask => check(),
                CheckSite::BlockingThread => tokio::task::spawn_blocking(check).await?,
            }
            .map_err(super::super::BindingError::into_anyhow)?
            .expect("both settings are stored");
            case.check(if settings.workspace.session_id.is_some() {
                GoverningSettings::Request
            } else {
                GoverningSettings::StoredEntry
            });
        }
        Ok(())
    }
}
