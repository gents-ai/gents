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
        let mut plans = Vec::new();
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
            plans.push(wiring);
        }
        let owner = self.core.node_did();
        let reports = if commit {
            access
                .transact_write("self_config.pack_wire", |txn| {
                    let plans = &plans;
                    Box::pin(async move {
                        guard_shared_wiring(txn, owner, agent, holds_catalog_grant).await?;
                        gents_pack::pack::wiring::apply_batch(
                            txn,
                            plans,
                            false,
                            &gents_inference::resolved::ProviderProfilePolicy,
                        )
                        .await
                    })
                })
                .await?
        } else {
            access
                .transact_readonly("self_config.pack_wire.preview", |txn| {
                    let plans = &plans;
                    Box::pin(async move {
                        guard_shared_wiring(txn, owner, agent, holds_catalog_grant).await?;
                        gents_pack::pack::wiring::apply_batch(
                            txn,
                            plans,
                            true,
                            &gents_inference::resolved::ProviderProfilePolicy,
                        )
                        .await
                    })
                })
                .await?
        };
        let wired: Vec<_> = plans
            .iter()
            .zip(reports)
            .map(|(plan, report)| {
                wire_receipt(
                    &format!("{}/{}", plan.plugin.namespace, plan.plugin.name),
                    report,
                )
            })
            .collect();
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

fn wire_receipt(plugin: &str, mut report: gents_pack::pack::WireReport) -> serde_json::Value {
    for warning in &mut report.policy_warnings {
        warning.render_for(gents_pack::pack::PolicyWarningSurface::ConfigTool);
    }
    json!({
        "plugin": plugin,
        "changed": report.changed,
        "job": report.job,
        "warnings": report.warnings,
        "policy_warnings": report.policy_warnings,
    })
}

#[cfg(test)]
mod policy_receipt_tests {
    use super::*;
    #[test]
    fn pack_wire_and_preview_keep_surface_native_policy_warnings() {
        use gents_protocol::job::{DynamicMode, JobPolicy};
        let report = gents_pack::pack::WireReport {
            changed: Vec::new(),
            job: None,
            warnings: Vec::new(),
            next_call: None,
            policy_warnings: vec![gents_pack::pack::PolicyWarning {
                code: gents_pack::pack::PolicyWarningCode::DynamicModeKept,
                plugin: "test/job".to_owned(),
                requested: Some(JobPolicy {
                    dynamic: DynamicMode::Run,
                    ..Default::default()
                }),
                kept: JobPolicy {
                    dynamic: DynamicMode::Check,
                    ..Default::default()
                },
                previous_digest: Some("old".to_owned()),
                installed_digest: "new".to_owned(),
                tools_id: "operator-tools".to_owned(),
                remedy: "CLI wording must not leak".to_owned(),
            }],
        };
        let receipt = wire_receipt("test/job", report);
        let warning = &receipt["policy_warnings"][0];
        assert_eq!(warning["code"], "dynamic_mode_kept");
        assert_eq!(warning["kept"]["dynamic"], "check");
        assert_eq!(warning["requested"]["dynamic"], "run");
        let remedy = warning["remedy"].as_str().unwrap();
        assert!(remedy.contains("argv:[\"tools\",\"get\"]"));
        assert!(remedy.contains("argv:[\"tools\",\"update\"]"));
        assert!(!remedy.contains("gents config"));
    }
}
