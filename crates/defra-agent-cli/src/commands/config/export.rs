use anyhow::Result;

use crate::cli::*;
use crate::print_json;
use crate::shared::*;
use crate::{build_config_export_bundle, resolve_agent_did, resolve_config_access};

pub(super) async fn config_export(args: ConfigExportArgs) -> Result<()> {
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())?;
    let (access, _) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), false).await?;
    let bundle = build_config_export_bundle(&access, &agent_did).await?;
    print_json(&serde_json::to_value(bundle)?)?;
    Ok(())
}
