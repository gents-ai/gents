use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::config_client::{ConfigAccess, SchemaInstallPlan};
use crate::document_config::PackConfig;
use crate::graph_package::{GraphInstallRecord, GraphPackageInstallBindings, LoadedGraphPackage};
use crate::pack_archive::PackArchive;

use super::super::home_install::materialize_asset_pack;
use super::super::{
    HomePackInstall, InstalledVia, PackInferenceBindingPreview, PackInferenceBindings,
    PackInstallOptions,
};
use super::{DriftPolicy, InstallReport, InstalledPackPlugin, PackIdentity};

fn sdl_paths(manifest: &super::super::PackManifest) -> std::collections::BTreeSet<&str> {
    let mut paths: std::collections::BTreeSet<&str> = manifest
        .metadata
        .assets
        .iter()
        .map(String::as_str)
        .filter(|path| {
            path.starts_with("schemas/") && (path.ends_with(".graphql") || path.ends_with(".gql"))
        })
        .collect();
    paths.extend(manifest.schemas.iter().map(String::as_str));
    paths
}

pub fn document_pack_schema_paths(manifest: &super::super::PackManifest) -> Result<Vec<&str>> {
    for path in manifest
        .metadata
        .assets
        .iter()
        .filter(|path| path.starts_with("schemas/"))
    {
        anyhow::ensure!(
            !path.ends_with(".patch.json") && !path.ends_with(".json-patch"),
            "document packs containing schema patches require the CLI installer"
        );
    }
    Ok(sdl_paths(manifest).into_iter().collect())
}

/// The fields of a declared collection and the field sets of its unique
/// indexes, as DefraDB's SDL parser reads them.
#[derive(Default)]
struct SdlType {
    fields: std::collections::BTreeSet<String>,
    unique: Vec<Vec<String>>,
}

fn sdl_types(sdl: &str) -> Result<BTreeMap<String, SdlType>> {
    let named = |value: &serde_json::Value| {
        value
            .get("Name")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let list = |value: &serde_json::Value, key: &str| -> Vec<serde_json::Value> {
        value
            .get(key)
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let mut types = BTreeMap::new();
    for collection in query::parse_sdl(sdl)? {
        let collection = serde_json::to_value(&collection)?;
        let mut declared = SdlType {
            fields: list(&collection, "Fields")
                .iter()
                .filter_map(named)
                .collect(),
            ..SdlType::default()
        };
        for index in list(&collection, "Indexes") {
            if index.get("Unique").and_then(serde_json::Value::as_bool) == Some(true) {
                declared
                    .unique
                    .push(list(&index, "Fields").iter().filter_map(named).collect());
            }
        }
        types.insert(
            named(&collection).context("collection is missing Name")?,
            declared,
        );
    }
    Ok(types)
}

/// Checks `plugin`'s `data` and `asset_mounts` against the pack that ships it
/// and returns each declared collection's unique key field sets and field
/// names. Every path that installs a pack plugin goes through this check.
pub fn plugin_data_unique_keys<'a>(
    manifest: &super::super::PackManifest,
    plugin: &super::super::PackPlugin,
    asset: &dyn Fn(&str) -> Result<&'a [u8]>,
) -> Result<(
    BTreeMap<String, Vec<Vec<String>>>,
    BTreeMap<String, Vec<String>>,
)> {
    let name = &plugin.name;
    for mount in &plugin.asset_mounts {
        let prefix = format!("{}/", mount.class);
        anyhow::ensure!(
            manifest.metadata.assets.iter().any(|path| {
                path.starts_with(&prefix) && super::super::is_distributable_asset_path(path)
            }),
            "plugin {name:?} asset_mounts class {:?} has no asset under {prefix}; ship one or \
             drop the mount",
            mount.class
        );
    }
    let Some(data) = &plugin.data else {
        return Ok((BTreeMap::new(), BTreeMap::new()));
    };
    let mut types = BTreeMap::new();
    for path in sdl_paths(manifest) {
        types.extend(sdl_types(std::str::from_utf8(asset(path)?)?)?);
    }
    let mut keys = BTreeMap::new();
    let mut fields = BTreeMap::new();
    for collection in &data.collections {
        let declared = types.get(collection).with_context(|| {
            format!(
                "plugin {name:?} data.collections names {collection:?}, which no schemas/*.graphql \
                 in the pack declares; add the type to the pack's SDL or drop it"
            )
        })?;
        keys.insert(collection.clone(), declared.unique.clone());
        fields.insert(
            collection.clone(),
            declared.fields.iter().cloned().collect(),
        );
    }
    for field in data.stamped.keys() {
        anyhow::ensure!(
            data.collections
                .iter()
                .any(|collection| types[collection].fields.contains(field)),
            "plugin {name:?} data.stamped field {field:?} is not a field of any collection in \
             data.collections; stamp a field the pack's SDL declares"
        );
    }
    Ok((keys, fields))
}

/// A dependency prepared for the install of its dependent.
pub enum PreparedDependency {
    Graph(LoadedGraphPackage, GraphPackageInstallBindings),
    /// Installed like the root, flagged non-explicit. `manifest`, `digest`
    /// and `assets` hold what its install reads: the manifest, the pack
    /// digest, and the schema and plugin files.
    Documents {
        coordinate: String,
        config: PackConfig,
        inference: PackInferenceBindingPreview,
        schemas: Vec<(String, SchemaInstallPlan)>,
        manifest: super::super::PackManifest,
        digest: String,
        assets: BTreeMap<String, Vec<u8>>,
    },
}

impl PreparedDependency {
    fn manifest(&self) -> &super::super::PackManifest {
        match self {
            Self::Graph(package, _) => &package.manifest,
            Self::Documents { manifest, .. } => manifest,
        }
    }

    fn config(&self) -> &PackConfig {
        match self {
            Self::Graph(package, _) => &package.config,
            Self::Documents { config, .. } => config,
        }
    }

    fn coordinate(&self) -> String {
        match self {
            Self::Graph(package, _) => {
                format!(
                    "{}/{}",
                    package.manifest.metadata.namespace, package.manifest.name
                )
            }
            Self::Documents { coordinate, .. } => coordinate.clone(),
        }
    }
}

pub struct PreparedDocumentPackInstall {
    pub config: PackConfig,
    pub inference: PackInferenceBindingPreview,
    pub dependency_inference: BTreeMap<String, PackInferenceBindingPreview>,
    pub dependencies: Vec<PreparedDependency>,
}

pub async fn prepare_document_pack_install(
    access: &ConfigAccess,
    owner: &str,
    manifest: &super::super::PackManifest,
    config: &PackConfig,
    requested: &PackInferenceBindings,
    dependencies: &[&PackArchive],
    installed_via: InstalledVia,
    plugin_home: Option<&Path>,
    environment: &(dyn Fn(&str) -> Option<String> + Sync),
) -> Result<PreparedDocumentPackInstall> {
    for dependency in dependencies {
        let dependency = dependency.manifest();
        anyhow::ensure!(
            matches!(
                dependency.metadata.kind,
                super::super::PackKind::Graph | super::super::PackKind::Documents
            ),
            "dependency {}/{} is a {:?} pack, which cannot be installed as a dependency; \
             install it with `gents pack install` or `gents plugin install`, then remove it from `dependencies`",
            dependency.metadata.namespace,
            dependency.name,
            dependency.metadata.kind
        );
        anyhow::ensure!(
            dependency.metadata.dependencies.is_empty(),
            "dependency {}/{} declares dependencies of its own; dependencies are one level: \
             install it with `gents pack install`, then remove it from `dependencies`",
            dependency.metadata.namespace,
            dependency.name
        );
    }
    for slot in requested.keys() {
        anyhow::ensure!(
            std::iter::once(manifest)
                .chain(dependencies.iter().map(|p| p.manifest()))
                .any(|m| m
                    .metadata
                    .inference_slots
                    .iter()
                    .any(|declared| &declared.name == slot)),
            "pack {} and its dependencies have no inference slot {slot:?}",
            manifest.name
        );
    }
    let selected = |manifest: &super::super::PackManifest| {
        requested
            .iter()
            .filter(|(slot, _)| {
                manifest
                    .metadata
                    .inference_slots
                    .iter()
                    .any(|declared| &declared.name == *slot)
            })
            .map(|(slot, profile)| (slot.clone(), profile.clone()))
            .collect()
    };
    let inference =
        super::super::preview_pack_inference_bindings(access, manifest, owner, &selected(manifest))
            .await?;
    let config = super::super::bind_pack_install_config(manifest, config, &inference.bindings)?;
    let mut dependency_inference = BTreeMap::new();
    let mut prepared_dependencies = Vec::new();
    for archive in dependencies {
        let preview = super::super::preview_pack_inference_bindings(
            access,
            archive.manifest(),
            owner,
            &selected(archive.manifest()),
        )
        .await?;
        let options = PackInstallOptions {
            node_did: owner.to_owned(),
        };
        let prepared = if archive.manifest().metadata.kind == super::super::PackKind::Documents {
            let authored = super::super::load_pack_config(
                archive.manifest(),
                &options,
                &|path| archive.asset(path).map(<[u8]>::to_vec),
                environment,
            )?;
            let config = super::super::bind_pack_install_config(
                archive.manifest(),
                &authored,
                &preview.bindings,
            )?;
            let mut schemas = Vec::new();
            let mut assets = BTreeMap::new();
            for path in document_pack_schema_paths(archive.manifest())? {
                let sdl = archive.asset(path)?;
                schemas.push((
                    path.to_owned(),
                    crate::config_client::preview_schema_install(access, std::str::from_utf8(sdl)?)
                        .await?,
                ));
                assets.insert(path.to_owned(), sdl.to_vec());
            }
            for plugin in &archive.manifest().metadata.plugins {
                for path in std::iter::once(&plugin.artifact).chain(&plugin.instructions) {
                    assets.insert(path.clone(), archive.asset(path)?.to_vec());
                }
            }
            PreparedDependency::Documents {
                coordinate: format!(
                    "{}/{}",
                    archive.manifest().metadata.namespace,
                    archive.manifest().name
                ),
                config,
                inference: preview.clone(),
                schemas,
                manifest: archive.manifest().clone(),
                digest: archive.digest().to_owned(),
                assets,
            }
        } else {
            let package = crate::graph_package::load_archive_graph_package_with_environment(
                archive,
                &options,
                environment,
            )?;
            let bindings = GraphPackageInstallBindings {
                node_did: owner.to_owned(),
                inference_slots: preview.bindings.clone(),
            };
            crate::graph_package::prepare_loaded_graph_package_install(access, &package, &bindings)
                .await?;
            PreparedDependency::Graph(package, bindings)
        };
        dependency_inference.insert(archive.manifest().name.clone(), preview);
        prepared_dependencies.push(prepared);
    }
    let coordinate = format!("{}/{}", manifest.metadata.namespace, manifest.name);
    let shipped: Vec<(String, &PackConfig)> = std::iter::once((coordinate.clone(), &config))
        .chain(
            prepared_dependencies
                .iter()
                .map(|dependency| (dependency.coordinate(), dependency.config())),
        )
        .collect();
    for tools in &config.tools {
        crate::plugin_job::settings::pack_job_block_admissible(
            tools,
            &manifest.metadata,
            &coordinate,
            &shipped,
            installed_via,
        )?;
    }
    ensure_plugin_references_resolve(
        &std::iter::once((manifest, &config))
            .chain(
                prepared_dependencies
                    .iter()
                    .map(|dependency| (dependency.manifest(), dependency.config())),
            )
            .collect::<Vec<_>>(),
        plugin_home,
    )?;
    Ok(PreparedDocumentPackInstall {
        config,
        inference,
        dependency_inference,
        dependencies: prepared_dependencies,
    })
}

/// Refuses Tools naming a plugin that no pack in `packs` (the root first, then
/// its dependencies) ships and that the node's plugin store lacks, since
/// startup cannot build that agent's tools. `plugin_home` is `None` when that
/// store is not visible to this installer (a remote GraphQL install);
/// references outside these packs are then left to startup, which reports the
/// agent unavailable.
pub(super) fn ensure_plugin_references_resolve(
    packs: &[(&super::super::PackManifest, &PackConfig)],
    plugin_home: Option<&Path>,
) -> Result<()> {
    let Some(home) = plugin_home else {
        return Ok(());
    };
    let root = &packs[0].0.name;
    let shipped = packs
        .iter()
        .flat_map(|(shipper, _)| {
            shipper
                .metadata
                .plugins
                .iter()
                .map(|plugin| format!("{}/{}", shipper.metadata.namespace, plugin.name))
        })
        .collect::<std::collections::BTreeSet<_>>();
    for (manifest, config) in packs {
        for tools in &config.tools {
            let references = tools
                .integrations
                .as_ref()
                .and_then(|integrations| integrations.plugins.as_deref())
                .unwrap_or_default();
            for reference in references {
                if shipped.contains(&reference.plugin) {
                    continue;
                }
                let (namespace, name) = crate::plugin::store::parse_coordinate(&reference.plugin)?;
                match crate::plugin::store::read_record(home, namespace, name) {
                    Ok(_) => {}
                    Err(error) if crate::plugin::store::is_not_found(&error) => anyhow::bail!(
                        "pack {} Tools {:?} names plugin {}, which neither the pack nor its dependencies ship and this node has not installed; install the pack that ships {} first (`gents pack install <namespace>/<pack>`), or declare that pack as a dependency of {}",
                        manifest.name,
                        tools.tools_id,
                        reference.plugin,
                        reference.plugin,
                        root
                    ),
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("reading installed plugin {}", reference.plugin)
                        })
                    }
                }
            }
        }
    }
    Ok(())
}

pub async fn install_prepared_document_pack(
    access: &ConfigAccess,
    owner: &str,
    archive: &PackArchive,
    dependency_archives: &[&PackArchive],
    prepared: &PreparedDocumentPackInstall,
    plugin_home: Option<&Path>,
    grant_authority: bool,
    installed_via: InstalledVia,
    policy: DriftPolicy,
) -> Result<InstallReport> {
    for plugin in &archive.manifest().metadata.plugins {
        plugin_data_unique_keys(archive.manifest(), plugin, &|path| archive.asset(path))?;
    }
    for dependency in dependency_archives {
        for plugin in &dependency.manifest().metadata.plugins {
            plugin_data_unique_keys(dependency.manifest(), plugin, &|path| {
                dependency.asset(path)
            })?;
        }
    }
    let all = std::iter::once(archive.manifest()).chain(
        prepared
            .dependencies
            .iter()
            .map(PreparedDependency::manifest),
    );
    for manifest in all {
        for plugin in &manifest.metadata.plugins {
            let home = plugin_home.context("pack plugins require the node's local plugin home")?;
            crate::plugin::store::grant_on_install(
                home,
                &manifest.metadata.namespace,
                plugin,
                grant_authority,
            )?;
        }
    }
    // The leases are held until the install's home record is written.
    let mut leases = Vec::new();
    let mut assets = String::new();
    let mut dependency_assets = Vec::new();
    if let Some(home) = plugin_home {
        for dependency in dependency_archives {
            if let Some((root, lease)) = materialize_asset_pack(home, dependency)? {
                dependency_assets.push((
                    format!(
                        "{}/{}",
                        dependency.manifest().metadata.namespace,
                        dependency.manifest().name
                    ),
                    root,
                ));
                leases.push(lease);
            }
        }
        if let Some((root, lease)) = materialize_asset_pack(home, archive)? {
            assets = root;
            leases.push(lease);
        }
    }
    let mut schemas = Vec::new();
    for path in document_pack_schema_paths(archive.manifest())? {
        let sdl = std::str::from_utf8(archive.asset(path)?)?;
        schemas.push((
            path,
            crate::config_client::preview_schema_install(access, sdl).await?,
        ));
    }
    for dependency in &prepared.dependencies {
        match dependency {
            PreparedDependency::Graph(package, bindings) => {
                let plugins = install_plugins(
                    owner,
                    plugin_home,
                    &package.manifest,
                    &package.package_digest,
                    &bindings.inference_slots,
                    &|path| package.asset(path),
                    grant_authority,
                )?;
                let identity =
                    PackIdentity::new(&package.manifest, &package.package_digest, plugins.clone());
                let record = GraphInstallRecord {
                    plugins,
                    explicit: false,
                };
                let receipt = crate::graph_package::install_loaded_graph_package(
                    access, owner, package, bindings, None, &record,
                )
                .await
                .context("graph installation failed after local pack plugins were published; the plugins remain installed")?;
                crate::graph_pipeline::activate_graph_revision_with_access(
                    access,
                    owner,
                    &receipt.graph_id,
                    &receipt.revision_digest,
                    receipt.predecessor_revision_digest.as_deref(),
                )
                .await
                .context("graph activation failed after local pack plugins were published; the plugins remain installed")?;
                if let Some(home) = plugin_home {
                    record_home_install(
                        home,
                        &package.manifest,
                        &identity,
                        String::new(),
                        Vec::new(),
                        installed_via,
                        false,
                    )?;
                }
            }
            PreparedDependency::Documents {
                config,
                inference,
                schemas,
                manifest,
                digest,
                assets,
                ..
            } => {
                for (path, plan) in schemas {
                    crate::config_client::apply_schema_install(
                        access,
                        std::str::from_utf8(&assets[path])?,
                        &plan.artifact_digest,
                    )
                    .await?;
                }
                let plugins = install_plugins(
                    owner,
                    plugin_home,
                    manifest,
                    digest,
                    &inference.bindings,
                    &|path| {
                        assets
                            .get(path)
                            .map(Vec::as_slice)
                            .with_context(|| format!("dependency asset {path} was not prepared"))
                    },
                    grant_authority,
                )?;
                let identity = PackIdentity {
                    explicit: false,
                    ..PackIdentity::new(manifest, digest, plugins)
                };
                super::super::install_pack_documents(access, owner, &identity, config, policy)
                    .await
                    .context("document pack installation failed after local pack plugins were published; the plugins remain installed")?;
            }
        }
    }
    for (path, plan) in schemas {
        crate::config_client::apply_schema_install(
            access,
            std::str::from_utf8(archive.asset(path)?)?,
            &plan.artifact_digest,
        )
        .await?;
    }
    let plugins = install_plugins(
        owner,
        plugin_home,
        archive.manifest(),
        archive.digest(),
        &prepared.inference.bindings,
        &|path| archive.asset(path),
        grant_authority,
    )?;
    let mut identity = PackIdentity::new(archive.manifest(), archive.digest(), plugins);
    identity.dependencies = prepared
        .dependencies
        .iter()
        .map(PreparedDependency::coordinate)
        .collect();
    let report =
        super::super::install_pack_documents(access, owner, &identity, &prepared.config, policy)
            .await
            .context("document pack installation failed after local pack plugins were published; the plugins remain installed")?;
    if let Some(home) = plugin_home {
        record_home_install(
            home,
            archive.manifest(),
            &identity,
            assets,
            dependency_assets,
            installed_via,
            true,
        )?;
    }
    Ok(report)
}

/// Writes the home record of an install through
/// [`record_after_install`](super::super::record_after_install), which decides
/// the provenance and writes nothing for a graph pack.
pub(super) fn record_home_install(
    home: &Path,
    manifest: &super::super::PackManifest,
    identity: &PackIdentity,
    assets: String,
    dependency_assets: Vec<(String, String)>,
    installed_via: InstalledVia,
    explicit: bool,
) -> Result<()> {
    super::super::record_after_install(
        home,
        HomePackInstall {
            coordinate: identity.coordinate.clone(),
            version: identity.version.clone(),
            digest: identity.digest.clone(),
            kind: manifest.metadata.kind.clone(),
            assets,
            dependency_assets,
            plugins: identity.plugins.clone(),
            ux: manifest.metadata.ux.clone(),
            installed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            installed_via: Some(installed_via),
        },
        installed_via,
        explicit,
    )
}

fn install_plugins<'a>(
    owner: &str,
    home: Option<&Path>,
    manifest: &super::super::PackManifest,
    digest: &str,
    bindings: &PackInferenceBindings,
    asset: &dyn Fn(&str) -> Result<&'a [u8]>,
    grant_authority: bool,
) -> Result<Vec<InstalledPackPlugin>> {
    if manifest.metadata.plugins.is_empty() {
        return Ok(Vec::new());
    }
    let home = home.context("pack plugins require the node's local plugin home")?;
    let plugins = crate::plugin::install::install_pack_plugins_with_bindings(
        home,
        manifest,
        digest,
        asset,
        grant_authority,
        Some(owner),
        bindings,
        |_| Ok(()),
    )?;
    Ok(plugins
        .iter()
        .map(|p| InstalledPackPlugin {
            name: p.name.clone(),
            digest: p.digest.clone(),
        })
        .collect())
}
