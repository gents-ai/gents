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

    /// A record carrying only what a plan reads.
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
    /// Inside an install: the pack the client itself just sent.
    SentPack(WireSubject),
    /// The plugin store of the client's own home, which the runtime serves.
    OwnHome(InstalledPlugin),
    /// A standalone wire: the record the runtime serves.
    Served(InstalledPlugin),
}

/// The record a client plans a wire from (`Configuration.wirePlanSubject`):
/// the pack it sent or its own home's record, or a served record only when
/// it is exactly what a pack in `approved` yields; `None` plans and signs
/// nothing. A served record contributes only the approved subject, so no
/// other field the runtime sent reaches the plan.
pub fn wire_plan_record(approved: &[WireSubject], record: WireRecord) -> Option<InstalledPlugin> {
    match record {
        WireRecord::SentPack(subject) => Some(subject.installed()),
        WireRecord::OwnHome(kept) => Some(kept),
        WireRecord::Served(served) => WireSubject::of_record(&served)
            .filter(|subject| approved.contains(subject))
            .map(|subject| subject.installed()),
    }
}

#[cfg(test)]
mod conformance;
