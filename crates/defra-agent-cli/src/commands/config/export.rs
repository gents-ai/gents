use anyhow::Result;

use crate::cli::*;
use crate::desired_state;
use crate::{build_config_export_bundle, resolve_agent_did, resolve_config_access};

pub(super) async fn config_export(args: ConfigExportArgs) -> Result<()> {
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())?;
    let (access, _) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), false).await?;
    let bundle = build_config_export_bundle(&access, &agent_did).await?;
    let manifest = desired_state::manifest_from_export_bundle(&bundle)?;
    desired_state::write_manifest_root(&args.root, &manifest, args.force)
        .map_err(|e| anyhow::anyhow!(e))?;
    println!("wrote manifest root to {}", args.root.display());
    Ok(())
}
