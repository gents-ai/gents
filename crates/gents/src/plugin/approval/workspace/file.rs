use std::path::Path;

use crate::plugin::approval::PluginApprovalError;

type Result<T, E = PluginApprovalError> = std::result::Result<T, E>;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{context, ActionConsent, BindingAction};

pub(crate) async fn authorize_file_mutation(
    tool: &str,
    root: &Path,
    path: &Path,
    input: &impl Serialize,
) -> Result<Option<ActionConsent>> {
    let Some(home) = context::current_home() else {
        return Ok(None);
    };
    let Some(decision) = super::binding_route(
        &home,
        || {
            Ok(BindingAction::FileMutation {
                tool: tool.to_owned(),
                input_digest: format!("{:x}", Sha256::digest(serde_json::to_vec(input)?)),
                path: path.to_owned(),
                root: root.to_owned(),
                existed: path.try_exists()?,
            })
        },
        context::within_workspace(path),
        || serde_json::to_value(input).map_err(Into::into),
    )?
    else {
        return Ok(None);
    };
    let binding = super::authorize_binding(
        &home,
        decision,
        "The file change was not approved for this workspace.",
    )
    .await?;
    Ok(Some(super::action_consent(&home, binding)))
}
