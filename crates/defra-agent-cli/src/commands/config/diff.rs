use anyhow::Result;

use crate::cli::*;
use crate::desired_state;
use crate::print_json;
use crate::{build_desired_state_live_bundle, live_manifest_from_bundle, resolve_config_access};

use super::validate::load_desired_manifest_or_bail;

pub(super) async fn config_diff(args: ConfigDiffArgs) -> Result<()> {
    let desired_manifest = load_desired_manifest_or_bail(&args.root)?;

    let (access, _) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), false).await?;
    let live_bundle = build_desired_state_live_bundle(&access, &desired_manifest).await?;
    let (live_principal, live_manifest) =
        live_manifest_from_bundle(&desired_manifest, &live_bundle)?;
    let report = desired_state::diff_manifests(
        &args.root,
        access.mode(),
        &desired_manifest,
        live_principal.as_ref(),
        &live_manifest,
    );
    print_json(&serde_json::to_value(&report)?)?;
    Ok(())
}

pub(crate) fn diff_has_pending_apply(
    counts: &desired_state::DesiredStateDiffCollectionsCounts,
) -> bool {
    [
        &counts.agent_principal,
        &counts.agent_behaviors,
        &counts.tool_selections,
        &counts.inference_backends,
        &counts.inference_profiles,
        &counts.tool_service_registries,
        &counts.scheduled_tasks,
    ]
    .iter()
    .any(|count| count.create > 0 || count.update > 0)
}
