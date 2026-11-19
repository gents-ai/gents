use std::collections::BTreeMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::pack::resolved_pack::ResolvedPack;
use crate::pack::PackPlugin;
use crate::plugin::store::{content_digest_hex, InstalledPlugin};

/// What a wiring plan reads of an installed plugin
/// (`Configuration.WireSubject`): the pack that owns it, by digest and
/// coordinate, and the rest of the record the plan writes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireSubject {
    pub owner_pack_digest: String,
    pub owner_pack_coordinate: String,
    pub namespace: String,
    pub name: String,
    pub version: String,
    pub digest: String,
    pub declaration: PackPlugin,
}

impl WireSubject {
    /// What `record` says a plan reads; `None` for a record no pack owns.
    pub fn of_record(record: &InstalledPlugin) -> Option<Self> {
        Some(Self {
            owner_pack_digest: record.owner_pack_digest.clone()?,
            owner_pack_coordinate: record.owner_pack_coordinate.clone()?,
            namespace: record.namespace.clone(),
            name: record.name.clone(),
            version: record.version.clone(),
            digest: record.digest.clone(),
            declaration: record.declaration.clone(),
        })
    }

    /// What installing `pack` records of each plugin it ships, as far as a
    /// plan reads it: the same fields the install writes, from the archive
    /// the client holds.
    pub fn of_sent_pack(pack: &ResolvedPack) -> Result<Vec<Self>> {
        let manifest = pack.manifest();
        let coordinate = format!("{}/{}", manifest.metadata.namespace, manifest.name);
        manifest
            .metadata
            .plugins
            .iter()
            .map(|plugin| {
                Ok(Self {
                    owner_pack_digest: pack.digest().to_owned(),
                    owner_pack_coordinate: coordinate.clone(),
                    namespace: manifest.metadata.namespace.clone(),
                    name: plugin.name.clone(),
                    version: manifest.version.clone(),
                    digest: format!(
                        "sha256:{}",
                        content_digest_hex(pack.asset(&plugin.artifact)?)
                    ),
                    declaration: plugin.clone(),
                })
            })
            .collect()
    }

    pub fn coordinate(&self) -> String {
        format!("{}/{}", self.namespace, self.name)
    }

    pub fn installed(&self) -> InstalledPlugin {
        InstalledPlugin {
            namespace: self.namespace.clone(),
            name: self.name.clone(),
            version: self.version.clone(),
            digest: self.digest.clone(),
            language: self.declaration.language.clone(),
            declaration: self.declaration.clone(),
            granted: None,
            instructions: None,
            owner_pack_coordinate: Some(self.owner_pack_coordinate.clone()),
            owner_pack_digest: Some(self.owner_pack_digest.clone()),
            model_binding: None,
            data_unique_keys: BTreeMap::new(),
            data_collection_fields: BTreeMap::new(),
            data_collection_schemas: BTreeMap::new(),
            gents_version: None,
        }
    }
}

/// Where a client's wiring plan takes the plugin's record from
/// (`Configuration.WireRecord`).
#[derive(Debug, Clone)]
pub enum WireRecord {
    SentPack(WireSubject),
    OwnHome(InstalledPlugin),
    Served(InstalledPlugin),
}

/// The record a client plans a wire from (`Configuration.wirePlanSubject`):
/// the pack it sent or its own home's record, or a served record only when
/// it is exactly what a pack in `approved` yields, and in every case only a
/// record of the `requested` plugin; `None` plans and signs nothing. A
/// served record contributes only the approved subject, so no other field
/// the runtime sent reaches the plan.
pub fn wire_plan_record(
    requested: &str,
    approved: &[WireSubject],
    record: WireRecord,
) -> Option<InstalledPlugin> {
    let named = |coordinate: String| coordinate == requested;
    match record {
        WireRecord::SentPack(subject) => named(subject.coordinate()).then(|| subject.installed()),
        WireRecord::OwnHome(kept) => {
            named(format!("{}/{}", kept.namespace, kept.name)).then_some(kept)
        }
        WireRecord::Served(served) => WireSubject::of_record(&served)
            .filter(|subject| named(subject.coordinate()) && approved.contains(subject))
            .map(|subject| subject.installed()),
    }
}

/// The document ids of a pack a client wires over a runtime that serves
/// another home (`Configuration.wireApprovedIds`): the `served` ids only when
/// each is one the approved pack yields; `None`, when the approval could not
/// derive its ids or the runtime reports another, wires none.
pub fn wire_approved_ids(approved: Option<&[String]>, served: &[String]) -> Option<Vec<String>> {
    let approved = approved?;
    served
        .iter()
        .all(|target| approved.contains(target))
        .then(|| served.to_vec())
}

/// One part of a document id a pack's configuration yields
/// (`Configuration.IdPart`): fixed text, or an agent slot whose DID only the
/// install fixes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum IdPart {
    Text(String),
    Slot(String),
}

/// `parts` as an id (`Configuration.fillId`): each slot filled with the
/// wired `agent`; `None` when a slot has no agent to fill it.
pub fn fill_id(agent: Option<&str>, parts: &[IdPart]) -> Option<String> {
    parts
        .iter()
        .map(|part| match part {
            IdPart::Text(text) => Some(text.as_str()),
            IdPart::Slot(_) => agent,
        })
        .collect::<Option<Vec<_>>>()
        .map(|filled| filled.concat())
}

/// The ids `approved` yields with its slots filled
/// (`Configuration.wireFilledIds`).
pub fn wire_filled_ids(
    approved: Option<&[Vec<IdPart>]>,
    agent: Option<&str>,
) -> Option<Vec<String>> {
    approved.map(|templates| {
        templates
            .iter()
            .filter_map(|parts| fill_id(agent, parts))
            .collect()
    })
}

/// The job block a client signs for a plugin over a runtime that serves
/// another home (`Configuration.wireReviewedJob`): the job the node's Tools
/// carry only when it is the one the approved pack yields; `None`, when the
/// approval could not derive it or the node reports another, plans nothing.
pub fn wire_reviewed_job<J>(approved: Option<Option<&J>>, served: Option<&J>) -> Option<Option<J>>
where
    J: PartialEq + Clone,
{
    let approved = approved?;
    (served == approved).then(|| approved.cloned())
}

#[cfg(test)]
mod conformance;
