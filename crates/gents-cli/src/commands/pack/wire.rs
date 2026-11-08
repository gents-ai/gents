//! `gents pack wire`: the one writer of an installed plugin's place in an
//! agent's Tools and Context. Consent is the install's alone (the recorded
//! grant bounds every call), so wiring takes no authority flag.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use gents::documents::apply_desired_state_plan;
use gents::documents::{load_agent_context_and_tools_in_txn, unwire_plugin, wire_plugin, Tools};
use gents::pack::home_install::recorded_provenance;
use gents::pack::{installed_pack_documents, read_home_install, PackKind};
use gents::plugin::authority::describe_plugin_authority;
use gents::plugin::store::read_record;
use gents::store::collection::Collection;
use gents::store::config_client::{
    read_desired_state_records_in_txn, ClientSigningCapability, ConfigAccess, ConfigApplyTxn,
    DesiredStateApplyDocument, DesiredStateApplyPlan, PackApprovalError, PackApprovalScope,
    PackEffectApprover, PackEvidence, PackMutation, PackRelationship, PackRelationshipEvidence,
};
use serde_json::{json, Value};

use super::split_namespace;
use crate::cli::PackWireArgs;

#[derive(Clone)]
pub(crate) struct WireRequest {
    pub(crate) plugin: String,
    pub(crate) agent: String,
    pub(crate) targets_from: Vec<String>,
    pub(crate) remove: bool,
    pub(crate) preview: bool,
}

#[cfg(test)]
pub(super) struct WireTestPause {
    pub(super) home: std::path::PathBuf,
    pub(super) reached: tokio::sync::Notify,
    pub(super) release: tokio::sync::Notify,
}

#[cfg(test)]
pub(super) static WIRE_TEST_PAUSE: std::sync::Mutex<Option<std::sync::Arc<WireTestPause>>> =
    std::sync::Mutex::new(None);

pub(crate) async fn wire(args: PackWireArgs) -> Result<()> {
    let (access, owner) = super::resolve_scope_owner(&args.scope).await?;
    let home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
    let request = WireRequest {
        plugin: args.plugin,
        agent: args.agent,
        targets_from: args.targets_from,
        remove: args.remove,
        preview: args.preview,
    };
    crate::print_json(&wire_in(&access, &owner, &home, &request).await?)
}

/// Wires (or with `remove`, unwires) `request.plugin` over a held `access`.
/// A preview computes the same report and writes nothing.
pub(crate) async fn wire_in(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    request: &WireRequest,
) -> Result<Value> {
    let access = access.clone();
    let owner = owner.to_owned();
    let home = home.to_owned();
    let request = request.clone();
    tokio::spawn(async move { wire_in_owned(&access, &owner, &home, &request).await }).await?
}

async fn wire_in_owned(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    request: &WireRequest,
) -> Result<Value> {
    if !request.preview && !request.remove {
        anyhow::ensure!(matches!(access, ConfigAccess::Local(_)),
            "cannot publish a plugin pin through GraphQL because the commit outcome may be unknown; stop the runtime serving the owning node home, then run `gents pack wire {} --agent {} --home <node-home>` without --graphql",
            request.plugin, request.agent);
    }
    let home_for_read = home.to_owned();
    let plugin_name = request.plugin.clone();
    let (_store_guard, plugin) = tokio::task::spawn_blocking(move || {
        let guard = gents::plugin::store::try_lock_store(&home_for_read)?;
        let (namespace, name) = split_namespace(&plugin_name);
        let plugin = read_record(&home_for_read, namespace, name).with_context(|| {
            format!("{plugin_name} is not installed. Next: gents pack install <pack>")
        })?;
        Ok::<_, anyhow::Error>((guard, plugin))
    })
    .await??;
    #[cfg(test)]
    {
        let pause = WIRE_TEST_PAUSE
            .lock()
            .unwrap()
            .as_ref()
            .filter(|pause| pause.home == home)
            .cloned();
        if let Some(pause) = pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }

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

    let mut targets_from = Vec::new();
    for pack in &request.targets_from {
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
        targets_from.push((pack.clone(), target_ids, via));
    }

    let plan = WirePlan {
        owner,
        plugin: &plugin,
        request,
        tools_ids: &tools_ids,
        skill_ids: &skill_ids,
        targets_from: &targets_from,
    };
    let report = if request.preview {
        access
            .transact_readonly("cli.pack.wire.preview", |txn| {
                let plan = &plan;
                Box::pin(async move { plan.apply(txn).await })
            })
            .await?
    } else if matches!(access, ConfigAccess::Local(_)) {
        use gents::NodeIdentity as _;
        let client = crate::home_state::load_client_identity().await?;
        anyhow::ensure!(
            access.actor_did().as_deref() == Some(client.did()),
            "pack wire requires the held User identity selected by the local transaction"
        );
        let scope = PackApprovalScope {
            node_did: owner.to_owned(),
            pack_digests: vec![plugin.digest.clone()],
            nonce: uuid::Uuid::new_v4().to_string(),
        };
        let approver = Arc::new(LocalWireApprover {
            actor: client.client_signing_capability()?,
            scope: scope.clone(),
        });
        access
            .transact_verified_pack_with_approver("cli.pack.wire", scope, approver, |txn| {
                let plan = &plan;
                Box::pin(async move { plan.apply(txn.approved_interpreter()?).await })
            })
            .await
            .map_err(|failure| {
                if failure.unconfirmed {
                    failure.error.context(
                        "pack wire commit was not confirmed; inspect the target before retrying",
                    )
                } else {
                    failure.error
                }
            })?
    } else {
        access
            .transact_write("cli.pack.wire", |txn| {
                let plan = &plan;
                Box::pin(async move { plan.apply(txn).await })
            })
            .await?
    };

    Ok(json!({
        "plugin": request.plugin,
        "agent": request.agent,
        "preview": request.preview,
        "authority": describe_plugin_authority(&plugin.declaration, &plugin.ceiling()),
        "report": report,
    }))
}

struct WirePlan<'a> {
    owner: &'a str,
    plugin: &'a gents::plugin::store::InstalledPlugin,
    request: &'a WireRequest,
    tools_ids: &'a [String],
    skill_ids: &'a [String],
    targets_from: &'a [(String, Vec<String>, gents::pack::InstalledVia)],
}

impl WirePlan<'_> {
    async fn apply(&self, txn: &ConfigApplyTxn<'_>) -> Result<gents::documents::WireReport> {
        let agent_did = &self.request.agent;
        let missing = |link: &str| format!("{link}. Next: gents tools explain --agent {agent_did}");
        let (context, tools) = load_agent_context_and_tools_in_txn(txn, self.owner, agent_did)
            .await
            .with_context(|| missing(&format!("cannot read the Tools of {agent_did}")))?;
        let context =
            context.with_context(|| missing(&format!("{agent_did} has no Context document")))?;
        let tools =
            tools.with_context(|| missing(&format!("{agent_did} has no Tools document")))?;
        let (tools, context, report) = if self.request.remove {
            unwire_plugin(
                &tools,
                &context,
                self.plugin,
                self.skill_ids,
                self.targets_from,
            )?
        } else {
            let job = reviewed_job(txn, self.owner, self.tools_ids, &self.request.plugin).await?;
            wire_plugin(
                &tools,
                &context,
                self.plugin,
                job.as_ref(),
                self.skill_ids,
                self.targets_from,
            )?
        };
        tools.validate()?;
        if !self.request.preview {
            let document = |collection, value: Value| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            };
            let plan = DesiredStateApplyPlan::new(vec![
                document(Collection::Tools, serde_json::to_value(&tools)?),
                document(Collection::AgentContext, serde_json::to_value(&context)?),
            ])?;
            apply_desired_state_plan(txn, &plan).await?;
        }
        Ok(report)
    }
}

struct LocalWireApprover {
    actor: ClientSigningCapability,
    scope: PackApprovalScope,
}

#[async_trait::async_trait]
impl PackEffectApprover for LocalWireApprover {
    async fn approve_graph(
        &self,
        _txn: &ConfigApplyTxn<'_>,
        mutation: &PackMutation,
        history: Option<Value>,
    ) -> std::result::Result<PackEvidence, PackApprovalError> {
        gents::store::config_client::prepare_pack_evidence_from_history(
            &self.actor,
            &self.scope,
            mutation,
            history,
        )
        .await
        .map_err(|error| PackApprovalError::Refused(error.to_string()))
    }
    async fn approve_relationship(
        &self,
        _txn: &ConfigApplyTxn<'_>,
        relationship: &PackRelationship,
    ) -> std::result::Result<PackRelationshipEvidence, PackApprovalError> {
        gents::store::config_client::prepare_pack_relationship(
            &self.actor,
            &self.scope,
            relationship,
        )
        .await
        .map_err(|error| PackApprovalError::Refused(error.to_string()))
    }
    async fn next_timestamp(
        &self,
        _txn: &ConfigApplyTxn<'_>,
        _kind: &'static str,
    ) -> std::result::Result<String, PackApprovalError> {
        Ok(gents::store::config_client::mint_recreate_identity_timestamp())
    }
    async fn graph_applied(&self, _result: &Value) -> std::result::Result<(), PackApprovalError> {
        Ok(())
    }
    async fn relationship_applied(&self) -> std::result::Result<(), PackApprovalError> {
        Ok(())
    }
    async fn client_finished(
        &self,
        _txn: &ConfigApplyTxn<'_>,
    ) -> std::result::Result<(), PackApprovalError> {
        Ok(())
    }
}

/// The `job` block of the first entry for `plugin` in the pack's own Tools
/// documents that carries one: what the pack's author reviewed.
async fn reviewed_job(
    txn: &gents::store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    tools_ids: &[String],
    plugin: &str,
) -> Result<Option<gents_protocol::job::PluginJobSettings>> {
    let ids: Vec<&str> = tools_ids.iter().map(String::as_str).collect();
    let mut documents =
        read_desired_state_records_in_txn(txn, Collection::Tools, owner, &ids).await?;
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
            .find(|entry| entry.plugin == plugin && entry.job.is_some())
            .and_then(|entry| entry.job);
        if job.is_some() {
            return Ok(job);
        }
    }
    Ok(None)
}
