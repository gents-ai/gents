//! Distribution/catalog boundary. Execution and writes remain owned by the
//! graph installer and desired-state installer, not by package resolution.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[path = "pack_asset_path.rs"]
mod asset_path;

include!(concat!(env!("OUT_DIR"), "/bundled_packs.rs"));

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

/// Return whether `name` is admissible at the pack catalog and source-pack
/// boundaries. Keep callers on this owner instead of growing parallel name
/// validators in adapters.
pub fn is_valid_pack_name(name: &str) -> bool {
    asset_path::is_snake_case_name(name)
}

impl ResolvedPack {
    pub fn asset(&self, path: &str) -> Result<&'static [u8]> {
        anyhow::ensure!(
            path == "manifest.json" || self.manifest.metadata.assets.iter().any(|p| p == path),
            "undeclared pack asset: {path}"
        );
        bundled_pack_asset(&self.manifest.name, path).context("missing bundled pack asset")
    }
}

pub fn resolve_pack(name: &str) -> Result<ResolvedPack> {
    anyhow::ensure!(
        BUNDLED_PACK_NAMES.contains(&name),
        "unknown pack {name:?}; use gents pack list"
    );
    let bytes = bundled_pack_asset(name, "manifest.json").context("missing manifest")?;
    let manifest: PackManifest = serde_json::from_slice(bytes)?;
    anyhow::ensure!(
        manifest.manifest_version == 1 && manifest.name == name,
        "invalid pack identity/version"
    );
    if manifest.metadata.kind == PackKind::Graph {
        crate::graph_package::graph_manifest_from_pack(&manifest)?;
    } else {
        anyhow::ensure!(manifest.graph.is_empty(), "unexpected manifest fields");
    }
    anyhow::ensure!(
        manifest.metadata.kind == PackKind::Documents || manifest.metadata.dependencies.is_empty(),
        "only document packs support package dependencies; nested graph/asset dependencies are unsupported"
    );
    anyhow::ensure!(
        !manifest.description.trim().is_empty() && !manifest.metadata.authors.is_empty(),
        "pack needs description and authors"
    );
    anyhow::ensure!(is_valid_pack_name(name), "pack name must be snake_case");
    let mut unique = BTreeSet::new();
    for path in &manifest.metadata.assets {
        anyhow::ensure!(
            asset_path::is_distributable_asset(path),
            "unsafe/private pack asset: {path}"
        );
        anyhow::ensure!(
            asset_path::has_canonical_asset_spelling(path),
            "non-canonical pack asset spelling: {path}"
        );
        anyhow::ensure!(unique.insert(path), "duplicate asset {path}");
    }
    anyhow::ensure!(
        unique.contains(&"README.md".to_owned()),
        "pack must declare README.md"
    );
    let mut paths = manifest.metadata.assets.clone();
    paths.push("manifest.json".to_owned());
    paths.sort();
    paths.dedup();
    let digest = crate::graph_package::digest_assets(name, &paths)?;
    Ok(ResolvedPack { manifest, digest })
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
    fn typed_graph_manifest_rejects_unknown_fields() {
        let pack = crate::graph_package::load_bundled_graph_package("code_review").unwrap();
        let mut value = serde_json::to_value(&pack.manifest).unwrap();
        value["unexpected_field"] = serde_json::json!(true);
        assert!(
            serde_json::from_value::<crate::graph_package::GraphPackageManifest>(value).is_err()
        );
        let mut distribution = resolve_pack("code_review").unwrap().manifest;
        distribution
            .graph
            .insert("unexpected_field".to_owned(), serde_json::json!(true));
        assert!(crate::graph_package::graph_manifest_from_pack(&distribution).is_err());
    }
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
