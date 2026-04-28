use anyhow::Result;

use crate::cli::*;
use crate::print_json;

pub(super) async fn config_validate(args: ConfigValidateArgs) -> Result<()> {
    let (_, report) = super::binding::load_desired_manifest_with_binding_report(
        &args.root,
        args.home.as_deref(),
        args.graphql.as_deref(),
        args.bind_agent_did,
        args.force_rebind_concrete_did,
        None,
    )
    .await?;
    print_json(&serde_json::to_value(&report)?)?;
    if report.is_ok() {
        Ok(())
    } else {
        anyhow::bail!("desired-state manifest validation failed")
    }
}
