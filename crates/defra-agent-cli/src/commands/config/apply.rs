use anyhow::{Context, Result};
use std::path::Path;

use defra_agent::graphql::escape_graphql_string;

use crate::cli::*;
use crate::config_writes::ConfigAccess;
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
    let report = apply_bound_desired_manifest(&args.root, &access, &bound).await?;
    print_json(&serde_json::to_value(&report)?)?;
    if report.ok {
        Ok(())
    } else {
        anyhow::bail!("desired-state apply did not converge")
    }
}

pub(crate) async fn apply_bound_desired_manifest(
    root: &Path,
    access: &ConfigAccess,
    bound: &super::binding::BoundDesiredManifest,
) -> Result<ConfigApplyReport> {
    let desired_manifest = &bound.manifest;

    // Apply-time live validation complements the static desired-state
    // validation. It probes the live node's GraphQL schema for EventTrigger
    // filter syntax and `doc.*` field resolution. We only run it from the
    // apply path, where we already hold a live `ConfigAccess`.
    let live_errs =
        desired_state::validate::validate_manifest_against_live(desired_manifest, &access).await?;
    if !live_errs.is_empty() {
        for e in &live_errs {
            eprintln!("error: {e}");
        }
        anyhow::bail!("{} live validation error(s)", live_errs.len());
    }

    let desired_bundle =
        desired_state::export_bundle_from_manifest(desired_manifest, access.mode())?;

    let live_bundle = build_desired_state_live_bundle(&access, desired_manifest).await?;
    let (live_principal, live_manifest) =
        live_manifest_from_bundle(desired_manifest, &live_bundle)?;
    let planned = desired_state::diff_manifests(
        root,
        access.mode(),
        desired_manifest,
        live_principal.as_ref(),
        &live_manifest,
    );

    let applied = {
        let txn = access
            .begin_apply_txn()
            .await
            .context("config apply: begin transaction")?;
        let counts = match apply_desired_state_changes(&txn, &desired_bundle, &planned).await {
            Ok(counts) => counts,
            Err(error) => {
                if let Err(discard_err) = txn.discard().await {
                    tracing::warn!(
                        %discard_err,
                        "config apply: tx discard failed after apply error"
                    );
                }
                return Err(error);
            }
        };
        if let Err(commit_err) = txn.commit().await {
            return Err(commit_err).context("config apply: commit failed");
        }
        counts
    };

    // === STOPGAP for sourcenetwork/defradb.rs#970 ===
    // DefraDB tx commits do not emit Update events on the EventBus, so the
    // in-process runtime watcher (control_watcher.rs) never reconciles after
    // a transactional config apply. Until upstream emits Update events on
    // commit, we issue a single auto-commit no-op write here to wake the
    // watcher. The data we just committed inside the tx is durable regardless
    // of whether this nudge succeeds.
    //
    // When defradb.rs#970 ships and we bump the workspace pin, remove:
    //   1. This comment block and the `if` block that follows it.
    //   2. The `nudge_runtime_watcher_after_apply` helper below.
    //   3. The `escape_graphql_string` import at the top of this file, if it
    //      becomes unused after (2).
    // Verify the removal by running:
    //   cargo test -p defra-agent-cli --test cli_config_apply_running
    // — `config_apply_reconciles_running_runtime_without_restart` must still
    // pass against the upstream-fixed defradb.rs without the nudge.
    if config_apply_counts_changed(&applied) {
        if let Err(nudge_err) = nudge_runtime_watcher_after_apply(&access, bound).await {
            tracing::warn!(
                agent_did = %bound.context.target_agent_did,
                %nudge_err,
                "config apply: post-commit runtime nudge failed; runtime may not reconcile until restart"
            );
        }
    }

    let remaining_bundle = build_desired_state_live_bundle(&access, desired_manifest).await?;
    let (remaining_principal, remaining_manifest) =
        live_manifest_from_bundle(desired_manifest, &remaining_bundle)?;
    let remaining = desired_state::diff_manifests(
        root,
        access.mode(),
        desired_manifest,
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
        root: root.display().to_string(),
        access_mode: access.mode().to_string(),
        agent_did: bound.context.target_agent_did.clone(),
        planned: planned.counts.clone(),
        applied,
        remaining: remaining.counts.clone(),
    };
    Ok(report)
}

/// Wake the runtime watcher after a transactional apply commit by issuing
/// an auto-commit write that triggers an Update event on the EventBus.
///
/// The write targets `AgentPrincipal::enabled`, re-setting it to the
/// manifest's declared value. This is a no-op when the live `enabled`
/// already matches the manifest; if the live value drifted out-of-band,
/// the nudge will also reconcile it back to the manifest value, which is
/// the correct manifest-wins semantics.
///
/// This helper is part of the stopgap for sourcenetwork/defradb.rs#970;
/// remove together with its call site when upstream emits Update events
/// on transaction commit.
async fn nudge_runtime_watcher_after_apply(
    access: &ConfigAccess,
    bound: &super::binding::BoundDesiredManifest,
) -> Result<()> {
    let agent_did = &bound.context.target_agent_did;
    let enabled = bound.manifest.agent_principal.enabled;
    let escaped_did = escape_graphql_string(agent_did);
    let mutation = format!(
        r#"mutation {{
            update_AgentPrincipal(
                filter: {{ agent_did: {{ _eq: "{escaped_did}" }} }},
                input: {{ enabled: {enabled} }}
            ) {{ _docID }}
        }}"#
    );
    let response = access.execute(&mutation).await?;
    tracing::debug!(
        agent_did = %agent_did,
        "config apply: post-commit runtime nudge succeeded; _docID={}",
        response
            .pointer("/data/update_AgentPrincipal/0/_docID")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("<none>")
    );
    Ok(())
}
