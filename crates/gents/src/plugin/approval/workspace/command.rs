use std::path::Path;

use anyhow::Result;

use super::{context, BindingAction};
use crate::toolset::{CommandExecutionMode, CommandExecutionPolicy};

/// Command admission must enforce the configured ceiling before consent is
/// requested. A working directory alone does not constrain command effects;
/// admitted read-only commands are routine, while broader effects need consent.
pub(crate) async fn authorize_command(
    argv: &[String],
    cwd: &Path,
    root: &Path,
    policy: &CommandExecutionPolicy,
) -> Result<()> {
    let Some(home) = context::current_home() else {
        return Ok(());
    };
    let Some(decision) = super::binding_route(
        &home,
        BindingAction::Command {
            argv: argv.to_vec(),
            cwd: cwd.to_owned(),
            root: root.to_owned(),
            policy: policy.clone(),
        },
        policy.mode == CommandExecutionMode::ReadOnly,
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
