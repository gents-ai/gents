use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{context, BindingAction};
use crate::tools::native::{CommandExecutionMode, CommandExecutionPolicy};

pub(crate) fn guard_home() -> Result<Option<PathBuf>> {
    let Some(home) = context::current_home() else {
        return Ok(None);
    };
    Ok(context::settings(&home)?.map(|_| home))
}

pub(crate) fn writable_root(root: &Path, policy: &CommandExecutionPolicy) -> Result<PathBuf> {
    if policy.mode != CommandExecutionMode::WorkspaceWrite {
        return Ok(root.to_owned());
    }
    let Some(home) = context::current_home() else {
        return Ok(root.to_owned());
    };
    let Some(settings) = context::settings(&home)? else {
        return Ok(root.to_owned());
    };
    if settings.selection.mode != super::Mode::FullAccess
        && settings.workspace.path.starts_with(root)
    {
        Ok(settings.workspace.path)
    } else {
        Ok(root.to_owned())
    }
}

/// Command admission must enforce the configured ceiling before consent is
/// requested. A working directory alone does not constrain command effects;
/// admitted read-only commands are routine, while broader effects need consent.
/// Workspace consent is an optional restriction on native tool authority, not
/// its source. Headless/configured invocations without a selected workspace keep
/// their configured ceiling; bounded command isolation is enforced independently
/// by the shared launch owner. A supplied but unavailable approval home is an
/// error at request setup, never evidence of an unconfigured workspace.
pub(crate) async fn authorize_command(
    argv: &[String],
    cwd: &Path,
    root: &Path,
    writable_root: Option<&Path>,
    policy: &CommandExecutionPolicy,
) -> Result<()> {
    let Some(home) = context::current_home() else {
        return Ok(());
    };
    let Some(decision) = super::binding_route(
        &home,
        || {
            Ok(BindingAction::Command {
                argv: argv.to_vec(),
                cwd: cwd.to_owned(),
                root: root.to_owned(),
                writable_root: writable_root.map(Path::to_owned),
                policy: policy.clone(),
            })
        },
        policy.mode == CommandExecutionMode::ReadOnly,
        || Ok(serde_json::Value::Null),
    )?
    else {
        return Ok(());
    };
    super::authorize_binding(
        &home,
        decision,
        "The command was not approved for this workspace.",
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::approval::workspace::{self, Mode, WorkspaceKey};

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Case {
        mode: CommandExecutionMode,
        configured: bool,
        full_access: bool,
        within: bool,
        selected_workspace: bool,
    }

    #[tokio::test]
    async fn generated_workspace_command_roots_preserve_the_tool_ceiling() {
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().workspace_command_root_cases;
        assert_eq!(cases.len(), 32);
        for value in cases {
            let case: Case = serde_json::from_value(value.clone()).unwrap();
            let folder = tempfile::tempdir().unwrap();
            let home = folder.path().join("runtime");
            std::fs::create_dir(&home).unwrap();
            let root = folder.path().join("tool-root");
            std::fs::create_dir(&root).unwrap();
            let root = root.canonicalize().unwrap();
            let project = if case.within {
                root.join("project")
            } else {
                folder.path().join("project")
            };
            std::fs::create_dir(&project).unwrap();
            let project = project.canonicalize().unwrap();
            let key = WorkspaceKey::canonical("node", "requester", "agent", &project).unwrap();
            if case.configured {
                let mode = if case.full_access {
                    Mode::FullAccess
                } else {
                    Mode::Ask
                };
                workspace::save_mode(&home, &key, 0, mode, None).unwrap();
            }
            let scope = workspace::for_request(
                Some(&home),
                "node",
                Some("requester"),
                "agent",
                Some(&project),
            )
            .unwrap();
            let policy = CommandExecutionPolicy::write_capable().with_mode(case.mode);
            let selected = workspace::scope(scope, async { writable_root(&root, &policy) })
                .await
                .unwrap();
            assert_eq!(selected == project, case.selected_workspace, "{value}");
            assert!(selected.starts_with(&root), "{value}");
        }
    }
}
