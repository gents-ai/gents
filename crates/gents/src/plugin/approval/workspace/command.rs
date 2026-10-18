use std::path::Path;

use anyhow::Result;

use super::{context, BindingAction};
use crate::plugin::approval::{self, Consent, Request};
use crate::toolset::{CommandExecutionMode, CommandExecutionPolicy};
use gents_loop::tool_policy::workspace_permissions::Route;

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
    super::validate_binding(&home, &decision.binding)?;
    let allowed = match decision.route {
        Route::Allow => true,
        Route::Deny => false,
        Route::Ask | Route::Review => match approval::consent(approval::interactive()) {
            Consent::Deny => false,
            Consent::Ask => {
                let session_id =
                    crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
                        .and_then(|context| context.session_id);
                let request = Request::for_binding(decision.binding, session_id);
                approval::ask(&home, &request, approval::WAIT).await?
            }
        },
    };
    anyhow::ensure!(allowed, "The command was not approved for this workspace.");
    Ok(())
}
