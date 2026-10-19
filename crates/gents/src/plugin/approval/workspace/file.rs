use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{context, Binding, BindingAction};

pub(crate) struct FileMutationConsent {
    home: PathBuf,
    binding: Binding,
}

impl FileMutationConsent {
    /// Consent may wait while another call acquires the mutation lock or changes
    /// an ancestor path. Revalidate under that lock before reading or writing.
    pub(crate) fn validate(&self) -> Result<()> {
        super::validate_binding(&self.home, &self.binding)?;
        Ok(())
    }
}

pub(crate) async fn authorize_file_mutation(
    tool: &str,
    root: &Path,
    path: &Path,
    input: &impl Serialize,
) -> Result<Option<FileMutationConsent>> {
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
    Ok(Some(FileMutationConsent { home, binding }))
}
