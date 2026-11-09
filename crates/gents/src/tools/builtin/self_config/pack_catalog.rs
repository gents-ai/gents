use super::*;

impl PackInstaller {
    pub(super) async fn list(&self, limit: usize, cursor: Option<&str>) -> anyhow::Result<String> {
        anyhow::ensure!(
            (1..=50).contains(&limit),
            "--limit must be between 1 and 50"
        );
        let store = self.home.as_deref().map(crate::pack::store::PackStore::new);
        let mut names: BTreeMap<_, _> = match &store {
            Some(store) => store.names()?.into_iter().collect(),
            None => BTreeMap::new(),
        };
        let access = self.core.access()?;
        let installed =
            crate::pack::installed_catalog(&access, self.core.node_did(), self.home.as_deref())
                .await?;
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
            let listing = crate::pack::store::list_pack_versions(
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
            let installable = manifest.is_some() && kind.is_some_and(installs_kind);
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
        let registry = crate::pack::registry::resolve_registry_url(None);
        let response = crate::pack::registry::RegistryClient::new(registry.clone())
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
            "guidance": "Inspect a candidate with pack get before installation: it reports its kind, inference slots and installed state. This installer supports document and plugins-only packs; search includes other kinds too.",
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
        let inference = crate::pack::inspect_pack_inference_bindings_with_node_binding(
            &access,
            distribution.manifest(),
            self.core.node_did(),
            &BTreeMap::new(),
            node_binding.as_ref(),
        )
        .await?;
        let installable = installs_kind(&distribution.manifest().metadata.kind);
        let installed = if installable {
            self.installation_status(distribution.manifest()).await?
        } else {
            None
        };
        Ok(serde_json::to_string_pretty(&json!({
            "source": distribution.source(),
            "manifest": distribution.manifest(),
            "artifact_digest": distribution.digest(),
            "registry_artifact_digest": distribution.registry_artifact_digest(),
            "inference": inference,
            "installed": installed,
            "installable": installable,
            "installation_blockers": self.plugin_install_blockers(distribution.manifest()),
            "supported_operations": if distribution.manifest().metadata.kind == crate::pack::PackKind::Plugins {
                Some(vec!["preview install", "install", "preview update", "update"])
            } else { installable.then_some(vec!["preview install", "install", "preview update", "update", "remove"]) },
        }))?)
    }

    /// Removes an installed pack's `PackInstallation` record and everything
    /// it lists. Never resolves the package (no
    /// store or registry lookup); releases its home and plugin store records
    /// but no plugin bytes or archive; use `gents pack remove` for that.
    pub(super) async fn remove(&self, package: &str) -> anyhow::Result<String> {
        let args = PackInstallParams {
            package: package.to_owned(),
            variables: BTreeMap::new(),
            inference_slots: BTreeMap::new(),
            expected_digest: None,
        };
        self.validate(&args)?;
        let spec = crate::pack::resolve::parse_pack_spec(package.trim())?;
        anyhow::ensure!(
            spec.version.is_none(),
            "pack remove takes a coordinate without a version; remove {}/{}",
            spec.namespace,
            spec.name
        );
        let coordinate = format!("{}/{}", spec.namespace, spec.name);
        if let Some(home) = self.home.as_deref() {
            if let Some(record) = crate::pack::read_home_install(home, &coordinate)? {
                anyhow::ensure!(!matches!(record.kind, crate::pack::PackKind::Assets | crate::pack::PackKind::Plugins), "native removal of home-installed plugin/assets packs is unavailable; the existing pack removal owner must release plugin and home records together. Current installation is unchanged");
            }
        }
        let access = self.core.access()?;
        let report = crate::pack::remove_pack(
            &access,
            self.core.node_did(),
            &coordinate,
            crate::pack::DriftPolicy::Refuse,
        )
        .await?;
        if let Some(home) = self.home.as_deref() {
            crate::pack::forget_home_install(home, &coordinate)?;
            for removed in std::iter::once(&report).chain(&report.dependencies) {
                crate::pack::plugin_lifecycle::release_plugin_records(
                    home,
                    &removed.pack,
                    &removed.documents.plugins,
                )?;
            }
        }
        ordered! {
            "pack": coordinate,
            "removed": report,
            "effect": "The package's documents were removed. Package SDL schemas stay.",
        }
        .pretty()
    }
}
