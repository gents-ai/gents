//! `config plugin build NAME`: a plugin compiled, tested and installed from
//! source the node holds as a `PluginSource` document. The document is the
//! only input; the scratch pack directory under the home is the build owner's
//! working copy, never the agent's tool root. Compilation runs in the command
//! sandbox through `run_plugin_build`, the cases through the pack case
//! runner, and the install through the plugins-pack installer, so a plugin
//! built here is the same artifact `pack build` and `pack install` produce.

use super::*;
use crate::session::output_render::ordered;

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
        // The builder is who selected node/NAME, so its pinned selections
        // follow the rebuild: each pin moves to the new digest through the
        // Tools edit path, with that path's authority, before the install's
        // pin check (Lean `Configuration.pluginReplacementSafe`) reads them.
        // A moved pin names a digest that is installed only once the install
        // commits, so a refused move or a failed install puts every moved pin
        // back: a Tools pinned to a digest that is not installed fails to
        // build its tool surface.
        let mut followed = Vec::new();
        let mut moved: Vec<super::super::super::PluginPinConflict> = Vec::new();
        let installed = async {
            for conflict in installer.pin_conflicts(&distribution).await? {
                if moved.iter().any(|done| done.tools == conflict.tools) {
                    continue;
                }
                let call = pin_move_call(&conflict);
                Box::pin(self.dispatch(&pin_edit(&conflict, &conflict.integrations)))
                    .await
                    .map_err(|error| CommandGuidance {
                        message: format!(
                            "plugin {coordinate} built and passed its cases, but nothing was installed: Tools {:?} of agent {} pins the previous build ({}) and could not be moved to {}: {error:#}. Move the pin with the tools update below, then repeat [\"plugin\",\"build\",{id:?}] with no set: the saved source builds to that same digest.",
                            conflict.tools, conflict.agent, conflict.pin, conflict.replacement
                        ),
                        next_call: call,
                    })?;
                followed.push(json!({"tools": conflict.tools, "agent": conflict.agent, "from": conflict.pin, "to": conflict.replacement}));
                moved.push(conflict);
            }
            installer
                .plugins_pack(
                    if prepared.installed {
                        "update"
                    } else {
                        "install"
                    },
                    &PackChange {
                        expected_digest: Some(distribution.digest().to_owned()),
                        ..PackChange::of(&coordinate)
                    },
                    &distribution,
                    true,
                )
                .await
        }
        .await;
        let receipt = match installed {
            Ok(receipt) => receipt,
            Err(error) => {
                let mut stranded = Vec::new();
                for conflict in moved.iter().rev() {
                    if let Err(restore) = Box::pin(
                        self.dispatch(&pin_edit(conflict, &conflict.previous_integrations)),
                    )
                    .await
                    {
                        stranded.push(format!(
                            "Tools {:?} of agent {} still pins {}, which is not installed ({restore:#}); set its pin back to {} with tools update",
                            conflict.tools, conflict.agent, conflict.replacement, conflict.pin
                        ));
                    }
                }
                return Err(error.context(if moved.is_empty() {
                    "no selection's pin was moved".to_owned()
                } else if stranded.is_empty() {
                    format!(
                        "the {} pin(s) this build had moved were put back to the installed build",
                        moved.len()
                    )
                } else {
                    stranded.join("; ")
                }));
            }
        };
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
}
