use anyhow::Result;
use std::path::Path;

use crate::cli::*;
use crate::config_writes::ConfigAccess;
use crate::desired_state;
use crate::print_json;
use crate::{build_desired_state_live_bundle, live_manifest_from_bundle, resolve_config_access};

pub(super) async fn config_diff(args: ConfigDiffArgs) -> Result<()> {
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let bound = super::binding::load_bound_manifest(super::binding::ManifestBindingOptions {
        root: &args.root,
        home: args.home.as_deref(),
        graphql: args.graphql.as_deref(),
        bind_agent_did: args.bind_agent_did,
        force_rebind_concrete_did: args.force_rebind_concrete_did,
        access: Some(&access),
    })
    .await?
    .require_valid()?;
    let report = diff_bound_desired_manifest(&args.root, &access, &bound).await?;
    print_json(&serde_json::to_value(&report)?)?;
    Ok(())
}

pub(crate) async fn diff_bound_desired_manifest(
    root: &Path,
    access: &ConfigAccess,
    bound: &super::binding::BoundDesiredManifest,
) -> Result<desired_state::DesiredStateDiffReport> {
    let desired_manifest = &bound.manifest;
    // Diff remains an observability command even when the desired state cannot
    // currently be applied. Pairing ownership collisions are included in the
    // structured report; apply still rejects them before opening a transaction.
    // Other live validation (such as EventTrigger schema probes) remains an
    // apply-time concern and must not hide the diff that helps diagnose it.
    let live_validation_errors =
        desired_state::validate::validate_peer_pairing_ownership_against_live(
            desired_manifest,
            access,
        )
        .await?;
    let live_bundle = build_desired_state_live_bundle(&access, desired_manifest).await?;
    let (live_principal, live_manifest) =
        live_manifest_from_bundle(desired_manifest, &live_bundle)?;
    let mut report = desired_state::diff_manifests(
        root,
        access.mode(),
        desired_manifest,
        live_principal.as_ref(),
        &live_manifest,
        false,
    );
    if !live_validation_errors.is_empty() {
        report.ok = false;
        report.live_validation_errors = live_validation_errors;
    }
    Ok(report)
}
