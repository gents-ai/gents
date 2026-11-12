use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::documents::PackConfig;
use crate::pack::archive::PackArchive;
use crate::store::config_client::{ConfigAccess, SchemaInstallPlan};
use crate::tools::{
    background::job::settings::pack_job_block_admissible, surface::ResolvedToolSelection,
};

use super::super::home_install::materialize_asset_pack;
use super::super::{
    HomePackInstall, InstalledVia, PackInferenceBindingPreview, PackInferenceBindings,
    PackInstallOptions,
};
use super::{DriftPolicy, InstallReport, InstalledPackPlugin, PackChangeError, PackIdentity};

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

/// The collections the SDL of the stored pack `digest` declares.
pub(crate) fn declared_collections(
    home: &Path,
    digest: &str,
) -> Result<std::collections::BTreeSet<String>> {
    archive_declared_collections(&crate::pack::store::PackStore::new(home).open(digest)?)
}

fn archive_declared_collections(
    archive: &PackArchive,
) -> Result<std::collections::BTreeSet<String>> {
    let mut declared = std::collections::BTreeSet::new();
    for path in sdl_paths(archive.manifest()) {
        let sdl = std::str::from_utf8(archive.asset(path)?)
            .with_context(|| format!("pack schema {path:?} is not UTF-8"))?;
        declared.extend(query::parse_sdl(sdl)?.into_iter().map(|schema| schema.name));
    }
    Ok(declared)
}

/// The collections `archive`'s SDL declares that are not on the node yet:
/// the ones installing it creates. Read before the install applies its
/// schemas and kept in the pack's home record, it is what later tells a
/// collection the pack brought from one it found
/// ([`HomePackInstall::created_collections`]).
pub async fn collections_install_creates(
    access: &ConfigAccess,
    archive: &PackArchive,
) -> Result<Vec<String>> {
    // One read per declared collection, the read the schema preview makes:
    // an installer needs no wider collection permission for this.
    let mut created = Vec::new();
    for collection in archive_declared_collections(archive)? {
        if access.collection_version(&collection).await?.is_none() {
            created.push(collection);
        }
    }
    Ok(created)
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
/// Each data collection a pack schema file declares alone, with that file's SDL.
pub type CollectionSchemas = BTreeMap<String, String>;

/// Checks `plugin`'s `data` against the pack that ships it
/// and returns each declared collection's unique key field sets and field
/// names. Every path that installs a pack plugin goes through this check.
pub fn plugin_data_unique_keys<'a>(
    manifest: &super::super::PackManifest,
    plugin: &super::super::PackPlugin,
    asset: &dyn Fn(&str) -> Result<&'a [u8]>,
) -> Result<(UniqueKeySets, CollectionFields, CollectionSchemas)> {
    let name = &plugin.name;
    let Some(data) = &plugin.data else {
        return Ok((BTreeMap::new(), BTreeMap::new(), BTreeMap::new()));
    };
    let mut types = BTreeMap::new();
    // A data collection one schema file declares alone keeps that file's
    // exact SDL: the trusted schema a client-signed write signs against.
    let mut schemas = BTreeMap::new();
    for path in sdl_paths(manifest) {
        let sdl = std::str::from_utf8(asset(path)?)?;
        let declared = sdl_types(sdl)?;
        if let [collection] = declared.keys().collect::<Vec<_>>().as_slice() {
            if data.collections.contains(collection) {
                schemas.insert((*collection).clone(), sdl.to_owned());
            }
        }
        types.extend(declared);
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
    Ok((keys, fields, schemas))
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
    pub(super) snapshot: Option<super::InstalledDependencySnapshot>,
    variables: BTreeMap<String, Option<String>>,
    reconfigure: bool,
}

pub struct PreparedDocumentPackInstall {
    pub installation_doc_id: Option<String>,
    pub agent_bindings: super::super::PackAgentBindings,
    pub(crate) node_binding: Option<crate::store::config_client::ConfigurationNodeBinding>,
    pub config: PackConfig,
    pub inference: PackInferenceBindingPreview,
    pub dependency_inference: BTreeMap<String, PackInferenceBindingPreview>,
    pub dependencies: Vec<PreparedDependency>,
    pub sources: BTreeMap<String, super::PackSource>,
    pub(super) root_snapshot: Option<super::InstalledDependencySnapshot>,
    pub(super) variables: BTreeMap<String, Option<String>>,
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
                let record = installed_plugin_record(home, namespace, name)?;
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

impl PreparedDocumentPackInstall {
    pub fn preserves_root_values(&self) -> bool {
        self.root_snapshot.is_some()
    }
    pub fn reused_dependency_config(&self, coordinate: &str) -> Option<&PackConfig> {
        self.dependencies
            .iter()
            .find(|dependency| dependency.coordinate == coordinate && dependency.snapshot.is_some())
            .map(|dependency| &dependency.config)
    }
}

/// The record a plugin installation left in the node's local plugin home.
fn installed_plugin_record(
    home: &Path,
    namespace: &str,
    name: &str,
) -> Result<crate::plugin::store::InstalledPlugin> {
    crate::plugin::store::read_record(home, namespace, name)
}

/// The inference profiles an installed dependency already uses, read from its
/// slot-bound Agents and its plugins' recorded model bindings.
pub(super) fn installed_dependency_bindings(
    manifest: &super::super::PackManifest,
    agent_bindings: &super::super::PackAgentBindings,
    config: &PackConfig,
    home: Option<&Path>,
    owner: &str,
) -> Result<PackInferenceBindings> {
    let mut bindings = PackInferenceBindings::new();
    for slot in &manifest.metadata.inference_slots {
        let mut profiles = std::collections::BTreeSet::new();
        for id in &slot.agents {
            let did = &agent_bindings
                .get(id)
                .with_context(|| format!("installed dependency agent slot {id:?} is unbound"))?
                .agent_did;
            let agent = config
                .agents
                .iter()
                .find(|agent| &agent.agent_did == did)
                .with_context(|| format!("installed dependency agent {id:?} is missing"))?;
            profiles.insert(agent.inference_profile_id.clone());
        }
        for plugin in manifest
            .metadata
            .plugins
            .iter()
            .filter(|plugin| plugin.model_slot.as_deref() == Some(slot.name.as_str()))
        {
            let home = home.context("installed dependency plugin requires its local home")?;
            let record = installed_plugin_record(home, &manifest.metadata.namespace, &plugin.name)?;
            let binding = record
                .model_binding
                .context("installed dependency plugin has no model binding")?;
            anyhow::ensure!(
                binding.node_did == owner,
                "installed dependency plugin has a different model owner"
            );
            profiles.insert(binding.profile_id);
        }
        anyhow::ensure!(
            profiles.len() <= 1,
            "installed dependency slot {:?} has inconsistent bindings",
            slot.name
        );
        if let Some(profile) = profiles.into_iter().next() {
            bindings.insert(slot.name.clone(), profile);
        } else {
            anyhow::ensure!(
                slot.optional,
                "installed dependency slot {:?} has no recoverable binding",
                slot.name
            );
        }
    }
    Ok(bindings)
}

pub(super) fn document_ux_install_allowed(has_ux: bool, has_home: bool) -> bool {
    !has_ux || has_home
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
    let home =
        home.context("a dependency with UX must first be installed in the node's local home")?;
    let record = super::super::read_home_install(home, &coordinate)?
        .context("a dependency with UX must first be installed")?;
    anyhow::ensure!(
        record.digest == digest,
        "dependency {coordinate} with UX must first be installed explicitly at the same digest"
    );
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
    overrides: &DependencyVariableOverrides,
) -> Result<PreparedDocumentPackInstall, PackChangeError> {
    // The caller renders `config`; this records the values it used, with an
    // installed root keeping its recorded values for omitted variables.
    let root = async {
        Ok::<_, anyhow::Error>(match root_archive {
            Some(archive) => {
                let root = root_install_variables(access, owner, manifest).await?;
                let path = manifest.config.as_deref().context("pack has no config")?;
                let authored: serde_json::Value = serde_json::from_slice(archive.asset(path)?)?;
                let captured = std::sync::Mutex::new(BTreeMap::new());
                super::super::interpolate::inspect_install_variables(&authored, &|name| {
                    if name == "GENTS_PACK_NODE_DID" {
                        return Some(owner.to_owned());
                    }
                    capture_variable(&captured, name, || root.resolve(name, environment))
                })
                .map_err(|names| anyhow::anyhow!("unset variables: {}", names.join(", ")))?;
                let variables = captured.into_inner().unwrap_or_else(|e| e.into_inner());
                root.with_variables(variables)
            }
            None => RootInstallVariables::default(),
        })
    }
    .await?;
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
        installed_via,
        plugin_home,
        root_archive,
        environment,
        None,
        root,
        overrides,
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
    overrides: &DependencyVariableOverrides,
) -> Result<PreparedDocumentPackInstall, PackChangeError> {
    prepare_document_pack_archive_with_provisioner(
        access,
        owner,
        archive,
        requested,
        dependencies,
        installed_via,
        false,
        plugin_home,
        environment,
        None,
        overrides,
    )
    .await
}

/// Prepare a local operator install with the held Node identity used for slots.
fn require_documents_dependency(dependency: &super::super::PackManifest) -> Result<()> {
    anyhow::ensure!(
        matches!(dependency.metadata.kind, super::super::PackKind::Documents),
        "dependency {}/{} is a {:?} pack, which cannot be installed as a dependency; \
         install it with `gents config pack install` or `gents config pack plugin install`, then remove it from `dependencies`",
        dependency.metadata.namespace, dependency.name, dependency.metadata.kind
    );
    anyhow::ensure!(
        dependency.metadata.dependencies.is_empty(),
        "dependency {}/{} declares dependencies of its own; dependencies are one level: \
         install it with `gents config pack install`, then remove it from `dependencies`",
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
    overrides: &DependencyVariableOverrides,
) -> Result<PreparedDocumentPackInstall, PackChangeError> {
    let node = access
        .local_node()
        .context("local pack preparation requires an embedded Node")?;
    if runtime.did() != owner || node.node_identity_did() != Some(owner) {
        return Err(
            anyhow::anyhow!("held runtime identity does not match the selected Node").into(),
        );
    }
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
        false,
        plugin_home,
        environment,
        Some(&provisioner),
        overrides,
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
    grant_authority: bool,
    plugin_home: Option<&Path>,
    environment: &(dyn Fn(&str) -> Option<String> + Sync),
    provisioner: Option<&super::super::agent_bindings::AgentProvisioner<'_>>,
    overrides: &DependencyVariableOverrides,
) -> Result<PreparedDocumentPackInstall, PackChangeError> {
    prepare_document_pack_archive_inputs(
        access,
        owner,
        archive,
        requested,
        dependencies,
        installed_via,
        grant_authority,
        plugin_home,
        environment,
        provisioner,
        None,
        overrides,
    )
    .await
}

/// Rebuild the installation from locally reviewed archive bytes and staged host facts.
#[expect(
    clippy::too_many_arguments,
    reason = "hosted preparation shares canonical install inputs"
)]
pub async fn prepare_hosted_document_pack_archive(
    access: &ConfigAccess,
    owner: &str,
    archive: &PackArchive,
    requested: &PackInferenceBindings,
    dependencies: &[&PackArchive],
    environment: &(dyn Fn(&str) -> Option<String> + Sync),
    hosted: &HostedPackPreparation,
    overrides: &DependencyVariableOverrides,
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
        false,
        None,
        environment,
        None,
        Some(hosted),
        overrides,
    )
    .await
    .map_err(PackChangeError::into_anyhow)
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
    grant_authority: bool,
    plugin_home: Option<&Path>,
    environment: &(dyn Fn(&str) -> Option<String> + Sync),
    provisioner: Option<&super::super::agent_bindings::AgentProvisioner<'_>>,
    hosted: Option<&HostedPackPreparation>,
    overrides: &DependencyVariableOverrides,
) -> Result<PreparedDocumentPackInstall, PackChangeError> {
    for archive in std::iter::once(archive).chain(dependencies.iter().copied()) {
        refuse_authored_host_roots(archive)?;
    }
    let declared = super::super::agent_bindings::declared_dependencies(archive.manifest());
    validate_dependency_overrides(dependencies, overrides)?;
    let mut preloaded = BTreeMap::new();
    let mut available = BTreeMap::new();
    for dependency in dependencies {
        let manifest = dependency.manifest();
        let coordinate = format!("{}/{}", manifest.metadata.namespace, manifest.name);
        if !declared.contains(&coordinate) {
            return Err(anyhow::anyhow!(
                "prepared dependency {coordinate:?} is not declared by the root pack"
            )
            .into());
        }
        require_documents_dependency(manifest)?;
        let loaded = load_dependency(
            access,
            owner,
            dependency,
            overrides.get(&coordinate),
            environment,
            installed_via,
            super::super::agent_bindings::PackBindingInputs {
                available: &BTreeMap::new(),
                provisioner,
                hosted: hosted.and_then(|facts| facts.agent_bindings.get(&coordinate)),
            },
        )
        .await?;
        available.insert(coordinate.clone(), loaded.bound.agent_bindings.clone());
        if preloaded.insert(coordinate, loaded).is_some() {
            return Err(anyhow::anyhow!("duplicate prepared dependency").into());
        }
    }
    let root = root_install_variables(access, owner, archive.manifest()).await?;
    let captured = std::sync::Mutex::new(BTreeMap::new());
    let loaded = super::super::agent_bindings::load_pack_install_config_with_dependencies(
        access,
        archive.manifest(),
        &PackInstallOptions {
            node_did: owner.to_owned(),
        },
        &|path| archive.asset(path).map(<[u8]>::to_vec),
        &|name| capture_variable(&captured, name, || root.resolve(name, environment)),
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
    let root = root.with_variables(captured.into_inner().unwrap_or_else(|e| e.into_inner()));
    let mut config = loaded.config;
    default_host_tool_roots(
        access,
        owner,
        provisioner.and_then(|context| context.selected_agent_did),
        &mut config,
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
        &config,
        requested,
        dependencies,
        installed_via,
        super::super::installer_provenance(installed_via, grant_authority),
        plugin_home,
        Some(archive),
        environment,
        hosted,
        root,
        overrides,
    )
    .await
}

/// The Tools of `tools` that select host tools and name no root: the ones an
/// install runs under its installer's root ([`installer_tool_root`]), by
/// position.
fn host_tools_without_root(tools: &[crate::documents::Tools]) -> Result<Vec<usize>> {
    let mut rootless = Vec::new();
    for (index, tools) in tools.iter().enumerate() {
        let unset = tools.host.as_ref().is_some_and(|host| {
            host.root
                .as_deref()
                .is_none_or(|root| root.trim().is_empty())
        });
        if unset && ResolvedToolSelection::from_document(tools)?.requires_filesystem_root() {
            rootless.push(index);
        }
    }
    Ok(rootless)
}

/// The Tools documents an install of the authored `tools` would run under
/// its installer's root, by `tools_id`: what an install preview lists and
/// an approval covers before anything is written.
pub fn tools_taking_installer_root(tools: &[crate::documents::Tools]) -> Result<Vec<String>> {
    Ok(host_tools_without_root(tools)?
        .into_iter()
        .map(|index| tools[index].tools_id.clone())
        .collect())
}

/// Refuses an archive whose Tools author a literal `host.root`, as
/// `pack export` refuses to write one: a pack cannot know a folder of the
/// node it is installed on, so a literal names one its author chose, and the
/// agent it ships would ask for the User's reader grant there. A root given
/// only through an install variable is the installer's to set, is listed with
/// the preview's install variables, and is never granted automatically.
fn refuse_authored_host_roots(archive: &PackArchive) -> Result<()> {
    let Some(path) = archive.manifest().config.as_deref() else {
        return Ok(());
    };
    let authored: serde_json::Value = serde_json::from_slice(archive.asset(path)?)?;
    let literal: Vec<String> = authored["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tools| {
            let root = tools["host"]["root"].as_str()?.trim();
            (!root.is_empty() && !root.contains("${")).then(|| {
                format!(
                    "Tools {} (host.root {root})",
                    tools["tools_id"].as_str().unwrap_or("without a tools_id")
                )
            })
        })
        .collect();
    anyhow::ensure!(
        literal.is_empty(),
        "pack {}/{} ships a host tool root, a path on the node it was written on, which a pack cannot carry: {}. Nothing was installed. Its author leaves host.root unset, so the Tools run under the installer's own root",
        archive.manifest().metadata.namespace,
        archive.manifest().name,
        literal.join(", ")
    );
    Ok(())
}

/// The root an install supplies to pack Tools that select host tools without
/// one: the tool root of the installing agent `installer`, or of the node's
/// default agent when the node's User installs. `None` when that agent has no
/// root.
pub async fn installer_tool_root(
    access: &ConfigAccess,
    owner: &str,
    installer: Option<&str>,
) -> Result<Option<String>> {
    access
        .transact_readonly("pack.default_tool_root", |txn| {
            Box::pin(async move {
                let agent = match installer {
                    Some(agent) => Some(agent.to_owned()),
                    None => crate::store::config_client::read_desired_state_record_in_txn(
                        txn,
                        crate::Collection::Node,
                        owner,
                        owner,
                    )
                    .await?
                    .and_then(|(_, node)| {
                        node.get("default_agent_did")
                            .and_then(serde_json::Value::as_str)
                            .map(ToOwned::to_owned)
                    }),
                };
                let Some(agent) = agent else {
                    return Ok::<_, anyhow::Error>(None);
                };
                Ok(
                    crate::documents::load_agent_tools_in_txn(txn, owner, &agent)
                        .await?
                        .and_then(|tools| tools.host)
                        .and_then(|host| host.root)
                        .filter(|root| !root.trim().is_empty()),
                )
            })
        })
        .await
}

/// A pack cannot name a folder on the node it will be installed on, so a pack
/// Tools document that selects host tools without a root takes its
/// installer's ([`installer_tool_root`]). The root is still admitted by the
/// node's WorkspaceRoot policy when the agent runs, and the agent still needs
/// the User's reader grant on the published root that covers it
/// (`tools::surface::agents_tool_root_readers`), which an approved install
/// makes for this root alone. With no such root to take, the document is left
/// as the pack wrote it.
async fn default_host_tool_roots(
    access: &ConfigAccess,
    owner: &str,
    installer: Option<&str>,
    config: &mut PackConfig,
) -> Result<()> {
    let rootless = host_tools_without_root(&config.tools)?;
    if rootless.is_empty() {
        return Ok(());
    }
    if let Some(root) = installer_tool_root(access, owner, installer).await? {
        for index in rootless {
            if let Some(host) = config.tools[index].host.as_mut() {
                host.root = Some(root.clone());
            }
        }
    }
    Ok(())
}

/// The installed root's live configuration, read before an upgrade so the
/// commit can refuse a concurrent edit, and the recorded values omitted
/// variables keep (Lean `Configuration.resolvePackUpgradeVariable`).
#[derive(Default)]
pub(super) struct RootInstallVariables {
    snapshot: Option<super::InstalledDependencySnapshot>,
    variables: BTreeMap<String, Option<String>>,
}

impl RootInstallVariables {
    fn resolve(
        &self,
        name: &str,
        environment: &(dyn Fn(&str) -> Option<String> + Sync),
    ) -> Option<String> {
        match &self.snapshot {
            Some(snapshot) => super::super::interpolate::resolve_upgrade_variable(
                snapshot.record.variables.get(name).cloned().flatten(),
                environment(name),
            ),
            None => environment(name),
        }
    }

    fn with_variables(self, variables: BTreeMap<String, Option<String>>) -> Self {
        Self {
            snapshot: self.snapshot,
            variables,
        }
    }
}

async fn root_install_variables(
    access: &ConfigAccess,
    owner: &str,
    manifest: &super::super::PackManifest,
) -> Result<RootInstallVariables> {
    let coordinate = format!("{}/{}", manifest.metadata.namespace, manifest.name);
    let snapshot = access
        .transact_readonly("pack.root.snapshot", |txn| {
            let coordinate = &coordinate;
            Box::pin(async move {
                super::dependency_snapshot_in_txn(txn, owner, coordinate, None).await
            })
        })
        .await?;
    Ok(RootInstallVariables {
        snapshot,
        variables: BTreeMap::new(),
    })
}

/// A repeated variable resolves once, so one installation cannot observe two values.
fn capture_variable(
    captured: &std::sync::Mutex<BTreeMap<String, Option<String>>>,
    name: &str,
    resolve: impl FnOnce() -> Option<String>,
) -> Option<String> {
    let mut captured = captured.lock().unwrap_or_else(|e| e.into_inner());
    captured
        .entry(name.to_owned())
        .or_insert_with(resolve)
        .clone()
}

fn validate_dependency_overrides(
    dependencies: &[&PackArchive],
    overrides: &DependencyVariableOverrides,
) -> Result<()> {
    for coordinate in overrides.keys() {
        anyhow::ensure!(
            dependencies.iter().any(|archive| format!(
                "{}/{}",
                archive.manifest().metadata.namespace,
                archive.manifest().name
            ) == *coordinate),
            "variable override names undeclared dependency {coordinate:?}"
        );
    }
    for selected in overrides.values() {
        anyhow::ensure!(
            !selected.contains_key("GENTS_PACK_NODE_DID"),
            "GENTS_PACK_NODE_DID is supplied by the runtime and cannot be overridden"
        );
    }
    Ok(())
}

/// A dependency as its dependent installs it: an installed dependency at the
/// same digest keeps its live configuration unless the operator overrides one
/// of its variables, which re-renders it from its recorded values.
pub(super) struct LoadedDependency {
    bound: super::super::BoundPackConfig,
    snapshot: Option<super::InstalledDependencySnapshot>,
    variables: BTreeMap<String, Option<String>>,
    reconfigure: bool,
}

async fn load_dependency(
    access: &ConfigAccess,
    owner: &str,
    archive: &PackArchive,
    selected: Option<&BTreeMap<String, Option<String>>>,
    environment: &(dyn Fn(&str) -> Option<String> + Sync),
    installed_via: InstalledVia,
    bindings: super::super::agent_bindings::PackBindingInputs<'_, '_>,
) -> Result<LoadedDependency> {
    let manifest = archive.manifest();
    let coordinate = format!("{}/{}", manifest.metadata.namespace, manifest.name);
    let (snapshot, record) = access
        .transact_readonly("pack.dependency.snapshot", |txn| {
            let coordinate = &coordinate;
            Box::pin(async move {
                let record = super::read_record(txn, owner, coordinate).await?;
                anyhow::ensure!(record.doc_id.is_none() || record.digest.as_deref() == Some(archive.digest()) || selected.is_some(),
                    "installed dependency {coordinate} has a different artifact; update it explicitly before installing its dependent");
                let snapshot = super::dependency_snapshot_in_txn(txn, owner, coordinate, Some(archive.digest())).await?;
                Ok::<_, anyhow::Error>((snapshot, record))
            })
        })
        .await?;
    if let (Some(snapshot), None) = (&snapshot, selected) {
        if let Some(hosted) = bindings.hosted {
            anyhow::ensure!(
                *hosted == snapshot.record.agent_bindings,
                "hosted agent bindings differ from the installed dependency {coordinate}"
            );
        }
        return Ok(LoadedDependency {
            bound: super::super::BoundPackConfig {
                installation_doc_id: snapshot.record.doc_id.clone(),
                config: snapshot.config(owner)?,
                agent_bindings: snapshot.record.agent_bindings.clone(),
            },
            variables: snapshot.record.variables.clone(),
            snapshot: Some(snapshot.clone()),
            reconfigure: false,
        });
    }
    let installer = bindings
        .provisioner
        .and_then(|context| context.selected_agent_did);
    let environment = super::super::agent_bindings::without_agent_bindings(environment);
    let resolver = super::super::loader::DependencyVariableResolver::new(
        record.doc_id.is_some(),
        &record.variables,
        selected,
        &environment,
    );
    let bound = super::super::agent_bindings::load_pack_install_config_with_dependencies(
        access,
        manifest,
        &PackInstallOptions {
            node_did: owner.to_owned(),
        },
        &|path| archive.asset(path).map(<[u8]>::to_vec),
        &|name| resolver.resolve(name),
        installed_via,
        bindings,
    )
    .await;
    let variables = resolver
        .finish()
        .with_context(|| format!("preparing dependency {coordinate}"))?;
    let mut bound = bound?;
    // A dependency rendered from its archive is installed by this install,
    // so its rootless host tools take the installer's root as the root's do.
    default_host_tool_roots(access, owner, installer, &mut bound.config).await?;
    Ok(LoadedDependency {
        bound,
        reconfigure: selected.is_some() && snapshot.is_some(),
        snapshot,
        variables,
    })
}

pub type DependencyVariableOverrides = BTreeMap<String, BTreeMap<String, Option<String>>>;

#[expect(
    clippy::too_many_arguments,
    reason = "install inputs and policies supplied by each install entry point"
)]
async fn prepare_document_pack_install_inner(
    installation_doc_id: Option<String>,
    agent_bindings: super::super::PackAgentBindings,
    mut preloaded: BTreeMap<String, LoadedDependency>,
    node_binding: Option<&crate::store::config_client::ConfigurationNodeBinding>,
    access: &ConfigAccess,
    owner: &str,
    manifest: &super::super::PackManifest,
    config: &PackConfig,
    requested: &PackInferenceBindings,
    dependencies: &[&PackArchive],
    installed_via: InstalledVia,
    recorded_via: InstalledVia,
    plugin_home: Option<&Path>,
    root_archive: Option<&PackArchive>,
    environment: &(dyn Fn(&str) -> Option<String> + Sync),
    hosted: Option<&HostedPackPreparation>,
    root: RootInstallVariables,
    overrides: &DependencyVariableOverrides,
) -> Result<PreparedDocumentPackInstall, PackChangeError> {
    if let Some(archive) = root_archive {
        if serde_json::to_value(archive.manifest()).map_err(anyhow::Error::from)?
            != serde_json::to_value(manifest).map_err(anyhow::Error::from)?
        {
            return Err(
                anyhow::anyhow!("prepared pack manifest does not match the archive").into(),
            );
        }
    }
    if !document_ux_install_allowed(
        !manifest.metadata.ux.is_empty(),
        plugin_home.is_some() || hosted.is_some(),
    ) {
        return Err(anyhow::anyhow!(
            "document packs with UX need the node's local home, where the desktop reads their pages"
        )
        .into());
    }
    for dependency in dependencies {
        if let Some(hosted) = hosted {
            if !dependency.manifest().metadata.ux.is_empty() {
                let coordinate = format!(
                    "{}/{}",
                    dependency.manifest().metadata.namespace,
                    dependency.manifest().name
                );
                if hosted
                    .operator_ux_digests
                    .get(&coordinate)
                    .map(String::as_str)
                    != Some(dependency.digest())
                {
                    return Err(anyhow::anyhow!("dependency {coordinate} with UX must be operator installed at the reviewed digest").into());
                }
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
    validate_dependency_overrides(dependencies, overrides)?;
    for slot in requested.keys() {
        if !std::iter::once(manifest)
            .chain(dependencies.iter().map(|p| p.manifest()))
            .any(|m| {
                m.metadata
                    .inference_slots
                    .iter()
                    .any(|declared| &declared.name == slot)
            })
        {
            return Err(anyhow::anyhow!(
                "pack {} and its dependencies have no inference slot {slot:?}",
                manifest.name
            )
            .into());
        }
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
        let coordinate = format!(
            "{}/{}",
            archive.manifest().metadata.namespace,
            archive.manifest().name
        );
        let loaded = match preloaded.remove(&coordinate) {
            Some(loaded) => loaded,
            None => {
                load_dependency(
                    access,
                    owner,
                    archive,
                    overrides.get(&coordinate),
                    environment,
                    installed_via,
                    super::super::agent_bindings::PackBindingInputs {
                        available: &BTreeMap::new(),
                        provisioner: None,
                        hosted: None,
                    },
                )
                .await?
            }
        };
        let requested_dependency = selected(archive.manifest());
        let preview = if let Some(snapshot) = &loaded.snapshot {
            let bindings = installed_dependency_bindings(
                archive.manifest(),
                &snapshot.record.agent_bindings,
                &snapshot.config(owner)?,
                plugin_home,
                owner,
            )?;
            for (slot, profile) in &requested_dependency {
                if bindings.get(slot) != Some(profile) {
                    return Err(anyhow::anyhow!("installed dependency {coordinate} already uses a different profile for {slot:?}; update it explicitly before installing its dependent").into());
                }
            }
            let mut preview = super::super::inspect_pack_inference_bindings_with_node_binding(
                access,
                archive.manifest(),
                owner,
                &BTreeMap::new(),
                node_binding,
            )
            .await?;
            preview.bindings = bindings;
            preview
        } else {
            super::super::preview_pack_inference_bindings_with_node_binding(
                access,
                archive.manifest(),
                owner,
                &requested_dependency,
                node_binding,
            )
            .await?
        };
        let prepared = {
            let LoadedDependency {
                bound,
                snapshot,
                variables,
                reconfigure,
            } = loaded;
            let config = if snapshot.is_some() && !reconfigure {
                bound.config
            } else {
                let bound_manifest = super::super::agent_bindings::manifest_with_agent_bindings(
                    archive.manifest(),
                    &bound.agent_bindings,
                );
                super::super::bind_pack_install_config(
                    &bound_manifest,
                    &bound.config,
                    &preview.bindings,
                )?
            };
            let mut schemas = Vec::new();
            let mut assets = BTreeMap::new();
            for path in document_pack_schema_paths(archive.manifest())? {
                let sdl = archive.asset(path)?;
                schemas.push((
                    path.to_owned(),
                    crate::store::config_client::preview_schema_install(
                        access,
                        std::str::from_utf8(sdl).map_err(anyhow::Error::from)?,
                    )
                    .await
                    .map_err(anyhow::Error::from)?,
                ));
                assets.insert(path.to_owned(), sdl.to_vec());
            }
            for plugin in &archive.manifest().metadata.plugins {
                for path in std::iter::once(&plugin.artifact).chain(&plugin.instructions) {
                    assets.insert(path.clone(), archive.asset(path)?.to_vec());
                }
            }
            PreparedDependency {
                installation_doc_id: bound.installation_doc_id,
                agent_bindings: bound.agent_bindings,
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
                snapshot,
                variables,
                reconfigure,
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
        pack_job_block_admissible(
            tools,
            &manifest.metadata,
            &coordinate,
            &shipped,
            recorded_via,
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
                    if previous != digest {
                        return Err(anyhow::anyhow!(
                            "pack graph ships conflicting artifacts for plugin {coordinate}"
                        )
                        .into());
                    }
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
        sources: BTreeMap::new(),
        root_snapshot: root.snapshot,
        variables: root.variables,
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
                    anyhow::bail!("pack {} Tools {:?} names plugin {}; remote GraphQL installation cannot verify plugin files on the node; run `gents config pack install {} --home <node-home>` on that node without --graphql", manifest.name, tools.tools_id, reference.plugin, root);
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
                match installed_plugin_record(home, namespace, name) {
                    Ok(record) => {
                        if let Some(pin) = &reference.digest {
                            anyhow::ensure!(pin == &record.digest,
                                "pack {} Tools {:?} pins plugin {} to {}, but the installed artifact is {}; install the matching plugin or update the pin",
                                manifest.name, tools.tools_id, reference.plugin, pin, record.digest);
                        }
                    }
                    Err(error) if crate::plugin::store::is_not_found(&error) => anyhow::bail!(
                        "pack {} Tools {:?} names plugin {}, which neither the pack nor its dependencies ship and this node has not installed; install the pack that ships {} first (`gents config pack install <namespace>/<pack>`), or declare that pack as a dependency of {}",
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
                "pack {root} ships plugin {plugin}; remote GraphQL installation cannot publish plugin files on the node; run `gents config pack install {root} --home <node-home>` on that node without --graphql"
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
            plugin_home.is_some()
        ),
        "document packs with UX need the node's local home, where the desktop reads their pages"
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
            .filter(|dependency| dependency.snapshot.is_none())
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
    let created_collections = collections_install_creates(access, archive).await?;
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
    .await
    .map_err(|error| error.into_anyhow())?;
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
            HomeInstallContents {
                assets,
                dependency_assets,
                created_collections,
            },
            super::super::installer_provenance(installed_via, grant_authority),
            true,
        )?;
    }
    Ok(report)
}

/// What one install left in the home: its asset root, its dependencies' asset
/// roots, and the collections it created.
#[derive(Default)]
pub(super) struct HomeInstallContents {
    pub assets: String,
    pub dependency_assets: Vec<(String, String)>,
    pub created_collections: Vec<String>,
}

/// Writes the home record of an install through
/// [`record_after_install`](super::super::record_after_install), which decides
/// the provenance.
pub(super) fn record_home_install(
    home: &Path,
    manifest: &super::super::PackManifest,
    identity: &PackIdentity,
    contents: HomeInstallContents,
    installed_via: InstalledVia,
    explicit: bool,
) -> Result<()> {
    let HomeInstallContents {
        assets,
        dependency_assets,
        created_collections,
    } = contents;
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
            source: None,
            source_history: Vec::new(),
            created_collections,
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
) -> Result<(InstallReport, Vec<crate::plugin::store::InstalledPlugin>), PackChangeError> {
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
            source: prepared.sources.get(&dependency.coordinate).cloned(),
            snapshot: dependency.snapshot.clone(),
            variables: Some(dependency.variables.clone()),
            reconfigure: dependency.reconfigure,
            reuse_plugins: dependency.snapshot.is_some(),
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
        source: prepared
            .sources
            .get(&format!(
                "{}/{}",
                archive.manifest().metadata.namespace,
                archive.manifest().name
            ))
            .cloned(),
        snapshot: prepared.root_snapshot.clone(),
        variables: Some(prepared.variables.clone()),
        reconfigure: prepared.root_snapshot.is_some(),
        reuse_plugins: false,
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
    pub(super) source: Option<super::PackSource>,
    pub(super) snapshot: Option<super::InstalledDependencySnapshot>,
    pub(super) variables: Option<BTreeMap<String, Option<String>>>,
    pub(super) reconfigure: bool,
    pub(super) reuse_plugins: bool,
}

impl DocumentPackStage {
    fn record(&self) -> StageRecord {
        StageRecord {
            source: self.source.clone(),
            snapshot: self.snapshot.clone(),
            variables: self.variables.clone(),
            reconfigure: self.reconfigure,
        }
    }
}

/// What a stage's installation record keeps: its source, the installed
/// configuration it was prepared against, and the variable values it records.
/// A reused stage only verifies its snapshot; a reconfigured one also writes.
#[derive(Clone)]
pub(super) struct StageRecord {
    source: Option<super::PackSource>,
    snapshot: Option<super::InstalledDependencySnapshot>,
    variables: Option<BTreeMap<String, Option<String>>>,
    reconfigure: bool,
}

/// The same document graph reconstructed by the host and the client.
#[derive(Clone)]
pub struct HostedDocumentGraph {
    identities: Vec<PackIdentity>,
    documents: Vec<Vec<crate::store::config_client::DesiredStateApplyDocument>>,
    records: Vec<StageRecord>,
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
    let mut records = Vec::new();
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
        let plugins = match &stage.snapshot {
            Some(snapshot) if stage.reuse_plugins => snapshot.record.plugins.clone(),
            _ => plugins,
        };
        let mut identity = PackIdentity::new(&stage.manifest, &stage.digest, plugins);
        identity.installation_doc_id = stage.installation_doc_id.clone();
        identity.agent_bindings = stage.agent_bindings.clone();
        identity.dependencies = stage.dependencies.clone();
        identity.explicit = stage.explicit;
        identities.push(identity);
        documents.push(super::installable_documents(&stage.config)?);
        records.push(stage.record());
    }
    Ok(HostedDocumentGraph {
        identities,
        documents,
        records,
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
            &graph.records,
            policy,
            graph.node_binding.as_ref(),
        )
        .await
        .map_err(PackChangeError::into_anyhow);
    }
    let (root, dependencies) = graph
        .identities
        .split_last()
        .context("hosted plugin root missing")?;
    let (root_documents, dependency_documents) = graph
        .documents
        .split_last()
        .context("hosted plugin graph missing")?;
    let (root_record, dependency_records) = graph
        .records
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
        dependency_records,
        policy,
        graph.node_binding.as_ref(),
    )
    .await
    .map_err(PackChangeError::into_anyhow)?;
    super::validate_plugin_replacements_in_txn(txn, owner, std::slice::from_ref(root)).await?;
    super::write_installed_plugin_identity(
        txn,
        owner,
        root,
        graph.node_binding.as_ref(),
        root_record.source.as_ref(),
    )
    .await?;
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
        .filter(|stage| !stage.reuse_plugins && !stage.manifest.metadata.plugins.is_empty())
        .map(|stage| crate::plugin::install::PluginStageInput {
            manifest: stage.manifest.clone(),
            digest: stage.digest.clone(),
            assets: stage.assets.clone(),
            bindings: stage.bindings.clone(),
        })
        .collect::<Vec<_>>();
    let has_references = stages.iter().any(|stage| {
        (stage.reuse_plugins && !stage.manifest.metadata.plugins.is_empty())
            || stage.config.tools.iter().any(|tools| {
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
    records: &[StageRecord],
    policy: DriftPolicy,
    node_binding: Option<&crate::store::config_client::ConfigurationNodeBinding>,
) -> Result<Vec<InstallReport>, PackChangeError> {
    if records.len() != identities.len() {
        return Err(anyhow::anyhow!("pack stage records do not match the document graph").into());
    }
    let mut reports = Vec::new();
    for ((identity, documents), record) in identities.iter().zip(documents).zip(records) {
        if let Some(snapshot) = &record.snapshot {
            let current = super::dependency_snapshot_in_txn(
                txn,
                owner,
                &identity.coordinate,
                snapshot.record.digest.as_deref(),
            )
            .await?;
            if current.as_ref() != Some(snapshot) {
                return Err(anyhow::anyhow!(
                    "installed dependency {} changed during preparation; retry the install",
                    identity.coordinate
                )
                .into());
            }
            if !record.reconfigure {
                reports.push(InstallReport::default());
                continue;
            }
        }
        reports.push(
            super::install_in_txn(
                txn,
                owner,
                identity,
                documents,
                policy,
                node_binding,
                record.source.as_ref(),
                record.variables.as_ref(),
            )
            .await?,
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
) -> Result<
    (
        Vec<InstallReport>,
        Vec<crate::plugin::store::InstalledPlugin>,
    ),
    PackChangeError,
> {
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
        let reused_plugins = stages
            .iter()
            .filter(|stage| stage.reuse_plugins && !stage.manifest.metadata.plugins.is_empty())
            .filter_map(|stage| {
                let snapshot = stage.snapshot.as_ref()?;
                Some((
                    stage.manifest.clone(),
                    stage.agent_bindings.clone(),
                    stage.digest.clone(),
                    stage.config.clone(),
                    stage.bindings.clone(),
                    snapshot.record.plugins.clone(),
                ))
            })
            .collect::<Vec<_>>();
        let identities = stages
            .iter()
            .map(|stage| {
                let coordinate = format!(
                    "{}/{}",
                    stage.manifest.metadata.namespace, stage.manifest.name
                );
                let recorded_plugins = stage.snapshot.as_ref().filter(|_| stage.reuse_plugins).map(|snapshot| snapshot.record.plugins.clone());
                let mut identity = PackIdentity::new(
                    &stage.manifest,
                    &stage.digest,
                    recorded_plugins.unwrap_or_else(|| plugins
                        .iter()
                        .filter(|plugin| {
                            plugin.owner_pack_coordinate.as_deref() == Some(coordinate.as_str())
                        })
                        .map(|plugin| InstalledPackPlugin {
                            name: plugin.name.clone(),
                            digest: plugin.digest.clone(),
                        })
                        .collect()),
                );
                identity.installation_doc_id = stage.installation_doc_id.clone();
                identity.agent_bindings = stage.agent_bindings.clone();
                identity.dependencies = stage.dependencies.clone();
                identity.explicit = stage.explicit;
                identity
            })
            .collect::<Vec<_>>();
        let records = stages.iter().map(DocumentPackStage::record).collect::<Vec<_>>();
        let result = access
            .transact_write_with_finality("pack.document_graph.install", |txn| {
                let documents = documents.clone();
                let identities = &identities;
                let records = &records;
                let owner = &owner;
                let reference_packs = reference_packs.clone();
                let home = home.clone();
                let plugins = plugins.clone();
                let staged_plugins = plugins.clone();
                let previous_plugins = previous_plugins.clone();
                let node_binding = node_binding.clone();
                let reused_plugins = reused_plugins.clone();
                let node_owner = owner.to_owned();
                Box::pin(async move {
                    txn.stage_plugin_delete_roots(
                        &previous_plugins
                            .iter()
                            .map(|plugin| plugin.projected())
                            .collect::<Vec<_>>(),
                        &staged_plugins
                            .iter()
                            .map(|plugin| plugin.projected())
                            .collect::<Vec<_>>(),
                    )
                    .await?;
                    tokio::task::spawn_blocking(move || {
                        for (manifest, agent_bindings, digest, config, bindings, recorded) in &reused_plugins {
                            let home = home.as_deref().context("installed dependency plugins require the local home")?;
                            anyhow::ensure!(installed_dependency_bindings(manifest, agent_bindings, config, Some(home), &node_owner)? == *bindings,
                                "installed dependency plugin bindings changed during preparation; retry the install");
                            for plugin in &manifest.metadata.plugins {
                                let expected = recorded.iter().find(|entry| entry.name == plugin.name)
                                    .context("installed dependency plugin is absent from its installation record")?;
                                let current = installed_plugin_record(home, &manifest.metadata.namespace, &plugin.name)?;
                                anyhow::ensure!(current.digest == expected.digest && current.owner_pack_digest.as_deref() == Some(digest.as_str()),
                                    "installed dependency plugin changed during preparation; retry the install");
                            }
                        }
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
                    .await
                    .map_err(anyhow::Error::from)??;
                    apply_document_graph_in_txn(
                        txn,
                        owner,
                        identities,
                        documents,
                        records,
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
                    Err(failure) => anyhow::Error::from(failure.error)
                        .context(format!("plugin store cleanup failed: {cleanup:#}")),
                    Ok(_) => cleanup
                        .context("plugin store cleanup failed after committed pack transaction"),
                }
                .into());
            }
        }
        #[cfg(test)]
        if let Some(pause) = &pause {
            pause.completed.notify_one();
        }
        result
            .map(|reports| (reports, plugins))
            .map_err(|failure| match failure.error {
                PackChangeError::Other(error) => {
                    PackChangeError::Other(error.context(if rejected {
                        "pack document transaction was rejected; local plugin records were restored"
                    } else {
                        "pack document outcome was not confirmed; local plugins remain published"
                    }))
                }
                refusal => refusal,
            })
    })
    .await
    .context("owned pack installation task failed")?
}

#[cfg(test)]
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
        source: None,
        snapshot: None,
        variables: None,
        reconfigure: false,
        reuse_plugins: false,
    };
    let (mut reports, plugins) =
        install_document_graph(access, owner, vec![stage], policy, home, consent, None).await?;
    Ok((
        reports.pop().context("pack graph has no root report")?,
        plugins,
    ))
}
