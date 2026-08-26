use std::collections::BTreeSet;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::document_config::SurfaceToolDecl;
use crate::graph_pipeline::{
    EntryBinding, GraphIntent, GraphLimits, PortSpec, ResultContract, StageAuthorityCeiling,
    COMPILER_VERSION,
};

include!(concat!(env!("OUT_DIR"), "/bundled_graph_packages.rs"));

#[derive(Deserialize)]
struct BundledToolSurface {
    entries: Vec<SurfaceToolDecl>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageVariablePhase {
    Install,
    Run,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageVariableKind {
    String,
    Integer,
    Boolean,
    DocumentRef,
    LocalGitRepository,
    GitRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageVariableDeclaration {
    pub name: String,
    pub phase: PackageVariablePhase,
    pub kind: PackageVariableKind,
    pub required: bool,
    pub sensitive: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageRoleDeclaration {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageDependency {
    pub name: String,
    pub version_requirement: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageUpgradePolicy {
    SuccessorRevision,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphPackageManifest {
    pub manifest_version: u32,
    pub name: String,
    pub version: String,
    pub description: String,
    pub compiler_version: String,
    pub upgrade_policy: PackageUpgradePolicy,
    #[serde(default)]
    pub dependencies: Vec<PackageDependency>,
    pub roles: Vec<PackageRoleDeclaration>,
    pub variables: Vec<PackageVariableDeclaration>,
    pub schemas: Vec<String>,
    pub intent: String,
    pub capabilities: String,
    #[serde(default)]
    pub requested_effects: Vec<String>,
    pub graph_ceiling: GraphLimits,
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
    pub max_invocations: u32,
    pub authority: StageAuthorityCeiling,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GraphPackageCatalogEntry {
    pub name: String,
    pub version: String,
    pub description: String,
    pub package_digest: String,
    pub catalog_digest: String,
    pub compiler_version: String,
    pub upgrade_policy: PackageUpgradePolicy,
    pub dependencies: Vec<PackageDependency>,
    pub roles: Vec<PackageRoleDeclaration>,
    pub variables: Vec<PackageVariableDeclaration>,
    pub requested_effects: Vec<String>,
    pub graph_ceiling: GraphLimits,
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
    pub catalog_digest: String,
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
            catalog_digest: self.catalog_digest.clone(),
            compiler_version: self.manifest.compiler_version.clone(),
            upgrade_policy: self.manifest.upgrade_policy.clone(),
            dependencies: self.manifest.dependencies.clone(),
            roles: self.manifest.roles.clone(),
            variables: self.manifest.variables.clone(),
            requested_effects: self.manifest.requested_effects.clone(),
            graph_ceiling: self.manifest.graph_ceiling.clone(),
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
    if intent.limits != manifest.graph_ceiling {
        anyhow::bail!("package intent limits do not match the declared graph ceiling");
    }
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
    if capabilities
        .iter()
        .any(|capability| capability.authority.max_invocations != capability.max_invocations)
    {
        anyhow::bail!("package capability invocation limit disagrees with its authority ceiling");
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
    let catalog_digest = format!(
        "sha256:{:x}",
        Sha256::digest(
            format!(
                "{}\0{}\0{}",
                manifest.name, manifest.version, package_digest
            )
            .as_bytes()
        )
    );
    Ok(BundledGraphPackage {
        manifest,
        intent,
        capabilities,
        package_digest,
        catalog_digest,
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
        assert_eq!(plan.results.len(), 7);
        assert_eq!(plan.entries[0].name, "review");
    }
}
