use super::*;
use gents_session::session::output_render::ordered;
use node_packs::PluginWiring;

impl PackInstaller {
    /// `pack wire PACKAGE`: selects each plugin the installed pack ships on an
    /// agent's Tools, with the job block its pack reviewed, the agent targets
    /// that block names and the pack's skills; `remove` takes them out again.
    /// The same owner as the operator's `gents config pack wire`.
    pub(super) async fn wire(
        &self,
        package: &str,
        agent: Option<&str>,
        remove: bool,
        commit: bool,
    ) -> anyhow::Result<String> {
        let coordinate = node_packs::coordinate_of(package, true)?;
        let home = self
            .home
            .as_deref()
            .context("pack wire requires a runtime plugin home")?;
        let record = node_packs::read_home_install(home, &coordinate)?.ok_or_else(|| {
            anyhow::Error::from(command::CommandGuidance {
                message: format!(
                    "{coordinate} is not installed on this node, so it has no plugin to wire. Install it first; pack list shows what is installed"
                ),
                next_call: json!({"argv":["pack","preview","install",coordinate]}),
            })
        })?;
        anyhow::ensure!(
            !record.plugins.is_empty(),
            "{coordinate} ships no plugin, so pack wire has nothing to select. Its agents are reached through agent-target and tools update set.agents"
        );
        let agent = agent.unwrap_or(self.core.agent_did());
        let (namespace, _) = node_packs::split_pack_coordinate(&coordinate);
        let access = self.core.access()?;
        let mut wired = Vec::new();
        for shipped in &record.plugins {
            let plugin = PluginWiring::installed_plugin(home, namespace, &shipped.name)
                .with_context(|| {
                    format!("reading the installed plugin {namespace}/{}", shipped.name)
                })?;
            let mut wiring =
                PluginWiring::read(&access, self.core.node_did(), home, agent, plugin, &[]).await?;
            wiring.remove = remove;
            wiring.preview = !commit;
            wiring.next_step = "config [\"tools\",\"get\"]".to_owned();
            let wiring = &wiring;
            let report = if commit {
                access
                    .transact_write("self_config.pack_wire", |txn| {
                        Box::pin(async move {
                            wiring
                                .apply(txn, &gents_inference::resolved::ProviderProfilePolicy)
                                .await
                        })
                    })
                    .await?
            } else {
                access
                    .transact_readonly("self_config.pack_wire.preview", |txn| {
                        Box::pin(async move {
                            wiring
                                .apply(txn, &gents_inference::resolved::ProviderProfilePolicy)
                                .await
                        })
                    })
                    .await?
            };
            wired.push(json!({
                "plugin": format!("{namespace}/{}", shipped.name),
                "changed": report.changed,
                "job": report.job,
                "warnings": report.warnings,
            }));
        }
        let call = |verb: &[&str]| {
            let mut argv = verb.to_vec();
            argv.push(&coordinate);
            if remove {
                argv.push("--remove");
            }
            json!({"argv": argv, "options": {"agent": agent}})
        };
        ordered! {
            "effect": match (commit, remove) {
                (true, false) => "The pack's plugins are selected on the agent's Tools; a fresh request can call them.",
                (true, true) => "The pack's plugins are no longer selected on the agent's Tools.",
                (false, _) => "Nothing was written; wired lists what the call below would change.",
            },
            "wired": wired,
            "pack": coordinate,
            "agent": agent,
            "committed": commit,
            "apply_with": (!commit).then(|| call(&["pack", "wire"])),
            "next_call": commit.then(|| json!({"argv":["tools","get"]})),
        }
        .pretty()
    }
}
