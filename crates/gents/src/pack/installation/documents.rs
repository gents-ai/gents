use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::documents::PackConfig;
use crate::pack::archive::PackArchive;
use crate::store::config_client::{ConfigAccess, SchemaInstallPlan};

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
    pub(crate) installation_doc_id: Option<String>,
    pub(crate) agent_bindings: super::super::PackAgentBindings,
    pub(crate) coordinate: String,
    pub(crate) config: PackConfig,
    pub(crate) inference: PackInferenceBindingPreview,
    pub(crate) schemas: Vec<(String, SchemaInstallPlan)>,
    pub(crate) manifest: super::super::PackManifest,
    pub(crate) digest: String,
    pub(crate) assets: BTreeMap<String, Vec<u8>>,
}

pub struct PreparedDocumentPackInstall {
    pub installation_doc_id: Option<String>,
    pub agent_bindings: super::super::PackAgentBindings,
    pub(crate) node_binding: Option<crate::store::config_client::ConfigurationNodeBinding>,
    pub config: PackConfig,
    pub inference: PackInferenceBindingPreview,
    pub dependency_inference: BTreeMap<String, PackInferenceBindingPreview>,
    pub dependencies: Vec<PreparedDependency>,
}

/// Public facts from the staged host, checked against the reviewed archives.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedPackPreparation {
    pub agent_bindings: BTreeMap<String, super::super::PackAgentBindings>,
    pub plugin_digests: BTreeMap<String, String>,
    pub operator_ux_digests: BTreeMap<String, String>,
}

impl PreparedDocumentPackInstall {
    pub fn hosted_preparation(
        &self,
        root: &super::super::PackManifest,
        home: &Path,
    ) -> Result<HostedPackPreparation> {
        let mut agent_bindings = BTreeMap::new();
        agent_bindings.insert(
            format!("{}/{}", root.metadata.namespace, root.name),
            self.agent_bindings.clone(),
        );
        let mut operator_ux_digests = BTreeMap::new();
        for dependency in &self.dependencies {
            agent_bindings.insert(
                dependency.coordinate.clone(),
                dependency.agent_bindings.clone(),
            );
            if !dependency.manifest.metadata.ux.is_empty() {
                let record = super::super::read_home_install(home, &dependency.coordinate)?
                    .context("dependency with UX has no operator installation")?;
                anyhow::ensure!(
                    record.installed_via == Some(InstalledVia::Operator),
                    "dependency UX was not installed by an operator"
                );
                operator_ux_digests.insert(dependency.coordinate.clone(), record.digest);
            }
        }
        let shipped = std::iter::once(root)
            .chain(self.dependencies.iter().map(|d| &d.manifest))
            .flat_map(|manifest| {
                manifest
                    .metadata
                    .plugins
                    .iter()
                    .map(|plugin| format!("{}/{}", manifest.metadata.namespace, plugin.name))
            })
            .collect::<std::collections::BTreeSet<_>>();
        let mut plugin_digests = BTreeMap::new();
        for reference in std::iter::once(&self.config)
            .chain(self.dependencies.iter().map(|d| &d.config))
            .flat_map(|config| &config.tools)
            .flat_map(|tools| {
                tools
                    .integrations
                    .as_ref()
                    .and_then(|i| i.plugins.as_ref())
                    .into_iter()
                    .flatten()
            })
        {
            if !shipped.contains(&reference.plugin) {
                let (namespace, name) = crate::plugin::store::parse_coordinate(&reference.plugin)?;
                let record = crate::plugin::store::read_record(home, namespace, name)?;
                plugin_digests.insert(reference.plugin.clone(), record.digest);
            }
        }
        Ok(HostedPackPreparation {
            agent_bindings,
            plugin_digests,
            operator_ux_digests,
        })
    }
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
    prepare_document_pack_install_inner(
        None,
        BTreeMap::new(),
        BTreeMap::new(),
        None,
        access,
        owner,
        manifest,
        config,
        requested,
        dependencies,
        installed_via,
        plugin_home,
        root_archive,
        environment,
        None,
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "archive preparation shares the canonical install inputs"
)]
pub async fn prepare_document_pack_archive(
    access: &ConfigAccess,
    owner: &str,
    archive: &PackArchive,
    requested: &PackInferenceBindings,
    dependencies: &[&PackArchive],
    installed_via: InstalledVia,
    plugin_home: Option<&Path>,
    environment: &(dyn Fn(&str) -> Option<String> + Sync),
) -> Result<PreparedDocumentPackInstall> {
    prepare_document_pack_archive_with_provisioner(
        access,
        owner,
        archive,
        requested,
        dependencies,
        installed_via,
        plugin_home,
        environment,
        None,
    )
    .await
}

/// Prepare a local operator install with the held Node identity used for slots.
fn require_documents_dependency(dependency: &super::super::PackManifest) -> Result<()> {
    anyhow::ensure!(
        matches!(dependency.metadata.kind, super::super::PackKind::Documents),
        "dependency {}/{} is a {:?} pack, which cannot be installed as a dependency; \
         install it with `gents pack install` or `gents plugin install`, then remove it from `dependencies`",
        dependency.metadata.namespace, dependency.name, dependency.metadata.kind
    );
    anyhow::ensure!(
        dependency.metadata.dependencies.is_empty(),
        "dependency {}/{} declares dependencies of its own; dependencies are one level: \
         install it with `gents pack install`, then remove it from `dependencies`",
        dependency.metadata.namespace,
        dependency.name
    );
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "archive preparation shares canonical install inputs"
)]
pub async fn prepare_document_pack_archive_with_runtime(
    access: &ConfigAccess,
    owner: &str,
    archive: &PackArchive,
    requested: &PackInferenceBindings,
    dependencies: &[&PackArchive],
    plugin_home: Option<&Path>,
    environment: &(dyn Fn(&str) -> Option<String> + Sync),
    runtime: &dyn crate::store::identity::NodeIdentity,
) -> Result<PreparedDocumentPackInstall> {
    let node = access
        .local_node()
        .context("local pack preparation requires an embedded Node")?;
    anyhow::ensure!(
        runtime.did() == owner && node.node_identity_did() == Some(owner),
        "held runtime identity does not match the selected Node"
    );
    let provisioner = super::super::agent_bindings::AgentProvisioner {
        node,
        runtime,
        actor: None,
        selected_agent_did: None,
        node_binding: None,
    };
    prepare_document_pack_archive_with_provisioner(
        access,
        owner,
        archive,
        requested,
        dependencies,
        InstalledVia::Operator,
        plugin_home,
        environment,
        Some(&provisioner),
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "archive preparation includes an optional executing Agent provisioner"
)]
pub(crate) async fn prepare_document_pack_archive_with_provisioner(
    access: &ConfigAccess,
    owner: &str,
    archive: &PackArchive,
    requested: &PackInferenceBindings,
    dependencies: &[&PackArchive],
    installed_via: InstalledVia,
    plugin_home: Option<&Path>,
    environment: &(dyn Fn(&str) -> Option<String> + Sync),
    provisioner: Option<&super::super::agent_bindings::AgentProvisioner<'_>>,
) -> Result<PreparedDocumentPackInstall> {
    prepare_document_pack_archive_inputs(
        access,
        owner,
        archive,
        requested,
        dependencies,
        installed_via,
        plugin_home,
        environment,
        provisioner,
        None,
    )
    .await
}

/// Rebuild the installation from locally reviewed archive bytes and staged host facts.
pub async fn prepare_hosted_document_pack_archive(
    access: &ConfigAccess,
    owner: &str,
    archive: &PackArchive,
    requested: &PackInferenceBindings,
    dependencies: &[&PackArchive],
    environment: &(dyn Fn(&str) -> Option<String> + Sync),
    hosted: &HostedPackPreparation,
) -> Result<PreparedDocumentPackInstall> {
    let expected = std::iter::once(archive)
        .chain(dependencies.iter().copied())
        .map(|archive| {
            format!(
                "{}/{}",
                archive.manifest().metadata.namespace,
                archive.manifest().name
            )
        })
        .collect::<std::collections::BTreeSet<_>>();
    anyhow::ensure!(
        hosted
            .agent_bindings
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            == expected,
        "hosted pack bindings do not match reviewed archives"
    );
    prepare_document_pack_archive_inputs(
        access,
        owner,
        archive,
        requested,
        dependencies,
        InstalledVia::Operator,
        None,
        environment,
        None,
        Some(hosted),
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "archive preparation shares canonical install inputs"
)]
async fn prepare_document_pack_archive_inputs(
    access: &ConfigAccess,
    owner: &str,
    archive: &PackArchive,
    requested: &PackInferenceBindings,
    dependencies: &[&PackArchive],
    installed_via: InstalledVia,
    plugin_home: Option<&Path>,
    environment: &(dyn Fn(&str) -> Option<String> + Sync),
    provisioner: Option<&super::super::agent_bindings::AgentProvisioner<'_>>,
    hosted: Option<&HostedPackPreparation>,
) -> Result<PreparedDocumentPackInstall> {
    let declared = super::super::agent_bindings::declared_dependencies(archive.manifest());
    let options = PackInstallOptions {
        node_did: owner.to_owned(),
    };
    let mut preloaded = BTreeMap::new();
    let mut available = BTreeMap::new();
    for dependency in dependencies {
        let manifest = dependency.manifest();
        let coordinate = format!("{}/{}", manifest.metadata.namespace, manifest.name);
        anyhow::ensure!(
            declared.contains(&coordinate),
            "prepared dependency {coordinate:?} is not declared by the root pack"
        );
        require_documents_dependency(manifest)?;
        let loaded = super::super::agent_bindings::load_pack_install_config_with_dependencies(
            access,
            manifest,
            &options,
            &|path| dependency.asset(path).map(<[u8]>::to_vec),
            environment,
            installed_via,
            super::super::agent_bindings::PackBindingInputs {
                available: &BTreeMap::new(),
                provisioner,
                hosted: hosted.and_then(|facts| facts.agent_bindings.get(&coordinate)),
            },
        )
        .await?;
        available.insert(coordinate.clone(), loaded.agent_bindings.clone());
        anyhow::ensure!(
            preloaded.insert(coordinate, loaded).is_none(),
            "duplicate prepared dependency"
        );
    }
    let loaded = super::super::agent_bindings::load_pack_install_config_with_dependencies(
        access,
        archive.manifest(),
        &PackInstallOptions {
            node_did: owner.to_owned(),
        },
        &|path| archive.asset(path).map(<[u8]>::to_vec),
        environment,
        installed_via,
        super::super::agent_bindings::PackBindingInputs {
            available: &available,
            provisioner,
            hosted: hosted.and_then(|facts| {
                facts.agent_bindings.get(&format!(
                    "{}/{}",
                    archive.manifest().metadata.namespace,
                    archive.manifest().name
                ))
            }),
        },
    )
    .await?;
    prepare_document_pack_install_inner(
        loaded.installation_doc_id,
        loaded.agent_bindings,
        preloaded,
        provisioner.and_then(|context| context.node_binding),
        access,
        owner,
        archive.manifest(),
        &loaded.config,
        requested,
        dependencies,
        installed_via,
        plugin_home,
        Some(archive),
        environment,
        hosted,
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "install inputs and policies supplied by each install entry point"
)]
async fn prepare_document_pack_install_inner(
    installation_doc_id: Option<String>,
    agent_bindings: super::super::PackAgentBindings,
    mut preloaded: BTreeMap<String, super::super::BoundPackConfig>,
    node_binding: Option<&crate::store::config_client::ConfigurationNodeBinding>,
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
    hosted: Option<&HostedPackPreparation>,
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
            plugin_home.is_some() || hosted.is_some(),
            installed_via
        ),
        "document packs with UX require an operator install with the node's local home"
    );
    for dependency in dependencies {
        if let Some(hosted) = hosted {
            if !dependency.manifest().metadata.ux.is_empty() {
                let coordinate = format!(
                    "{}/{}",
                    dependency.manifest().metadata.namespace,
                    dependency.manifest().name
                );
                anyhow::ensure!(hosted.operator_ux_digests.get(&coordinate).map(String::as_str) == Some(dependency.digest()),
                    "dependency {coordinate} with UX must be operator installed at the reviewed digest");
            }
        } else {
            require_installed_dependency_ux(
                dependency.manifest(),
                dependency.digest(),
                plugin_home,
            )?;
        }
        require_documents_dependency(dependency.manifest())?;
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
    let inference = super::super::preview_pack_inference_bindings_with_node_binding(
        access,
        manifest,
        owner,
        &selected(manifest),
        node_binding,
    )
    .await?;
    let bound_manifest =
        super::super::agent_bindings::manifest_with_agent_bindings(manifest, &agent_bindings);
    let config =
        super::super::bind_pack_install_config(&bound_manifest, config, &inference.bindings)?;
    let mut dependency_inference = BTreeMap::new();
    let mut prepared_dependencies = Vec::new();
    for archive in dependencies {
        let preview = super::super::preview_pack_inference_bindings_with_node_binding(
            access,
            archive.manifest(),
            owner,
            &selected(archive.manifest()),
            node_binding,
        )
        .await?;
        let options = PackInstallOptions {
            node_did: owner.to_owned(),
        };
        let prepared = {
            let coordinate = format!(
                "{}/{}",
                archive.manifest().metadata.namespace,
                archive.manifest().name
            );
            let authored = match preloaded.remove(&coordinate) {
                Some(loaded) => loaded,
                None => {
                    super::super::load_pack_install_config(
                        access,
                        archive.manifest(),
                        &options,
                        &|path| archive.asset(path).map(<[u8]>::to_vec),
                        environment,
                        installed_via,
                    )
                    .await?
                }
            };
            let bound_manifest = super::super::agent_bindings::manifest_with_agent_bindings(
                archive.manifest(),
                &authored.agent_bindings,
            );
            let config = super::super::bind_pack_install_config(
                &bound_manifest,
                &authored.config,
                &preview.bindings,
            )?;
            let mut schemas = Vec::new();
            let mut assets = BTreeMap::new();
            for path in document_pack_schema_paths(archive.manifest())? {
                let sdl = archive.asset(path)?;
                schemas.push((
                    path.to_owned(),
                    crate::store::config_client::preview_schema_install(
                        access,
                        std::str::from_utf8(sdl)?,
                    )
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
                installation_doc_id: authored.installation_doc_id,
                agent_bindings: authored.agent_bindings,
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
        crate::tools::background::job::settings::pack_job_block_admissible(
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
    ensure_plugin_references_resolve_with_inventory(
        &std::iter::once((manifest, &config))
            .chain(
                prepared_dependencies
                    .iter()
                    .map(|dependency| (&dependency.manifest, &dependency.config)),
            )
            .collect::<Vec<_>>(),
        plugin_home,
        &shipped_digests,
        hosted.map(|facts| &facts.plugin_digests),
    )?;
    Ok(PreparedDocumentPackInstall {
        installation_doc_id,
        agent_bindings,
        node_binding: node_binding.cloned(),
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
    ensure_plugin_references_resolve_with_inventory(packs, plugin_home, shipped_digests, None)
}

fn ensure_plugin_references_resolve_with_inventory(
    packs: &[(&super::super::PackManifest, &PackConfig)],
    plugin_home: Option<&Path>,
    shipped_digests: &BTreeMap<String, String>,
    inventory: Option<&BTreeMap<String, String>>,
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
                if plugin_home.is_none() && inventory.is_none() {
                    anyhow::bail!("pack {} Tools {:?} names plugin {}; remote GraphQL installation cannot verify plugin files on the node; run `gents pack install {} --home <node-home>` on that node without --graphql", manifest.name, tools.tools_id, reference.plugin, root);
                }
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
                if let Some(inventory) = inventory {
                    let digest = inventory.get(&reference.plugin).with_context(|| {
                        format!("hosted plugin {} is unavailable", reference.plugin)
                    })?;
                    if let Some(pin) = &reference.digest {
                        anyhow::ensure!(
                            pin == digest,
                            "hosted plugin {} does not match its pinned digest",
                            reference.plugin
                        );
                    }
                    continue;
                }
                let home = plugin_home.context("plugin home is unavailable")?;
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
    if plugin_home.is_none() && inventory.is_none() {
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
            crate::store::config_client::preview_schema_install(access, sdl).await?,
        ));
    }
    for (path, plan) in schemas {
        crate::store::config_client::apply_schema_install(
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
            crate::store::config_client::apply_schema_install(
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
    let stages = document_graph_stages(archive, prepared, explicit)?;
    let (reports, plugins) = install_document_graph(
        access,
        owner,
        stages,
        policy,
        home,
        consent,
        prepared.node_binding.clone(),
    )
    .await?;
    Ok((assemble_document_graph_report(prepared, reports)?, plugins))
}

pub fn assemble_document_graph_report(
    prepared: &PreparedDocumentPackInstall,
    mut reports: Vec<InstallReport>,
) -> Result<InstallReport> {
    let mut report = reports.pop().context("pack graph has no root report")?;
    for (dependency, installed) in prepared.dependencies.iter().zip(reports) {
        for (slot, did) in installed.agents {
            report
                .agents
                .insert(format!("{}#{slot}", dependency.coordinate), did);
        }
    }
    Ok(report)
}

pub(super) fn document_graph_stages(
    archive: &PackArchive,
    prepared: &PreparedDocumentPackInstall,
    explicit: bool,
) -> Result<Vec<DocumentPackStage>> {
    let mut stages = Vec::new();
    for dependency in &prepared.dependencies {
        stages.push(DocumentPackStage {
            installation_doc_id: dependency.installation_doc_id.clone(),
            agent_bindings: dependency.agent_bindings.clone(),
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
        installation_doc_id: prepared.installation_doc_id.clone(),
        agent_bindings: prepared.agent_bindings.clone(),
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
    Ok(stages)
}

#[derive(Clone)]
pub(super) struct DocumentPackStage {
    pub(super) installation_doc_id: Option<String>,
    pub(super) agent_bindings: super::super::PackAgentBindings,
    pub(super) manifest: super::super::PackManifest,
    pub(super) digest: String,
    pub(super) config: PackConfig,
    pub(super) assets: BTreeMap<String, Vec<u8>>,
    pub(super) bindings: PackInferenceBindings,
    pub(super) dependencies: Vec<String>,
    pub(super) explicit: bool,
}

/// The same document graph reconstructed by the host and the client.
#[derive(Clone)]
pub struct HostedDocumentGraph {
    identities: Vec<PackIdentity>,
    documents: Vec<Vec<crate::store::config_client::DesiredStateApplyDocument>>,
    node_binding: Option<crate::store::config_client::ConfigurationNodeBinding>,
    plugin_root: bool,
}

pub fn hosted_document_graph(
    archive: &PackArchive,
    prepared: &PreparedDocumentPackInstall,
    explicit: bool,
) -> Result<HostedDocumentGraph> {
    let stages = document_graph_stages(archive, prepared, explicit)?;
    hosted_document_graph_from_stages(&stages, prepared)
}

pub(super) fn hosted_document_graph_from_stages(
    stages: &[DocumentPackStage],
    prepared: &PreparedDocumentPackInstall,
) -> Result<HostedDocumentGraph> {
    let mut identities = Vec::new();
    let mut documents = Vec::new();
    for stage in stages {
        let mut plugins = Vec::new();
        for plugin in &stage.manifest.metadata.plugins {
            let bytes = stage
                .assets
                .get(&plugin.artifact)
                .with_context(|| format!("missing reviewed plugin artifact {}", plugin.artifact))?;
            plugins.push(InstalledPackPlugin {
                name: plugin.name.clone(),
                digest: format!("sha256:{}", crate::plugin::store::content_digest_hex(bytes)),
            });
        }
        let mut identity = PackIdentity::new(&stage.manifest, &stage.digest, plugins);
        identity.installation_doc_id = stage.installation_doc_id.clone();
        identity.agent_bindings = stage.agent_bindings.clone();
        identity.dependencies = stage.dependencies.clone();
        identity.explicit = stage.explicit;
        identities.push(identity);
        documents.push(super::installable_documents(&stage.config)?);
    }
    Ok(HostedDocumentGraph {
        identities,
        documents,
        node_binding: prepared.node_binding.clone(),
        plugin_root: stages.last().is_some_and(|stage| {
            matches!(
                stage.manifest.metadata.kind,
                super::super::PackKind::Plugins | super::super::PackKind::Assets
            )
        }),
    })
}

pub async fn apply_hosted_document_graph(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    graph: &HostedDocumentGraph,
    policy: DriftPolicy,
) -> Result<Vec<InstallReport>> {
    if !graph.plugin_root {
        return apply_document_graph_in_txn(
            txn,
            owner,
            &graph.identities,
            graph.documents.clone(),
            policy,
            graph.node_binding.as_ref(),
        )
        .await;
    }
    let (root, dependencies) = graph
        .identities
        .split_last()
        .context("hosted plugin root missing")?;
    let (root_documents, dependency_documents) = graph
        .documents
        .split_last()
        .context("hosted plugin graph missing")?;
    anyhow::ensure!(
        root_documents.is_empty(),
        "plugin root cannot replace pack documents"
    );
    let mut reports = apply_document_graph_in_txn(
        txn,
        owner,
        dependencies,
        dependency_documents.to_vec(),
        policy,
        graph.node_binding.as_ref(),
    )
    .await?;
    super::validate_plugin_replacements_in_txn(txn, owner, std::slice::from_ref(root)).await?;
    super::write_installed_plugin_identity(txn, owner, root, graph.node_binding.as_ref()).await?;
    super::dependencies::claim_in_txn(txn, owner, &root.coordinate, &root.dependencies, policy)
        .await?;
    reports.push(super::InstallReport {
        plugins: root.plugins.clone(),
        ..Default::default()
    });
    Ok(reports)
}

pub(super) async fn stage_document_plugins(
    stages: &[DocumentPackStage],
    home: Option<&Path>,
    consent: bool,
    owner: &str,
) -> Result<Option<crate::plugin::install::StagedPluginInstall>> {
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
    if input.is_empty() && !has_references {
        return Ok(None);
    }
    Ok(Some(
        crate::plugin::install::stage_pack_graph(
            home.context("pack plugins require the node's local plugin home")?,
            input,
            consent,
            owner,
        )
        .await?,
    ))
}

pub(super) async fn apply_document_graph_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    identities: &[PackIdentity],
    documents: Vec<Vec<crate::store::config_client::DesiredStateApplyDocument>>,
    policy: DriftPolicy,
    node_binding: Option<&crate::store::config_client::ConfigurationNodeBinding>,
) -> Result<Vec<InstallReport>> {
    let mut reports = Vec::new();
    for (identity, documents) in identities.iter().zip(documents) {
        reports.push(
            super::install_in_txn(txn, owner, identity, documents, policy, node_binding).await?,
        );
    }
    super::validate_plugin_replacements_in_txn(txn, owner, identities).await?;
    Ok(reports)
}

pub(super) async fn install_document_graph(
    access: &ConfigAccess,
    owner: &str,
    stages: Vec<DocumentPackStage>,
    policy: DriftPolicy,
    home: Option<&Path>,
    consent: bool,
    node_binding: Option<crate::store::config_client::ConfigurationNodeBinding>,
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
        let plugin_stage =
            stage_document_plugins(&stages, home.as_deref(), consent, &owner).await?;
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
        let previous_plugins = plugin_stage
            .as_ref()
            .map(|stage| stage.previous_plugins())
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
                identity.installation_doc_id = stage.installation_doc_id.clone();
                identity.agent_bindings = stage.agent_bindings.clone();
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
                let staged_plugins = plugins.clone();
                let previous_plugins = previous_plugins.clone();
                let node_binding = node_binding.clone();
                Box::pin(async move {
                    txn.stage_plugin_delete_roots(&previous_plugins, &staged_plugins)
                        .await?;
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
                    apply_document_graph_in_txn(
                        txn,
                        owner,
                        identities,
                        documents,
                        policy,
                        node_binding.as_ref(),
                    )
                    .await
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
        installation_doc_id: None,
        agent_bindings: BTreeMap::new(),
        manifest: manifest.clone(),
        digest: digest.into(),
        config: config.clone(),
        assets,
        bindings: bindings.clone(),
        dependencies,
        explicit,
    };
    let (mut reports, plugins) =
        install_document_graph(access, owner, vec![stage], policy, home, consent, None).await?;
    Ok((
        reports.pop().context("pack graph has no root report")?,
        plugins,
    ))
}
