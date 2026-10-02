use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;

use crate::graph_pipeline::{EntryBinding, ResultContract};
use crate::pack::PackInstallOptions;
pub use crate::pack::PackageExternalDependency;

/// Graph packages use the common manifest and canonical configuration loader.
pub type GraphPackageManifest = crate::pack::PackManifest;
pub type PackageCapabilityTemplate = crate::graph_pipeline::StageCapability;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GraphPackageCatalogEntry {
    pub name: String,
    pub version: String,
    pub description: String,
    pub package_digest: String,
    pub compiler_version: String,
    pub external_dependencies: Vec<PackageExternalDependency>,
    pub entries: Vec<EntryBinding>,
    pub results: Vec<ResultContract>,
    pub capabilities: Vec<PackageCapabilityTemplate>,
}

#[derive(Clone, Debug)]
pub struct LoadedGraphPackage {
    pub manifest: GraphPackageManifest,
    pub config: crate::document_config::PackConfig,
    pub package_digest: String,
    assets: BTreeMap<String, Vec<u8>>,
}

impl LoadedGraphPackage {
    pub fn asset(&self, path: &str) -> Result<&[u8]> {
        self.assets.get(path).map(Vec::as_slice).with_context(|| {
            format!(
                "asset {path:?} is not declared by package {}",
                self.manifest.name
            )
        })
    }

    pub fn asset_text(&self, path: &str) -> Result<&str> {
        std::str::from_utf8(self.asset(path)?)
            .with_context(|| format!("pack asset {path:?} is not UTF-8"))
    }

    pub fn catalog_entry(&self) -> GraphPackageCatalogEntry {
        GraphPackageCatalogEntry {
            name: self.manifest.name.clone(),
            version: self.manifest.version.clone(),
            description: self.manifest.description.clone(),
            package_digest: self.package_digest.clone(),
            compiler_version: crate::graph_pipeline::COMPILER_VERSION.to_owned(),
            external_dependencies: self.manifest.external_dependencies.clone(),
            entries: self
                .config
                .graph_intents
                .iter()
                .flat_map(|intent| intent.entries.clone())
                .collect(),
            results: self
                .config
                .graph_intents
                .iter()
                .flat_map(|intent| intent.results.clone())
                .collect(),
            capabilities: self.config.graph_capabilities.clone(),
        }
    }
}

pub fn load_archive_graph_package_with_environment(
    archive: &crate::pack_archive::PackArchive,
    options: &PackInstallOptions,
    environment: &dyn Fn(&str) -> Option<String>,
) -> Result<LoadedGraphPackage> {
    load_package_from_assets(
        archive.manifest().clone(),
        archive.digest().to_owned(),
        options,
        &|path| Ok(archive.asset(path)?.to_vec()),
        environment,
    )
}

/// Where a graph's compiled plan travels inside its pack: the graph id in the
/// snake_case every pack file name uses.
pub fn graph_plan_path(graph_id: &str) -> String {
    format!("graphs/{}.plan.json", graph_id.replace('-', "_"))
}

/// Compiles every graph a pack declares from its files, for the placeholder
/// owner `agent_did`. A plan names no owner, so the result is the plan any
/// install of these files compiles; `gents pack build` ships it and install
/// recompiles to verify it.
pub fn compile_pack_graphs(
    manifest: &crate::pack::PackManifest,
    asset: &dyn Fn(&str) -> Result<Vec<u8>>,
    agent_did: &str,
) -> Result<Vec<crate::graph_pipeline::GraphPlan>> {
    let options = PackInstallOptions {
        agent_did: agent_did.to_owned(),
    };
    let config = crate::pack::load_pack_config(manifest, &options, asset, &|_| None)?;
    anyhow::ensure!(
        !config.graph_intents.is_empty(),
        "a graph pack declares at least one graph"
    );
    config
        .graph_intents
        .iter()
        .map(|intent| {
            crate::graph_pipeline::compile_graph(
                intent,
                &config.graph_capabilities,
                agent_did,
                &crate::graph_pipeline::CompilerPolicy::default(),
            )
            .with_context(|| format!("graph {} does not compile", intent.graph_id))
        })
        .collect()
}

/// Loads a graph pack as an install would and compiles every graph it
/// declares for the placeholder owner `agent_did`, writing nothing. Returns
/// the plans; a shipped plan that differs from its recompilation is refused.
pub fn check_graph_pack(
    archive: &crate::pack_archive::PackArchive,
    agent_did: &str,
) -> Result<Vec<crate::graph_pipeline::GraphPlan>> {
    let options = PackInstallOptions {
        agent_did: agent_did.to_owned(),
    };
    load_archive_graph_package_with_environment(archive, &options, &|_| None)?;
    let plans = compile_pack_graphs(
        archive.manifest(),
        &|path| Ok(archive.asset(path)?.to_vec()),
        agent_did,
    )?;
    for plan in &plans {
        verify_shipped_plan(plan, &|path| archive.asset(path).ok().map(<[u8]>::to_vec))?;
    }
    Ok(plans)
}

/// Refuses a pack whose shipped plan for `compiled`'s graph differs from what
/// this build compiles: it was built by another compiler, or edited after
/// build. A pack that ships no plan is compiled at install, as always.
pub fn verify_shipped_plan(
    compiled: &crate::graph_pipeline::GraphPlan,
    asset: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Result<()> {
    let Some(bytes) = asset(&graph_plan_path(&compiled.graph_id)) else {
        return Ok(());
    };
    let shipped: crate::graph_pipeline::GraphPlan =
        serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "the shipped plan for graph {} is not a plan",
                compiled.graph_id
            )
        })?;
    anyhow::ensure!(
        shipped.digest == compiled.digest,
        "graph {} was built into this pack by compiler {} and compiles differently here ({}); \
         rebuild the pack with gents pack build",
        compiled.graph_id,
        shipped.compiler_version,
        compiled.compiler_version
    );
    Ok(())
}

fn load_package_from_assets(
    manifest: crate::pack::PackManifest,
    package_digest: String,
    options: &PackInstallOptions,
    asset: &dyn Fn(&str) -> Result<Vec<u8>>,
    environment: &dyn Fn(&str) -> Option<String>,
) -> Result<LoadedGraphPackage> {
    crate::pack::validate_pack_manifest(&manifest)?;
    anyhow::ensure!(
        manifest.metadata.kind == crate::pack::PackKind::Graph,
        "pack is not a graph"
    );
    let config = crate::pack::load_pack_config(&manifest, options, asset, environment)?;
    let mut asset_paths = manifest.metadata.assets.clone();
    asset_paths.push("manifest.json".to_owned());
    asset_paths.sort();
    asset_paths.dedup();
    let assets = asset_paths
        .iter()
        .map(|path| Ok((path.clone(), asset(path)?)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let resolved_digest = crate::pack::digest_declared_assets(&manifest, |path| {
        assets
            .get(path)
            .map(Vec::as_slice)
            .with_context(|| format!("resolved graph package is missing asset {path:?}"))
    })?;
    anyhow::ensure!(
        resolved_digest == package_digest,
        "graph distribution digest changed after resolution"
    );
    // Capabilities reference the same owned Task documents as ordinary packs.
    // Port/topology/caller/schema checks remain in the compiler and publication
    // owner; this loader never constructs behavior/model/tool overrides.
    for surface in &config.datastore_tool_surfaces {
        for entry in surface.entries.iter().flatten() {
            entry
                .validate()
                .with_context(|| format!("invalid datastore surface {}", surface.surface_id))?;
        }
    }
    for capability in &config.graph_capabilities {
        anyhow::ensure!(
            capability.agent_did == options.agent_did,
            "foreign graph capability owner"
        );
        // A plugin node's artifact was matched and pinned when the config loaded.
        let Some(task_id) = capability.target.task_id() else {
            continue;
        };
        anyhow::ensure!(
            config
                .tasks
                .iter()
                .filter(|task| task.agent_did == capability.agent_did && task.task_id == task_id)
                .count()
                == 1,
            "graph capability {} must reference exactly one owned task {task_id}",
            capability.capability_id,
        );
    }
    Ok(LoadedGraphPackage {
        manifest,
        config,
        package_digest,
        assets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> PackInstallOptions {
        PackInstallOptions {
            agent_did: "did:key:fixture".to_owned(),
        }
    }

    /// The checked-in `review_graph` fixture directory. Callers pack it
    /// fresh with `pack_dir`, so mutating the resulting bytes never touches
    /// this path or another test's copy.
    fn review_graph_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/packs/review_graph")
    }

    #[test]
    fn verified_archive_uses_the_same_graph_loader_and_content_digest() {
        let (bytes, _) = crate::pack_archive::pack_dir(&review_graph_dir()).unwrap();
        let directory_archive = crate::pack_archive::PackArchive::from_bytes(&bytes).unwrap();
        let home = tempfile::tempdir().unwrap();
        let store = crate::pack_store::PackStore::new(home.path());
        let stored = store.import(bytes.as_slice(), None).unwrap();
        let store_archive = store.open(&stored.header.digest).unwrap();
        let environment =
            |name: &str| (name == "GENTS_REVIEW_MODEL").then(|| "test-model".to_owned());
        let from_directory = load_archive_graph_package_with_environment(
            &directory_archive,
            &options(),
            &environment,
        )
        .unwrap();
        let from_store =
            load_archive_graph_package_with_environment(&store_archive, &options(), &environment)
                .unwrap();
        assert_eq!(from_directory.package_digest, from_store.package_digest);
        assert_eq!(
            serde_json::to_value(&from_directory.manifest).unwrap(),
            serde_json::to_value(&from_store.manifest).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&from_directory.config).unwrap(),
            serde_json::to_value(&from_store.config).unwrap()
        );
        for path in std::iter::once("manifest.json").chain(
            from_directory
                .manifest
                .metadata
                .assets
                .iter()
                .map(String::as_str),
        ) {
            assert_eq!(
                from_directory.asset(path).unwrap(),
                from_store.asset(path).unwrap()
            );
        }
    }
}
