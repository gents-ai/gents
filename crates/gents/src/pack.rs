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
    /// Nothing but capabilities: compiled modules a graph stage or a model
    /// can call. A tools pack installs no documents of its own, and its
    /// tools are callable by any pack in the same home, which is what lets
    /// one pack build on another's capabilities instead of vendoring them.
    Tools,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackMetadata {
    pub kind: PackKind,
    pub authors: Vec<String>,
    pub tags: Vec<String>,
    pub assets: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// The capabilities this pack builds and ships.
    ///
    /// A tool is compiled WASM that runs sandboxed, and the same admitted
    /// tool is callable two ways: as a deterministic stage inside this
    /// pack's graph, and as an ordinary tool a model can pick. Naming it
    /// here is what makes it either.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<PackTool>,
}

/// One capability a pack ships.
///
/// The module is compiled from the source the pack carries, addressed by
/// its path inside the pack, and admitted by digest at install time. The
/// schema is what a model is shown; the manifold is what the pack asks the
/// sandbox to allow, which an operator's ceiling can narrow and never
/// widen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackTool {
    /// What a graph stage or a model calls it by, unique within the pack.
    pub name: String,
    /// What it does, in the words a model is given.
    pub description: String,
    /// The compiled module inside the pack, relative to the pack root
    /// (`tools/<name>.wasm` by convention).
    pub module: String,
    /// Where the module is built from, relative to the pack root. Present
    /// for a pack that carries its sources, absent for one that ships only
    /// the compiled module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// JSON Schema for the arguments, and what the model is shown.
    pub input_schema: serde_json::Value,
    /// What the tool asks the sandbox for. Absent means it asks for
    /// nothing, which is the right default for a pure transform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifold: Option<serde_json::Value>,
}

impl PackTool {
    /// The rules a tool has to satisfy before anything will build or admit
    /// it. Checked when a pack is resolved, so a malformed tool is a
    /// refusal at the pack rather than a surprise at the call.
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            is_valid_pack_name(&self.name),
            "tool name must be snake_case: {:?}",
            self.name
        );
        anyhow::ensure!(
            !self.description.trim().is_empty(),
            "tool {:?} needs a description; it is what the model is shown",
            self.name
        );
        anyhow::ensure!(
            self.module.ends_with(".wasm"),
            "tool {:?} module must be a .wasm path, got {:?}",
            self.name,
            self.module
        );
        anyhow::ensure!(
            is_distributable_asset_path(&self.module),
            "unsafe tool module path: {:?}",
            self.module
        );
        if let Some(source) = &self.source {
            anyhow::ensure!(
                is_distributable_asset_path(source),
                "unsafe tool source path: {source:?}"
            );
        }
        anyhow::ensure!(
            self.input_schema.is_object(),
            "tool {:?} needs an object input_schema",
            self.name
        );
        if let Some(manifold) = &self.manifold {
            // A tool is called, never a server. Asking to bind a port is
            // refused at the pack rather than silently dropped later, so a
            // pack author learns it here instead of wondering why it never
            // worked.
            let listen = manifold.get("listen");
            anyhow::ensure!(
                listen.is_none_or(|value| value == "None" || value == &serde_json::json!("None")),
                "tool {:?} asks to listen on a port; a pack's tools are called, not served",
                self.name
            );
        }
        Ok(())
    }
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

/// Whether a path may appear in a pack archive.
///
/// The archive reader and the pack resolver have to agree exactly about
/// this, so they ask the same function rather than each carrying a copy of
/// the rule.
pub fn is_distributable_asset_path(path: &str) -> bool {
    asset_path::is_distributable_asset(path) && asset_path::has_canonical_asset_spelling(path)
}

/// Everything a pack manifest has to be true of, whichever way the pack
/// arrived.
///
/// A pack compiled into this binary and a pack downloaded from a registry
/// are held to one set of rules, checked here, because a pack that only
/// passes when it comes from a trusted place is not checked at all.
pub fn validate_manifest(name: &str, manifest: &PackManifest) -> Result<()> {
    anyhow::ensure!(
        manifest.manifest_version == 1 && manifest.name == name,
        "invalid pack identity/version"
    );
    if manifest.metadata.kind == PackKind::Graph {
        crate::graph_package::graph_manifest_from_pack(manifest)?;
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

    let mut tool_names = BTreeSet::new();
    for tool in &manifest.metadata.tools {
        tool.validate()?;
        anyhow::ensure!(
            tool_names.insert(tool.name.as_str()),
            "pack declares the tool {:?} twice",
            tool.name
        );
        // A tool's module travels as a declared asset like anything else,
        // so the pack's own digest covers it and nothing can be swapped
        // underneath the name it was admitted under.
        anyhow::ensure!(
            unique.contains(&tool.module),
            "pack declares the tool {:?} but not its module {:?} as an asset",
            tool.name,
            tool.module
        );
    }
    anyhow::ensure!(
        manifest.metadata.kind != PackKind::Tools || !manifest.metadata.tools.is_empty(),
        "a tools pack must declare at least one tool"
    );
    Ok(())
}

/// The paths that make up a pack's identity: its manifest and every asset
/// it declares, sorted, each once.
pub fn declared_paths(manifest: &PackManifest) -> Vec<String> {
    let mut paths = manifest.metadata.assets.clone();
    paths.push("manifest.json".to_owned());
    paths.sort();
    paths.dedup();
    paths
}

/// A pack's digest, over its declared contents rather than over whatever
/// container carried them.
///
/// This is what makes a pack the same pack wherever it came from: the same
/// documents give the same digest whether they were compiled into a binary
/// or downloaded and unpacked, so a container that recompresses differently
/// does not change the pack's identity, and neither does the route it took.
pub fn digest_declared_assets<'a>(
    manifest: &PackManifest,
    asset: impl Fn(&str) -> Result<&'a [u8]>,
) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for path in declared_paths(manifest) {
        let bytes =
            asset(&path).with_context(|| format!("pack references missing asset {path:?}"))?;
        hasher.update((path.len() as u64).to_be_bytes());
        hasher.update(path.as_bytes());
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

pub fn resolve_pack(name: &str) -> Result<ResolvedPack> {
    anyhow::ensure!(
        BUNDLED_PACK_NAMES.contains(&name),
        "unknown pack {name:?}; use gents pack list"
    );
    let bytes = bundled_pack_asset(name, "manifest.json").context("missing manifest")?;
    let manifest: PackManifest = serde_json::from_slice(bytes)?;
    validate_manifest(name, &manifest)?;
    let digest = crate::graph_package::digest_assets(name, &declared_paths(&manifest))?;
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
