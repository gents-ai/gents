use super::*;
use crate::session::output_render::ordered;

impl PackInstaller {
    pub(super) async fn plugins_pack(
        &self,
        operation: &str,
        args: &PackChange,
        distribution: &Distribution,
        commit: bool,
    ) -> Result<String> {
        let access = self.core.access()?;
        let packs = self.packs(&access);
        let node_binding = self.core.refresh_node_binding().await?;
        let manifest = distribution.manifest();
        let plan = packs
            .plan_plugins(operation, args, distribution, node_binding.as_ref())
            .await?;
        let ready = plan.blockers.is_empty() && plan.missing_slots.is_empty();
        if !commit {
            return ordered! {
                "committed": false,
                "ready": ready,
                "operation": operation,
                "apply_with": ready.then(|| json!({"argv":["pack",operation,distribution.pinned_spec(&args.package)],"options":{"digest":distribution.digest(),"inference-slot":args.inference_slots.iter().map(|(name,profile)|format!("{name}={profile}")).collect::<Vec<_>>()}})),
                "operator_action": (!plan.blockers.is_empty()).then_some("Resolve installation_blockers through their existing owners. Plugin authority or increased limits require operator approval of this exact artifact through the plugin grant owner; model input cannot grant it. Repeat this pinned preview after approval."),
                "installation_blockers": plan.blockers,
                "missing_inference_slots": plan.missing_slots,
                "installed": plan.installed,
                "would_grant": plan.authority,
                "manifest": manifest,
                "artifact_digest": distribution.digest(),
                "inference": plan.inference,
            }.pretty();
        }
        anyhow::ensure!(ready, "plugin installation blocked; call [\"pack\",\"preview\",{operation:?},{:?}] and resolve its operator approval or inference blockers", distribution.pinned_spec(&args.package));
        let inference = packs
            .bound_inference(manifest, &args.inference_slots, node_binding.as_ref())
            .await?;
        self.fence_bindings(&inference.bindings).await?;
        let installed = packs
            .install_plugins(
                distribution,
                &inference.bindings,
                self.core.identity().ok(),
                node_binding,
            )
            .await?;
        let discovery = installed
            .plugins
            .iter()
            .map(|plugin| json!({"argv": ["plugin", "get", format!("{}/{}", plugin.namespace, plugin.name)]}))
            .collect::<Vec<_>>();
        let single = discovery.first().cloned().unwrap_or(json!(null));
        ordered! {
            "committed": true,
            "installed": installed.record,
            "next_call": single,
            "next_calls": discovery,
            "guidance": if installed.plugins.is_empty() { "The pack's assets and pages are installed; a page appears in ux list." } else { "Installation installs every plugin in `plugins` but selects no tool: installed is not selected. Read the current Tools document first and preserve its full `integrations` object, including every existing entry and plugin; do not generate a replacement patch or overwrite the whole object. Then explicitly select each installed plugin by name in `Tools.integrations.plugins`, one entry per plugin. Read config tools get and config get after selection; start a fresh request to call each selected plugin." },
            "granted_authority": plan.authority,
            "plugins": installed.plugins,
            "artifact_digest": distribution.digest(),
        }.pretty()
    }
}
