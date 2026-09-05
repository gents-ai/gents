//! Distribution/catalog boundary. Execution and writes remain owned by the
//! graph installer and desired-state installer, not by package resolution.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

include!(concat!(env!("OUT_DIR"), "/bundled_graph_packages.rs"));

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackKind {
    Graph,
    Documents,
    Assets,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackMetadata {
    pub kind: PackKind,
    pub authors: Vec<String>,
    pub tags: Vec<String>,
    pub assets: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackManifest {
    pub manifest_version: u32,
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(flatten)]
    pub metadata: PackMetadata,
    // Graph-specific fields are validated by the existing graph loader.
    #[serde(flatten)]
    pub graph: BTreeMap<String, serde_json::Value>,
}

pub struct ResolvedPack {
    pub manifest: PackManifest,
    pub digest: String,
}

impl ResolvedPack {
    pub fn asset(&self, path: &str) -> Result<&'static [u8]> {
        anyhow::ensure!(
            path == "manifest.json" || self.manifest.metadata.assets.iter().any(|p| p == path),
            "undeclared pack asset: {path}"
        );
        bundled_graph_package_asset(&self.manifest.name, path).context("missing bundled pack asset")
    }
}

pub fn resolve_pack(name: &str) -> Result<ResolvedPack> {
    anyhow::ensure!(
        BUNDLED_PACK_NAMES.contains(&name),
        "unknown pack {name:?}; use gents pack list"
    );
    let bytes = bundled_graph_package_asset(name, "manifest.json").context("missing manifest")?;
    let manifest: PackManifest = serde_json::from_slice(bytes)?;
    let graph_fields = [
        "compiler_version",
        "roles",
        "schemas",
        "intent",
        "capabilities",
        "external_dependencies",
    ];
    anyhow::ensure!(
        manifest
            .graph
            .keys()
            .all(|key| matches!(manifest.metadata.kind, PackKind::Graph)
                && graph_fields.contains(&key.as_str())),
        "unexpected manifest fields"
    );
    anyhow::ensure!(
        manifest.manifest_version == 1 && manifest.name == name,
        "invalid pack identity/version"
    );
    anyhow::ensure!(
        !manifest.description.trim().is_empty() && !manifest.metadata.authors.is_empty(),
        "pack needs description and authors"
    );
    anyhow::ensure!(
        name.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_'),
        "pack name must be snake_case"
    );
    let mut unique = BTreeSet::new();
    let mut hash = Sha256::new();
    hash.update(bytes);
    for path in &manifest.metadata.assets {
        anyhow::ensure!(
            std::path::Path::new(path)
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
                && !path.split('/').any(|part| part.starts_with('.')
                    || matches!(part, "runs" | "target" | "node_modules" | "__pycache__")),
            "unsafe/private pack asset: {path}"
        );
        anyhow::ensure!(unique.insert(path), "duplicate asset {path}");
        let data = bundled_graph_package_asset(name, path)
            .with_context(|| format!("missing {name}/{path}"))?;
        hash.update((path.len() as u64).to_le_bytes());
        hash.update(path.as_bytes());
        hash.update((data.len() as u64).to_le_bytes());
        hash.update(data);
    }
    anyhow::ensure!(
        unique.contains(&"README.md".to_owned()),
        "pack must declare README.md"
    );
    Ok(ResolvedPack {
        manifest,
        digest: format!("{:x}", hash.finalize()),
    })
}

pub fn pack_catalog() -> Result<Vec<PackManifest>> {
    BUNDLED_PACK_NAMES
        .iter()
        .map(|name| Ok(resolve_pack(name)?.manifest))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_packs_resolve_with_declared_assets_and_dependencies() {
        let catalog = pack_catalog().unwrap();
        assert!(catalog.len() >= 11);
        for pack in catalog {
            for dependency in pack.metadata.dependencies {
                resolve_pack(&dependency).unwrap();
            }
        }
        assert!(resolve_pack("code-review").is_err());
        assert!(resolve_pack("../code_review").is_err());
    }
}
