use super::*;

fn plugin_discovery_calls(plugins: &[(String, String)]) -> Vec<serde_json::Value> {
    plugins
        .iter()
        .map(|(namespace, name)| json!({"argv": ["plugin", "get", format!("{namespace}/{name}")]}))
        .collect()
}

impl PackInstaller {
    pub(super) async fn plugins_pack(
        &self,
        operation: &str,
        args: &PackInstallParams,
        distribution: &ConfigPackDistribution,
        commit: bool,
    ) -> Result<String> {
        let home = self
            .home
            .as_deref()
            .context("plugins packs require a runtime plugin home")?;
        let manifest = distribution.manifest();
        let coordinate = format!("{}/{}", manifest.metadata.namespace, manifest.name);
        let installed = crate::pack::read_home_install(home, &coordinate)?;
        anyhow::ensure!(
            operation != "update" || installed.is_some(),
            "pack {coordinate:?} is not installed; use pack preview install"
        );
        anyhow::ensure!(
            args.variables.is_empty(),
            "plugins-only packs do not consume document variables"
        );
        let access = crate::config_client::ConfigAccess::local(self.node.clone());
        let inference = crate::pack::inspect_pack_inference_bindings(
            &access,
            manifest,
            self.core.node_did(),
            &args.inference_slots,
        )
        .await?;
        let missing = inference
            .slots
            .iter()
            .filter(|slot| !slot.optional && !args.inference_slots.contains_key(&slot.name))
            .map(|slot| slot.name.clone())
            .collect::<Vec<_>>();
        let mut blockers = self.plugin_install_blockers(manifest);
        if let Some(previous) = &installed {
            anyhow::ensure!(
                previous.kind == crate::pack::PackKind::Plugins,
                "the existing home install is not a plugins-only pack"
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
        for plugin in &manifest.metadata.plugins {
            if let Err(error) = crate::plugin::store::check_plugin_ownership(
                home,
                &manifest.metadata.namespace,
                &plugin.name,
                &coordinate,
            ) {
                blockers.push(json!({"plugin":plugin.name,"reason":format!("{error:#}")}));
            }
            crate::plugin::PluginRunner::compile(
                distribution.0.archive.asset(&plugin.artifact)?,
                plugin,
            )?;
        }
        let ready = blockers.is_empty() && missing.is_empty();
        if !commit {
            return Ok(ordered! {
                "committed": false,
                "ready": ready,
                "operation": operation,
                "apply_with": ready.then(|| json!({"argv":["pack",operation,distribution.pinned_spec(&args.package)],"options":{"digest":distribution.digest(),"inference-slot":args.inference_slots.iter().map(|(name,profile)|format!("{name}={profile}")).collect::<Vec<_>>()}})),
                "operator_action": (!blockers.is_empty()).then_some("Resolve installation_blockers through their existing owners. Plugin authority or increased limits require operator approval of this exact artifact through the plugin grant owner; model input cannot grant it. Repeat this pinned preview after approval."),
                "installation_blockers": blockers,
                "missing_inference_slots": missing,
                "installed": installed,
                "manifest": manifest,
                "artifact_digest": distribution.digest(),
                "inference": inference,
            }.pretty()?);
        }
        anyhow::ensure!(ready, "plugin installation blocked; call [\"pack\",\"preview\",{operation:?},{:?}] and resolve its operator approval or inference blockers", distribution.pinned_spec(&args.package));
        let inference = crate::pack::preview_pack_inference_bindings(
            &access,
            manifest,
            self.core.node_did(),
            &args.inference_slots,
        )
        .await?;
        self.fence_bindings(&inference.bindings).await?;
        let (record, plugins) =
            crate::pack::plugin_lifecycle::install(
                home,
                &distribution.0.archive,
                false,
                Some(self.core.node_did()),
                &inference.bindings,
                |identity| {
                    let access = access.clone();
                    let node_did = self.core.node_did().to_owned();
                    let identity_did = self.core.identity().ok();
                    async move {
                        crate::pack::record_plugin_store_change_as(
                            &access,
                            &node_did,
                            Some(identity_did.ok_or_else(|| {
                                anyhow::anyhow!("node DID is not ACP-addressable")
                            })?),
                            home,
                            &identity.coordinate,
                            Some(&identity),
                        )
                        .await
                    }
                },
            )
            .await?;
        let discovery = plugin_discovery_calls(
            &plugins
                .iter()
                .map(|plugin| (plugin.namespace.clone(), plugin.name.clone()))
                .collect::<Vec<_>>(),
        );
        let single = discovery.first().cloned().unwrap_or(json!(null));
        Ok(ordered! {
            "committed": true,
            "installed": record,
            "next_call": single,
            "next_calls": discovery,
            "guidance": "Installation installs every plugin in `plugins` but selects no tool: installed is not selected. Read the current Tools document first and preserve its full `integrations` object, including every existing entry and plugin; do not generate a replacement patch or overwrite the whole object. Then explicitly select each installed plugin by name in `Tools.integrations.plugins`, one entry per plugin. Read config tools get and config get after selection; start a fresh request to call each selected plugin.",
            "plugins": plugins,
            "artifact_digest": distribution.digest(),
        }.pretty()?)
    }
}
