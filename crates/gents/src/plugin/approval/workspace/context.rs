use std::future::Future;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use super::WorkspaceKey;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Context {
    home: PathBuf,
    workspace: WorkspaceKey,
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
    }))
}

pub(crate) async fn scope<F: Future>(context: Option<Context>, future: F) -> F::Output {
    CURRENT.scope(context, future).await
}

pub(crate) fn carry<F: Future>(future: F) -> impl Future<Output = F::Output> {
    let context = CURRENT.try_with(Clone::clone).ok().flatten();
    scope(context, future)
}

pub(super) fn settings(home: &Path) -> Result<Option<super::Settings>> {
    let Some(context) = CURRENT.try_with(Clone::clone).ok().flatten() else {
        return Ok(None);
    };
    anyhow::ensure!(
        home.canonicalize().context("resolving the approval home")? == context.home,
        "the plugin and request have different approval homes"
    );
    super::get(&context.home, &context.workspace)
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
            let inherited = tokio::spawn(super::super::super::carry_interactive(async {
                CURRENT.try_with(Clone::clone).ok().flatten()
            }))
            .await?;
            assert_eq!(inherited, expected);
            assert!(scope(None, async { CURRENT.with(Clone::clone) })
                .await
                .is_none());
            assert_eq!(CURRENT.with(Clone::clone), expected);
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
}
