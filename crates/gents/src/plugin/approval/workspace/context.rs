use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};

use super::WorkspaceKey;
use gents_loop::tool_policy::access_review::History;

#[derive(Clone, Debug)]
pub(crate) struct Context {
    home: PathBuf,
    workspace: WorkspaceKey,
    review_history: Arc<Mutex<History>>,
}

tokio::task_local! {
    static CURRENT: Option<Context>;
}

/// The home comes from the host's runtime options. The request's original
/// workspace remains fixed even when a tool changes its working directory.
pub(crate) fn for_request(
    home: Option<&Path>,
    node_did: &str,
    requester_did: Option<&str>,
    agent_id: &str,
    workspace: Option<&Path>,
) -> Result<Option<Context>> {
    let (Some(home), Some(requester), Some(workspace)) = (home, requester_did, workspace) else {
        return Ok(None);
    };
    let home = match home.canonicalize() {
        Ok(home) => home,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("resolving the approval home"),
    };
    Ok(Some(Context {
        home,
        workspace: WorkspaceKey::canonical(node_did, requester, agent_id, workspace)?,
        review_history: Arc::default(),
    }))
}

pub(super) fn review_route(
    input: &super::Input<WorkspaceKey, super::BindingAction>,
    verdict: super::Review,
    grant: &super::Binding,
) -> super::Route {
    CURRENT
        .try_with(|context| {
            context.as_ref().map(|context| {
                if context.workspace != input.current.workspace {
                    return super::Route::Deny;
                }
                let mut history = context
                    .review_history
                    .lock()
                    .expect("review history lock poisoned");
                gents_loop::tool_policy::access_review::apply(input, &mut history, verdict, grant)
            })
        })
        .ok()
        .flatten()
        .unwrap_or(super::Route::Deny)
}

pub(crate) async fn scope<F: Future>(context: Option<Context>, future: F) -> F::Output {
    CURRENT.scope(context, future).await
}

pub(crate) fn carry<F: Future>(future: F) -> impl Future<Output = F::Output> {
    let context = CURRENT.try_with(Clone::clone).ok().flatten();
    scope(context, future)
}

pub(super) fn current_home() -> Option<PathBuf> {
    CURRENT
        .try_with(|context| context.as_ref().map(|context| context.home.clone()))
        .ok()
        .flatten()
}

pub(super) fn within_workspace(path: &Path) -> bool {
    CURRENT
        .try_with(|context| {
            context
                .as_ref()
                .is_some_and(|context| path.starts_with(&context.workspace.path))
        })
        .unwrap_or(false)
}

pub(super) fn settings(home: &Path) -> Result<Option<super::Settings>> {
    let Some(context) = CURRENT.try_with(Clone::clone).ok().flatten() else {
        return Ok(None);
    };
    anyhow::ensure!(
        home.canonicalize().context("resolving the approval home")? == context.home,
        "the plugin and request have different approval homes"
    );
    let settings = super::get(&context.home, &context.workspace)?;
    if settings.is_some() {
        crate::plugin::allowed::canonical_target(&context.workspace.path, Some(true))?;
    }
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn background_calls_retain_the_original_scope_without_leaking_it_to_other_requests(
    ) -> Result<()> {
        let home = tempfile::tempdir()?;
        let first = tempfile::tempdir()?;
        let second = tempfile::tempdir()?;
        let context = for_request(
            Some(home.path()),
            "node",
            Some("requester"),
            "agent",
            Some(first.path()),
        )?;
        let expected = context.clone();
        scope(context, async move {
            let check = |observed: Option<Context>| {
                let observed = observed.unwrap();
                let expected = expected.as_ref().unwrap();
                assert_eq!(observed.home, expected.home);
                assert_eq!(observed.workspace, expected.workspace);
                assert!(Arc::ptr_eq(
                    &observed.review_history,
                    &expected.review_history
                ));
            };
            let inherited = tokio::spawn(super::super::super::carry_interactive(async {
                CURRENT.try_with(Clone::clone).ok().flatten()
            }))
            .await?;
            check(inherited);
            assert!(scope(None, async { CURRENT.with(Clone::clone) })
                .await
                .is_none());
            check(CURRENT.with(Clone::clone));
            Ok::<_, anyhow::Error>(())
        })
        .await?;
        assert!(CURRENT.try_with(Clone::clone).is_err());
        let other = for_request(
            Some(home.path()),
            "node",
            Some("requester"),
            "agent",
            Some(second.path()),
        )?
        .unwrap();
        assert_eq!(other.workspace.path, second.path().canonicalize()?);
        assert!(
            for_request(Some(home.path()), "node", Some("requester"), "agent", None,)?.is_none()
        );
        assert!(for_request(
            Some(&home.path().join("uninitialized")),
            "node",
            Some("requester"),
            "agent",
            Some(first.path()),
        )?
        .is_none());
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
                policy: crate::toolset::CommandExecutionPolicy::write_capable(),
            },
            ceiling_allows: true,
            crosses_boundary: true,
            grant: None,
        };
        let grant = Grant {
            scope: input.current.clone(),
            action: input.action.clone(),
        };
        scope(Some(first), async {
            for _ in 0..3 {
                assert_eq!(review_route(&input, Review::Deny, &grant), Route::Deny);
            }
            let inherited = tokio::spawn(carry({
                let input = input.clone();
                let grant = grant.clone();
                async move { review_route(&input, Review::Allow, &grant) }
            }))
            .await?;
            assert_eq!(inherited, Route::Ask);
            assert_eq!(
                scope(Some(second), async {
                    review_route(&input, Review::Allow, &grant)
                })
                .await,
                Route::Allow
            );
            assert_eq!(review_route(&input, Review::Allow, &grant), Route::Ask);
            Ok::<_, anyhow::Error>(())
        })
        .await?;
        assert_eq!(review_route(&input, Review::Allow, &grant), Route::Deny);
        Ok(())
    }
}
