use anyhow::Result;

use crate::cli::*;
use crate::desired_state;
use crate::{build_config_export_bundle, resolve_config_access};

pub(super) async fn config_export(args: ConfigExportArgs) -> Result<()> {
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let node_did = super::binding::resolve_target_node_did(
        args.node_did.as_deref(),
        args.bind_node_did,
        args.home.as_deref(),
        args.graphql.as_deref(),
        Some(&access),
    )
    .await?;
    let bundle = build_config_export_bundle(&access, &node_did).await?;
    let manifest = desired_state::manifest_from_export_bundle(&bundle)?;
    desired_state::write_manifest_root(&args.root, &manifest, args.force)
        .map_err(|e| anyhow::anyhow!(e))?;
    println!("wrote manifest root to {}", args.root.display());
    Ok(())
}
