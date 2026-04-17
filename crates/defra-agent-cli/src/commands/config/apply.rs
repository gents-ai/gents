use anyhow::Result;

use crate::cli::*;
use crate::desired_state;
use crate::print_json;
use crate::shared::*;
use crate::{
    apply_desired_state_changes, build_desired_state_live_bundle, config_apply_counts_changed,
    diff_has_pending_apply, live_manifest_from_bundle, resolve_config_access,
};

use super::validate::load_desired_manifest_or_bail;

pub(super) async fn config_apply(args: ConfigApplyArgs) -> Result<()> {
    let desired_manifest = load_desired_manifest_or_bail(&args.root)?;
    let (access, _) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;
    let desired_bundle =
        desired_state::export_bundle_from_manifest(&desired_manifest, access.mode())?;

    let live_bundle = build_desired_state_live_bundle(&access, &desired_manifest).await?;
    let (live_principal, live_manifest) =
        live_manifest_from_bundle(&desired_manifest, &live_bundle)?;
    let planned = desired_state::diff_manifests(
        &args.root,
        access.mode(),
        &desired_manifest,
        live_principal.as_ref(),
        &live_manifest,
    );

    let applied = apply_desired_state_changes(&access, &desired_bundle, &planned).await?;

    let remaining_bundle = build_desired_state_live_bundle(&access, &desired_manifest).await?;
    let (remaining_principal, remaining_manifest) =
        live_manifest_from_bundle(&desired_manifest, &remaining_bundle)?;
    let remaining = desired_state::diff_manifests(
        &args.root,
        access.mode(),
        &desired_manifest,
        remaining_principal.as_ref(),
        &remaining_manifest,
    );

    let report = ConfigApplyReport {
        status: if config_apply_counts_changed(&applied) {
            "applied"
        } else {
            "noop"
        },
        ok: !diff_has_pending_apply(&remaining.counts),
        exact_match: remaining.ok,
        changed: config_apply_counts_changed(&applied),
        root: args.root.display().to_string(),
        access_mode: access.mode().to_string(),
        agent_did: desired_manifest.agent_principal.agent_did.clone(),
        planned: planned.counts.clone(),
        applied,
        remaining: remaining.counts.clone(),
    };
    print_json(&serde_json::to_value(&report)?)?;
    if report.ok {
        Ok(())
    } else {
        anyhow::bail!("desired-state apply did not converge")
    }
}
