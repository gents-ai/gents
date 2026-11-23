use std::path::Path;

use anyhow::{bail, Result};

/// Refuses the operator's plugin access settings, which no tool may use.
pub fn ensure_unprotected(path: &Path) -> Result<()> {
    if crate::plugin::allowed::is_protected(path) {
        bail!(
            "{} holds the operator's plugin access settings; tools cannot use it",
            path.display()
        );
    }
    Ok(())
}
