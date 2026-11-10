use super::*;
use crate::pack;

impl PackInstaller {
    pub(super) async fn list(&self, limit: usize, cursor: Option<&str>) -> anyhow::Result<String> {
        anyhow::ensure!(
            (1..=50).contains(&limit),
            "--limit must be between 1 and 50"
        );
        let store = self.home.as_deref().map(pack::store::PackStore::new);
        let mut names: BTreeMap<_, _> = match &store {
            Some(store) => store.names()?.into_iter().collect(),
            None => BTreeMap::new(),
        };
        let access = self.core.access()?;
        let installed =
            pack::installed_catalog(&access, self.core.node_did(), self.home.as_deref()).await?;
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
            let listing = pack::store::list_pack_versions(
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
        Ok(serde_json::to_string_pretty(&json!({
            "source": "store_and_installations",
            "page": {
                "limit": limit,
                "total": total,
                "returned": items.len(),
                "truncated": truncated,
                "next_cursor": next_cursor,
            },
            "items": items,
            "registry_lookup": "Use pack search to discover registry packages, then pack get NAMESPACE/NAME to inspect one. List shows installed packs and artifacts this home's store already holds.",
        }))?)
    }

    pub(super) async fn search(&self, query: &str, page: u32) -> anyhow::Result<String> {
        let registry = pack::registry::resolve_registry_url(None);
        let response = pack::registry::RegistryClient::new(registry.clone())
            .search(query, page)
            .await?;
        let packs = response["packs"]
            .as_array()
            .context("registry search returned no packs array")?;
        let items = packs
            .iter()
            .map(|pack| {
                let mut item = pack.clone();
                if let (Some(namespace), Some(name)) =
                    (pack["namespace"].as_str(), pack["name"].as_str())
                {
                    item["inspect"] = json!({"argv":["pack","get",format!("{namespace}/{name}")]});
                }
                item
            })
            .collect::<Vec<_>>();
        let has_more = response["has_more"].as_bool().unwrap_or(false);
        ordered! {
            "items": items,
            "next_call": if has_more { page.checked_add(1).map(|next| json!({"argv":["pack","search"],"options":{"query":query,"page":next}})) } else { None },
            "guidance": "Inspect a candidate with pack get before installation: it reports its kind, inference slots and installed state. This installer installs document, plugins and assets packs.",
            "registry": registry,
            "page": page,
            "has_more": has_more,
        }.pretty()
    }

    pub(super) async fn get(&self, package: &str) -> anyhow::Result<String> {
        let args = PackInstallParams {
            package: package.to_owned(),
            variables: BTreeMap::new(),
            inference_slots: BTreeMap::new(),
            expected_digest: None,
        };
        let distribution = self.resolve("get", &args).await?;
        let access = self.core.access()?;
        let node_binding = self.core.refresh_node_binding().await?;
        let inference = pack::inspect_pack_inference_bindings_with_node_binding(
            &access,
            distribution.manifest(),
            self.core.node_did(),
            &BTreeMap::new(),
            node_binding.as_ref(),
        )
        .await?;
        let installed = self.installation_status(distribution.manifest()).await?;
        Ok(serde_json::to_string_pretty(&json!({
            "source": distribution.source(),
            "manifest": distribution.manifest(),
            "artifact_digest": distribution.digest(),
            "registry_artifact_digest": distribution.registry_artifact_digest(),
            "inference": inference,
            "installed": installed,
            "installable": true,
            "installation_blockers": self.plugin_install_blockers(distribution.manifest()),
            "supported_operations": ["preview install", "install", "preview update", "update", "remove"],
        }))?)
    }

    /// Removes an assets or plugins pack: its home record and the plugin
    /// records it owns, released together by the local removal owner, then
    /// the node is told. Retiring plugins needs a complete reference inventory
    /// (`ensure_plugins_unreferenced`), so a pack that ships plugins is refused
    /// where that inventory is unavailable and stays installed.
    async fn remove_home_pack(
        &self,
        access: &crate::store::config_client::ConfigAccess,
        home: &std::path::Path,
        coordinate: String,
        record: pack::HomePackInstall,
    ) -> anyhow::Result<String> {
        let (namespace, _) = pack::registry::split_pack_coordinate(&coordinate);
        let plugins = record
            .plugins
            .iter()
            .map(|plugin| format!("{namespace}/{}", plugin.name))
            .collect();
        pack::ensure_plugins_unreferenced(access, self.core.node_did(), &plugins).await?;
        let local_home = home.to_owned();
        let local_coordinate = coordinate.clone();
        let removal = tokio::task::spawn_blocking(move || {
            pack::plugin_lifecycle::remove_local(
                &local_home,
                &local_coordinate,
                |current| {
                    anyhow::ensure!(
                        *current == record,
                        "{local_coordinate} changed while checking plugin references; retry removal"
                    );
                    Ok(())
                },
                |()| Ok(()),
            )
        })
        .await
        .context("pack removal task did not finish")??;
        if !removal.plugins.is_empty() {
            pack::record_plugin_store_change_as(
                access,
                self.core.node_did(),
                self.core.identity().ok(),
                home,
                &coordinate,
                None,
            )
            .await?;
        }
        ordered! {
            "pack": coordinate,
            "removed": json!({"kind": removal.record.kind, "plugins": removal.plugins, "pages": removal.record.ux.iter().map(|page| page.name.clone()).collect::<Vec<_>>()}),
            "effect": "The pack's install record and plugin records were released. Its cached assets, plugin bytes and archive stay until the operator runs `gents pack prune`.",
        }
        .pretty()
    }

    /// Removes an installed pack's `PackInstallation` record and everything
    /// it lists. Never resolves the package (no
    /// store or registry lookup); releases its home and plugin store records
    /// but no plugin bytes or archive; use `gents pack remove` for that.
    /// `with_collections` then also empties and removes each collection the
    /// pack's SDL declared that nothing else on the node names.
    pub(super) async fn remove(
        &self,
        package: &str,
        with_collections: bool,
    ) -> anyhow::Result<String> {
        let args = PackInstallParams {
            package: package.to_owned(),
            variables: BTreeMap::new(),
            inference_slots: BTreeMap::new(),
            expected_digest: None,
        };
        self.validate(&args)?;
        let spec = pack::resolve::parse_pack_spec(package.trim())?;
        anyhow::ensure!(
            spec.version.is_none(),
            "pack remove takes a coordinate without a version; remove {}/{}",
            spec.namespace,
            spec.name
        );
        let coordinate = format!("{}/{}", spec.namespace, spec.name);
        let access = self.core.access()?;
        if let Some(home) = self.home.as_deref() {
            if let Some(record) = pack::read_home_install(home, &coordinate)? {
                if matches!(
                    record.kind,
                    pack::PackKind::Assets | pack::PackKind::Plugins
                ) {
                    return self
                        .remove_home_pack(&access, home, coordinate, record)
                        .await;
                }
            }
        }
        let declared = if with_collections {
            let home = self
                .home
                .clone()
                .context("--with-collections reads the pack's SDL from the node home, which this runtime does not have; remove without it, then use schema collection preview remove NAME")?;
            crate::collection_use::pack_declared_collections(
                &access,
                &home,
                self.core.node_did(),
                &coordinate,
            )
            .await
            .with_context(|| format!("{coordinate}: its declared collections cannot be read, so nothing was removed; remove without --with-collections"))?
        } else {
            Default::default()
        };
        let report = pack::remove_pack(
            &access,
            self.core.node_did(),
            &coordinate,
            pack::DriftPolicy::Refuse,
        )
        .await?;
        if let Some(home) = self.home.as_deref() {
            pack::forget_home_install(home, &coordinate)?;
            for removed in std::iter::once(&report).chain(&report.dependencies) {
                pack::plugin_lifecycle::release_plugin_records(
                    home,
                    &removed.pack,
                    &removed.documents.plugins,
                )?;
            }
        }
        let collections = crate::collection_use::remove_unused_collections(
            &access,
            self.home.as_deref(),
            self.core.node_did(),
            declared,
        )
        .await
        .with_context(|| format!("{coordinate} and its documents were removed, but cleaning up its collections failed; inspect them with schema collection list"))?;
        ordered! {
            "pack": coordinate,
            "removed": report,
            "collections": with_collections.then_some(collections),
            "effect": if with_collections {
                "The package's documents were removed. Each declared collection nothing else names was emptied and removed; the rest are listed with what still uses them."
            } else {
                "The package's documents were removed. Its collections and their documents stay; pack remove PACKAGE --with-collections also removes the ones nothing else uses, or use schema collection preview remove NAME."
            },
        }
        .pretty()
    }
}
