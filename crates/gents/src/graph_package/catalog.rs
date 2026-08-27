use std::collections::BTreeSet;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::document_config::SurfaceToolDecl;
use crate::graph_pipeline::{
    EntryBinding, GraphIntent, PortSpec, ResultContract, WorkspaceAuthorityCeiling,
    COMPILER_VERSION,
};

include!(concat!(env!("OUT_DIR"), "/bundled_graph_packages.rs"));

#[derive(Deserialize)]
struct BundledToolSurface {
    entries: Vec<SurfaceToolDecl>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageRoleDeclaration {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphPackageManifest {
    pub manifest_version: u32,
    pub name: String,
    pub version: String,
    pub description: String,
    pub compiler_version: String,
    pub roles: Vec<PackageRoleDeclaration>,
    pub schemas: Vec<String>,
    pub intent: String,
    pub capabilities: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageCapabilityTemplate {
    pub capability_id: String,
    pub revision: String,
    pub role: String,
    pub behavior_asset: String,
    pub system_prompt_asset: String,
    pub task_asset: String,
    pub task_prompt_asset: String,
    pub tool_selection_asset: String,
    #[serde(default)]
    pub tool_surface_assets: Vec<String>,
    pub input_ports: Vec<PortSpec>,
    pub output_ports: Vec<PortSpec>,
    pub workspace_authority: WorkspaceAuthorityCeiling,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GraphPackageCatalogEntry {
    pub name: String,
    pub version: String,
    pub description: String,
    pub package_digest: String,
    pub compiler_version: String,
    pub roles: Vec<PackageRoleDeclaration>,
    pub entries: Vec<EntryBinding>,
    pub results: Vec<ResultContract>,
    pub capabilities: Vec<PackageCapabilityTemplate>,
}

#[derive(Clone, Debug)]
pub struct BundledGraphPackage {
    pub manifest: GraphPackageManifest,
    pub intent: GraphIntent,
    pub capabilities: Vec<PackageCapabilityTemplate>,
    pub package_digest: String,
    asset_paths: Vec<String>,
}

impl BundledGraphPackage {
    pub fn asset(&self, path: &str) -> Result<&'static [u8]> {
        if !self.asset_paths.iter().any(|candidate| candidate == path) {
            anyhow::bail!(
                "asset {path:?} is not declared by package {}",
                self.manifest.name
            );
        }
        bundled_graph_package_asset(&self.manifest.name, path)
            .with_context(|| format!("bundled asset {path:?} is missing"))
    }

    pub fn asset_text(&self, path: &str) -> Result<&'static str> {
        std::str::from_utf8(self.asset(path)?)
            .with_context(|| format!("bundled asset {path:?} is not UTF-8"))
    }

    pub fn catalog_entry(&self) -> GraphPackageCatalogEntry {
        GraphPackageCatalogEntry {
            name: self.manifest.name.clone(),
            version: self.manifest.version.clone(),
            description: self.manifest.description.clone(),
            package_digest: self.package_digest.clone(),
            compiler_version: self.manifest.compiler_version.clone(),
            roles: self.manifest.roles.clone(),
            entries: self.intent.entries.clone(),
            results: self.intent.results.clone(),
            capabilities: self.capabilities.clone(),
        }
    }
}

fn digest_assets(package_name: &str, paths: &[String]) -> Result<String> {
    let mut hasher = Sha256::new();
    for path in paths {
        let bytes = bundled_graph_package_asset(package_name, path)
            .with_context(|| format!("bundled package references missing asset {path:?}"))?;
        hasher.update((path.len() as u64).to_be_bytes());
        hasher.update(path.as_bytes());
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn validate_tool_surface_asset(package_name: &str, path: &str) -> Result<()> {
    let bytes = bundled_graph_package_asset(package_name, path)
        .with_context(|| format!("bundled package references missing asset {path:?}"))?;
    let surface: BundledToolSurface = serde_json::from_slice(bytes)
        .with_context(|| format!("bundled tool surface asset {path:?} is malformed"))?;
    for entry in &surface.entries {
        entry
            .validate()
            .with_context(|| format!("bundled tool surface asset {path:?} is invalid"))?;
    }
    Ok(())
}

fn load_package(package_name: &str) -> Result<BundledGraphPackage> {
    let manifest_bytes = bundled_graph_package_asset(package_name, "manifest.json")
        .with_context(|| format!("bundled package {package_name:?} has no manifest"))?;
    let manifest: GraphPackageManifest = serde_json::from_slice(manifest_bytes)?;
    if manifest.name != package_name {
        anyhow::bail!(
            "bundled package directory {package_name:?} disagrees with manifest name {:?}",
            manifest.name
        );
    }
    if manifest.manifest_version != 1 {
        anyhow::bail!("unsupported bundled package manifest version");
    }
    if manifest.compiler_version != COMPILER_VERSION {
        anyhow::bail!(
            "package compiler {} does not match runtime {}",
            manifest.compiler_version,
            COMPILER_VERSION
        );
    }
    let intent: GraphIntent = serde_json::from_slice(
        bundled_graph_package_asset(package_name, &manifest.intent)
            .with_context(|| format!("bundled package intent {:?} is missing", manifest.intent))?,
    )?;
    let capabilities: Vec<PackageCapabilityTemplate> = serde_json::from_slice(
        bundled_graph_package_asset(package_name, &manifest.capabilities).with_context(|| {
            format!(
                "bundled package capabilities {:?} are missing",
                manifest.capabilities
            )
        })?,
    )?;
    let roles = manifest
        .roles
        .iter()
        .map(|role| role.name.as_str())
        .collect::<BTreeSet<_>>();
    if roles.len() != manifest.roles.len()
        || capabilities
            .iter()
            .any(|capability| !roles.contains(capability.role.as_str()))
    {
        anyhow::bail!("package capabilities reference missing or duplicate logical roles");
    }
    let mut assets = vec![
        "manifest.json".to_owned(),
        manifest.intent.clone(),
        manifest.capabilities.clone(),
    ];
    assets.extend(manifest.schemas.iter().cloned());
    for capability in &capabilities {
        assets.extend([
            capability.behavior_asset.clone(),
            capability.system_prompt_asset.clone(),
            capability.task_asset.clone(),
            capability.task_prompt_asset.clone(),
            capability.tool_selection_asset.clone(),
        ]);
        for path in &capability.tool_surface_assets {
            validate_tool_surface_asset(package_name, path)?;
        }
        assets.extend(capability.tool_surface_assets.iter().cloned());
    }
    assets.sort();
    assets.dedup();
    let package_digest = digest_assets(package_name, &assets)?;
    Ok(BundledGraphPackage {
        manifest,
        intent,
        capabilities,
        package_digest,
        asset_paths: assets,
    })
}

pub fn load_bundled_graph_package(name: &str) -> Result<BundledGraphPackage> {
    if !BUNDLED_GRAPH_PACKAGE_NAMES.contains(&name) {
        anyhow::bail!("unknown bundled graph package {name:?}");
    }
    load_package(name)
}

pub fn graph_package_catalog() -> Result<Vec<GraphPackageCatalogEntry>> {
    BUNDLED_GRAPH_PACKAGE_NAMES
        .iter()
        .map(|name| Ok(load_package(name)?.catalog_entry()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_config::{SurfaceToolDecl, WriteToolFieldFill};
    use crate::graph_pipeline::{compile_graph, CompilerPolicy, StageCapability};

    #[test]
    fn bundled_catalog_is_read_only_complete_and_compiler_valid() {
        let package = load_bundled_graph_package("code-review").unwrap();
        assert_eq!(graph_package_catalog().unwrap().len(), 1);
        assert!(package.package_digest.starts_with("sha256:"));
        for path in &package.asset_paths {
            assert!(!package.asset(path).unwrap().is_empty(), "{path}");
        }
        let capabilities = package
            .capabilities
            .iter()
            .map(|template| StageCapability {
                capability_id: template.capability_id.clone(),
                revision: template.revision.clone(),
                task_id: format!("fixture-task-{}", template.capability_id),
                input_ports: template.input_ports.clone(),
                output_ports: template.output_ports.clone(),
                allowed_callers: vec!["did:key:fixture".to_owned()],
            })
            .collect::<Vec<_>>();
        let plan = compile_graph(
            &package.intent,
            &capabilities,
            "did:key:fixture",
            &CompilerPolicy::default(),
        )
        .unwrap();
        assert_eq!(plan.nodes.len(), 4);
        assert_eq!(plan.results.len(), 2);
        assert_eq!(plan.entries[0].name, "review");
    }

    #[test]
    fn code_review_scan_writes_use_the_trigger_area_id() {
        let package = load_bundled_graph_package("code-review").unwrap();
        let surface: BundledToolSurface = serde_json::from_str(
            package
                .asset_text("datastore-tool-surfaces/review-scan-writes/object.json")
                .unwrap(),
        )
        .unwrap();
        for tool_name in ["write_candidate_finding", "write_scan_result"] {
            let entry = surface
                .entries
                .iter()
                .find(|entry| entry.tool_name() == tool_name)
                .unwrap();
            let SurfaceToolDecl::Create(entry) = entry else {
                panic!("{tool_name} must be a create tool");
            };
            let area_id = entry
                .fields
                .iter()
                .find(|field| field.name == "area_id")
                .unwrap();
            assert!(!area_id.required, "{tool_name}");
            assert_eq!(
                area_id.fill,
                Some(WriteToolFieldFill::SourceField("area_id".to_owned())),
                "{tool_name}"
            );
        }
    }
}
