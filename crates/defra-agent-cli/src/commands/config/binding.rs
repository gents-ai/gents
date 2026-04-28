use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::cli::ManifestAgentDidBindingArg;
use crate::config_writes::ConfigAccess;
use crate::desired_state::{
    self, DesiredStateCounts, DesiredStateManifest, DesiredStateValidationReport,
};
use crate::print_json;

pub(super) async fn load_desired_manifest_with_binding_or_bail(
    root: &Path,
    home: Option<&Path>,
    graphql: Option<&str>,
    bind_agent_did: Option<ManifestAgentDidBindingArg>,
    force_rebind_concrete_did: bool,
    access: Option<&ConfigAccess>,
) -> Result<DesiredStateManifest> {
    let (manifest, report) = load_desired_manifest_with_binding_report(
        root,
        home,
        graphql,
        bind_agent_did,
        force_rebind_concrete_did,
        access,
    )
    .await?;
    if !report.is_ok() {
        print_json(&serde_json::to_value(&report)?)?;
        anyhow::bail!("desired-state manifest validation failed")
    }
    manifest.ok_or_else(|| anyhow::anyhow!("validated manifest root produced no manifest"))
}

pub(super) async fn load_desired_manifest_with_binding_report(
    root: &Path,
    home: Option<&Path>,
    graphql: Option<&str>,
    bind_agent_did: Option<ManifestAgentDidBindingArg>,
    force_rebind_concrete_did: bool,
    access: Option<&ConfigAccess>,
) -> Result<(Option<DesiredStateManifest>, DesiredStateValidationReport)> {
    let root_display = root.display().to_string();
    let (manifest, initial_report) = desired_state::load_manifest_root(root);
    let Some(mut manifest) = manifest else {
        return Ok((None, initial_report));
    };

    if bind_agent_did.is_none() {
        return Ok((Some(manifest), initial_report));
    }

    if !initial_report.is_ok()
        && !initial_report
            .errors
            .iter()
            .all(|error| is_rebindable_agent_did_error(error))
    {
        return Ok((Some(manifest), initial_report));
    }

    let target_did = resolve_bound_agent_did(bind_agent_did, home, graphql, access).await?;
    enforce_identity_binding_safety(root, &target_did, force_rebind_concrete_did)?;
    enforce_manifest_rebind_safety(&manifest, &target_did, force_rebind_concrete_did)?;
    rebind_manifest_agent_did(&mut manifest, &target_did);

    let report = validation_report_for_manifest(root_display, &manifest);
    Ok((Some(manifest), report))
}

pub(super) fn write_identity_binding(root: &Path, agent_did: &str) -> Result<()> {
    let path = root.join("identity.json");
    let mut value = if path.exists() {
        let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_slice(&bytes).with_context(|| format!("decoding {}", path.display()))?
    } else {
        json!({})
    };

    let object = value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{} must contain a JSON object", path.display()))?;
    object.insert(
        "identity_status".to_string(),
        Value::String("provisioned".to_string()),
    );
    object.insert("did".to_string(), Value::String(agent_did.to_string()));
    object
        .entry("identity_backend".to_string())
        .or_insert_with(|| Value::String("file".to_string()));

    let bytes = serde_json::to_vec_pretty(&value).context("encoding identity binding JSON")?;
    fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn validation_report_for_manifest(
    root_display: String,
    manifest: &DesiredStateManifest,
) -> DesiredStateValidationReport {
    let mut errors = Vec::new();
    desired_state::validate::validate_manifest(manifest, &mut errors);
    DesiredStateValidationReport {
        status: if errors.is_empty() {
            "validated"
        } else {
            "invalid"
        },
        ok: errors.is_empty(),
        root: root_display,
        agent_did: Some(manifest.agent_principal.agent_did.clone()),
        counts: counts_for_manifest(manifest),
        errors,
    }
}

fn counts_for_manifest(manifest: &DesiredStateManifest) -> DesiredStateCounts {
    DesiredStateCounts {
        agent_principal: 1,
        agent_behaviors: manifest.agent_behaviors.len(),
        tool_selections: manifest.tool_selections.len(),
        inference_backends: manifest.inference_backends.len(),
        inference_profiles: manifest.inference_profiles.len(),
        tool_service_registries: manifest.tool_service_registries.len(),
        tasks: manifest.tasks.len(),
        schedules: manifest.schedules.len(),
        event_triggers: manifest.event_triggers.len(),
    }
}

async fn resolve_bound_agent_did(
    bind_agent_did: Option<ManifestAgentDidBindingArg>,
    home: Option<&Path>,
    graphql: Option<&str>,
    access: Option<&ConfigAccess>,
) -> Result<String> {
    match bind_agent_did {
        Some(ManifestAgentDidBindingArg::Home) => crate::resolve_agent_did(home, None)
            .context("resolving agent DID from initialized home"),
        Some(ManifestAgentDidBindingArg::Live) => {
            if let Some(access) = access {
                return resolve_live_agent_did(access).await;
            }
            let (access, _) = crate::resolve_config_access(home, graphql, false).await?;
            resolve_live_agent_did(&access).await
        }
        None => anyhow::bail!("agent DID binding mode is required"),
    }
}

async fn resolve_live_agent_did(access: &ConfigAccess) -> Result<String> {
    let response = access
        .execute(
            r#"{
                AgentRuntime {
                    agent_did
                }
            }"#,
        )
        .await
        .context("querying live AgentRuntime for agent DID")?;
    let dids = response
        .pointer("/data/AgentRuntime")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| row.get("agent_did").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>();

    match dids.len() {
        0 => anyhow::bail!("live runtime did not return an AgentRuntime agent_did"),
        1 => Ok(dids.into_iter().next().expect("one DID was present")),
        _ => anyhow::bail!(
            "live runtime returned multiple agent DIDs; use --bind-agent-did home or apply against a single-agent runtime"
        ),
    }
}

fn enforce_identity_binding_safety(
    root: &Path,
    target_did: &str,
    force_rebind_concrete_did: bool,
) -> Result<()> {
    let path = root.join("identity.json");
    if !path.exists() {
        return Ok(());
    }
    let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let value: Value =
        serde_json::from_slice(&bytes).with_context(|| format!("decoding {}", path.display()))?;
    let did = match value.get("did") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => {
            let value = value.trim();
            if value.is_empty() || value.eq_ignore_ascii_case("unprovisioned") {
                None
            } else {
                Some(value.to_string())
            }
        }
        Some(other) => anyhow::bail!(
            "{}.did must be a string or null, got {other}",
            path.display()
        ),
    };

    if let Some(did) = did {
        if did != target_did && !force_rebind_concrete_did {
            anyhow::bail!(
                "identity.json.did is {did}, but resolved runtime DID is {target_did}; pass --force-rebind-concrete-did to rebind this provisioned manifest"
            );
        }
    }
    Ok(())
}

fn enforce_manifest_rebind_safety(
    manifest: &DesiredStateManifest,
    target_did: &str,
    force_rebind_concrete_did: bool,
) -> Result<()> {
    let concrete_mismatches = manifest_agent_dids(manifest)
        .into_iter()
        .filter(|did| did != target_did && !is_legacy_placeholder_did(did))
        .collect::<Vec<_>>();
    if !concrete_mismatches.is_empty() && !force_rebind_concrete_did {
        anyhow::bail!(
            "manifest contains concrete agent DID(s) that do not match resolved runtime DID {target_did}: {}; pass --force-rebind-concrete-did to rebind them",
            concrete_mismatches.join(", ")
        );
    }
    Ok(())
}

fn manifest_agent_dids(manifest: &DesiredStateManifest) -> BTreeSet<String> {
    let mut dids = BTreeSet::new();
    insert_nonempty(&mut dids, &manifest.agent_principal.agent_did);
    for behavior in &manifest.agent_behaviors {
        insert_nonempty(&mut dids, &behavior.agent_did);
    }
    for selection in &manifest.tool_selections {
        insert_nonempty(&mut dids, &selection.agent_did);
    }
    dids
}

fn insert_nonempty(values: &mut BTreeSet<String>, value: &str) {
    let value = value.trim();
    if !value.is_empty() {
        values.insert(value.to_string());
    }
}

fn rebind_manifest_agent_did(manifest: &mut DesiredStateManifest, target_did: &str) {
    manifest.agent_principal.agent_did = target_did.to_string();
    for behavior in &mut manifest.agent_behaviors {
        behavior.agent_did = target_did.to_string();
    }
    for selection in &mut manifest.tool_selections {
        selection.agent_did = target_did.to_string();
    }
}

fn is_legacy_placeholder_did(did: &str) -> bool {
    did.trim().starts_with("did:defra-agent:")
}

fn is_rebindable_agent_did_error(error: &str) -> bool {
    (error.starts_with("behavior ") || error.starts_with("tool selection "))
        && error.contains(" belongs to ")
        && error.contains(" not ")
}
