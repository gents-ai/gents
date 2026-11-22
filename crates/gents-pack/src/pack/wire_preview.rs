//! What wiring a pack's plugins into agents would write, planned from the
//! pack's archive, before or without its install: `install --preview --wire`.

use anyhow::{Context, Result};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;
use serde::Serialize;
use serde_json::Value;

use crate::pack::plugin_wire::{
    explain_tools_command, wire_outcome, WireBatchOptions, WireRequest,
};
use crate::pack::resolved_pack::ResolvedPack;
use crate::pack::wire_subject::{wire_plan_record, WireRecord, WireSubject};
use crate::pack::wiring::{apply_batch, BatchWrite, PluginWiring, ReviewedJob, Vouch};
use crate::pack::InstalledVia;

/// Why a preview plans no wiring for a pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WireSkipReason {
    /// The pack authors an agent its install creates, whose identity does not
    /// exist yet, and its configuration names that agent.
    AgentIdentityPending,
}

/// A pair of an agent and a plugin a preview plans nothing for, and what
/// to do instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WireSkip {
    pub skipped: bool,
    pub preview: bool,
    pub agent: String,
    pub plugin: String,
    /// The authored agent slot the install has yet to create.
    pub slot: String,
    pub reason: WireSkipReason,
    pub next: String,
}

/// The pack's own documents vouch for themselves: nothing of the node is read
/// as the pack's.
struct Archive;

impl Vouch for Archive {
    fn own(&self, _pack: &str, _collection: Collection, _ids: &[String]) -> Result<()> {
        Ok(())
    }

    fn target(
        &self,
        _pack: &str,
        _installed: Option<&str>,
        _ids: &[String],
    ) -> Result<InstalledVia> {
        Ok(InstalledVia::Operator)
    }
}

/// What wiring each plugin `pack` ships into each of `agents` would write
/// once it is installed, validated with the documents the install writes,
/// writing nothing. A pack whose configuration names an agent its install
/// creates plans one [`WireSkip`] per slot, agent and plugin instead.
pub async fn preview_pack_wiring(
    access: &ConfigAccess,
    owner: &str,
    pack: &ResolvedPack,
    agents: &[String],
    options: WireBatchOptions,
) -> Result<Vec<Value>> {
    let manifest = pack.manifest();
    let subjects = WireSubject::of_sent_pack(pack)?;
    if agents.is_empty() || subjects.is_empty() {
        return Ok(Vec::new());
    }
    let read_asset = |path: &str| Ok(pack.asset(path)?.to_vec());
    let pending =
        crate::pack::agent_bindings::pending_agent_slots(access, manifest, owner, &read_asset)
            .await?;
    if !pending.is_empty() {
        let mut skipped = Vec::new();
        for slot in &pending {
            for agent in agents {
                for subject in &subjects {
                    skipped.push(serde_json::to_value(WireSkip {
                        skipped: true,
                        preview: true,
                        agent: agent.clone(),
                        plugin: subject.coordinate(),
                        slot: slot.clone(),
                        reason: WireSkipReason::AgentIdentityPending,
                        next: format!(
                            "install the pack, which creates the agent of slot {slot}, then wire {} into {agent}",
                            subject.coordinate()
                        ),
                    })?);
                }
            }
        }
        return Ok(skipped);
    }
    let config = crate::pack::load_pack_install_config(
        access,
        manifest,
        &crate::pack::PackInstallOptions {
            node_did: owner.to_owned(),
        },
        &read_asset,
        &|name| (name == "GENTS_PACK_NODE_DID").then(|| owner.to_owned()),
        InstalledVia::Operator,
    )
    .await?
    .config;
    let skills: Vec<String> = config
        .skills
        .iter()
        .map(|skill| skill.skill_id.clone())
        .collect();
    let mut planned = Vec::with_capacity(agents.len() * subjects.len());
    for agent in agents {
        for subject in &subjects {
            let coordinate = subject.coordinate();
            let record = wire_plan_record(&coordinate, &[], WireRecord::SentPack(subject.clone()))
                .with_context(|| format!("{coordinate} is not a plugin this pack ships"))?;
            let reviewed = config
                .tools
                .iter()
                .filter_map(|tools| tools.integrations.as_ref()?.plugins.as_ref())
                .flatten()
                .find(|entry| entry.plugin == coordinate && entry.job.is_some())
                .and_then(|entry| entry.job.clone());
            let mut plan =
                PluginWiring::read_with(access, owner, agent, record.clone(), &[], &Archive)
                    .await?;
            plan.skill_ids = skills.clone();
            plan.reviewed = ReviewedJob::Archive(reviewed);
            plan.tool_calls_consent = options.tool_calls_consent;
            plan.next_step = explain_tools_command(agent);
            let request = WireRequest {
                plugin: coordinate,
                agent: agent.clone(),
                targets_from: Vec::new(),
                remove: false,
                preview: true,
                tool_calls_consent: options.tool_calls_consent,
            };
            planned.push((request, record, plan));
        }
    }
    let plans: Vec<&PluginWiring> = planned.iter().map(|(_, _, plan)| plan).collect();
    let reports = access
        .transact_readonly("pack.wire.preview", |txn| {
            let (plans, config) = (&plans, &config);
            Box::pin(async move {
                apply_batch(
                    txn,
                    plans,
                    BatchWrite::Preview {
                        installing: Some(config),
                    },
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await?;
    planned
        .iter()
        .zip(&reports)
        .map(|((request, record, _), report)| wire_outcome(request, record, report))
        .collect()
}
