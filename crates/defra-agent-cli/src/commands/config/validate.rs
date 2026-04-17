use std::path::Path;

use anyhow::Result;

use crate::cli::*;
use crate::desired_state;
use crate::print_json;

pub(super) async fn config_validate(args: ConfigValidateArgs) -> Result<()> {
    let report = desired_state::validate_manifest_root(&args.root);
    print_json(&serde_json::to_value(&report)?)?;
    if report.is_ok() {
        Ok(())
    } else {
        anyhow::bail!("desired-state manifest validation failed")
    }
}

pub(crate) fn load_desired_manifest_or_bail(
    root: &Path,
) -> Result<desired_state::DesiredStateManifest> {
    let (desired_manifest, validation_report) = desired_state::load_manifest_root(root);
    if !validation_report.is_ok() {
        print_json(&serde_json::to_value(&validation_report)?)?;
        anyhow::bail!("desired-state manifest validation failed")
    }
    desired_manifest.ok_or_else(|| anyhow::anyhow!("validated manifest root produced no manifest"))
}
