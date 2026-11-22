use serde::Serialize;

use gents_documents::{AgentContext, IntegrationTools, PluginToolRef, Tools};

/// What wiring changed, answer first: the changes, then the job block the
/// operator consents to, then warnings and the next call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WireReport {
    pub changed: Vec<String>,
    /// The plugin's job block after the change; `None` means non-job verbs only.
    pub job: Option<gents_protocol::job::PluginJobSettings>,
    pub warnings: Vec<String>,
    /// What the incoming block asked of the dynamic policy that the
    /// operator's kept policy does not give, and how to change it.
    pub policy_warnings: Vec<PolicyWarning>,
    /// Set only when no job block was written (R14).
    pub next_call: Option<String>,
}

/// Lean `PluginWiring.Warning`: the warnings about the operator's dynamic
/// policy a wiring keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyWarningCode {
    DynamicModeKept,
    DynamicTargetsKept,
    DynamicRunRepinned,
}

/// Who reads a policy warning's remedy, which names that reader's own calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemedySurface {
    Cli,
    ConfigTool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PolicyWarning {
    pub code: PolicyWarningCode,
    pub plugin: String,
    /// The Tools document whose job block holds the kept policy.
    pub tools_id: String,
    /// What the incoming block asked for; `None` when a re-pin had none.
    pub requested: Option<gents_protocol::job::JobPolicy>,
    pub kept: gents_protocol::job::JobPolicy,
    pub previous_digest: Option<String>,
    pub installed_digest: String,
    pub remedy: String,
}

impl PolicyWarning {
    fn remedy_for(&self, surface: RemedySurface) -> String {
        let (plugin, tools_id) = (&self.plugin, &self.tools_id);
        let kept = match self.code {
            PolicyWarningCode::DynamicRunRepinned => format!(
                "{plugin} now runs {} under the job.policy.dynamic run decided for {}.",
                self.installed_digest,
                self.previous_digest.as_deref().unwrap_or("another artifact")
            ),
            PolicyWarningCode::DynamicModeKept | PolicyWarningCode::DynamicTargetsKept => {
                format!("Wiring kept the job.policy of {plugin} and changed nothing in it.")
            }
        };
        let change = match surface {
            RemedySurface::Cli => format!(
                "To change it: gents config tools show {tools_id} > tools.json, edit that plugin's job.policy, then gents config tools set --file tools.json, with the --home or --graphql of this call."
            ),
            RemedySurface::ConfigTool => format!(
                "To change it: config {{\"argv\":[\"tools\",\"get\"],\"target_id\":\"{tools_id}\"}}, then [\"tools\",\"edit\"] on the same target_id with set.integrations sent back and that plugin's job.policy changed."
            ),
        };
        format!("{kept} {change}")
    }

    /// This warning with the remedy `surface`'s reader acts on.
    pub fn rendered_for(mut self, surface: RemedySurface) -> Self {
        self.remedy = self.remedy_for(surface);
        self
    }
}

impl WireReport {
    /// This report with each policy warning's remedy for `surface`.
    pub fn rendered_for(mut self, surface: RemedySurface) -> Self {
        self.policy_warnings = self
            .policy_warnings
            .into_iter()
            .map(|warning| warning.rendered_for(surface))
            .collect();
        self
    }
}

fn union_into(list: &mut Vec<String>, items: &[String]) {
    for item in items {
        if !list.contains(item) {
            list.push(item.clone());
        }
    }
}

/// Lean `PluginWiring.merge`: merges `new` into `existing` without widening
/// anything `existing` grants and keeping its dynamic mode and targets
/// (targets and trusted packs are the additions).
fn merge_job_blocks(
    existing: &gents_protocol::job::PluginJobSettings,
    new: &gents_protocol::job::PluginJobSettings,
) -> gents_protocol::job::PluginJobSettings {
    let mut merged = existing.clone();
    union_into(&mut merged.agent_targets, &new.agent_targets);
    union_into(&mut merged.policy.trusted_packs, &new.policy.trusted_packs);
    merged.max_live = existing.max_live.min(new.max_live);
    merged.max_spawns = existing.max_spawns.min(new.max_spawns);
    merged.max_messages_per_slot = existing
        .max_messages_per_slot
        .min(new.max_messages_per_slot);
    merged.result_bytes = existing.result_bytes.min(new.result_bytes);
    merged.max_wall_secs = existing.max_wall_secs.min(new.max_wall_secs);
    merged
}

/// Lean `PluginWiring.policyWarnings`.
fn policy_warning_codes(
    existing: &gents_protocol::job::JobPolicy,
    incoming: &gents_protocol::job::JobPolicy,
) -> Vec<PolicyWarningCode> {
    use gents_protocol::job::DynamicMode;
    let mut codes = Vec::new();
    if existing.dynamic != incoming.dynamic {
        codes.push(PolicyWarningCode::DynamicModeKept);
    }
    if incoming.dynamic != DynamicMode::Off
        && incoming
            .dynamic_targets
            .iter()
            .any(|target| !existing.dynamic_targets.contains(target))
    {
        codes.push(PolicyWarningCode::DynamicTargetsKept);
    }
    codes
}

/// Wires an installed plugin into an agent's Tools and Context: the plugin
/// entry pinned to the installed digest, its job block (the reviewed block, or
/// the defaults over `targets_from`; a merge never widens an existing block),
/// the targets, trusted packs and skills. Each `targets_from` entry is
/// (pack coordinate, the AgentTarget ids that pack ships, its provenance).
/// `agents.enabled` is never written.
pub fn wire_plugin(
    tools: &Tools,
    context: &AgentContext,
    plugin: &crate::plugin::store::InstalledPlugin,
    reviewed_job: Option<&gents_protocol::job::PluginJobSettings>,
    skill_ids: &[String],
    targets_from: &[(String, Vec<String>, crate::pack::InstalledVia)],
) -> anyhow::Result<(Tools, AgentContext, WireReport)> {
    use gents_documents::plugin_job_settings;
    use gents_protocol::job::{JobPolicy, PluginJobSettings};

    let coordinate = format!("{}/{}", plugin.namespace, plugin.name);
    let tools_id = tools.tools_id.clone();
    let mut tools = tools.clone();
    let mut context = context.clone();
    let mut changed = Vec::new();
    let mut warnings = Vec::new();
    let mut policy_codes = Vec::new();

    let plugins = tools
        .integrations
        .get_or_insert_with(Default::default)
        .plugins
        .get_or_insert_with(Default::default);
    let prior_digest = plugins
        .iter()
        .find(|entry| entry.plugin == coordinate)
        .and_then(|entry| entry.digest.clone());
    let index = match plugins.iter().position(|entry| entry.plugin == coordinate) {
        Some(index) => {
            plugins[index].digest = Some(plugin.digest.clone());
            changed.push(format!("pinned plugin {coordinate} to {}", plugin.digest));
            index
        }
        None => {
            plugins.push(PluginToolRef {
                plugin: coordinate.clone(),
                digest: Some(plugin.digest.clone()),
                tool_calls: false,
                input_fields: Vec::new(),
                job: None,
            });
            changed.push(format!("added plugin {coordinate} at {}", plugin.digest));
            plugins.len() - 1
        }
    };

    let agents = tools.agents.get_or_insert_with(Default::default);
    let targets_before = agents.target_ids.clone();
    let mut job = plugins[index].job.clone();
    let repinned_under_run = prior_digest.as_ref().is_some_and(|prior| *prior != plugin.digest)
        && job
            .as_ref()
            .is_some_and(|job| job.policy.dynamic == gents_protocol::job::DynamicMode::Run);
    let mut requested = None;
    if plugin.declaration.job.0 {
        let incoming = reviewed_job.cloned().or_else(|| {
            (!targets_from.is_empty()).then(|| PluginJobSettings {
                agent_targets: Vec::new(),
                max_live: PluginJobSettings::DEFAULT_MAX_LIVE,
                max_spawns: PluginJobSettings::DEFAULT_MAX_SPAWNS,
                max_messages_per_slot: PluginJobSettings::DEFAULT_MAX_MESSAGES_PER_SLOT,
                result_bytes: PluginJobSettings::DEFAULT_RESULT_BYTES,
                max_wall_secs: PluginJobSettings::DEFAULT_MAX_WALL_SECS,
                policy: JobPolicy {
                    trusted_packs: plugin.owner_pack_coordinate.iter().cloned().collect(),
                    ..JobPolicy::default()
                },
            })
        });
        if let Some(incoming) = incoming {
            if let Some(existing) = &job {
                policy_codes = policy_warning_codes(&existing.policy, &incoming.policy);
            }
            requested = Some(incoming.policy.clone());
            let merged = job.is_some();
            let mut block = match &job {
                Some(existing) => merge_job_blocks(existing, &incoming),
                None => incoming,
            };
            for (pack, pack_targets, via) in targets_from {
                (block.policy, block.agent_targets, agents.target_ids) =
                    gents_protocol::job_settings::wire_targets_from(
                        pack,
                        pack_targets,
                        block.policy,
                        &block.agent_targets,
                        &agents.target_ids,
                    );
                changed.push(format!("trusted pack {pack} and wired its targets"));
                if *via != crate::pack::InstalledVia::Operator {
                    warnings.push(format!(
                        "trusted_pack_not_operator: {pack} is not operator-installed; its scripts classify as dynamic"
                    ));
                }
            }
            union_into(&mut agents.target_ids, &block.agent_targets);
            let problems = plugin_job_settings::validate(&block);
            anyhow::ensure!(
                problems.is_empty(),
                "job block for {coordinate} is not valid: {}",
                problems.join("; ")
            );
            changed.push(format!(
                "{} job block for {coordinate}",
                if merged { "merged" } else { "wrote" }
            ));
            job = Some(block);
        }
    }
    plugins[index].job = job.clone();
    if job
        .as_ref()
        .is_some_and(|job| job.policy.dynamic == gents_protocol::job::DynamicMode::Check)
    {
        warnings.push(format!(
            "dynamic_check: {}",
            plugin_job_settings::dynamic_check_notice(&coordinate)
        ));
    }

    let added: Vec<String> = agents
        .target_ids
        .iter()
        .filter(|id| !targets_before.contains(id))
        .cloned()
        .collect();
    if !added.is_empty() {
        changed.push(format!("added agent targets {}", added.join(", ")));
        if agents.enabled == Some(true) {
            warnings.push(format!(
                "agent_new_widened: {} are also reachable by agent_new",
                added.join(", ")
            ));
        }
    }

    let skills: Vec<String> = skill_ids
        .iter()
        .filter(|id| !context.skill_ids.contains(id))
        .cloned()
        .collect();
    if !skills.is_empty() {
        changed.push(format!("added skills {}", skills.join(", ")));
        context.skill_ids.extend(skills);
    }

    if repinned_under_run {
        policy_codes.push(PolicyWarningCode::DynamicRunRepinned);
    }
    let policy_warnings = job
        .as_ref()
        .map(|kept| {
            policy_codes
                .into_iter()
                .map(|code| {
                    PolicyWarning {
                        code,
                        plugin: coordinate.clone(),
                        tools_id: tools_id.clone(),
                        requested: requested.clone(),
                        kept: kept.policy.clone(),
                        previous_digest: prior_digest.clone(),
                        installed_digest: plugin.digest.clone(),
                        remedy: String::new(),
                    }
                    .rendered_for(RemedySurface::Cli)
                })
                .collect()
        })
        .unwrap_or_default();
    let next_call = (plugin.declaration.job.0 && job.is_none()).then(|| {
        format!("gents config pack wire {coordinate} --agent <agent> --targets-from <pack>")
    });
    Ok((
        tools,
        context,
        WireReport {
            changed,
            job,
            warnings,
            policy_warnings,
            next_call,
        },
    ))
}

/// Applies the node owner's consent to `coordinate` calling its caller's own
/// tools: the selection [`wire_plugin`] wrote gets `tool_calls: true`. A pack
/// cannot ship that grant for an agent it does not own, so it is given where
/// the owner approves the pack.
pub fn consent_tool_calls(tools: &mut Tools, coordinate: &str, report: &mut WireReport) {
    let Some(selection) = tools
        .integrations
        .iter_mut()
        .flat_map(|integrations| integrations.plugins.iter_mut().flatten())
        .find(|plugin| plugin.plugin == coordinate && !plugin.tool_calls)
    else {
        return;
    };
    selection.tool_calls = true;
    report.changed.push(format!(
        "set tool_calls true for {coordinate}: the node owner approved it calling this agent's tools"
    ));
}

/// Lean `PluginWiring.dynamicEnabled`: the block's dynamic scripts run.
pub fn dynamic_enabled(job: &gents_protocol::job::PluginJobSettings) -> bool {
    job.policy.dynamic == gents_protocol::job::DynamicMode::Run
}

/// What the operator's enable changed on one plugin's block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DynamicEnableReceipt {
    pub plugin: String,
    pub before: gents_protocol::job::JobPolicy,
    pub after: gents_protocol::job::JobPolicy,
}

/// The Tools select the plugin without a job block, so there is no dynamic
/// policy to enable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{plugin} has no job block on these Tools, so there is no dynamic policy to enable. Next: gents config pack wire {plugin} --agent <agent> --targets-from <pack>")]
pub struct NoJobBlock {
    pub plugin: String,
}

/// Lean `PluginWiring.operatorEnable`: the operator's explicit enable of
/// `coordinate`'s dynamic scripts, the one way a block reaches `run`.
pub fn operator_enable_dynamic(
    tools: &Tools,
    coordinate: &str,
    targets: &[String],
) -> Result<(Tools, DynamicEnableReceipt), NoJobBlock> {
    let mut tools = tools.clone();
    let job = tools
        .integrations
        .iter_mut()
        .flat_map(|integrations| integrations.plugins.iter_mut().flatten())
        .find(|plugin| plugin.plugin == coordinate)
        .and_then(|plugin| plugin.job.as_mut())
        .ok_or_else(|| NoJobBlock {
            plugin: coordinate.to_owned(),
        })?;
    let before = job.policy.clone();
    job.policy.dynamic = gents_protocol::job::DynamicMode::Run;
    job.policy.dynamic_targets = targets.to_vec();
    union_into(&mut job.agent_targets, targets);
    let after = job.policy.clone();
    union_into(
        &mut tools.agents.get_or_insert_with(Default::default).target_ids,
        targets,
    );
    Ok((
        tools,
        DynamicEnableReceipt {
            plugin: coordinate.to_owned(),
            before,
            after,
        },
    ))
}

/// Inverse of [`wire_plugin`]. With no `targets_from`, drops the plugin entry
/// and then its job targets and the `skill_ids`, each only when no other
/// plugin entry still needs it (skills are not recorded per plugin, so any
/// remaining plugin keeps them). With `targets_from`, keeps the plugin and
/// removes only those packs' targets and trust entries; a block left without
/// targets is dropped. `agents.enabled` and `policy.dynamic` are never written.
pub fn unwire_plugin(
    tools: &Tools,
    context: &AgentContext,
    plugin: &crate::plugin::store::InstalledPlugin,
    skill_ids: &[String],
    targets_from: &[(String, Vec<String>, crate::pack::InstalledVia)],
) -> anyhow::Result<(Tools, AgentContext, WireReport)> {
    use gents_documents::plugin_job_settings;

    let coordinate = format!("{}/{}", plugin.namespace, plugin.name);
    let mut tools = tools.clone();
    let mut context = context.clone();
    let mut changed = Vec::new();
    let mut warnings = Vec::new();
    let mut next_call = None;

    let mut plugins = tools
        .integrations
        .as_ref()
        .and_then(|integrations| integrations.plugins.clone())
        .unwrap_or_default();
    let Some(index) = plugins.iter().position(|entry| entry.plugin == coordinate) else {
        warnings.push(format!("{coordinate} is not wired; nothing to remove"));
        return Ok((
            tools,
            context,
            WireReport {
                changed,
                job: None,
                warnings,
                policy_warnings: Vec::new(),
                next_call,
            },
        ));
    };
    let entry = plugins.remove(index);
    let referenced_elsewhere = |id: &String| {
        plugins
            .iter()
            .filter_map(|other| other.job.as_ref())
            .any(|block| {
                block.agent_targets.contains(id) || block.policy.dynamic_targets.contains(id)
            })
    };

    let mut job = entry.job.clone();
    let mut dropped_targets: Vec<String> = Vec::new();
    if targets_from.is_empty() {
        changed.push(format!("removed plugin {coordinate}"));
        dropped_targets = entry
            .job
            .iter()
            .flat_map(|block| block.agent_targets.clone())
            .collect();
        job = None;
        let skills: Vec<&String> = skill_ids
            .iter()
            .filter(|id| context.skill_ids.contains(id))
            .collect();
        if plugins.is_empty() && !skills.is_empty() {
            changed.push(format!(
                "removed skills {}",
                skills
                    .iter()
                    .map(|id| id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            context.skill_ids.retain(|id| !skill_ids.contains(id));
        }
    } else if let Some(block) = job.as_mut() {
        for (pack, pack_targets, _) in targets_from {
            dropped_targets.extend(
                block
                    .agent_targets
                    .iter()
                    .filter(|id| pack_targets.contains(id))
                    .cloned(),
            );
            block.agent_targets.retain(|id| !pack_targets.contains(id));
            if plugin.owner_pack_coordinate.as_deref() != Some(pack.as_str()) {
                block.policy.trusted_packs.retain(|trusted| trusted != pack);
            }
            changed.push(format!("untrusted pack {pack} and unwired its targets"));
        }
        block
            .policy
            .dynamic_targets
            .retain(|id| block.agent_targets.contains(id));
        if block.agent_targets.is_empty() {
            job = None;
            changed.push(format!("dropped the job block for {coordinate}"));
            next_call = Some(format!(
                "{coordinate} now serves non-job verbs only; to run jobs again: gents config pack wire {coordinate} --agent <agent> --targets-from <pack>"
            ));
        } else {
            let problems = plugin_job_settings::validate(block);
            anyhow::ensure!(
                problems.is_empty(),
                "job block for {coordinate} is not valid: {}",
                problems.join("; ")
            );
        }
    }

    if let Some(agents) = tools.agents.as_mut() {
        let kept = |id: &String| {
            referenced_elsewhere(id) || job.as_ref().is_some_and(|b| b.agent_targets.contains(id))
        };
        let removed: Vec<String> = dropped_targets
            .into_iter()
            .filter(|id| agents.target_ids.contains(id) && !kept(id))
            .collect();
        if !removed.is_empty() {
            agents.target_ids.retain(|id| !removed.contains(id));
            changed.push(format!("removed agent targets {}", removed.join(", ")));
        }
    }

    if targets_from.is_empty() {
        if plugins.is_empty() {
            if let Some(integrations) = tools.integrations.as_mut() {
                integrations.plugins = None;
                if *integrations == IntegrationTools::default() {
                    tools.integrations = None;
                }
            }
        } else {
            tools
                .integrations
                .get_or_insert_with(Default::default)
                .plugins = Some(plugins);
        }
    } else {
        let mut entry = entry;
        entry.job = job.clone();
        plugins.insert(index, entry);
        tools
            .integrations
            .get_or_insert_with(Default::default)
            .plugins = Some(plugins);
    }

    Ok((
        tools,
        context,
        WireReport {
            changed,
            job,
            warnings,
            policy_warnings: Vec::new(),
            next_call,
        },
    ))
}

#[cfg(test)]
mod tests;
