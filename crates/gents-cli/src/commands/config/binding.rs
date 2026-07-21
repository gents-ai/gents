use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gents::{AgentIdentity, KeyIdentity};
use serde_json::Value;

use crate::cli::ManifestAgentDidBindingArg;
use crate::config_writes::ConfigAccess;
use crate::desired_state::{
    self, DesiredStateCounts, DesiredStateManifest, DesiredStateValidationReport,
};
use crate::print_json;

pub(crate) struct ManifestBindingOptions<'a> {
    pub(crate) root: &'a Path,
    pub(crate) home: Option<&'a Path>,
    pub(crate) graphql: Option<&'a str>,
    pub(crate) bind_agent_did: Option<ManifestAgentDidBindingArg>,
    pub(crate) force_rebind_concrete_did: bool,
    pub(crate) access: Option<&'a ConfigAccess>,
}

pub(crate) struct BoundManifestLoad {
    pub(crate) bound: Option<BoundDesiredManifest>,
    pub(crate) report: DesiredStateValidationReport,
}

pub(crate) struct BoundDesiredManifest {
    pub(crate) context: ManifestBindingContext,
    pub(crate) manifest: DesiredStateManifest,
}

#[derive(Debug, Clone)]
// Provision orchestration will consume the full context; config validate/diff/apply
// currently only need the target DID after loading.
#[allow(dead_code)]
pub(crate) struct ManifestBindingContext {
    pub(crate) bind_mode: ManifestBindMode,
    pub(crate) target_agent_did: String,
    pub(crate) source_manifest_dids: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManifestBindMode {
    Manifest,
    Home,
    Live,
}

impl ManifestBindMode {
    fn from_cli(value: Option<ManifestAgentDidBindingArg>) -> Self {
        match value {
            None => Self::Manifest,
            Some(ManifestAgentDidBindingArg::Home) => Self::Home,
            Some(ManifestAgentDidBindingArg::Live) => Self::Live,
        }
    }
}

impl BoundManifestLoad {
    pub(crate) fn require_valid(self) -> Result<BoundDesiredManifest> {
        if !self.report.is_ok() {
            print_json(&serde_json::to_value(&self.report)?)?;
            anyhow::bail!("desired-state manifest validation failed");
        }
        self.bound
            .ok_or_else(|| anyhow::anyhow!("validated manifest root produced no manifest"))
    }
}

pub(crate) async fn load_bound_manifest(
    options: ManifestBindingOptions<'_>,
) -> Result<BoundManifestLoad> {
    let root_display = options.root.display().to_string();
    let (manifest, initial_report) = desired_state::load_manifest_root(options.root);
    let Some(mut manifest) = manifest else {
        return Ok(BoundManifestLoad {
            bound: None,
            report: initial_report,
        });
    };

    let bind_mode = ManifestBindMode::from_cli(options.bind_agent_did);
    let source_manifest_dids = manifest_agent_dids(&manifest);

    if bind_mode == ManifestBindMode::Manifest {
        let target_agent_did = manifest.agent_principal.agent_did.clone();
        return Ok(BoundManifestLoad {
            bound: Some(BoundDesiredManifest {
                context: ManifestBindingContext {
                    bind_mode,
                    target_agent_did,
                    source_manifest_dids,
                },
                manifest,
            }),
            report: initial_report,
        });
    }

    if !initial_report.is_ok()
        && !initial_report
            .errors
            .iter()
            .all(|error| is_rebindable_agent_did_error(error))
    {
        let agent_did = manifest.agent_principal.agent_did.clone();
        return Ok(BoundManifestLoad {
            bound: Some(BoundDesiredManifest {
                context: ManifestBindingContext {
                    bind_mode,
                    target_agent_did: agent_did,
                    source_manifest_dids,
                },
                manifest,
            }),
            report: initial_report,
        });
    }

    let target_did =
        resolve_bound_agent_did(bind_mode, options.home, options.graphql, options.access).await?;
    enforce_manifest_rebind_safety(&manifest, &target_did, options.force_rebind_concrete_did)?;
    rebind_manifest_agent_did(&mut manifest, &target_did);

    let report = validation_report_for_manifest(root_display, &manifest);
    Ok(BoundManifestLoad {
        bound: Some(BoundDesiredManifest {
            context: ManifestBindingContext {
                bind_mode,
                target_agent_did: target_did,
                source_manifest_dids,
            },
            manifest,
        }),
        report,
    })
}

pub(crate) async fn resolve_target_agent_did(
    explicit_agent_did: Option<&str>,
    bind_agent_did: Option<ManifestAgentDidBindingArg>,
    home: Option<&Path>,
    graphql: Option<&str>,
    access: Option<&ConfigAccess>,
) -> Result<String> {
    if explicit_agent_did
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
        && bind_agent_did.is_some()
    {
        anyhow::bail!("pass either --agent-did or --bind-agent-did, not both");
    }

    if let Some(agent_did) = explicit_agent_did
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(agent_did.to_string());
    }

    let Some(bind_agent_did) = bind_agent_did else {
        return crate::resolve_agent_did(home, None);
    };

    resolve_bound_agent_did(
        ManifestBindMode::from_cli(Some(bind_agent_did)),
        home,
        graphql,
        access,
    )
    .await
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
        skills: manifest.skills.len(),
        tool_selections: manifest.tool_selections.len(),
        inference_backends: manifest.inference_backends.len(),
        inference_profiles: manifest.inference_profiles.len(),
        tool_service_registries: manifest.tool_service_registries.len(),
        projection_acp_bindings: manifest.projection_acp_bindings.len(),
        peer_pairings: manifest.peer_pairings.len(),
        tasks: manifest.tasks.len(),
        schedules: manifest.schedules.len(),
        event_triggers: manifest.event_triggers.len(),
    }
}

async fn resolve_bound_agent_did(
    bind_mode: ManifestBindMode,
    home: Option<&Path>,
    graphql: Option<&str>,
    access: Option<&ConfigAccess>,
) -> Result<String> {
    match bind_mode {
        ManifestBindMode::Manifest => {
            anyhow::bail!("manifest binding mode does not resolve a runtime DID")
        }
        ManifestBindMode::Home => resolve_home_binding_agent_did(home)
            .context("resolving agent DID from initialized home"),
        ManifestBindMode::Live => {
            if let Some(access) = access {
                return resolve_live_agent_did(access).await;
            }
            let (access, _) = crate::resolve_config_access(home, graphql, false).await?;
            resolve_live_agent_did(&access).await
        }
    }
}

fn resolve_home_binding_agent_did(home: Option<&Path>) -> Result<String> {
    let home_dir = crate::resolve_home_dir(home);
    let init_config = crate::read_init_config(&home_dir)?.ok_or_else(|| {
        anyhow::anyhow!(
            "initialized home metadata is required for --bind-agent-did home; run `gents init --identity-only --home {}` first",
            home_dir.display()
        )
    })?;

    let init_did = init_config.agent_did.trim();
    if !init_did.is_empty() {
        if let Some(key_did) = load_init_key_did(init_config.key_path.as_deref(), &home_dir)? {
            if key_did != init_did {
                anyhow::bail!(
                    "initialized home {} has agent DID {init_did}, but identity key resolves to {key_did}; rerun `gents init --identity-only` or repair the home identity metadata",
                    home_dir.display()
                );
            }
        }
        return Ok(init_did.to_string());
    }

    load_init_key_did(init_config.key_path.as_deref(), &home_dir)?.ok_or_else(|| {
        anyhow::anyhow!(
            "initialized home {} does not contain an agent DID or identity key path; rerun `gents init --identity-only`",
            home_dir.display()
        )
    })
}

fn load_init_key_did(key_path: Option<&str>, home_dir: &Path) -> Result<Option<String>> {
    let Some(key_path) = key_path
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    else {
        return Ok(None);
    };
    if !key_path.exists() {
        anyhow::bail!(
            "initialized home {} points to missing identity key {}; rerun `gents init --identity-only`",
            home_dir.display(),
            key_path.display()
        );
    }

    let identity = KeyIdentity::load_or_create(&key_path, None)
        .with_context(|| format!("loading identity key {}", key_path.display()))?;
    Ok(Some(identity.did().to_string()))
}

async fn resolve_live_agent_did(access: &ConfigAccess) -> Result<String> {
    let response = access
        .execute(
            r#"{
                AgentRuntime(order: { updated_at: DESC }) {
                    agent_did
                    process_state
                    updated_at
                }
            }"#,
        )
        .await
        .context("querying live AgentRuntime for agent DID")?;
    let rows = response
        .pointer("/data/AgentRuntime")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let selected = rows
        .iter()
        .copied()
        .find(|row| {
            row.get("process_state")
                .and_then(Value::as_str)
                .is_some_and(is_active_runtime_state)
        })
        .or_else(|| rows.first().copied())
        .ok_or_else(|| anyhow::anyhow!("live runtime did not return an AgentRuntime agent_did"))?;
    let agent_did = selected
        .get("agent_did")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("live runtime returned an AgentRuntime row without agent_did")
        })?;
    Ok(agent_did.to_string())
}

fn is_active_runtime_state(state: &str) -> bool {
    match state.trim() {
        "shutdown" | "shuttingDown" => false,
        value => !value.trim().is_empty(),
    }
}

fn enforce_manifest_rebind_safety(
    manifest: &DesiredStateManifest,
    target_did: &str,
    force_rebind_concrete_did: bool,
) -> Result<()> {
    let concrete_mismatches = manifest_agent_dids(manifest)
        .into_iter()
        .filter(|did| did != target_did)
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

fn is_rebindable_agent_did_error(error: &str) -> bool {
    (error.starts_with("behavior ") || error.starts_with("tool selection "))
        && error.contains(" belongs to ")
        && error.contains(" not ")
}
