use super::*;

impl PackInstaller {
    pub(super) async fn list(&self, limit: usize, cursor: Option<&str>) -> anyhow::Result<String> {
        let access = self.core.access()?;
        let listing = self.packs(&access).list(limit, cursor).await?;
        Ok(serde_json::to_string_pretty(&json!({
            "source": "store_and_installations",
            "page": {
                "limit": limit,
                "total": listing.total,
                "returned": listing.items.len(),
                "truncated": listing.truncated,
                "next_cursor": listing.next_cursor,
            },
            "items": listing.items,
            "registry_lookup": "Use pack search to discover registry packages, then pack get NAMESPACE/NAME to inspect one. List shows installed packs and artifacts this home's store already holds.",
        }))?)
    }

    pub(super) async fn search(&self, query: &str, page: u32) -> anyhow::Result<String> {
        let (registry, response) = NodePacks::search(query, page).await?;
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
        let distribution = self.resolve("get", &PackChange::of(package)).await?;
        let access = self.core.access()?;
        let packs = self.packs(&access);
        let node_binding = self.core.refresh_node_binding().await?;
        let inference = packs
            .inference(
                distribution.manifest(),
                &BTreeMap::new(),
                node_binding.as_ref(),
            )
            .await?;
        let installed = packs.status(distribution.manifest()).await?;
        Ok(serde_json::to_string_pretty(&json!({
            "source": distribution.source(),
            "manifest": distribution.manifest(),
            "artifact_digest": distribution.digest(),
            "registry_artifact_digest": distribution.registry_artifact_digest(),
            "inference": inference,
            "installed": installed,
            "installable": true,
            "installation_blockers": packs.install_blockers(distribution.manifest()),
            "supported_operations": ["preview install", "install", "preview update", "update", "remove"],
        }))?)
    }

    /// Why a pack's plugins were not retired, for the model: the Tools it can
    /// see selecting them, and the call that drops the first such selection.
    /// Retirement itself stays refused on every surface until a complete
    /// reference inventory exists (Lean `incomplete_plugin_inventory_refuses`).
    fn plugin_retirement_refusal(
        coordinate: &str,
        plugins: &BTreeSet<String>,
        selections: Option<Vec<node_packs::PluginSelection>>,
        cause: anyhow::Error,
    ) -> anyhow::Error {
        let Some(selections) = selections else {
            return cause;
        };
        let names = plugins.iter().cloned().collect::<Vec<_>>().join(", ");
        let source = plugins
            .iter()
            .filter_map(|plugin| plugin.strip_prefix("node/"))
            .map(|name| format!(" To stop building {name}, delete its source document with plugin-source preview delete."))
            .collect::<String>();
        let reason = "Retiring a plugin needs proof that no Tools on the node selects it, and a read filtered by document access cannot give that proof, so no surface removes an installed plugin yet";
        let (seen, next_call) = match selections.first() {
            Some(first) => (
                format!(
                    "Selected by {}. The tools update below drops the first selection, so no model is offered the plugin; it stays installed.",
                    selections
                        .iter()
                        .map(|selection| format!(
                            "Tools {:?} of agent {} ({})",
                            selection.tools, selection.agent, selection.plugin
                        ))
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                json!({
                    "argv": ["tools", "update"],
                    // Deselecting is the drop the caller's own-Tools guard
                    // otherwise refuses.
                    "options": {"agent": first.agent, "shared": "acknowledge", "allow-drop": "integrations"},
                    "set": {"integrations": first.integrations},
                }),
            ),
            None => (
                "No Tools this agent can read selects it, so no model is offered it; it stays installed and unused.".to_owned(),
                json!({"argv": ["plugin", "get", plugins.iter().next()]}),
            ),
        };
        command::CommandGuidance {
            message: format!(
                "{coordinate} was not removed ({names}). {reason}. {seen}{source} Cause: {cause:#}"
            ),
            next_call,
        }
        .into()
    }

    /// What `pack remove` would do to an installed pack, read from its
    /// installation record: nothing is resolved or changed.
    pub(super) async fn preview_remove(&self, package: &str) -> anyhow::Result<String> {
        let coordinate = node_packs::coordinate_of(package, false)?;
        let access = self.core.access()?;
        let installed = self
            .packs(&access)
            .installed(&coordinate)
            .await?
            .with_context(|| format!("{coordinate} is not installed; see pack list"))?;
        let edited = self.edited_documents(&coordinate, true).await?;
        let collections = match &self.home {
            Some(home) => {
                let pack = crate::collection_use::pack_collections(
                    &access,
                    home,
                    self.core.node_did(),
                    &coordinate,
                )
                .await;
                match pack {
                    Ok(pack) => json!({
                        "note": "What --with-collections would do to each collection the pack declares. removable: true is emptied and removed with the documents counted here, which are only the ones you can read. A user that is one of this pack's own documents goes with the pack; whatever still names a collection after that keeps it.",
                        "collections": crate::collection_use::collection_cleanup_plan(
                            &access,
                            Some(home),
                            self.core.node_did(),
                            &pack,
                        )
                        .await?,
                    }),
                    Err(error) => json!({
                        "unavailable": format!("the pack's declared collections cannot be read ({error:#}), so --with-collections would be refused and remove nothing"),
                    }),
                }
            }
            None => json!({
                "unavailable": "this runtime has no node home to read the pack's SDL from, so --with-collections is refused; remove without it, then use schema collection preview remove NAME",
            }),
        };
        ordered! {
            "committed": false,
            "operation": "remove",
            "pack": coordinate,
            "installed": installed,
            "edited_documents": edited,
            "with_collections": collections,
            "apply_with": if edited.is_some() {
                json!({"argv":["pack","remove",coordinate],"options":{"edited":"keep"}})
            } else {
                json!({"argv":["pack","remove",coordinate]})
            },
            "effect": "Removes the documents the pack created; its collections and their documents stay unless --with-collections is added, which does what with_collections lists.",
        }
        .pretty()
    }

    /// Removes an installed pack. An assets or plugins pack releases its home
    /// record and the plugin records it owns; a documents pack removes its
    /// `PackInstallation` record and everything it lists. Nothing is resolved
    /// (no store or registry lookup) and no plugin bytes or archive are
    /// released; use `gents config pack remove` for that. `with_collections`
    /// then also empties and removes each collection the pack's SDL declared
    /// that nothing else on the node names.
    pub(super) async fn remove(
        &self,
        package: &str,
        with_collections: bool,
        edited: Option<&str>,
        words: &[String],
    ) -> anyhow::Result<String> {
        let edited = node_packs::edited_policy(edited)?;
        PackChange::of(package).validate()?;
        let coordinate = node_packs::coordinate_of(package, true)?;
        let access = self.core.access()?;
        let packs = self.packs(&access);
        if let Some(record) = packs.home_pack(&coordinate)? {
            let removal = packs
                .remove_home_pack(&coordinate, record, self.core.identity().ok())
                .await
                .map_err(|error| match error {
                    HomePackRemovalError::PluginsNotRetired {
                        plugins,
                        selections,
                        cause,
                    } => Self::plugin_retirement_refusal(&coordinate, &plugins, selections, cause),
                    HomePackRemovalError::Other(error) => error,
                })?;
            return ordered! {
                "pack": coordinate,
                "removed": json!({"kind": removal.record.kind, "plugins": removal.plugins, "pages": removal.record.ux.iter().map(|page| page.name.clone()).collect::<Vec<_>>()}),
                "effect": "The pack's install record and plugin records were released. Its cached assets, plugin bytes and archive stay until the operator runs `gents config pack prune`.",
            }
            .pretty();
        }
        let declared = if with_collections {
            let home = self
                .home
                .clone()
                .context("--with-collections reads the pack's SDL from the node home, which this runtime does not have; remove without it, then use schema collection preview remove NAME")?;
            crate::collection_use::pack_collections(
                &access,
                &home,
                self.core.node_did(),
                &coordinate,
            )
            .await
            .with_context(|| format!("{coordinate}: its declared collections cannot be read, so nothing was removed; remove without --with-collections"))?
        } else {
            crate::collection_use::PackCollections::default()
        };
        self.require_edited_decision(&coordinate, true, edited, words)
            .await?;
        let report = packs.remove_documents(&coordinate, edited).await?;
        let collections = crate::collection_use::remove_unused_collections(
            &access,
            self.home.as_deref(),
            self.core.node_did(),
            &declared,
        )
        .await
        .with_context(|| format!("{coordinate} and its documents were removed, but cleaning up its collections failed; inspect them with schema collection list"))?;
        ordered! {
            "pack": coordinate,
            "removed": report,
            "collections": with_collections.then_some(collections),
            "effect": if with_collections {
                "The package's documents were removed. Each collection its install created that nothing else names was emptied and removed; the rest are listed with what keeps them."
            } else {
                "The package's documents were removed. Its collections and their documents stay; pack remove PACKAGE --with-collections also removes the ones nothing else uses, or use schema collection preview remove NAME."
            },
        }
        .pretty()
    }
}
