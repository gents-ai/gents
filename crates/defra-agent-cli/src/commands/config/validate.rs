use anyhow::Result;

use crate::cli::*;
use crate::print_json;

pub(super) async fn config_validate(args: ConfigValidateArgs) -> Result<()> {
    let load = super::binding::load_bound_manifest(super::binding::ManifestBindingOptions {
        root: &args.root,
        home: args.home.as_deref(),
        graphql: args.graphql.as_deref(),
        bind_agent_did: args.bind_agent_did,
        force_rebind_concrete_did: args.force_rebind_concrete_did,
        access: None,
    })
    .await?;
    let report = load.report;
    print_json(&serde_json::to_value(&report)?)?;
    if report.is_ok() {
        Ok(())
    } else {
        anyhow::bail!("desired-state manifest validation failed")
    }
}
