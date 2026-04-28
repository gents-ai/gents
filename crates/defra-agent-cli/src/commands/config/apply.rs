use anyhow::Result;

use crate::cli::*;
use crate::desired_state;
use crate::print_json;
use crate::shared::*;
use crate::{
    apply_desired_state_changes, build_desired_state_live_bundle, config_apply_counts_changed,
    diff_has_pending_apply, live_manifest_from_bundle, resolve_config_access,
};

pub(super) async fn config_apply(args: ConfigApplyArgs) -> Result<()> {
    let (access, _) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;
    let desired_manifest = super::binding::load_desired_manifest_with_binding_or_bail(
        &args.root,
        args.home.as_deref(),
        args.graphql.as_deref(),
        args.bind_agent_did,
        args.force_rebind_concrete_did,
        Some(&access),
    )
    .await?;

    // Apply-time live validation complements the static desired-state
    // validation. It probes the live node's GraphQL schema for EventTrigger
    // filter syntax and `doc.*` field resolution. We only run it from the
    // apply path, where we already hold a live `ConfigAccess`.
    let live_errs =
        desired_state::validate::validate_manifest_against_live(&desired_manifest, &access).await?;
    if !live_errs.is_empty() {
        for e in &live_errs {
            eprintln!("error: {e}");
        }
        anyhow::bail!("{} live validation error(s)", live_errs.len());
    }

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
    if report.ok && args.write_identity_binding {
        super::binding::write_identity_binding(
            &args.root,
            &desired_manifest.agent_principal.agent_did,
        )?;
    }
    print_json(&serde_json::to_value(&report)?)?;
    if report.ok {
        Ok(())
    } else {
        anyhow::bail!("desired-state apply did not converge")
    }
}
