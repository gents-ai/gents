//! The one writer of an installed plugin's place in an agent's Tools and
//! Context. The operator's `gents config pack wire` and the config tool's
//! `pack wire` both read their inputs and apply them through this owner, each
//! inside its own actor's transaction.

use anyhow::{Context, Result};
use serde_json::Value;

use super::home_install::recorded_provenance;
use super::plugin_lifecycle::InstalledPlugin;
use super::{
    consent_tool_calls, installed_pack_documents, installed_pack_record, read_home_install,
    unwire_plugin, wire_plugin, InstalledVia, PackKind, WireReport,
};
use gents_documents::{
    apply_desired_state_plan, load_agent_context_and_tools_in_txn,
    read_desired_state_records_in_txn, DesiredStateApplyDocument, DesiredStateApplyPlan, Tools,
};
use gents_store::collection::Collection;
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};

/// What a wiring plan takes of a node's word about the packs it reads.
pub trait Vouch: Sync {
    /// Refuses the `collection` `ids` the node records the plugin's own
    /// `pack` as having installed unless the caller vouches for each.
    fn own(&self, pack: &str, collection: Collection, ids: &[String]) -> Result<()>;
    /// Refuses the AgentTarget `ids` the node records `pack`, at the
    /// `installed` digest, as having installed unless the caller vouches for
    /// each; otherwise who installed it.
    fn target(&self, pack: &str, installed: Option<&str>, ids: &[String]) -> Result<InstalledVia>;
}

/// The node is the home's, so its own install records vouch for it.
struct HomeRecords<'a>(&'a std::path::Path);

impl Vouch for HomeRecords<'_> {
    fn own(&self, _pack: &str, _collection: Collection, _ids: &[String]) -> Result<()> {
        Ok(())
    }

    fn target(
        &self,
        pack: &str,
        _installed: Option<&str>,
        _ids: &[String],
    ) -> Result<InstalledVia> {
        let record = read_home_install(self.0, pack)?;
        Ok(recorded_provenance(
            record
                .as_ref()
                .map_or(PackKind::Documents, |record| record.kind.clone()),
            record.map(|record| record.installed_via),
        ))
    }
}

/// One installed plugin's wiring into (or with `remove`, out of) `agent`'s
/// Tools and Context.
pub struct PluginWiring {
    pub owner: String,
    pub agent: String,
    pub plugin: InstalledPlugin,
    /// The Tools and Skill documents the plugin's own pack installed.
    pub tools_ids: Vec<String>,
    pub skill_ids: Vec<String>,
    /// (pack coordinate, the AgentTarget ids it ships, its provenance).
    pub targets_from: Vec<(String, Vec<String>, InstalledVia)>,
    pub remove: bool,
    /// Computes the same report and writes nothing.
    pub preview: bool,
    /// The job settings owner's consent step, set when the node owner
    /// approved dynamic scripts for this plugin: its job block goes from
    /// `check` to `run` (Lean `consentedDynamic`).
    pub dynamic_consent: Option<fn(&mut Tools, &str, &mut WireReport)>,
    /// Set when the node owner approved this plugin calling the agent's own
    /// tools: its selection is written with `tool_calls: true`.
    pub tool_calls_consent: bool,
    /// The caller's own command that shows why an agent's Tools or Context
    /// cannot be read, named when they cannot.
    pub next_step: String,
    pub reviewed: ReviewedJob,
}

#[derive(Debug, Clone, Default)]
pub enum ReviewedJob {
    #[default]
    Read,
    /// Its pack's Tools on the node, signed only when they carry the job
    /// the pack the caller approved yields (`Configuration.wireReviewedJob`);
    /// `None` when the approval could not derive it.
    Approved(Option<Option<gents_protocol::job::PluginJobSettings>>),
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
        Self::read_with(
            access,
            owner,
            agent,
            plugin,
            targets_from,
            &HomeRecords(home),
        )
        .await
    }

    /// [`Self::read`] with what the node records of each pack checked by
    /// `vouch` instead of taken from a home's install records.
    pub async fn read_with(
        access: &ConfigAccess,
        owner: &str,
        agent: &str,
        plugin: InstalledPlugin,
        targets_from: &[String],
        vouch: &dyn Vouch,
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
            vouch.own(pack, Collection::Tools, &tools_ids)?;
            vouch.own(pack, Collection::Skill, &skill_ids)?;
        }
        let mut targets = Vec::new();
        for pack in targets_from {
            let (installed, documents) = installed_pack_record(access, owner, pack).await?;
            let target_ids: Vec<String> = documents
                .into_iter()
                .filter(|(collection, _)| *collection == Collection::AgentTarget)
                .map(|(_, id)| id)
                .collect();
            anyhow::ensure!(!target_ids.is_empty(), "{pack} ships no AgentTarget");
            let provenance = vouch.target(pack, installed.as_deref(), &target_ids)?;
            targets.push((pack.clone(), target_ids, provenance));
        }
        Ok(Self {
            owner: owner.to_owned(),
            agent: agent.to_owned(),
            plugin,
            tools_ids,
            skill_ids,
            targets_from: targets,
            remove: false,
            preview: false,
            dynamic_consent: None,
            tool_calls_consent: false,
            next_step: String::new(),
            reviewed: ReviewedJob::Read,
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
        let agent_did = &self.agent;
        let missing = |link: &str| format!("{link}. Next: {}", self.next_step);
        let (context, tools) = load_agent_context_and_tools_in_txn(txn, &self.owner, agent_did)
            .await
            .with_context(|| missing(&format!("cannot read the Tools of {agent_did}")))?;
        let context =
            context.with_context(|| missing(&format!("{agent_did} has no Context document")))?;
        let tools =
            tools.with_context(|| missing(&format!("{agent_did} has no Tools document")))?;
        let (mut tools, context, mut report) = if self.remove {
            unwire_plugin(
                &tools,
                &context,
                &self.plugin,
                &self.skill_ids,
                &self.targets_from,
            )?
        } else {
            let job = self.reviewed_job(txn).await?;
            let job = match &self.reviewed {
                ReviewedJob::Read => job,
                ReviewedJob::Approved(approved) => {
                    crate::pack::wire_subject::wire_reviewed_job(
                        approved.as_ref().map(Option::as_ref),
                        job.as_ref(),
                    )
                    .with_context(|| {
                        format!(
                            "the node's Tools carry a job for {} that the pack this device approved does not yield, so nothing is wired",
                            self.coordinate()
                        )
                    })?
                }
            };
            wire_plugin(
                &tools,
                &context,
                &self.plugin,
                job.as_ref(),
                &self.skill_ids,
                &self.targets_from,
            )?
        };
        if let Some(consent) = self.dynamic_consent.filter(|_| !self.remove) {
            consent(&mut tools, &self.coordinate(), &mut report);
        }
        if self.tool_calls_consent && !self.remove {
            consent_tool_calls(&mut tools, &self.coordinate(), &mut report);
        }
        tools.validate()?;
        if !self.preview {
            let document = |collection, value: Value| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            };
            let plan = DesiredStateApplyPlan::new(vec![
                document(Collection::Tools, serde_json::to_value(&tools)?),
                document(Collection::AgentContext, serde_json::to_value(&context)?),
            ])?;
            apply_desired_state_plan(txn, &plan, profile_policy).await?;
        }
        Ok(report)
    }

    /// The `job` block of the first entry for this plugin in its pack's own
    /// Tools documents that carries one: what the pack's author reviewed.
    async fn reviewed_job(
        &self,
        txn: &ConfigApplyTxn<'_>,
    ) -> Result<Option<gents_protocol::job::PluginJobSettings>> {
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
