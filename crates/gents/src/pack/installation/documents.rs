use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::config_client::{ConfigAccess, SchemaInstallPlan};
use crate::document_config::PackConfig;
use crate::pack_archive::PackArchive;

use super::super::home_install::materialize_asset_pack;
use super::super::{
    HomePackInstall, InstalledVia, PackInferenceBindingPreview, PackInferenceBindings,
    PackInstallOptions,
};
use super::{DriftPolicy, InstallReport, InstalledPackPlugin, PackIdentity};

pub(crate) fn sdl_paths(manifest: &super::super::PackManifest) -> std::collections::BTreeSet<&str> {
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

/// Unique key field sets per declared collection.
pub type UniqueKeySets = BTreeMap<String, Vec<Vec<String>>>;
/// Field names per declared collection.
pub type CollectionFields = BTreeMap<String, Vec<String>>;

/// Checks `plugin`'s `data` against the pack that ships it
/// and returns each declared collection's unique key field sets and field
/// names. Every path that installs a pack plugin goes through this check.
pub fn plugin_data_unique_keys<'a>(
    manifest: &super::super::PackManifest,
    plugin: &super::super::PackPlugin,
    asset: &dyn Fn(&str) -> Result<&'a [u8]>,
) -> Result<(UniqueKeySets, CollectionFields)> {
    let name = &plugin.name;
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

/// A dependency prepared for the install of its dependent, installed like
/// the root but flagged non-explicit. `manifest`, `digest` and `assets` hold
/// what its install reads: the manifest, the pack digest, and the schema and
/// plugin files.
pub struct PreparedDependency {
    pub(crate) coordinate: String,
    pub(crate) config: PackConfig,
    pub(crate) inference: PackInferenceBindingPreview,
    pub(crate) schemas: Vec<(String, SchemaInstallPlan)>,
    pub(crate) manifest: super::super::PackManifest,
    pub(crate) digest: String,
    pub(crate) assets: BTreeMap<String, Vec<u8>>,
}

pub struct PreparedDocumentPackInstall {
    pub config: PackConfig,
    pub inference: PackInferenceBindingPreview,
    pub dependency_inference: BTreeMap<String, PackInferenceBindingPreview>,
    pub dependencies: Vec<PreparedDependency>,
}

pub(super) fn document_ux_install_allowed(
    has_ux: bool,
    has_home: bool,
    installed_via: InstalledVia,
) -> bool {
    !has_ux || (has_home && installed_via == InstalledVia::Operator)
}

pub(super) fn require_installed_dependency_ux(
    manifest: &super::super::PackManifest,
    digest: &str,
    home: Option<&Path>,
) -> Result<()> {
    if manifest.metadata.ux.is_empty() {
        return Ok(());
    }
    let coordinate = format!("{}/{}", manifest.metadata.namespace, manifest.name);
    let home = home.context(
        "a dependency with UX must first be installed by the operator in the node's local home",
    )?;
    let record = super::super::read_home_install(home, &coordinate)?
        .context("a dependency with UX must first be installed by the operator")?;
    anyhow::ensure!(record.digest == digest && record.installed_via == Some(InstalledVia::Operator),
        "dependency {coordinate} with UX must first be installed explicitly by the operator at the same digest");
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "install inputs and policies supplied by each install entry point"
)]
pub async fn prepare_document_pack_install(
    access: &ConfigAccess,
    owner: &str,
    manifest: &super::super::PackManifest,
    config: &PackConfig,
    requested: &PackInferenceBindings,
    dependencies: &[&PackArchive],
    installed_via: InstalledVia,
    plugin_home: Option<&Path>,
    root_archive: Option<&PackArchive>,
    environment: &(dyn Fn(&str) -> Option<String> + Sync),
) -> Result<PreparedDocumentPackInstall> {
    if let Some(archive) = root_archive {
        anyhow::ensure!(
            serde_json::to_value(archive.manifest())? == serde_json::to_value(manifest)?,
            "prepared pack manifest does not match the archive"
        );
    }
    anyhow::ensure!(
        document_ux_install_allowed(
            !manifest.metadata.ux.is_empty(),
            plugin_home.is_some(),
            installed_via
        ),
        "document packs with UX require an operator install with the node's local home"
    );
    for dependency in dependencies {
        require_installed_dependency_ux(dependency.manifest(), dependency.digest(), plugin_home)?;
        let dependency = dependency.manifest();
        anyhow::ensure!(
            matches!(
                dependency.metadata.kind,
                super::super::PackKind::Documents
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
        let prepared = {
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
            PreparedDependency {
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
        };
        dependency_inference.insert(archive.manifest().name.clone(), preview);
        prepared_dependencies.push(prepared);
    }
    let coordinate = format!("{}/{}", manifest.metadata.namespace, manifest.name);
    let shipped: Vec<(String, &PackConfig)> = std::iter::once((coordinate.clone(), &config))
        .chain(
            prepared_dependencies
                .iter()
                .map(|dependency| (dependency.coordinate.clone(), &dependency.config)),
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
    let mut shipped_digests = BTreeMap::new();
    let pinned = std::iter::once(&config)
        .chain(
            prepared_dependencies
                .iter()
                .map(|dependency| &dependency.config),
        )
        .flat_map(|config| &config.tools)
        .flat_map(|tools| {
            tools
                .integrations
                .as_ref()
                .and_then(|i| i.plugins.as_ref())
                .into_iter()
                .flatten()
        })
        .filter(|reference| reference.digest.is_some())
        .map(|reference| reference.plugin.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    for (shipper, archive) in std::iter::once((manifest, root_archive)).chain(
        dependencies
            .iter()
            .map(|archive| (archive.manifest(), Some(*archive))),
    ) {
        for plugin in &shipper.metadata.plugins {
            let coordinate = format!("{}/{}", shipper.metadata.namespace, plugin.name);
            if pinned.contains(coordinate.as_str()) {
                let archive =
                    archive.context("a pinned shipped plugin requires its pack archive")?;
                let digest = format!(
                    "sha256:{}",
                    crate::plugin::store::content_digest_hex(archive.asset(&plugin.artifact)?)
                );
                if let Some(previous) = shipped_digests.insert(coordinate.clone(), digest.clone()) {
                    anyhow::ensure!(
                        previous == digest,
                        "pack graph ships conflicting artifacts for plugin {coordinate}"
                    );
                }
            }
        }
    }
    ensure_plugin_references_resolve(
        &std::iter::once((manifest, &config))
            .chain(
                prepared_dependencies
                    .iter()
                    .map(|dependency| (&dependency.manifest, &dependency.config)),
            )
            .collect::<Vec<_>>(),
        plugin_home,
        &shipped_digests,
    )?;
    Ok(PreparedDocumentPackInstall {
        config,
        inference,
        dependency_inference,
        dependencies: prepared_dependencies,
    })
}

/// Refuses unresolved Tools references before any pack writes. A missing
/// `plugin_home` cannot establish remote file availability: GraphQL installation
/// records are not a plugin inventory and this interface cannot publish bytes
/// to that host. Only plugin-free content can be prepared without a local home.
pub(super) fn ensure_plugin_references_resolve(
    packs: &[(&super::super::PackManifest, &PackConfig)],
    plugin_home: Option<&Path>,
    shipped_digests: &BTreeMap<String, String>,
) -> Result<()> {
    let root = format!("{}/{}", packs[0].0.metadata.namespace, packs[0].0.name);
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
                let home = plugin_home.with_context(|| format!(
                    "pack {} Tools {:?} names plugin {}; remote GraphQL installation cannot verify plugin files on the node; run `gents pack install {} --home <node-home>` on that node without --graphql",
                    manifest.name, tools.tools_id, reference.plugin, root
                ))?;
                if shipped.contains(&reference.plugin) {
                    if let Some(pin) = &reference.digest {
                        let actual = shipped_digests.get(&reference.plugin).with_context(|| {
                            format!("cannot verify shipped plugin {} digest", reference.plugin)
                        })?;
                        anyhow::ensure!(pin == actual,
                            "pack {} Tools {:?} pins plugin {} to {}, but the shipped artifact is {}; update the pin or the pack",
                            manifest.name, tools.tools_id, reference.plugin, pin, actual);
                    }
                    continue;
                }
                let (namespace, name) = crate::plugin::store::parse_coordinate(&reference.plugin)?;
                match crate::plugin::store::read_record(home, namespace, name) {
                    Ok(record) => {
                        if let Some(pin) = &reference.digest {
                            anyhow::ensure!(pin == &record.digest,
                                "pack {} Tools {:?} pins plugin {} to {}, but the installed artifact is {}; install the matching plugin or update the pin",
                                manifest.name, tools.tools_id, reference.plugin, pin, record.digest);
                        }
                    }
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
    if plugin_home.is_none() {
        if let Some(plugin) = shipped.first() {
            anyhow::bail!(
                "pack {root} ships plugin {plugin}; remote GraphQL installation cannot publish plugin files on the node; run `gents pack install {root} --home <node-home>` on that node without --graphql"
            );
        }
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "install inputs and policies supplied by each install entry point"
)]
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
    anyhow::ensure!(
        document_ux_install_allowed(
            !archive.manifest().metadata.ux.is_empty(),
            plugin_home.is_some(),
            installed_via
        ),
        "document packs with UX require an operator install with the node's local home"
    );
    for dependency in &prepared.dependencies {
        require_installed_dependency_ux(&dependency.manifest, &dependency.digest, plugin_home)?;
    }
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
            .map(|dependency| &dependency.manifest),
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
    apply_prepared_dependency_schemas(access, prepared).await?;
    let mut schemas = Vec::new();
    for path in document_pack_schema_paths(archive.manifest())? {
        let sdl = std::str::from_utf8(archive.asset(path)?)?;
        schemas.push((
            path,
            crate::config_client::preview_schema_install(access, sdl).await?,
        ));
    }
    for (path, plan) in schemas {
        crate::config_client::apply_schema_install(
            access,
            std::str::from_utf8(archive.asset(path)?)?,
            &plan.artifact_digest,
        )
        .await?;
    }
    let dependency_coordinates = prepared
        .dependencies
        .iter()
        .map(|dependency| dependency.coordinate.clone())
        .collect::<Vec<_>>();
    let (report, plugins) = install_prepared_document_graph(
        access,
        owner,
        archive,
        prepared,
        policy,
        plugin_home,
        grant_authority,
        true,
    )
    .await?;
    let root_coordinate = format!(
        "{}/{}",
        archive.manifest().metadata.namespace,
        archive.manifest().name
    );
    let mut identity = PackIdentity::new(
        archive.manifest(),
        archive.digest(),
        plugins
            .iter()
            .filter(|plugin| {
                plugin.owner_pack_coordinate.as_deref() == Some(root_coordinate.as_str())
            })
            .map(|plugin| InstalledPackPlugin {
                name: plugin.name.clone(),
                digest: plugin.digest.clone(),
            })
            .collect(),
    );
    identity.dependencies = dependency_coordinates;
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
/// the provenance.
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

#[cfg(test)]
#[derive(Clone)]
pub(super) struct InstallationPause {
    pub coordinate: String,
    pub reached: std::sync::Arc<tokio::sync::Notify>,
    pub release: std::sync::Arc<tokio::sync::Notify>,
    pub completed: std::sync::Arc<tokio::sync::Notify>,
}

#[cfg(test)]
pub(super) static INSTALLATION_PAUSE: std::sync::Mutex<Option<InstallationPause>> =
    std::sync::Mutex::new(None);

pub async fn apply_prepared_dependency_schemas(
    access: &ConfigAccess,
    prepared: &PreparedDocumentPackInstall,
) -> Result<()> {
    for dependency in &prepared.dependencies {
        for (path, plan) in &dependency.schemas {
            crate::config_client::apply_schema_install(
                access,
                std::str::from_utf8(&dependency.assets[path])?,
                &plan.artifact_digest,
            )
            .await?;
        }
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "prepared graph installation inputs"
)]
pub async fn install_prepared_document_graph(
    access: &ConfigAccess,
    owner: &str,
    archive: &PackArchive,
    prepared: &PreparedDocumentPackInstall,
    policy: DriftPolicy,
    home: Option<&Path>,
    consent: bool,
    explicit: bool,
) -> Result<(InstallReport, Vec<crate::plugin::store::InstalledPlugin>)> {
    let mut stages = Vec::new();
    for dependency in &prepared.dependencies {
        stages.push(DocumentPackStage {
            manifest: dependency.manifest.clone(),
            digest: dependency.digest.clone(),
            config: dependency.config.clone(),
            assets: dependency.assets.clone(),
            bindings: dependency.inference.bindings.clone(),
            dependencies: Vec::new(),
            explicit: false,
        });
    }
    let mut assets = BTreeMap::new();
    for path in crate::plugin::install::stage_asset_paths(archive.manifest())? {
        assets.insert(path.to_owned(), archive.asset(path)?.to_vec());
    }
    stages.push(DocumentPackStage {
        manifest: archive.manifest().clone(),
        digest: archive.digest().to_owned(),
        config: prepared.config.clone(),
        assets,
        bindings: prepared.inference.bindings.clone(),
        dependencies: prepared
            .dependencies
            .iter()
            .map(|d| d.coordinate.clone())
            .collect(),
        explicit,
    });
    let (mut reports, plugins) =
        install_document_graph(access, owner, stages, policy, home, consent).await?;
    Ok((
        reports.pop().context("pack graph has no root report")?,
        plugins,
    ))
}

#[derive(Clone)]
pub(super) struct DocumentPackStage {
    pub(super) manifest: super::super::PackManifest,
    pub(super) digest: String,
    pub(super) config: PackConfig,
    pub(super) assets: BTreeMap<String, Vec<u8>>,
    pub(super) bindings: PackInferenceBindings,
    pub(super) dependencies: Vec<String>,
    pub(super) explicit: bool,
}

pub(super) async fn install_document_graph(
    access: &ConfigAccess,
    owner: &str,
    stages: Vec<DocumentPackStage>,
    policy: DriftPolicy,
    home: Option<&Path>,
    consent: bool,
) -> Result<(
    Vec<InstallReport>,
    Vec<crate::plugin::store::InstalledPlugin>,
)> {
    let access = access.clone();
    let owner = owner.to_owned();
    let home = home.map(Path::to_owned);
    tokio::spawn(async move {
        let documents = stages
            .iter()
            .map(|stage| super::installable_documents(&stage.config))
            .collect::<Result<Vec<_>>>()?;
        let input = stages
            .iter()
            .filter(|stage| !stage.manifest.metadata.plugins.is_empty())
            .map(|stage| crate::plugin::install::PluginStageInput {
                manifest: stage.manifest.clone(),
                digest: stage.digest.clone(),
                assets: stage.assets.clone(),
                bindings: stage.bindings.clone(),
            })
            .collect::<Vec<_>>();
        let has_references = stages.iter().any(|stage| {
            stage.config.tools.iter().any(|tools| {
                tools
                    .integrations
                    .as_ref()
                    .and_then(|integrations| integrations.plugins.as_ref())
                    .is_some_and(|references| !references.is_empty())
            })
        });
        let plugin_stage = if input.is_empty() && !has_references {
            None
        } else {
            Some(
                crate::plugin::install::stage_pack_graph(
                    home.as_deref()
                        .context("pack plugins require the node's local plugin home")?,
                    input,
                    consent,
                    &owner,
                )
                .await?,
            )
        };
        #[cfg(test)]
        let pause = {
            let mut slot = INSTALLATION_PAUSE.lock().unwrap();
            if slot.as_ref().is_some_and(|pause| {
                stages.iter().any(|stage| {
                    pause.coordinate
                        == format!(
                            "{}/{}",
                            stage.manifest.metadata.namespace, stage.manifest.name
                        )
                })
            }) {
                slot.take()
            } else {
                None
            }
        };
        #[cfg(test)]
        if let Some(pause) = &pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
        let plugins = plugin_stage
            .as_ref()
            .map(|stage| stage.installed.clone())
            .unwrap_or_default();
        let reference_packs = stages
            .iter()
            .rev()
            .map(|stage| (stage.manifest.clone(), stage.config.clone()))
            .collect::<Vec<_>>();
        let identities = stages
            .iter()
            .map(|stage| {
                let coordinate = format!(
                    "{}/{}",
                    stage.manifest.metadata.namespace, stage.manifest.name
                );
                let mut identity = PackIdentity::new(
                    &stage.manifest,
                    &stage.digest,
                    plugins
                        .iter()
                        .filter(|plugin| {
                            plugin.owner_pack_coordinate.as_deref() == Some(coordinate.as_str())
                        })
                        .map(|plugin| InstalledPackPlugin {
                            name: plugin.name.clone(),
                            digest: plugin.digest.clone(),
                        })
                        .collect(),
                );
                identity.dependencies = stage.dependencies.clone();
                identity.explicit = stage.explicit;
                identity
            })
            .collect::<Vec<_>>();
        let result = access
            .transact_write_with_finality("pack.document_graph.install", |txn| {
                let documents = documents.clone();
                let identities = &identities;
                let owner = &owner;
                let reference_packs = reference_packs.clone();
                let home = home.clone();
                let plugins = plugins.clone();
                Box::pin(async move {
                    tokio::task::spawn_blocking(move || {
                        let packs = reference_packs
                            .iter()
                            .map(|(manifest, config)| (manifest, config))
                            .collect::<Vec<_>>();
                        let digests = plugins
                            .iter()
                            .map(|plugin| {
                                (
                                    format!("{}/{}", plugin.namespace, plugin.name),
                                    plugin.digest.clone(),
                                )
                            })
                            .collect();
                        ensure_plugin_references_resolve(&packs, home.as_deref(), &digests)
                    })
                    .await??;
                    let mut reports = Vec::new();
                    for (identity, documents) in identities.iter().zip(documents) {
                        reports.push(
                            super::install_in_txn(txn, owner, identity, documents, policy).await?,
                        );
                    }
                    super::validate_plugin_replacements_in_txn(txn, owner, identities).await?;
                    Ok::<_, anyhow::Error>(reports)
                })
            })
            .await;
        let rejected = result
            .as_ref()
            .err()
            .is_some_and(|error| !error.unconfirmed);
        if let Some(stage) = plugin_stage {
            if let Err(cleanup) = stage.finish(!rejected).await {
                return Err(match result {
                    Err(failure) => failure
                        .error
                        .context(format!("plugin store cleanup failed: {cleanup:#}")),
                    Ok(_) => cleanup
                        .context("plugin store cleanup failed after committed pack transaction"),
                });
            }
        }
        #[cfg(test)]
        if let Some(pause) = &pause {
            pause.completed.notify_one();
        }
        result
            .map_err(|error| error.error)
            .map(|reports| (reports, plugins))
            .with_context(|| {
                if rejected {
                    "pack document transaction was rejected; local plugin records were restored"
                } else {
                    "pack document outcome was not confirmed; local plugins remain published"
                }
            })
    })
    .await
    .context("owned pack installation task failed")?
}

#[expect(
    clippy::too_many_arguments,
    reason = "the document and plugin owners share an installation outcome"
)]
pub async fn install_documents_with_plugins(
    access: &ConfigAccess,
    owner: &str,
    manifest: &super::super::PackManifest,
    digest: &str,
    config: &PackConfig,
    policy: DriftPolicy,
    home: Option<&Path>,
    assets: BTreeMap<String, Vec<u8>>,
    consent: bool,
    bindings: &PackInferenceBindings,
    dependencies: Vec<String>,
    explicit: bool,
) -> Result<(InstallReport, Vec<crate::plugin::store::InstalledPlugin>)> {
    let stage = DocumentPackStage {
        manifest: manifest.clone(),
        digest: digest.into(),
        config: config.clone(),
        assets,
        bindings: bindings.clone(),
        dependencies,
        explicit,
    };
    let (mut reports, plugins) =
        install_document_graph(access, owner, vec![stage], policy, home, consent).await?;
    Ok((
        reports.pop().context("pack graph has no root report")?,
        plugins,
    ))
}
