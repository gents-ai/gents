//! The one writer of an installed plugin's place in an agent's Tools and
//! Context. The operator's `gents config pack wire` and the config tool's
//! `pack wire` both read their inputs and apply them through this owner, each
//! inside its own actor's transaction.

use anyhow::{Context, Result};
use serde_json::Value;

use super::home_install::recorded_provenance;
use super::plugin_lifecycle::InstalledPlugin;
use super::{
    consent_tool_calls, installed_pack_documents, read_home_install, unwire_plugin, wire_plugin,
    InstalledVia, PackKind, WireReport,
};
use gents_documents::{
    apply_desired_state_plan, load_agent_context_and_tools_in_txn,
    read_desired_state_records_in_txn, DesiredStateApplyDocument, DesiredStateApplyPlan, Tools,
};
use gents_store::collection::Collection;
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};

/// One installed plugin's wiring into (or with `remove`, out of) `agent`'s
/// Tools and Context.
pub struct PluginWiring {
    pub owner: String,
    pub agent: String,
    pub plugin: InstalledPlugin,
    /// The Tools and Skill documents the plugin's own pack installed.
    pub tools_ids: Vec<String>,
    pub reviewed_override: Option<Option<gents_protocol::job::PluginJobSettings>>,
    pub skill_ids: Vec<String>,
    /// (pack coordinate, the AgentTarget ids it ships, its provenance).
    pub targets_from: Vec<(String, Vec<String>, InstalledVia)>,
    pub remove: bool,
    /// Computes the same report and writes nothing.
    pub preview: bool,
    /// Set when the node owner approved this plugin calling the agent's own
    /// tools: its selection is written with `tool_calls: true`.
    pub tool_calls_consent: bool,
    /// The caller's own command that shows why an agent's Tools or Context
    /// cannot be read, named when they cannot.
    pub next_step: String,
}

impl PluginWiring {
    /// Reads what `plugin`'s pack and each `targets_from` pack installed, as
    /// `access`'s actor.
    pub async fn read(
        access: &ConfigAccess,
        owner: &str,
        home: &std::path::Path,
        agent: &str,
        plugin: InstalledPlugin,
        targets_from: &[String],
    ) -> Result<Self> {
        let (mut tools_ids, mut skill_ids) = (Vec::new(), Vec::new());
        if let Some(pack) = plugin.owner_pack_coordinate.as_deref() {
            for (collection, id) in installed_pack_documents(access, owner, pack).await? {
                match collection {
                    Collection::Tools => tools_ids.push(id),
                    Collection::Skill => skill_ids.push(id),
                    _ => {}
                }
            }
        }
        let mut targets = Vec::new();
        for pack in targets_from {
            let target_ids: Vec<String> = installed_pack_documents(access, owner, pack)
                .await?
                .into_iter()
                .filter(|(collection, _)| *collection == Collection::AgentTarget)
                .map(|(_, id)| id)
                .collect();
            anyhow::ensure!(!target_ids.is_empty(), "{pack} ships no AgentTarget");
            let record = read_home_install(home, pack)?;
            let via = recorded_provenance(
                record
                    .as_ref()
                    .map_or(PackKind::Documents, |record| record.kind.clone()),
                record.map(|record| record.installed_via),
            );
            targets.push((pack.clone(), target_ids, via));
        }
        Ok(Self {
            owner: owner.to_owned(),
            agent: agent.to_owned(),
            plugin,
            tools_ids,
            reviewed_override: None,
            skill_ids,
            targets_from: targets,
            remove: false,
            preview: false,
            tool_calls_consent: false,
            next_step: String::new(),
        })
    }

    /// The published record of the installed plugin `namespace/name`, which
    /// [`Self::read`] wires.
    pub fn installed_plugin(
        home: &std::path::Path,
        namespace: &str,
        name: &str,
    ) -> Result<InstalledPlugin> {
        super::plugin_lifecycle::installed_plugin(home, namespace, name)
    }

    fn coordinate(&self) -> String {
        format!("{}/{}", self.plugin.namespace, self.plugin.name)
    }

    /// Applies the wiring inside `txn` and reports what changed.
    pub async fn apply(
        &self,
        txn: &ConfigApplyTxn<'_>,
        profile_policy: &'static dyn gents_documents::inference_backend::AdvertisedProfilePolicy,
    ) -> Result<WireReport> {
        apply_batch(
            txn,
            std::slice::from_ref(self),
            self.preview,
            profile_policy,
        )
        .await?
        .pop()
        .context("wire produced no report")
    }

    async fn compute(
        &self,
        txn: &ConfigApplyTxn<'_>,
        tools: &Tools,
        context: &gents_documents::AgentContext,
    ) -> Result<(Tools, gents_documents::AgentContext, WireReport)> {
        let incoming = if self.remove {
            None
        } else {
            self.reviewed_job(txn).await?
        };
        let (mut tools, context, mut report) = if self.remove {
            unwire_plugin(
                tools,
                context,
                &self.plugin,
                &self.skill_ids,
                &self.targets_from,
            )?
        } else {
            wire_plugin(
                tools,
                context,
                &self.plugin,
                incoming.as_ref(),
                &self.skill_ids,
                &self.targets_from,
            )?
        };
        if self.tool_calls_consent && !self.remove {
            consent_tool_calls(&mut tools, &self.coordinate(), &mut report);
        }
        tools.validate()?;
        Ok((tools, context, report))
    }

    /// The `job` block of the first entry for this plugin in its pack's own
    /// Tools documents that carries one: what the pack's author reviewed.
    async fn reviewed_job(
        &self,
        txn: &ConfigApplyTxn<'_>,
    ) -> Result<Option<gents_protocol::job::PluginJobSettings>> {
        if let Some(job) = &self.reviewed_override {
            return Ok(job.clone());
        }
        let coordinate = self.coordinate();
        let ids: Vec<&str> = self.tools_ids.iter().map(String::as_str).collect();
        let mut documents =
            read_desired_state_records_in_txn(txn, Collection::Tools, &self.owner, &ids).await?;
        for id in &ids {
            let Some((_, value)) = documents.remove(*id) else {
                continue;
            };
            let tools: Tools = serde_json::from_value(value)
                .with_context(|| format!("decoding the pack's Tools document {id}"))?;
            let job = tools
                .integrations
                .and_then(|integrations| integrations.plugins)
                .into_iter()
                .flatten()
                .find(|entry| entry.plugin == coordinate && entry.job.is_some())
                .and_then(|entry| entry.job);
            if job.is_some() {
                return Ok(job);
            }
        }
        Ok(None)
    }
}

/// Compose the complete batch against cumulative state, validate the final graph,
/// then write it once through the caller's single transaction.
pub async fn apply_batch(
    txn: &ConfigApplyTxn<'_>,
    plans: &[PluginWiring],
    preview: bool,
    profile_policy: &'static dyn gents_documents::inference_backend::AdvertisedProfilePolicy,
) -> Result<Vec<WireReport>> {
    apply_batch_with_config(txn, plans, preview, None, profile_policy).await
}

/// Validate wiring against the documents the pending install would provide,
/// without publishing either the install or the wiring.
pub(super) async fn preview_batch(
    txn: &ConfigApplyTxn<'_>,
    plans: &[PluginWiring],
    config: Option<&gents_documents::PackConfig>,
    profile_policy: &'static dyn gents_documents::inference_backend::AdvertisedProfilePolicy,
) -> Result<Vec<WireReport>> {
    apply_batch_with_config(txn, plans, true, config, profile_policy).await
}

async fn apply_batch_with_config(
    txn: &ConfigApplyTxn<'_>,
    plans: &[PluginWiring],
    preview: bool,
    config: Option<&gents_documents::PackConfig>,
    profile_policy: &'static dyn gents_documents::inference_backend::AdvertisedProfilePolicy,
) -> Result<Vec<WireReport>> {
    let mut tools_by_id = std::collections::BTreeMap::<String, Tools>::new();
    let mut contexts_by_id =
        std::collections::BTreeMap::<String, gents_documents::AgentContext>::new();
    let mut reports = Vec::new();
    for plan in plans {
        let (context, tools) =
            load_agent_context_and_tools_in_txn(txn, &plan.owner, &plan.agent).await?;
        let context = context.with_context(|| {
            format!(
                "{} has no Context document. Next: {}",
                plan.agent, plan.next_step
            )
        })?;
        let tools = tools.with_context(|| {
            format!(
                "{} has no Tools document. Next: {}",
                plan.agent, plan.next_step
            )
        })?;
        let context = contexts_by_id.get(&context.context_id).unwrap_or(&context);
        let tools = tools_by_id.get(&tools.tools_id).unwrap_or(&tools);
        let (tools, context, report) = plan.compute(txn, tools, context).await?;
        tools_by_id.insert(tools.tools_id.clone(), tools);
        contexts_by_id.insert(context.context_id.clone(), context);
        reports.push(report);
    }
    if plans.is_empty() {
        return Ok(reports);
    }
    // The install's documents supply references (targets, skills, etc.) that
    // do not exist yet. Computed wiring replaces any same-ID Tools/Context in
    // that proposed graph; the combined graph still gets full validation.
    let mut documents = if let Some(config) = config {
        let mut config = config.clone();
        config
            .tools
            .retain(|tools| !tools_by_id.contains_key(&tools.tools_id));
        config
            .contexts
            .retain(|context| !contexts_by_id.contains_key(&context.context_id));
        DesiredStateApplyPlan::from_pack_config(&config)?
            .documents()
            .to_vec()
    } else {
        Vec::new()
    };
    let mut document = |collection, value: Value| {
        documents.push(DesiredStateApplyDocument {
            collection,
            add: value.clone(),
            update: value,
        })
    };
    for tools in tools_by_id.values() {
        document(Collection::Tools, serde_json::to_value(tools)?);
    }
    for context in contexts_by_id.values() {
        document(Collection::AgentContext, serde_json::to_value(context)?);
    }
    let desired = DesiredStateApplyPlan::new(documents)?;
    if preview {
        gents_documents::validate_desired_state_plan(txn, &desired, profile_policy).await?;
    } else {
        apply_desired_state_plan(txn, &desired, profile_policy).await?;
    }
    Ok(reports)
}
