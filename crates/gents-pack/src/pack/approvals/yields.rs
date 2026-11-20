use std::collections::BTreeMap;

use gents_protocol::job::PluginJobSettings;
use gents_protocol::operations::pack::PackVariables;
use gents_store::collection::Collection;
use serde::{Deserialize, Serialize};

use crate::pack::resolved_pack::ResolvedPack;
use crate::pack::wire_subject::{fill_id, IdPart};

/// What `load_pack_authoring_config` writes for an agent slot's DID.
const SLOT_STAND_IN: &str = "did:pack:slot:";

/// The ids of the documents a pack's configuration yields that a wire reads
/// (`Configuration.PackApproval.yields`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackYield {
    pub tools: Vec<Vec<IdPart>>,
    pub skills: Vec<Vec<IdPart>>,
    pub agent_targets: Vec<Vec<IdPart>>,
    /// By plugin coordinate, the first job block in the pack's order.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub jobs: BTreeMap<String, YieldedJob>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct YieldedJob {
    pub agent_targets: Vec<Vec<IdPart>>,
    /// The rest of the job, its `agent_targets` empty.
    pub settings: PluginJobSettings,
}

impl PackYield {
    pub fn ids(&self, collection: Collection) -> &[Vec<IdPart>] {
        match collection {
            Collection::Tools => &self.tools,
            Collection::Skill => &self.skills,
            Collection::AgentTarget => &self.agent_targets,
            _ => &[],
        }
    }

    /// The job `plugin`'s pack yields for it wired into `agent`
    /// (`Configuration.wireReviewedJob`'s `approved`): `Some(None)` when the
    /// pack carries none.
    pub fn job(&self, plugin: &str, agent: &str) -> Option<Option<PluginJobSettings>> {
        let Some(job) = self.jobs.get(plugin) else {
            return Some(None);
        };
        let agent_targets = job
            .agent_targets
            .iter()
            .map(|parts| fill_id(Some(agent), parts))
            .collect::<Option<Vec<_>>>()?;
        Some(Some(PluginJobSettings {
            agent_targets,
            ..job.settings.clone()
        }))
    }
}

/// The ids and job blocks `pack`'s configuration yields on `node_did` with
/// `variables` and nothing else, a slot the variables bind to an agent as
/// that agent's DID and any other slot as a slot. `None` when the
/// configuration does not load from them alone, say because it reads a
/// variable only the runtime's environment supplies, or when a slot cannot
/// be told apart from the text around it.
pub(super) fn derive(
    pack: &ResolvedPack,
    coordinate: &str,
    node_did: &str,
    variables: &PackVariables,
) -> Option<PackYield> {
    let manifest = pack.manifest();
    let Some(config) = manifest.config.as_deref() else {
        return Some(PackYield::default());
    };
    let lookup = |name: &str| {
        variables
            .scoped
            .get(coordinate)
            .and_then(|scoped| scoped.get(name))
            .or_else(|| variables.shared.get(name))
            .cloned()
    };
    let supplied = variables
        .shared
        .values()
        .chain(variables.scoped.values().flat_map(BTreeMap::values));
    if contains_stand_in(pack.asset(config).ok()?)
        || supplied
            .into_iter()
            .any(|value| value.contains(SLOT_STAND_IN))
    {
        return None;
    }
    let slots: Vec<(String, Option<String>)> = manifest
        .metadata
        .inference_slots
        .iter()
        .flat_map(|slot| slot.agents.iter())
        .map(|slot| {
            let bound = lookup(&crate::pack::agent_binding_variable(slot))
                .map(|agent| agent.trim().to_owned())
                .filter(|agent| !agent.is_empty());
            (slot.clone(), bound)
        })
        .collect();
    let authored = crate::pack::load_pack_authoring_config(
        manifest,
        &crate::pack::PackInstallOptions {
            node_did: node_did.to_owned(),
        },
        &|path| Ok(pack.asset(path)?.to_vec()),
        &lookup,
    )
    .ok()?;
    let split_all = |ids: Vec<String>| -> Option<Vec<Vec<IdPart>>> {
        ids.iter().map(|id| split(id, &slots)).collect()
    };
    let documents = authored.documents;
    let mut jobs = BTreeMap::new();
    for entry in documents
        .tools
        .iter()
        .filter_map(|tools| tools.integrations.as_ref()?.plugins.as_ref())
        .flatten()
    {
        let Some(job) = &entry.job else { continue };
        if jobs.contains_key(&entry.plugin) {
            continue;
        }
        let mut settings = job.clone();
        let agent_targets = split_all(std::mem::take(&mut settings.agent_targets))?;
        jobs.insert(
            entry.plugin.clone(),
            YieldedJob {
                agent_targets,
                settings,
            },
        );
    }
    Some(PackYield {
        tools: split_all(
            documents
                .tools
                .into_iter()
                .map(|tools| tools.tools_id)
                .collect(),
        )?,
        skills: split_all(
            documents
                .skills
                .into_iter()
                .map(|skill| skill.skill_id)
                .collect(),
        )?,
        agent_targets: split_all(
            documents
                .agent_targets
                .into_iter()
                .map(|target| target.target_id)
                .collect(),
        )?,
        jobs,
    })
}

fn contains_stand_in(config: &[u8]) -> bool {
    config
        .windows(SLOT_STAND_IN.len())
        .any(|window| window == SLOT_STAND_IN.as_bytes())
}

/// `id` as text and the `slots` its stand-ins name, a bound slot as its
/// agent's DID; `None` when a stand-in names no slot or more than one.
fn split(id: &str, slots: &[(String, Option<String>)]) -> Option<Vec<IdPart>> {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut rest = id;
    while let Some(start) = rest.find(SLOT_STAND_IN) {
        text.push_str(&rest[..start]);
        let after = &rest[start + SLOT_STAND_IN.len()..];
        let named: Vec<_> = slots
            .iter()
            .filter(|(slot, _)| after.starts_with(slot.as_str()))
            .collect();
        let [(slot, bound)] = named.as_slice() else {
            return None;
        };
        match bound {
            Some(agent) => text.push_str(agent),
            None => {
                if !text.is_empty() {
                    parts.push(IdPart::Text(std::mem::take(&mut text)));
                }
                parts.push(IdPart::Slot(slot.clone()));
            }
        }
        rest = &after[slot.len()..];
    }
    text.push_str(rest);
    if !text.is_empty() || parts.is_empty() {
        parts.push(IdPart::Text(text));
    }
    Some(parts)
}

#[cfg(test)]
mod tests;
