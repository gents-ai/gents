use super::*;
use gents_session::session::output_render::ordered;
use node_packs::PluginWiring;

impl PackInstaller {
    /// `pack wire PACKAGE`: selects each plugin the installed pack ships on an
    /// agent's Tools, with the job block its pack reviewed, the agent targets
    /// that block names and the pack's skills; `remove` takes them out again.
    /// The same owner as the operator's `gents config pack wire`.
    ///
    /// The write lands on every agent that selects the wired Context or
    /// Tools, so it runs under the Tools edit's sharing guard
    /// (`ops::guard_shared_authority_in_txn`) in the wiring transaction: a
    /// document other agents select needs `holds_catalog_grant`. Naming the
    /// pack and the agent is the acknowledgement a Tools edit asks for.
    pub(super) async fn wire(
        &self,
        package: &str,
        agent: Option<&str>,
        remove: bool,
        commit: bool,
        holds_catalog_grant: bool,
    ) -> anyhow::Result<String> {
        let coordinate = node_packs::coordinate_of(package, true)?;
        let home = self
            .home()
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
        let mut plans = Vec::with_capacity(record.plugins.len());
        for shipped in &record.plugins {
            let plugin = PluginWiring::installed_plugin(home, namespace, &shipped.name)
                .with_context(|| {
                    format!("reading the installed plugin {namespace}/{}", shipped.name)
                })?;
            let mut wiring =
                PluginWiring::read(&access, self.core.node_did(), home, agent, plugin, &[]).await?;
            wiring.remove = remove;
            wiring.next_step = "config [\"tools\",\"get\"]".to_owned();
            plans.push(wiring);
        }
        let plans: Vec<&PluginWiring> = plans.iter().collect();
        let owner = self.core.node_did();
        let plans = &plans;
        let reports = if commit {
            access
                .transact_write("self_config.pack_wire", |txn| {
                    Box::pin(guarded_batch(
                        txn,
                        owner,
                        agent,
                        holds_catalog_grant,
                        plans,
                        gents_pack::pack::wiring::BatchWrite::Apply,
                    ))
                })
                .await?
        } else {
            access
                .transact_readonly("self_config.pack_wire.preview", |txn| {
                    Box::pin(guarded_batch(
                        txn,
                        owner,
                        agent,
                        holds_catalog_grant,
                        plans,
                        gents_pack::pack::wiring::BatchWrite::Preview { installing: None },
                    ))
                })
                .await?
        };
        let wired: Vec<_> = record
            .plugins
            .iter()
            .zip(reports)
            .map(|(shipped, report)| {
                let report = report.rendered_for(gents_pack::pack::RemedySurface::ConfigTool);
                json!({
                    "plugin": format!("{namespace}/{}", shipped.name),
                    "changed": report.changed,
                    "job": report.job,
                    "warnings": report.warnings,
                    "policy_warnings": report.policy_warnings,
                })
            })
            .collect();
        let rebinding = if commit {
            crate::tools_rebinding::after_tools_commit(&self.envelope).await
        } else {
            crate::tools_rebinding::ToolsRebinding::NotOwn
        };
        let rebound = matches!(
            rebinding,
            crate::tools_rebinding::ToolsRebinding::Rebound { .. }
        );
        let call = |verb: &[&str]| {
            let mut argv = verb.to_vec();
            argv.push(&coordinate);
            if remove {
                argv.push("--remove");
            }
            json!({"argv": argv, "options": {"agent": agent}})
        };
        ordered! {
            "effect": match (commit, remove, rebound) {
                (true, false, true) => "The pack's plugins are selected on your Tools; call them on your next tool call in this same response (keep working, do not end your reply).",
                (true, false, false) => "The pack's plugins are selected on the agent's Tools; a fresh request can call them.",
                (true, true, _) => "The pack's plugins are no longer selected on the agent's Tools.",
                (false, _, _) => "Nothing was written; wired lists what the call below would change.",
            },
            "next_turn": super::rebinding::next_turn(&rebinding),
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

/// `plans` in one write under [`guard_shared_wiring`], as `write` says.
async fn guarded_batch(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    agent: &str,
    holds_catalog_grant: bool,
    plans: &[&PluginWiring],
    write: gents_pack::pack::wiring::BatchWrite<'_>,
) -> anyhow::Result<Vec<gents_pack::pack::WireReport>> {
    guard_shared_wiring(txn, owner, agent, holds_catalog_grant).await?;
    gents_pack::pack::wiring::apply_batch(
        txn,
        plans,
        write,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await
}

/// The Tools edit's sharing guard for the Context and Tools `agent` selects,
/// which a wiring writes.
async fn guard_shared_wiring(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    agent: &str,
    holds_catalog_grant: bool,
) -> anyhow::Result<()> {
    let (context, tools) =
        gents_documents::load_agent_context_and_tools_in_txn(txn, owner, agent).await?;
    for (target, id) in [
        (
            SelfConfigTarget::AgentContext,
            context.map(|context| context.context_id),
        ),
        (SelfConfigTarget::Tools, tools.map(|tools| tools.tools_id)),
    ] {
        if let Some(id) = id {
            ops::guard_shared_authority_in_txn(txn, target, owner, &id, true, holds_catalog_grant)
                .await?;
        }
    }
    Ok(())
}
