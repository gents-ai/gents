//! `config plugin build NAME`: a plugin compiled, tested and installed from
//! source the node holds as a `PluginSource` document. The document is the
//! only input; the scratch pack directory under the home is the build owner's
//! working copy, never the agent's tool root. Compilation runs in the command
//! sandbox through `run_plugin_build`, the cases through the pack case
//! runner, and the install through the plugins-pack installer, so a plugin
//! built here is the same artifact `pack build` and `pack install` produce.

use super::*;

use crate::documents::PluginSourceDocument;

impl ConfigCommandTool {
    /// The plugin source document `id` on this node, as the calling agent may
    /// read it.
    async fn plugin_source_document(&self, id: &str) -> Result<PluginSourceDocument> {
        let owner = self.node_did.clone();
        let lookup = id.to_owned();
        let target = SelfConfigTarget::PluginSource;
        let document = crate::store::config_client::ConfigAccess::transact_local_readonly(
            &self.node,
            Some(self.core.identity()?),
            "self_config.plugin_source_read",
            |txn| {
                let owner = owner.clone();
                let lookup = lookup.clone();
                Box::pin(async move { ops::read_owned_doc(txn, target, &owner, &lookup).await })
            },
        )
        .await?
        .map(|(_, document)| document)
        .ok_or(super::ops::MissingConfigDocument {
            target,
            id: id.to_owned(),
        })?;
        serde_json::from_value(Value::Object(document))
            .context("decoding the plugin source document")
    }

    /// The node's SDL for each collection the plugin's `data` names. A plugin
    /// built from a source document has no pack to declare them, so they must
    /// be application collections already on this node.
    async fn plugin_source_data_schemas(
        &self,
        id: &str,
        document: &PluginSourceDocument,
    ) -> Result<BTreeMap<String, String>> {
        let Some(data) = &document.data else {
            return Ok(BTreeMap::new());
        };
        let access = self.core.access()?;
        let mut schemas = BTreeMap::new();
        let mut managed = Vec::new();
        let mut absent = Vec::new();
        for collection in &data.collections {
            if crate::store::schema::migration::DEFAULT_REGISTRY
                .managed_names()
                .any(|name| name == collection)
            {
                managed.push(collection.as_str());
            } else if access
                .collection_version(collection)
                .await
                .with_context(|| format!("plugin {id} was not built: its data.collections names {collection}, and this agent cannot read collection definitions; that takes the schema tool or a datastore tool on its Tools. The source document is saved"))?
                .is_none()
            {
                absent.push(collection.as_str());
            } else {
                schemas.insert(
                    collection.clone(),
                    collection_sdl(&access, collection).await?,
                );
            }
        }
        if managed.is_empty() && absent.is_empty() {
            return Ok(schemas);
        }
        let mut reasons = Vec::new();
        if !absent.is_empty() {
            reasons.push(format!(
                "Not on this node: {} (create it with the schema tool, or correct the name)",
                absent.join(", ")
            ));
        }
        if !managed.is_empty() {
            reasons.push(format!(
                "Gents-managed, which a plugin's data calls cannot use: {} (read those documents with config)",
                managed.join(", ")
            ));
        }
        Err(anyhow!(
            "plugin {id} was not built: data.collections takes application collections on this node. {}. The source document is saved; set data.collections with [\"plugin\",\"build\",{id:?}] and build again.",
            reasons.join(". ")
        ))
    }

    pub(in super::super) async fn plugin_build(&self, argv: &[String]) -> Result<String> {
        self.ensure_resource("tools")?;
        let grants = self.core.held_grants();
        anyhow::ensure!(
            grants.pack_install,
            "plugin build installs into the plugin store, which needs self_config.enable_pack_install on this agent's Tools"
        );
        anyhow::ensure!(
            grants.pack_authority,
            "plugin build compiles source, which needs self_config.enable_pack_authority on this agent's Tools"
        );
        let id = required_resource_id(argv.first(), "plugin name")?.clone();
        let target = SelfConfigTarget::PluginSource;
        // No fields rebuilds the saved document.
        let patch = match &argv[1..] {
            [] => Vec::new(),
            fields => parse_patch(fields, target)?,
        };
        if !patch.is_empty() {
            let mut request = ApplyRequest::new(target, patch);
            request.allow_create = true;
            let unique = id.clone();
            request.resolve_unique = Box::new(move |_| Ok(unique.clone()));
            let owner = self.node_did.clone();
            request.on_create = Box::new(move |id, doc| {
                doc.insert(target.unique_field().into(), json!(id));
                doc.insert("node_did".into(), json!(owner));
                Ok(())
            });
            self.patch(&self.core, "edit", request).await?;
        }
        let document = self.plugin_source_document(&id).await?;
        let coordinate = document.coordinate();
        let home = self
            .plugins
            .home()
            .context("this runtime has no plugin home")?
            .to_owned();
        let schemas = self.plugin_source_data_schemas(&id, &document).await?;
        let prepare_home = home.clone();
        let prepared = tokio::task::spawn_blocking(move || {
            source_pack::prepare_plugin_source(&prepare_home, &document, &schemas)
        })
        .await
        .context("the plugin source task did not finish")??;

        self.execution.enter_mutation();
        let build = crate::tools::native::run_plugin_build(&prepared.root, &prepared.dir, true)
            .await
            .map_err(|error| {
                anyhow!(
                    "plugin {coordinate} did not build; nothing was installed. Its source document is saved: fix it with [\"plugin\",\"build\",{id:?}] and set.source, or read it with [\"plugin-source\",\"get\",{id:?}].\n{error:#}"
                )
            })?;
        let cases = build.output["cases"]
            .as_array()
            .and_then(|plugins| plugins.first())
            .cloned()
            .unwrap_or_default();
        let passed = cases["passed"].as_u64().unwrap_or(0);
        let failures = cases["failures"]
            .as_array()
            .map(|failures| {
                failures
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        anyhow::ensure!(
            failures.is_empty(),
            "plugin {coordinate} compiled, but {} of {} test cases failed; nothing was installed. Its source document is saved.\n{}",
            failures.len(),
            passed + failures.len() as u64,
            failures
                .iter()
                .map(|failure| format!("- {failure}"))
                .collect::<Vec<_>>()
                .join("\n")
        );

        let open_home = home.clone();
        let built = prepared.clone();
        let distribution = Distribution::resolved(
            tokio::task::spawn_blocking(move || {
                source_pack::open_built_plugin_source(&open_home, &built)
            })
            .await
            .context("the plugin pack task did not finish")??,
        );
        let installer = PackInstaller {
            core: self.core.clone(),
            node: self.node.clone(),
            actor: self.identity.clone(),
            home: Some(home),
        };
        // A dropped request (an interrupt cancels the tool call's future)
        // must not stop between a pin move and the install it waits for, so
        // the moves, the install and their compensation run as one owned
        // task that finishes whether or not this call is still awaited.
        let owned = self.clone();
        let update = prepared.installed;
        let (owned_coordinate, owned_id) = (coordinate.clone(), id.clone());
        let (receipt, followed) = tokio::spawn(async move {
            let outcome = owned
                .install_following_pins(
                    installer,
                    distribution,
                    &owned_coordinate,
                    &owned_id,
                    update,
                )
                .await;
            #[cfg(test)]
            if let Some(pause) = REBUILD_TEST_PAUSE.lock().unwrap().clone() {
                pause.finished.notify_one();
            }
            outcome
        })
        .await
        .context("the plugin install task did not finish")??;
        let receipt: Value =
            serde_json::from_str(&receipt).context("reading the install receipt")?;
        let digest = receipt["plugins"][0]["digest"].clone();
        ordered! {
            "installed": true,
            "plugin": coordinate,
            "digest": digest,
            "version": prepared.version,
            "passed": passed,
            "failed": 0,
            "sandbox": build.sandbox,
            "selection": json!({"plugin": coordinate, "digest": digest}),
            "selections_moved": (!followed.is_empty()).then_some(&followed),
            "next_call": json!({"argv":["plugin","run",coordinate],"options":{"input":{}}}),
            "guidance": "The source and cases are the PluginSource document; build again after editing it. To give a model this tool, add the selection to Tools.integrations.plugins with tools update, preserving the other entries. A rebuild moves the pins of Tools that already select it; selections_moved lists them.",
        }
        .pretty()
    }

    /// A pin edit through the Tools edit path. The declared `Send` future is
    /// what lets the owned install task hold a call back into `dispatch`.
    fn pin_edit_call(&self, argv: Vec<String>) -> futures::future::BoxFuture<'_, Result<String>> {
        Box::pin(async move { self.dispatch(&argv).await })
    }

    /// Installs the built `distribution` of `coordinate`, moving the pins of
    /// the Tools that select it. Returns the install receipt and the moves.
    ///
    /// The builder is who selected node/NAME, so its pinned selections follow
    /// the rebuild: each pin moves to the new digest through the Tools edit
    /// path, with that path's authority, before the install's pin check (Lean
    /// `Configuration.pluginReplacementSafe`) reads them. The plugin store
    /// holds one build of a plugin, and that check refuses a replacement
    /// while a pin names another digest, so the pins move first. A moved pin
    /// names a digest that is installed only once the install publishes it,
    /// so a refused move or an install whose publication was rolled back
    /// puts every moved pin back: a Tools pinned to a digest that is not
    /// installed fails to build its tool surface. An install that failed
    /// after its publication became final
    /// ([`node_packs::InstallError::Retained`]) keeps the moved pins, which
    /// name the build that is installed.
    async fn install_following_pins(
        &self,
        installer: PackInstaller,
        distribution: Distribution,
        coordinate: &str,
        id: &str,
        update: bool,
    ) -> Result<(String, Vec<Value>)> {
        let mut followed = Vec::new();
        let mut moved: Vec<super::super::super::PluginPinConflict> = Vec::new();
        let installed = async {
            for conflict in installer.pin_conflicts(&distribution).await? {
                if moved.iter().any(|done| done.tools == conflict.tools) {
                    continue;
                }
                let call = pin_move_call(&conflict);
                self.pin_edit_call(pin_edit(&conflict, &conflict.integrations))
                    .await
                    .map_err(|error| anyhow::Error::from(CommandGuidance {
                        message: format!(
                            "plugin {coordinate} built and passed its cases, but nothing was installed: Tools {:?} of agent {} pins the previous build ({}) and could not be moved to {}: {error:#}. Move the pin with the tools update below, then repeat [\"plugin\",\"build\",{id:?}] with no set: the saved source builds to that same digest.",
                            conflict.tools, conflict.agent, conflict.pin, conflict.replacement
                        ),
                        next_call: call,
                    }))?;
                followed.push(json!({"tools": conflict.tools, "agent": conflict.agent, "from": conflict.pin, "to": conflict.replacement}));
                moved.push(conflict);
                #[cfg(test)]
                {
                    let pause = REBUILD_TEST_PAUSE.lock().unwrap().clone();
                    if let Some(pause) = pause {
                        pause.moved.notify_one();
                        pause.release.notified().await;
                    }
                }
            }
            installer
                .plugins_pack_final(
                    if update { "update" } else { "install" },
                    &PackChange {
                        expected_digest: Some(distribution.digest().to_owned()),
                        ..PackChange::of(coordinate)
                    },
                    &distribution,
                    true,
                )
                .await
        }
        .await;
        match installed {
            Ok(receipt) => Ok((receipt, followed)),
            Err(retained @ node_packs::InstallError::Retained(_)) => {
                Err(anyhow::Error::new(retained).context(if moved.is_empty() {
                    "the new build is installed; no selection's pin was moved".to_owned()
                } else {
                    format!(
                        "the new build is installed and the {} pin(s) this build moved name it; repeat [\"plugin\",\"build\",{id:?}] with no set to reconcile the node record",
                        moved.len()
                    )
                }))
            }
            Err(node_packs::InstallError::NotInstalled(error)) => {
                let error = error.into_anyhow();
                let mut stranded = Vec::new();
                for conflict in moved.iter().rev() {
                    if let Err(restore) = self
                        .pin_edit_call(pin_edit(conflict, &conflict.previous_integrations))
                        .await
                    {
                        stranded.push(format!(
                            "Tools {:?} of agent {} still pins {}, which is not installed ({restore:#}); set its pin back to {} with tools update",
                            conflict.tools, conflict.agent, conflict.replacement, conflict.pin
                        ));
                    }
                }
                Err(error.context(if moved.is_empty() {
                    "no selection's pin was moved".to_owned()
                } else if stranded.is_empty() {
                    format!(
                        "the {} pin(s) this build had moved were put back to the installed build",
                        moved.len()
                    )
                } else {
                    stranded.join("; ")
                }))
            }
        }
    }
}

/// A test's hold on a rebuild between a pin move and the install.
#[cfg(test)]
pub(in super::super::super) struct RebuildTestPause {
    pub(in super::super::super) moved: tokio::sync::Notify,
    pub(in super::super::super) release: tokio::sync::Notify,
    pub(in super::super::super) finished: tokio::sync::Notify,
}

#[cfg(test)]
pub(in super::super::super) static REBUILD_TEST_PAUSE: std::sync::Mutex<
    Option<Arc<RebuildTestPause>>,
> = std::sync::Mutex::new(None);
