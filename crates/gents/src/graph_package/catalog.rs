use serde::Serialize;
use std::collections::BTreeMap;

use crate::graph_pipeline::{EntryBinding, ResultContract};
use crate::pack::PackInstallOptions;
pub use crate::pack::PackageExternalDependency;

pub type GraphPackageManifest = crate::pack::PackManifest;
pub type PackageCapabilityTemplate = crate::graph_pipeline::StageCapability;

#[derive(Debug, thiserror::Error)]
pub enum GraphPackageError {
    #[error("asset {path:?} is not declared by package {package}")]
    AssetNotDeclared { path: String, package: String },
    #[error("pack asset {path:?} is not UTF-8")]
    AssetNotUtf8 {
        path: String,
        #[source]
        source: std::str::Utf8Error,
    },
    #[error("running git in {}", repo.display())]
    GitSpawn {
        repo: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("git {arguments} failed in {repo}: {stderr}")]
    GitFailed {
        arguments: String,
        repo: String,
        stderr: String,
    },
    #[error(transparent)]
    GitOutputNotUtf8(#[from] std::string::FromUtf8Error),
    #[error("Git emitted non-UTF-8 diff output")]
    GitDiffNotUtf8(#[source] std::string::FromUtf8Error),
    #[error("graph {graph_id:?} has no entry named {name:?}")]
    EntryNotNamed { graph_id: String, name: String },
    #[error("graph {graph_id:?} declares no entry")]
    NoEntries { graph_id: String },
    #[error("graph {graph_id:?} has {count} entries; name the entry to start (one of: {names})")]
    AmbiguousEntries {
        graph_id: String,
        count: usize,
        names: String,
    },
    #[error("entry {entry_name:?} input is missing string field {field:?}")]
    RequiredStringField { entry_name: String, field: String },
    #[error("selected graph is not in package")]
    SelectedGraphNotInPackage,
    #[error("selected graph is ambiguous in package")]
    SelectedGraphAmbiguous,
    #[error("package contains no graph intent")]
    PackageNoGraphIntent,
    #[error("package contains multiple graphs; use the explicit graph selection entry point")]
    PackageMultipleGraphs,
    #[error("package graph definition is ambiguous within owner")]
    PackageGraphDefinitionAmbiguous,
    #[error("the shipped plan for graph {graph_id} is not a plan")]
    ShippedPlanNotPlan {
        graph_id: String,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "graph {graph_id} was built into this pack by compiler {shipped_compiler} and compiles differently here ({compiled_compiler}); rebuild the pack with gents pack build"
    )]
    ShippedPlanDrift {
        graph_id: String,
        shipped_compiler: String,
        compiled_compiler: String,
    },
    #[error(transparent)]
    Pack(#[from] crate::pack::PackError),
    #[error(transparent)]
    Loader(#[from] crate::pack::LoaderError),
    #[error(transparent)]
    Archive(#[from] crate::pack_archive::PackArchiveError),
    #[error(transparent)]
    Asset(crate::pack::CallbackError),
    #[error("a graph pack declares at least one graph")]
    NoGraphs,
    #[error("graph {graph_id} does not compile")]
    Compile {
        graph_id: String,
        #[source]
        source: crate::graph_pipeline::GraphCompileError,
    },
    #[error("pack is not a graph")]
    NotGraphPack,
    #[error("graph distribution digest changed after resolution")]
    DigestChanged,
    #[error("invalid datastore surface {surface_id}")]
    DatastoreSurface {
        surface_id: String,
        #[source]
        source: Box<crate::document_config::DocumentConfigError>,
    },
    #[error("foreign graph capability owner")]
    ForeignCapabilityOwner,
    #[error("graph capability {capability_id} must reference exactly one owned task {task_id}")]
    CapabilityTask {
        capability_id: String,
        task_id: String,
    },
}

type Result<T, E = GraphPackageError> = std::result::Result<T, E>;

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
    pub fn asset(&self, path: &str) -> Result<&[u8], GraphPackageError> {
        self.assets.get(path).map(Vec::as_slice).ok_or_else(|| {
            GraphPackageError::AssetNotDeclared {
                path: path.to_owned(),
                package: self.manifest.name.clone(),
            }
        })
    }

    pub fn asset_text(&self, path: &str) -> Result<&str, GraphPackageError> {
        std::str::from_utf8(self.asset(path)?).map_err(|source| GraphPackageError::AssetNotUtf8 {
            path: path.to_owned(),
            source,
        })
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
        &|path| archive.asset(path).map(<[u8]>::to_vec),
        environment,
    )
}

pub fn graph_plan_path(graph_id: &str) -> String {
    format!("graphs/{}.plan.json", graph_id.replace('-', "_"))
}

pub fn compile_pack_graphs<E>(
    manifest: &crate::pack::PackManifest,
    asset: &dyn Fn(&str) -> Result<Vec<u8>, E>,
    node_did: &str,
) -> Result<Vec<crate::graph_pipeline::GraphPlan>>
where
    E: Into<crate::pack::CallbackError>,
{
    let options = PackInstallOptions {
        node_did: node_did.to_owned(),
    };
    let config = crate::pack::load_pack_config(manifest, &options, asset, &|_| None)?;
    if config.graph_intents.is_empty() {
        return Err(GraphPackageError::NoGraphs);
    }
    config
        .graph_intents
        .iter()
        .map(|intent| {
            crate::graph_pipeline::compile_graph(
                intent,
                &config.graph_capabilities,
                node_did,
                &crate::graph_pipeline::CompilerPolicy::default(),
            )
            .map_err(|source| GraphPackageError::Compile {
                graph_id: intent.graph_id.clone(),
                source,
            })
        })
        .collect()
}

pub fn check_graph_pack(
    archive: &crate::pack_archive::PackArchive,
    node_did: &str,
) -> Result<Vec<crate::graph_pipeline::GraphPlan>> {
    let options = PackInstallOptions {
        node_did: node_did.to_owned(),
    };
    load_archive_graph_package_with_environment(archive, &options, &|_| None)?;
    let plans = compile_pack_graphs(
        archive.manifest(),
        &|path| archive.asset(path).map(<[u8]>::to_vec),
        node_did,
    )?;
    for plan in &plans {
        verify_shipped_plan(plan, &|path| archive.asset(path).ok().map(<[u8]>::to_vec))?;
    }
    Ok(plans)
}

pub fn verify_shipped_plan(
    compiled: &crate::graph_pipeline::GraphPlan,
    asset: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Result<(), GraphPackageError> {
    let Some(bytes) = asset(&graph_plan_path(&compiled.graph_id)) else {
        return Ok(());
    };
    let shipped: crate::graph_pipeline::GraphPlan =
        serde_json::from_slice(&bytes).map_err(|source| GraphPackageError::ShippedPlanNotPlan {
            graph_id: compiled.graph_id.clone(),
            source,
        })?;
    if shipped.digest != compiled.digest {
        return Err(GraphPackageError::ShippedPlanDrift {
            graph_id: compiled.graph_id.clone(),
            shipped_compiler: shipped.compiler_version.clone(),
            compiled_compiler: compiled.compiler_version.clone(),
        });
    }
    Ok(())
}

fn load_package_from_assets<E>(
    manifest: crate::pack::PackManifest,
    package_digest: String,
    options: &PackInstallOptions,
    asset: &dyn Fn(&str) -> Result<Vec<u8>, E>,
    environment: &dyn Fn(&str) -> Option<String>,
) -> Result<LoadedGraphPackage>
where
    E: Into<crate::pack::CallbackError>,
{
    crate::pack::validate_pack_manifest(&manifest)?;
    if manifest.metadata.kind != crate::pack::PackKind::Graph {
        return Err(GraphPackageError::NotGraphPack);
    }
    let config = crate::pack::load_pack_config(&manifest, options, asset, environment)?;
    let mut asset_paths = manifest.metadata.assets.clone();
    asset_paths.push("manifest.json".to_owned());
    asset_paths.sort();
    asset_paths.dedup();
    let assets = asset_paths
        .iter()
        .map(|path| {
            asset(path)
                .map(|bytes| (path.clone(), bytes))
                .map_err(|error| GraphPackageError::Asset(error.into()))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let resolved_digest =
        crate::pack::digest_declared_assets(&manifest, |path| assets.get(path).map(Vec::as_slice))?;
    if resolved_digest != package_digest {
        return Err(GraphPackageError::DigestChanged);
    }
    for surface in &config.datastore_tool_surfaces {
        for entry in surface.entries.iter().flatten() {
            entry
                .validate()
                .map_err(|source| GraphPackageError::DatastoreSurface {
                    surface_id: surface.surface_id.clone(),
                    source: Box::new(source),
                })?;
        }
    }
    for capability in &config.graph_capabilities {
        if capability.node_did != options.node_did {
            return Err(GraphPackageError::ForeignCapabilityOwner);
        }
        let Some(task_id) = capability.target.task_id() else {
            continue;
        };
        if config
            .tasks
            .iter()
            .filter(|task| task.node_did == capability.node_did && task.task_id == task_id)
            .count()
            != 1
        {
            return Err(GraphPackageError::CapabilityTask {
                capability_id: capability.capability_id.clone(),
                task_id: task_id.to_owned(),
            });
        }
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
            node_did: "did:key:fixture".to_owned(),
        }
    }

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
