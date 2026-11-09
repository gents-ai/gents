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
        let patch = parse_patch(&argv[1..], target)?;
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
        let prepare_home = home.clone();
        let prepared = tokio::task::spawn_blocking(move || {
            source_pack::prepare_plugin_source(&prepare_home, &document)
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
        let distribution = ConfigPackDistribution(
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
        let receipt = installer
            .plugins_pack(
                if prepared.installed {
                    "update"
                } else {
                    "install"
                },
                &PackInstallParams {
                    package: coordinate.clone(),
                    variables: BTreeMap::new(),
                    inference_slots: Default::default(),
                    expected_digest: Some(distribution.digest().to_owned()),
                },
                &distribution,
                true,
            )
            .await?;
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
            "next_call": json!({"argv":["plugin","run",coordinate],"options":{"input":{}}}),
            "guidance": "The source and cases are the PluginSource document; build again after editing it. To give a model this tool, add the selection to Tools.integrations.plugins with tools update, preserving the other entries; a rebuild changes the digest, so update the selection after it.",
        }
        .pretty()
    }
}
