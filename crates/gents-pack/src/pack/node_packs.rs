//! The pack operations an actor performs on its own node: resolving a named
//! or local pack, deciding what an install grants and what blocks it,
//! installing it and removing it. A caller supplies the acting access and
//! renders the answers; the decisions are made here.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::archive::PackArchive;
use super::resolve::{ResolvedFrom, ResolvedNamedPack};
use super::store::PackStore;
use super::{
    authoring, local, plugin_lifecycle, registry, resolve, HomePackInstall, InstallReport,
    InstalledVia, PackInferenceBindingPreview, PackInferenceBindings, PackKind, PackManifest,
    PackSource, PreparedDocumentPackInstall, RemoveReport,
};
use gents_documents::ConfigurationNodeBinding;
use gents_protocol::operations::pack::{PackActor, PackChangeInput};
use gents_store::config_client::{ConfigAccess, SchemaInstallPlan};

pub use super::plugin_lifecycle::InstallError;
pub use super::registry::split_pack_coordinate;
pub use super::wiring::PluginWiring;
pub use super::AgentProvisioner;
pub use super::{
    agent_binding_variable, installed_pack_documents, read_home_install, visible_plugin_selections,
};
pub use super::{DriftPolicy, PluginPinConflict, PluginSelection};

/// The file or directory `change` names. The caller admits it under its own
/// file authority before [`NodePacks::open_path`] reads it.
pub fn local_path(change: &PackChangeInput) -> Option<&Path> {
    match local::classify(change.package.trim()) {
        Some(local::LocalName::Path(path)) => Some(path),
        _ => None,
    }
}

/// `NAMESPACE/NAME` of `package`, whose version a removal does not take.
pub fn coordinate_of(package: &str, removal: bool) -> Result<String> {
    let spec = gents_documents::pack_spec::parse_pack_spec(package.trim())?;
    anyhow::ensure!(
        !removal || spec.version.is_none(),
        "pack remove takes a coordinate without a version; remove {}/{}",
        spec.namespace,
        spec.name
    );
    Ok(format!("{}/{}", spec.namespace, spec.name))
}

/// A resolved pack and the provenance its installation records.
pub struct Distribution {
    resolved: ResolvedNamedPack,
    source: PackSource,
}

impl Distribution {
    /// A pack the resolver found, recorded as coming from where it found it.
    pub fn resolved(resolved: ResolvedNamedPack) -> Self {
        let source = PackSource::of_resolution(&resolved);
        Self { resolved, source }
    }

    /// An archive the home's store holds, recorded as coming from `source`.
    pub fn stored(archive: PackArchive, source: PackSource) -> Self {
        Self {
            resolved: ResolvedNamedPack::stored(archive),
            source,
        }
    }

    pub fn archive(&self) -> &PackArchive {
        &self.resolved.archive
    }

    pub fn manifest(&self) -> &PackManifest {
        self.resolved.archive.manifest()
    }

    pub fn coordinate(&self) -> String {
        let manifest = self.manifest();
        format!("{}/{}", manifest.metadata.namespace, manifest.name)
    }

    /// Whether this is a documents pack; otherwise it installs into the home
    /// as a plugins or assets pack.
    pub fn is_documents(&self) -> bool {
        self.manifest().metadata.kind == PackKind::Documents
    }

    pub fn digest(&self) -> &str {
        self.resolved.archive.digest()
    }

    pub fn source(&self) -> &'static str {
        self.source.kind()
    }

    /// Pin the preview's resolution so applying it resolves the same pack
    /// again rather than whatever is newest by then.
    pub fn pinned_spec(&self, requested: &str) -> String {
        if local::classify(requested).is_some() {
            return self.digest().to_owned();
        }
        let manifest = self.manifest();
        if gents_documents::pack_spec::is_valid_index_version(&manifest.version) {
            format!(
                "{}/{}@{}",
                manifest.metadata.namespace, manifest.name, manifest.version
            )
        } else {
            requested.to_owned()
        }
    }

    pub fn registry_artifact_digest(&self) -> Option<&str> {
        match &self.resolved.from {
            ResolvedFrom::Registry {
                artifact_digest, ..
            } => Some(artifact_digest),
            _ => None,
        }
    }
}

/// Why a pack named by digest or path was not opened.
#[derive(Debug, thiserror::Error)]
pub enum LocalPackError {
    #[error("{0} is not in this node's pack store")]
    NotStored(String),
    /// A directory whose plugins have source installs as the pack a build
    /// wrote; `built` is that pack when it exists.
    #[error("the pack directory has plugin source ({}), which a directory install does not compile", .sources.join(", "))]
    PluginSource {
        sources: Vec<String>,
        built: Option<PathBuf>,
    },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// What a documents install or update will do, before anything is prepared.
pub struct DocumentsPlan {
    pub prior: Option<Value>,
    pub dependencies: Vec<Distribution>,
    pub blockers: Vec<Value>,
    pub missing_slots: BTreeSet<String>,
    pub inspected: Vec<PackInferenceBindingPreview>,
}

/// What a plugins or assets install or update will do.
pub struct PluginsPlan {
    pub coordinate: String,
    pub installed: Option<HomePackInstall>,
    pub inference: PackInferenceBindingPreview,
    pub missing_slots: Vec<String>,
    pub blockers: Vec<Value>,
    pub authority: Vec<Value>,
}

/// One page of the packs this node has installed or stored.
pub struct PackListing {
    pub total: usize,
    pub truncated: bool,
    pub next_cursor: Option<String>,
    pub items: Vec<Value>,
}

/// Why an assets or plugins pack was not removed.
#[derive(Debug, thiserror::Error)]
pub enum HomePackRemovalError {
    /// The pack's plugins were not retired: Tools the actor can read still
    /// select them. `selections` are those Tools, or `None` when reading
    /// them failed.
    #[error("{cause:#}")]
    PluginsNotRetired {
        plugins: BTreeSet<String>,
        selections: Option<Vec<PluginSelection>>,
        cause: anyhow::Error,
    },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// An actor's pack operations on the node `node_did`, through its `access`.
/// The node is always the install owner: no caller selects another DID,
/// registry endpoint or control-plane endpoint.
pub struct NodePacks<'a> {
    pub access: &'a ConfigAccess,
    pub node_did: &'a str,
    /// `None` resolves from the registry in memory and installs no plugins.
    pub home: Option<&'a Path>,
    /// Whether this actor's installs carry the operator's consent to what a
    /// pack's plugins ask for.
    pub grants_authority: bool,
}

impl NodePacks<'_> {
    /// The pack a coordinate or store digest names: the store first, then the
    /// registry. An update without a version takes the registry's newest.
    pub async fn resolve_named(
        &self,
        operation: &str,
        change: &PackChangeInput,
    ) -> Result<Distribution, LocalPackError> {
        crate::operations::pack::admit_input(change).map_err(anyhow::Error::new)?;
        let coordinate = change.package.trim();
        match local::classify(coordinate) {
            Some(local::LocalName::Digest(digest)) => {
                let home = self
                    .home
                    .context("local packs require a runtime plugin home")?;
                if !PackStore::new(home).contains(digest)? {
                    return Err(LocalPackError::NotStored(digest.to_owned()));
                }
                let local = local::LocalName::Digest(digest);
                let source = PackSource::of_local_name(&local);
                return Ok(Distribution::stored(local::open(&local, home)?, source));
            }
            Some(local::LocalName::Path(_)) => {
                return Err(anyhow::anyhow!(
                    "{coordinate} names a path, which is admitted before it is opened"
                )
                .into());
            }
            None => {}
        }
        let registry_url = registry::resolve_registry_url(None);
        let spec = match gents_documents::pack_spec::parse_pack_spec(coordinate)? {
            parsed if operation == "update" && parsed.version.is_none() => {
                let client = registry::RegistryClient::new(registry_url.clone());
                let latest = registry::resolve_pack_coordinate(
                    &client,
                    parsed.namespace,
                    parsed.name,
                    None,
                )
                .await
                .with_context(|| {
                    format!(
                        "the newest version of {coordinate} could not be looked up on the registry at {registry_url}"
                    )
                })?;
                format!("{}/{}@{}", parsed.namespace, parsed.name, latest.version)
            }
            _ => coordinate.to_owned(),
        };
        let installed =
            super::installed_packs(self.home, Some((self.access, self.node_did))).await?;
        let resolved = resolve::resolve_named(
            &spec,
            &resolve::ResolveOptions {
                home: self.home,
                registry_url,
                installed: &installed,
            },
        )
        .await?;
        Ok(Distribution::resolved(resolved))
    }

    /// The pack at `admitted`, the path the caller's file authority resolved
    /// `requested` to. A directory install packs what is on disk, and a
    /// present artifact may predate its source, so plugin source installs as
    /// the pack a build wrote. The no-follow import carries the handle the
    /// admission validated, so a swap after admission cannot change which
    /// bytes are verified and stored.
    pub fn open_path(
        &self,
        requested: &Path,
        admitted: &Path,
    ) -> Result<Distribution, LocalPackError> {
        let home = self
            .home
            .context("local packs require a runtime plugin home")?;
        if admitted.is_dir() {
            let manifest = authoring::read_manifest(admitted)?;
            let sources = authoring::plugins_with_source(&manifest);
            if !sources.is_empty() {
                let built = authoring::built_pack_path(admitted, &manifest);
                return Err(LocalPackError::PluginSource {
                    sources: sources.into_iter().map(Into::into).collect(),
                    built: built.is_file().then_some(built),
                });
            }
        } else if !(admitted.is_file()
            && admitted
                .extension()
                .is_some_and(|extension| extension == super::archive::EXTENSION))
        {
            return Err(anyhow::anyhow!(
                "{} is neither a pack directory nor a built .pack file",
                requested.display()
            )
            .into());
        }
        let source = PackSource::of_local_name(&local::LocalName::Path(requested));
        Ok(Distribution::stored(
            local::open(&local::LocalName::Path(admitted), home)?,
            source,
        ))
    }

    /// The installation record of `manifest`'s pack, when it is installed.
    pub async fn status(&self, manifest: &PackManifest) -> Result<Option<Value>> {
        let coordinate = format!("{}/{}", manifest.metadata.namespace, manifest.name);
        if manifest.metadata.kind != PackKind::Documents {
            return self
                .home
                .map(|home| super::read_home_install(home, &coordinate))
                .transpose()?
                .flatten()
                .map(serde_json::to_value)
                .transpose()
                .map_err(Into::into);
        }
        super::read_installed_pack(self.access, self.node_did, &coordinate)
            .await?
            .map(serde_json::to_value)
            .transpose()
            .map_err(Into::into)
    }

    /// The installation record `coordinate` would be removed through: the
    /// home's for an assets or plugins pack, the node's otherwise.
    pub async fn installed(&self, coordinate: &str) -> Result<Option<Value>> {
        if let Some(record) = self.home_pack(coordinate)? {
            return Ok(Some(serde_json::to_value(record)?));
        }
        super::read_installed_pack(self.access, self.node_did, coordinate)
            .await?
            .map(serde_json::to_value)
            .transpose()
            .map_err(Into::into)
    }

    /// The home record of `coordinate` when it is an assets or plugins pack.
    pub fn home_pack(&self, coordinate: &str) -> Result<Option<HomePackInstall>> {
        Ok(match self.home {
            Some(home) => super::read_home_install(home, coordinate)?
                .filter(|record| record.kind != PackKind::Documents),
            None => None,
        })
    }

    /// The pack's slots and the owner's profiles, checking only `requested`.
    pub async fn inference(
        &self,
        manifest: &PackManifest,
        requested: &PackInferenceBindings,
        node_binding: Option<&ConfigurationNodeBinding>,
    ) -> Result<PackInferenceBindingPreview> {
        super::inspect_pack_inference_bindings_with_node_binding(
            self.access,
            manifest,
            self.node_did,
            requested,
            node_binding,
        )
        .await
    }

    /// What installing `manifests` grants each plugin that asks for anything:
    /// a successful install grants exactly what the plugin declares.
    pub fn authority_asked<'m>(
        &self,
        manifests: impl IntoIterator<Item = &'m PackManifest>,
    ) -> Result<Vec<Value>> {
        let mut asked = Vec::new();
        for manifest in manifests {
            asked.extend(plugin_lifecycle::authority_asked(manifest)?);
        }
        Ok(asked)
    }

    /// Why `manifest`'s plugins cannot be installed with this actor's consent.
    pub fn install_blockers(&self, manifest: &PackManifest) -> Vec<Value> {
        plugin_lifecycle::install_blockers(self.home, manifest, self.grants_authority)
    }

    /// The documents of `coordinate` edited since it was installed, which an
    /// install, update or removal meets.
    pub async fn edited_documents(&self, coordinate: &str, removal: bool) -> Result<Vec<String>> {
        super::edited_installed_documents(self.access, self.node_did, coordinate, removal).await
    }

    /// The selections whose pins refuse the plugins `distribution` publishes.
    pub async fn pin_conflicts(
        &self,
        distribution: &Distribution,
    ) -> Result<Vec<PluginPinConflict>> {
        let replacements = plugin_lifecycle::published_digests(distribution.archive())?;
        super::plugin_pin_conflicts(self.access, self.node_did, &replacements).await
    }

    /// Resolves a documents pack's dependencies and inspects every slot the
    /// pack and they declare against `change`'s bindings.
    pub async fn plan_documents(
        &self,
        operation: &str,
        change: &PackChangeInput,
        distribution: &Distribution,
        node_binding: Option<&ConfigurationNodeBinding>,
    ) -> Result<DocumentsPlan> {
        let prior = self.status(distribution.manifest()).await?;
        anyhow::ensure!(
            operation != "update" || prior.is_some(),
            "pack {:?} is not installed; use pack install",
            distribution.manifest().name
        );
        let mut dependencies = Vec::new();
        for coordinate in &distribution.manifest().metadata.dependencies {
            dependencies.push(
                self.resolve_named(
                    "get",
                    &PackChangeInput {
                        variables: change.variables.clone(),
                        ..PackChangeInput::of(coordinate)
                    },
                )
                .await
                .map_err(anyhow::Error::from)?,
            );
        }
        let mut blockers = self.install_blockers(distribution.manifest());
        for dependency in &dependencies {
            blockers.extend(self.install_blockers(dependency.manifest()));
        }
        let manifests = || {
            std::iter::once(distribution.manifest())
                .chain(dependencies.iter().map(Distribution::manifest))
        };
        for slot in change.inference_slots.keys() {
            anyhow::ensure!(
                manifests().any(|manifest| manifest
                    .metadata
                    .inference_slots
                    .iter()
                    .any(|declared| &declared.name == slot)),
                "pack and its dependencies have no inference slot {slot:?}"
            );
        }
        let mut missing_slots = BTreeSet::new();
        let mut inspected = Vec::new();
        for manifest in manifests() {
            let requested = change
                .inference_slots
                .iter()
                .filter(|(slot, _)| {
                    manifest
                        .metadata
                        .inference_slots
                        .iter()
                        .any(|declared| &declared.name == *slot)
                })
                .map(|(slot, profile)| (slot.clone(), profile.clone()))
                .collect();
            let preview = self.inference(manifest, &requested, node_binding).await?;
            for slot in &preview.slots {
                if !slot.optional && !change.inference_slots.contains_key(&slot.name) {
                    missing_slots.insert(slot.name.clone());
                }
            }
            inspected.push(preview);
        }
        Ok(DocumentsPlan {
            prior,
            dependencies,
            blockers,
            missing_slots,
            inspected,
        })
    }

    /// Binds and materializes a documents pack and its dependencies for this
    /// node, recording where each archive came from. Nothing is written.
    pub async fn prepare_documents(
        &self,
        change: &PackChangeInput,
        distribution: &Distribution,
        dependencies: &[Distribution],
        provisioner: Option<&AgentProvisioner<'_>>,
    ) -> Result<PreparedDocumentPackInstall> {
        let environment = |name: &str| {
            crate::operations::pack::install_value(PackActor::Agent, &change.variables, name)
        };
        let mut prepared = super::prepare_document_pack_archive_with_provisioner(
            self.access,
            self.node_did,
            distribution.archive(),
            &change.inference_slots,
            &dependencies
                .iter()
                .map(Distribution::archive)
                .collect::<Vec<_>>(),
            InstalledVia::SelfConfig,
            self.grants_authority,
            self.home,
            &environment,
            provisioner,
            &crate::operations::pack::dependency_overrides(&change.variables),
        )
        .await?;
        for source in std::iter::once(distribution).chain(dependencies) {
            prepared
                .sources
                .insert(source.coordinate(), source.source.clone());
        }
        Ok(prepared)
    }

    /// What installing `distribution`'s SDL does to this node's collections.
    pub async fn schema_previews(
        &self,
        distribution: &Distribution,
    ) -> Result<Vec<SchemaInstallPlan>> {
        let mut schemas = Vec::new();
        for path in super::document_pack_schema_paths(distribution.manifest())? {
            let sdl = std::str::from_utf8(distribution.archive().asset(path)?)?;
            schemas
                .push(gents_store::config_client::preview_schema_install(self.access, sdl).await?);
        }
        Ok(schemas)
    }

    /// The logical IDs `prepared` materializes, by collection.
    pub fn materialized_ids(
        prepared: &PreparedDocumentPackInstall,
    ) -> Result<BTreeMap<String, Vec<String>>> {
        let mut materialized = BTreeMap::<String, Vec<String>>::new();
        for (collection, id) in super::pack_document_digests(&prepared.config)?.into_keys() {
            materialized.entry(collection).or_default().push(id);
        }
        Ok(materialized)
    }

    /// Installs a prepared documents pack and returns its receipt and the
    /// installation record the node now holds for it.
    pub async fn install_documents(
        &self,
        distribution: &Distribution,
        dependencies: &[Distribution],
        prepared: &PreparedDocumentPackInstall,
        edited: DriftPolicy,
        profile_policy: &'static dyn gents_documents::inference_backend::AdvertisedProfilePolicy,
    ) -> Result<(InstallReport, Value)> {
        let receipt = super::install_prepared_document_pack(
            self.access,
            self.node_did,
            distribution.archive(),
            &dependencies
                .iter()
                .map(Distribution::archive)
                .collect::<Vec<_>>(),
            prepared,
            self.home,
            self.grants_authority,
            InstalledVia::SelfConfig,
            edited,
            profile_policy,
        )
        .await?;
        let effective = self
            .status(distribution.manifest())
            .await?
            .context("installed document pack is not discoverable")?;
        anyhow::ensure!(
            effective["digest"].as_str() == Some(distribution.digest()),
            "installed document pack digest does not match requested artifact"
        );
        Ok((receipt, effective))
    }

    /// What installing or updating a plugins or assets pack will do, and
    /// what blocks it.
    pub async fn plan_plugins(
        &self,
        operation: &str,
        change: &PackChangeInput,
        distribution: &Distribution,
        node_binding: Option<&ConfigurationNodeBinding>,
    ) -> Result<PluginsPlan> {
        let home = self
            .home
            .context("plugins and assets packs require a runtime plugin home")?;
        let manifest = distribution.manifest();
        let coordinate = distribution.coordinate();
        let installed = super::read_home_install(home, &coordinate)?;
        anyhow::ensure!(
            operation != "update" || installed.is_some(),
            "pack {coordinate:?} is not installed; use pack preview install"
        );
        anyhow::ensure!(
            change.variables.is_empty(),
            "plugins and assets packs do not consume document variables"
        );
        let inference = self
            .inference(manifest, &change.inference_slots, node_binding)
            .await?;
        let missing_slots = inference
            .slots
            .iter()
            .filter(|slot| !slot.optional && !change.inference_slots.contains_key(&slot.name))
            .map(|slot| slot.name.clone())
            .collect();
        let mut blockers = self.install_blockers(manifest);
        if let Some(previous) = &installed {
            anyhow::ensure!(
                previous.kind == manifest.metadata.kind,
                "the existing home install of {coordinate} is a {:?} pack, not {:?}",
                previous.kind,
                manifest.metadata.kind
            );
            if previous.plugins.iter().any(|old| {
                !manifest
                    .metadata
                    .plugins
                    .iter()
                    .any(|plugin| plugin.name == old.name)
            }) {
                blockers.push(json!({"reason":"This update drops plugin names; native removal of obsolete plugins is unavailable. Keep the current install and use the owning removal workflow."}));
            }
        }
        blockers.extend(plugin_lifecycle::admission_blockers(
            home,
            distribution.archive(),
        )?);
        Ok(PluginsPlan {
            coordinate,
            installed,
            inference,
            missing_slots,
            blockers,
            authority: self.authority_asked([manifest])?,
        })
    }

    /// The bindings a plugins or assets install applies: every required slot
    /// bound, or refused.
    pub async fn bound_inference(
        &self,
        manifest: &PackManifest,
        requested: &PackInferenceBindings,
        node_binding: Option<&ConfigurationNodeBinding>,
    ) -> Result<PackInferenceBindingPreview> {
        super::preview_pack_inference_bindings_with_node_binding(
            self.access,
            manifest,
            self.node_did,
            requested,
            node_binding,
        )
        .await
        .map_err(super::SlotBindingError::into_anyhow)
    }

    /// Installs a plugins or assets pack into the home and tells the node as
    /// `actor`. Local plugin records are the publication boundary; later
    /// failures do not roll them back.
    pub async fn install_plugins(
        &self,
        distribution: &Distribution,
        bindings: &PackInferenceBindings,
        actor: Option<::identity::Did>,
        node_binding: Option<ConfigurationNodeBinding>,
        profile_policy: &'static dyn gents_documents::inference_backend::AdvertisedProfilePolicy,
    ) -> Result<plugin_lifecycle::InstalledHomePack, InstallError> {
        let home = self
            .home
            .context("plugins and assets packs require a runtime plugin home")?;
        let notify_access = self.access.clone();
        let notify_node = self.node_did.to_owned();
        let notify_home = home.to_owned();
        let (record, plugins) = plugin_lifecycle::install_final(
            home,
            distribution.archive(),
            self.grants_authority,
            Some(self.node_did),
            bindings,
            super::installer_provenance(InstalledVia::SelfConfig, self.grants_authority),
            distribution.source.clone(),
            move |identity, changes| async move {
                super::record_staged_plugin_store_change_as_with_node_binding(
                    &notify_access,
                    &notify_node,
                    Some(actor.ok_or_else(|| {
                        super::PluginStoreChangeError::Rejected(anyhow::anyhow!(
                            "node DID is not ACP-addressable"
                        ))
                    })?),
                    &notify_home,
                    &identity.coordinate,
                    Some(&identity),
                    super::PluginStageContext {
                        node_binding: node_binding.as_ref(),
                        changes: Some(&changes),
                        retired: false,
                    },
                    profile_policy,
                )
                .await
            },
        )
        .await?;
        Ok(plugin_lifecycle::InstalledHomePack { record, plugins })
    }

    /// One page of the packs installed on this node or held by its store.
    pub async fn list(&self, limit: usize, cursor: Option<&str>) -> Result<PackListing> {
        anyhow::ensure!(
            (1..=50).contains(&limit),
            "--limit must be between 1 and 50"
        );
        let store = self.home.map(PackStore::new);
        let mut names: BTreeMap<_, _> = match &store {
            Some(store) => store.names()?.into_iter().collect(),
            None => BTreeMap::new(),
        };
        let installed = super::installed_catalog(self.access, self.node_did, self.home).await?;
        for coordinate in installed.keys() {
            names.entry(coordinate.clone()).or_default();
        }
        let total = names.len();
        let mut selected = names
            .into_iter()
            .filter(|(coordinate, _)| cursor.is_none_or(|cursor| coordinate.as_str() > cursor))
            .take(limit + 1)
            .collect::<Vec<_>>();
        let truncated = selected.len() > limit;
        selected.truncate(limit);
        let next_cursor = truncated
            .then(|| selected.last().map(|(coordinate, _)| coordinate.clone()))
            .flatten();
        let mut items = Vec::with_capacity(selected.len());
        for (coordinate, versions) in selected {
            let current = installed.get(&coordinate);
            let listing = super::store::list_pack_versions(
                &versions,
                current.map(|entry| entry.stored.clone()),
            );
            let preferred = versions.first();
            let display = listing.installed.as_ref().or(preferred);
            let archive = match (&store, display) {
                (Some(store), Some(entry)) => store.open(&entry.digest).map(Some),
                _ => Ok(None),
            };
            let (manifest, error) = match &archive {
                Ok(Some(archive)) => (Some(archive.manifest()), None),
                Ok(None) => (None, None),
                Err(error) => (
                    None,
                    Some(format!("the stored archive could not be opened: {error:#}")),
                ),
            };
            let kind = current
                .map(|entry| &entry.kind)
                .or_else(|| manifest.map(|manifest| &manifest.metadata.kind));
            let installable = manifest.is_some();
            items.push(json!({
                "name": coordinate,
                "version": listing.installed.as_ref().map(|entry| &entry.version),
                "versions": versions.iter().map(|stored| stored.version.as_str()).collect::<Vec<_>>(),
                "available_version": preferred.map(|entry| &entry.version),
                "other_stored_versions": listing.other_stored.iter().map(|entry| json!({"version": entry.version, "digest": entry.digest})).collect::<Vec<_>>(),
                "description": manifest.map(|manifest| &manifest.description),
                "kind": kind,
                "artifact_digest": display.map(|entry| &entry.digest),
                "installed_digest": listing.installed.as_ref().map(|entry| &entry.digest),
                "inference_slots": manifest.map(|manifest| &manifest.metadata.inference_slots),
                "installable": installable,
                "installed": current.map(|entry| &entry.record),
                "error": error,
            }));
        }
        Ok(PackListing {
            total,
            truncated,
            next_cursor,
            items,
        })
    }

    /// One page of the registry's answer to `query`, and the registry asked.
    pub async fn search(query: &str, page: u32) -> Result<(String, Value)> {
        let registry = registry::resolve_registry_url(None);
        let response = registry::RegistryClient::new(registry.clone())
            .search(query, page)
            .await?;
        Ok((registry, response))
    }

    /// Removes an assets or plugins pack under the plugin store lock: its
    /// plugins retire unless a Tools the actor can read still selects one
    /// ([`super::ensure_plugins_unreferenced`]), then the home record, the
    /// plugin records it owns, its asset cache and the plugin bytes and
    /// archive nothing else references are released
    /// ([`super::home_removal::remove_checked`]). The node is told as `actor`
    /// that the plugins retired before their bytes are released: a retirement
    /// it did not record is an error, and the bytes it still lists stay.
    pub async fn remove_home_pack(
        &self,
        coordinate: &str,
        record: HomePackInstall,
        actor: Option<::identity::Did>,
        profile_policy: &'static dyn gents_documents::inference_backend::AdvertisedProfilePolicy,
    ) -> Result<super::home_removal::HomeRemoval, HomePackRemovalError> {
        let home = self
            .home
            .context("assets and plugins packs live in the node home")?;
        let lock_home = home.to_owned();
        let guard =
            tokio::task::spawn_blocking(move || crate::plugin::store::try_lock_store(&lock_home))
                .await
                .context("plugin store lock task did not finish")??;
        let (namespace, _) = registry::split_pack_coordinate(coordinate);
        let plugins: BTreeSet<String> = record
            .plugins
            .iter()
            .map(|plugin| format!("{namespace}/{}", plugin.name))
            .collect();
        if let Err(cause) =
            super::ensure_plugins_unreferenced(self.access, self.node_did, &plugins).await
        {
            drop(guard);
            let selections = super::visible_plugin_selections(self.access, self.node_did, &plugins)
                .await
                .ok();
            return Err(HomePackRemovalError::PluginsNotRetired {
                plugins,
                selections,
                cause,
            });
        }
        let mut removal = super::home_removal::remove_checked_keeping_bytes(
            guard,
            home,
            coordinate,
            record,
            Some(self.access),
        )
        .await?;
        if removal.plugins.is_empty() {
            return Ok(removal);
        }
        // The node drops the plugins from its record before their bytes go:
        // a retirement that is not recorded leaves the bytes the node still
        // lists. The home record is released by then, so a repeated removal
        // finds no pack here; the bytes are content-addressed, and installing
        // the pack again takes them back for its own removal to release.
        super::record_plugin_retirement_as(
            self.access,
            self.node_did,
            actor,
            home,
            coordinate,
            profile_policy,
        )
        .await
        .map_err(anyhow::Error::from)?;
        let bytes_home = home.to_owned();
        let removal = tokio::task::spawn_blocking(move || {
            super::home_removal::release_removed_plugin_bytes(&bytes_home, &mut removal)
                .map(|()| removal)
        })
        .await
        .context("plugin byte release task did not finish")??;
        Ok(removal)
    }

    /// Removes an installed documents pack's `PackInstallation` record and
    /// everything it lists, then releases what it held in the node home
    /// ([`super::home_removal::release_documents_pack`]). Nothing is
    /// resolved: no store or registry lookup.
    pub async fn remove_documents(
        &self,
        coordinate: &str,
        edited: DriftPolicy,
        profile_policy: &'static dyn gents_documents::inference_backend::AdvertisedProfilePolicy,
    ) -> Result<(RemoveReport, super::home_removal::DocumentsRelease)> {
        let report = super::remove_pack(
            self.access,
            self.node_did,
            coordinate,
            edited,
            profile_policy,
        )
        .await?;
        let released = match self.home {
            Some(home) => {
                super::home_removal::release_documents_pack(home, Some(self.access), &report)
                    .await?
            }
            None => Default::default(),
        };
        Ok((report, released))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_removal_names_a_coordinate_and_a_path_is_admitted_before_it_opens() {
        assert_eq!(coordinate_of(" acme/demo ", true).unwrap(), "acme/demo");
        assert_eq!(
            coordinate_of("acme/demo@1.2.0", false).unwrap(),
            "acme/demo"
        );
        let versioned = coordinate_of("acme/demo@1.2.0", true).unwrap_err();
        assert!(format!("{versioned:#}").contains("remove acme/demo"));

        assert_eq!(
            local_path(&PackChangeInput::of("./demo")),
            Some(Path::new("./demo"))
        );
        assert_eq!(local_path(&PackChangeInput::of("acme/demo")), None);
        assert_eq!(local_path(&PackChangeInput::of("sha256:00")), None);
    }
}
