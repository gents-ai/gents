use super::*;
use gents_session::session::output_render::ordered;

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
        let distribution = self.resolve("get", &PackChangeInput::of(package)).await?;
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
    /// read that still select them, and the `pack wire --remove` call that
    /// drops the first such selection (Lean `Configuration.selected_plugin_refuses_retirement`).
    fn plugin_retirement_refusal(
        coordinate: &str,
        plugins: &BTreeSet<String>,
        selections: Option<Vec<node_packs::PluginSelection>>,
        cause: anyhow::Error,
        reported: &str,
    ) -> anyhow::Error {
        let Some(first) = selections
            .as_ref()
            .and_then(|selections| selections.first())
        else {
            return cause;
        };
        let selections = selections.iter().flatten();
        let names = plugins.iter().cloned().collect::<Vec<_>>().join(", ");
        command::CommandGuidance {
            message: format!(
                "{coordinate} was not removed ({names}): selected by {}. The pack wire call below takes the first selection out of those Tools; when no Tools selects the plugin, repeat [\"pack\",\"remove\",{coordinate:?}].{reported}",
                selections
                    .map(|selection| format!(
                        "Tools {:?} of agent {} ({})",
                        selection.tools, selection.agent, selection.plugin
                    ))
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            next_call: json!({
                "argv": ["pack", "wire", coordinate, "--remove"],
                "options": {"agent": first.agent},
            }),
        }
        .into()
    }

    /// A documents pack retires the plugins it ships inside its removal
    /// transaction, which a Tools outside the pack that still selects one
    /// refuses. When the caller can read such a selection, the failed removal
    /// is answered as that refusal, with the unwiring call; an agent the pack
    /// ships goes with the pack and refuses nothing. Otherwise `cause` stands.
    async fn surviving_selection_refusal(
        &self,
        access: &gents_store::config_client::ConfigAccess,
        coordinate: &str,
        cause: anyhow::Error,
    ) -> anyhow::Error {
        let Some(Ok(Some(record))) = self
            .home
            .as_deref()
            .map(|home| node_packs::read_home_install(home, coordinate))
        else {
            return cause;
        };
        let (namespace, _) = node_packs::split_pack_coordinate(coordinate);
        let plugins: BTreeSet<String> = record
            .plugins
            .iter()
            .map(|plugin| format!("{namespace}/{}", plugin.name))
            .collect();
        let node_did = self.core.node_did();
        let Ok(owned) = node_packs::installed_pack_documents(access, node_did, coordinate).await
        else {
            return cause;
        };
        let selections = node_packs::visible_plugin_selections(access, node_did, &plugins)
            .await
            .ok()
            .map(|selections| {
                selections
                    .into_iter()
                    .filter(|selection| {
                        !owned.iter().any(|(collection, id)| {
                            *collection == gents_store::collection::Collection::Agent
                                && *id == selection.agent
                        })
                    })
                    .collect()
            });
        // The transaction may have stopped on something else first, so what
        // it said is kept beside the selection the caller must also drop.
        let reported = format!(" The removal reported: {cause:#}");
        Self::plugin_retirement_refusal(coordinate, &plugins, selections, cause, &reported)
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
    /// record and retires the plugins it owns; a documents pack removes its
    /// `PackInstallation` record and everything it lists. Both release the
    /// cached assets, plugin bytes and archives nothing else references.
    /// Nothing is resolved (no store or registry lookup). `with_collections`
    /// then also empties and removes each collection the pack's SDL declared
    /// that nothing else on the node names.
    pub(super) async fn remove(
        &self,
        package: &str,
        with_collections: bool,
        edited: Option<&str>,
        words: &[String],
    ) -> anyhow::Result<String> {
        let refusal = |error| Self::refusal(PackOperation::Remove, package, words, error);
        let edited = EditedDocuments::decision(edited).map_err(refusal)?;
        gents_pack::operations::pack::admit_input(&PackChangeInput::of(package))
            .map_err(refusal)?;
        let coordinate = node_packs::coordinate_of(package, true)?;
        let access = self.core.access()?;
        let packs = self.packs(&access);
        if let Some(record) = packs.home_pack(&coordinate)? {
            let (removal, unrecorded) = packs
                .remove_home_pack(
                    &coordinate,
                    record,
                    self.core.identity().ok(),
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
                .map_err(|error| match error {
                    HomePackRemovalError::PluginsNotRetired {
                        plugins,
                        selections,
                        cause,
                    } => Self::plugin_retirement_refusal(
                        &coordinate,
                        &plugins,
                        selections,
                        cause,
                        "",
                    ),
                    HomePackRemovalError::Other(error) => error,
                })?;
            // A plugin built on the node keeps its source document, which
            // `plugin build` installs again.
            let source = coordinate
                .strip_prefix("node/")
                .filter(|_| !removal.plugins.is_empty());
            return ordered! {
                "pack": coordinate,
                "removed": json!({"kind": removal.record.kind, "plugins": removal.plugins, "plugin_bytes": removal.plugin_bytes, "assets": removal.assets, "retained": removal.retained, "archives": removal.archives, "pages": removal.record.ux.iter().map(|page| page.name.clone()).collect::<Vec<_>>()}),
                "effect": if removal.plugins.is_empty() {
                    "The pack's install record, its cached assets and the archive nothing else uses were released."
                } else {
                    "The pack's plugins are retired: no call resolves them and plugin list no longer shows them. Their records, the bytes no other plugin uses, the install record, its cached assets and the archive nothing else uses were released."
                },
                "node_not_told": unrecorded.map(|reason| format!("The node was not told the plugins retired ({reason}): a running runtime keeps the removed tools listed, and their calls fail, until its configuration changes.")),
                "source": source.map(|name| format!("The source document {name} is kept, and [\"plugin\",\"build\",{name:?}] installs it again. Delete it with the call below to stop building it.")),
                "next_call": source.map(|name| json!({"argv":["plugin-source","preview","delete"],"target_id":name})),
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
        gents_pack::operations::pack::admit_edited(
            &access,
            self.core.node_did(),
            &coordinate,
            true,
            edited,
        )
        .await
        .map_err(refusal)?;
        let (report, released) = match packs
            .remove_documents(
                &coordinate,
                gents_pack::operations::pack::drift_policy(edited),
                &gents_inference::resolved::ProviderProfilePolicy,
            )
            .await
        {
            Ok(report) => report,
            Err(cause) => {
                return Err(self
                    .surviving_selection_refusal(&access, &coordinate, cause)
                    .await)
            }
        };
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
            "released": released,
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
