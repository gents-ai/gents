//! Distribution/catalog boundary. Execution and writes remain owned by the
//! graph installer and desired-state installer, not by package resolution.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub mod interpolate;
mod loader;
pub use loader::{decode_pack_config, load_pack_config};

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
    #[serde(
        default,
        deserialize_with = "crate::document_config::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tags: Vec<String>,
    pub assets: Vec<String>,
    #[serde(
        default,
        deserialize_with = "crate::document_config::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub dependencies: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackManifest {
    pub manifest_version: u32,
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(flatten)]
    pub metadata: PackMetadata,
    /// Declared asset decoded as PackConfig by the common loader. Required for
    /// document/graph packs; absent for asset-only packs. Sidecars are relative
    /// to this config asset. Graph topology/capabilities live in this same bundle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::document_config::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub schemas: Vec<String>,
    /// Required for graph compilation; absent for packs without graph topology.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiler_version: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::document_config::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub external_dependencies: Vec<PackageExternalDependency>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageExternalDependency {
    pub service_id: String,
    pub description: String,
    pub repository_url: String,
    pub install_command: String,
}

/// Installation scope shared by document and graph packs. Logical references
/// resolve through the same canonical configuration loader. Graph installation
/// adds topology/revision validation, not behavior/model selection overrides.
/// Before strict decoding, fill omitted root owners from this explicit scope;
/// reject mismatched explicit owners. Never rewrite target/caller/signer DIDs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackInstallOptions {
    pub agent_did: String,
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
    pub fn load_config(
        &self,
        options: &PackInstallOptions,
    ) -> Result<crate::document_config::PackConfig> {
        load_pack_config(
            &self.manifest,
            options,
            &|path| Ok(self.asset(path)?.to_vec()),
            &|name| std::env::var(name).ok(),
        )
    }

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
    validate_pack_manifest(&manifest)?;
    let mut paths = manifest.metadata.assets.clone();
    paths.push("manifest.json".to_owned());
    paths.sort();
    paths.dedup();
    let digest = crate::graph_package::digest_assets(name, &paths)?;
    Ok(ResolvedPack { manifest, digest })
}

/// Distribution validation shared by bundled and source-pack loaders.
pub fn validate_pack_manifest(manifest: &PackManifest) -> Result<()> {
    anyhow::ensure!(
        manifest.manifest_version == 1,
        "unsupported pack manifest version"
    );
    anyhow::ensure!(
        manifest.metadata.kind == PackKind::Documents || manifest.metadata.dependencies.is_empty(),
        "only document packs support package dependencies; nested graph/asset dependencies are unsupported"
    );
    anyhow::ensure!(
        !manifest.description.trim().is_empty() && !manifest.metadata.authors.is_empty(),
        "pack needs description and authors"
    );
    anyhow::ensure!(
        is_valid_pack_name(&manifest.name),
        "pack name must be snake_case"
    );
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
    match manifest.metadata.kind {
        PackKind::Documents | PackKind::Graph => {
            let config = manifest
                .config
                .as_deref()
                .context("document/graph pack requires a config asset")?;
            anyhow::ensure!(
                manifest.metadata.assets.iter().any(|asset| asset == config),
                "config asset must be declared"
            );
        }
        PackKind::Assets => {
            anyhow::ensure!(
                manifest.config.is_none(),
                "asset-only pack cannot declare configuration"
            );
            anyhow::ensure!(
                manifest.schemas.is_empty(),
                "asset-only pack cannot declare installed schemas"
            );
        }
    }
    let mut schemas = BTreeSet::new();
    for schema in &manifest.schemas {
        anyhow::ensure!(
            manifest.metadata.assets.contains(schema),
            "schema asset must be declared: {schema}"
        );
        anyhow::ensure!(schemas.insert(schema), "duplicate schema asset: {schema}");
    }
    if manifest.metadata.kind == PackKind::Graph {
        anyhow::ensure!(
            manifest.compiler_version.as_deref() == Some(crate::graph_pipeline::COMPILER_VERSION),
            "graph pack compiler version does not match runtime"
        );
    } else {
        anyhow::ensure!(
            manifest.compiler_version.is_none(),
            "non-graph pack cannot select a graph compiler"
        );
    }
    Ok(())
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
    fn typed_manifest_rejects_unknown_fields() {
        let mut value = serde_json::json!({"manifest_version":1,"name":"example","version":"1","description":"Example","kind":"documents","authors":["Example"],"assets":["README.md","config.json"],"config":"config.json"});
        assert!(serde_json::from_value::<PackManifest>(value.clone()).is_ok());
        value["unexpected_field"] = true.into();
        assert!(serde_json::from_value::<PackManifest>(value).is_err());
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
