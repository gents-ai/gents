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
    // Capture source/local DIDs before overwriting so same-deployment
    // `subagent_targets` entries can be recognized after the top-level rewrite.
    // Only DIDs this function already treats as local (principal, behaviors,
    // selections) are candidates — genuine cross-deployment target DIDs are
    // left unchanged.
    let source_local_dids = manifest_agent_dids(manifest);

    manifest.agent_principal.agent_did = target_did.to_string();
    for behavior in &mut manifest.agent_behaviors {
        behavior.agent_did = target_did.to_string();
    }
    for selection in &mut manifest.tool_selections {
        selection.agent_did = target_did.to_string();
        rebind_subagent_target_dids(
            &mut selection.subagent_targets,
            &source_local_dids,
            target_did,
        );
    }
}

/// Rewrite DIDs embedded in `subagent_targets` JSON entries that match a
/// source/local DID being rebound. Malformed entries are preserved so desired-
/// state validation can still report the precise parse error.
fn rebind_subagent_target_dids(
    entries: &mut [String],
    source_local_dids: &BTreeSet<String>,
    target_did: &str,
) {
    for entry in entries {
        let Ok(mut target) = gents::SubagentTarget::parse(entry) else {
            continue;
        };
        if source_local_dids.contains(target.agent_did.trim()) {
            target.agent_did = target_did.to_string();
            *entry = target.to_entry();
        }
    }
}

fn is_rebindable_agent_did_error(error: &str) -> bool {
    (error.starts_with("behavior ") || error.starts_with("tool selection "))
        && error.contains(" belongs to ")
        && error.contains(" not ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desired_state::{
        DesiredAgentBehavior, DesiredAgentPrincipal, DesiredStateManifest, DesiredToolSelection,
    };
    use gents::{subagent_target_entry, SubagentTarget};

    const SOURCE_DID: &str = "did:defra-agent:amy";
    const TARGET_DID: &str = "did:key:z6MkiResolvedRuntimeDid";
    const REMOTE_DID: &str = "did:key:z6MkRemoteOtherDeployment";

    fn empty_manifest(agent_did: &str) -> DesiredStateManifest {
        DesiredStateManifest {
            agent_principal: DesiredAgentPrincipal {
                agent_did: agent_did.to_string(),
                display_name: Some("Amy".to_string()),
                default_behavior_id: Some("default".to_string()),
                enabled: true,
            },
            agent_behaviors: Vec::new(),
            skills: Vec::new(),
            tool_selections: Vec::new(),
            inference_backends: Vec::new(),
            inference_profiles: Vec::new(),
            tool_service_registries: Vec::new(),
            projection_acp_bindings: Vec::new(),
            peer_pairings: Vec::new(),
            tasks: Vec::new(),
            schedules: Vec::new(),
            event_triggers: Vec::new(),
        }
    }

    fn sample_behavior(agent_did: &str) -> DesiredAgentBehavior {
        DesiredAgentBehavior {
            behavior_id: "default".to_string(),
            agent_did: agent_did.to_string(),
            display_name: Some("Default".to_string()),
            description: None,
            summary: None,
            system_prompt: Some("Be helpful.".to_string()),
            request_context_template: None,
            backend_id: Some("default-backend".to_string()),
            model_name: Some("mock-model".to_string()),
            tool_selection_id: Some("default-tools".to_string()),
            inference_profile_id: None,
            compaction_strategy: None,
            compaction_threshold: None,
            enabled: true,
            skill_refs: Vec::new(),
            skill_excludes: Vec::new(),
        }
    }

    fn sample_selection(agent_did: &str, targets: Vec<String>) -> DesiredToolSelection {
        DesiredToolSelection {
            selection_id: "default-tools".to_string(),
            agent_did: agent_did.to_string(),
            display_name: Some("Standard".to_string()),
            tool_policy_version: None,
            enable_file_tools: false,
            file_tools_mode: "ReadOnly".to_string(),
            file_tool_root: None,
            enable_bash: false,
            bash_mode: "ReadOnly".to_string(),
            command_execution_policy: None,
            command_allowed_argv_prefixes: Vec::new(),
            command_forbidden_argv_prefixes: Vec::new(),
            read_only_command_allowlist: Vec::new(),
            command_network_mode: None,
            cli_tool_names: Vec::new(),
            enable_meta_tools: true,
            allowed_mcp_service_ids: Vec::new(),
            delegate_to: Vec::new(),
            backgroundable_tool_names: Vec::new(),
            enable_memory: false,
            enable_session_history_tool: false,
            enable_context_budget: true,
            enable_defra_query: true,
            defra_query_collections: Vec::new(),
            subagent_targets: targets,
            subagent_spawn_enabled: true,
            orchestration_enabled: false,
            subagent_steering_enabled: false,
            subagent_background_enabled: false,
            subagent_default_await_mode: None,
            subagent_allow_cross_deployment: false,
            cross_deployment_spawn_timeout_seconds: None,
            write_tools: Vec::new(),
            enable_self_config: false,
            self_config_categories: Vec::new(),
            self_config_no_lockout: false,
            self_config_dry_run: false,
        }
    }

    fn parse_targets(entries: &[String]) -> Vec<SubagentTarget> {
        entries
            .iter()
            .map(|entry| SubagentTarget::parse(entry).expect("entry must parse"))
            .collect()
    }

    #[test]
    fn rebind_rewrites_same_deployment_subagent_target_dids() {
        let mut manifest = empty_manifest(SOURCE_DID);
        manifest.agent_behaviors.push(sample_behavior(SOURCE_DID));
        manifest.tool_selections.push(sample_selection(
            SOURCE_DID,
            vec![
                subagent_target_entry(
                    "session-classifier",
                    SOURCE_DID,
                    "session-classifier",
                    Some("Classifies the active session".to_string()),
                ),
                subagent_target_entry("glm52", SOURCE_DID, "glm52", None),
            ],
        ));

        rebind_manifest_agent_did(&mut manifest, TARGET_DID);

        assert_eq!(manifest.agent_principal.agent_did, TARGET_DID);
        assert_eq!(manifest.agent_behaviors[0].agent_did, TARGET_DID);
        let selection = &manifest.tool_selections[0];
        assert_eq!(selection.agent_did, TARGET_DID);

        let targets = parse_targets(&selection.subagent_targets);
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].name, "session-classifier");
        assert_eq!(targets[0].agent_did, TARGET_DID);
        assert_eq!(targets[0].behavior_id, "session-classifier");
        assert_eq!(
            targets[0].description.as_deref(),
            Some("Classifies the active session")
        );
        assert_eq!(targets[1].name, "glm52");
        assert_eq!(targets[1].agent_did, TARGET_DID);
        assert_eq!(targets[1].behavior_id, "glm52");
        assert_eq!(targets[1].description, None);

        // Post-bind validation must not misclassify same-deployment targets as remote.
        let mut errors = Vec::new();
        desired_state::validate::validate_manifest(&manifest, &mut errors);
        assert!(
            !errors
                .iter()
                .any(|msg| msg.contains("cross-deployment subagent delegation is deferred")),
            "same-deployment targets must validate after rebind, got {errors:?}"
        );
    }

    #[test]
    fn rebind_preserves_cross_deployment_target_did() {
        let mut manifest = empty_manifest(SOURCE_DID);
        manifest.agent_behaviors.push(sample_behavior(SOURCE_DID));
        let mut selection = sample_selection(
            SOURCE_DID,
            vec![
                subagent_target_entry("local-helper", SOURCE_DID, "helper", None),
                subagent_target_entry("remote-researcher", REMOTE_DID, "amy-research", None),
            ],
        );
        selection.subagent_allow_cross_deployment = true;
        manifest.tool_selections.push(selection);

        rebind_manifest_agent_did(&mut manifest, TARGET_DID);

        let targets = parse_targets(&manifest.tool_selections[0].subagent_targets);
        assert_eq!(targets[0].agent_did, TARGET_DID);
        assert_eq!(targets[1].name, "remote-researcher");
        assert_eq!(targets[1].agent_did, REMOTE_DID);
        assert_eq!(targets[1].behavior_id, "amy-research");
    }

    #[test]
    fn rebind_preserves_malformed_subagent_target_json() {
        let malformed = r#"{"name":"broken","agent_did":"#;
        let mut manifest = empty_manifest(SOURCE_DID);
        manifest.agent_behaviors.push(sample_behavior(SOURCE_DID));
        manifest.tool_selections.push(sample_selection(
            SOURCE_DID,
            vec![
                subagent_target_entry("ok", SOURCE_DID, "helper", None),
                malformed.to_string(),
            ],
        ));

        rebind_manifest_agent_did(&mut manifest, TARGET_DID);

        let selection = &manifest.tool_selections[0];
        assert_eq!(selection.subagent_targets[1], malformed);

        let mut errors = Vec::new();
        desired_state::validate::validate_manifest(&manifest, &mut errors);
        assert!(
            errors.iter().any(|msg| {
                msg.contains("is not valid SubagentTarget JSON") && msg.contains("broken")
            }),
            "malformed entry must still produce an actionable validation error, got {errors:?}"
        );
        // The well-formed same-deployment target was still rebound.
        let ok = SubagentTarget::parse(&selection.subagent_targets[0]).unwrap();
        assert_eq!(ok.agent_did, TARGET_DID);
    }

    #[test]
    fn rebind_is_idempotent() {
        let mut manifest = empty_manifest(SOURCE_DID);
        manifest.agent_behaviors.push(sample_behavior(SOURCE_DID));
        manifest.tool_selections.push(sample_selection(
            SOURCE_DID,
            vec![
                subagent_target_entry(
                    "session-classifier",
                    SOURCE_DID,
                    "session-classifier",
                    Some("Classifies the active session".to_string()),
                ),
                subagent_target_entry("remote-researcher", REMOTE_DID, "amy-research", None),
            ],
        ));
        manifest.tool_selections[0].subagent_allow_cross_deployment = true;

        rebind_manifest_agent_did(&mut manifest, TARGET_DID);
        let once = manifest.clone();
        rebind_manifest_agent_did(&mut manifest, TARGET_DID);

        assert_eq!(manifest.agent_principal, once.agent_principal);
        assert_eq!(manifest.agent_behaviors, once.agent_behaviors);
        assert_eq!(manifest.tool_selections, once.tool_selections);
    }

    #[test]
    fn rebind_does_not_weaken_cross_deployment_guard() {
        let mut manifest = empty_manifest(SOURCE_DID);
        manifest.agent_behaviors.push(sample_behavior(SOURCE_DID));
        let mut selection = sample_selection(
            SOURCE_DID,
            vec![subagent_target_entry(
                "remote-researcher",
                REMOTE_DID,
                "amy-research",
                None,
            )],
        );
        // Guard remains off: a truly remote target must still be rejected.
        selection.subagent_allow_cross_deployment = false;
        manifest.tool_selections.push(selection);

        rebind_manifest_agent_did(&mut manifest, TARGET_DID);

        let targets = parse_targets(&manifest.tool_selections[0].subagent_targets);
        assert_eq!(targets[0].agent_did, REMOTE_DID);

        let mut errors = Vec::new();
        desired_state::validate::validate_manifest(&manifest, &mut errors);
        assert!(
            errors.iter().any(|msg| {
                msg.contains("cross-deployment subagent delegation is deferred")
                    && msg.contains("remote-researcher")
                    && msg.contains("subagent_allow_cross_deployment=true")
            }),
            "default cross-deployment guard must still reject a remote target, got {errors:?}"
        );
    }

    #[test]
    fn rebind_preserves_target_list_order() {
        let mut manifest = empty_manifest(SOURCE_DID);
        manifest.agent_behaviors.push(sample_behavior(SOURCE_DID));
        manifest.tool_selections.push(sample_selection(
            SOURCE_DID,
            vec![
                subagent_target_entry("first", SOURCE_DID, "b1", None),
                subagent_target_entry("second", REMOTE_DID, "b2", None),
                subagent_target_entry("third", SOURCE_DID, "b3", Some("last local".to_string())),
            ],
        ));
        manifest.tool_selections[0].subagent_allow_cross_deployment = true;

        rebind_manifest_agent_did(&mut manifest, TARGET_DID);

        let names: Vec<_> = parse_targets(&manifest.tool_selections[0].subagent_targets)
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names, vec!["first", "second", "third"]);
    }
}
