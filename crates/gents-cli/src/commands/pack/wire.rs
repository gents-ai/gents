//! `gents pack wire`: the one writer of an installed plugin's place in an
//! agent's Tools and Context. Consent is the install's alone (the recorded
//! grant bounds every call), so wiring takes no authority flag.

use std::path::Path;

use anyhow::{Context, Result};
use gents::collection::Collection;
use gents::config_client::{
    apply_desired_state_plan, read_desired_state_records_in_txn, ConfigAccess,
    DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::document_config::{
    load_agent_context_and_tools_in_txn, unwire_plugin, wire_plugin, Tools,
};
use gents::pack::home_install::recorded_provenance;
use gents::pack::{installed_pack_documents, read_home_install, PackKind};
use gents::plugin::authority::describe_plugin_authority;
use gents::plugin::store::read_record;
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

    let (plugin_ref, agent_id, remove, preview) =
        (&plugin, &request.agent, request.remove, request.preview);
    let (tools_ids, skill_ids, targets_from) = (&tools_ids, &skill_ids, &targets_from);
    let report = access
        .transact_write("cli.pack.wire", move |txn| {
            Box::pin(async move {
                let missing =
                    |link: &str| format!("{link}. Next: gents tools explain --agent {agent_id}");
                let (context, tools) = load_agent_context_and_tools_in_txn(txn, owner, agent_id)
                    .await
                    .with_context(|| missing(&format!("cannot read the Tools of {agent_id}")))?;
                let context = context
                    .with_context(|| missing(&format!("{agent_id} has no Context document")))?;
                let tools =
                    tools.with_context(|| missing(&format!("{agent_id} has no Tools document")))?;

                let (tools, context, report) = if remove {
                    unwire_plugin(&tools, &context, plugin_ref, skill_ids, targets_from)?
                } else {
                    let reviewed_job = reviewed_job(txn, owner, tools_ids, &request.plugin).await?;
                    wire_plugin(
                        &tools,
                        &context,
                        plugin_ref,
                        reviewed_job.as_ref(),
                        skill_ids,
                        targets_from,
                    )?
                };
                tools.validate()?;
                if !preview {
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
                Ok::<_, anyhow::Error>(report)
            })
        })
        .await?;

    Ok(json!({
        "plugin": request.plugin,
        "agent": request.agent,
        "preview": request.preview,
        "authority": describe_plugin_authority(&plugin.declaration, &plugin.ceiling()),
        "report": report,
    }))
}

/// The `job` block of the first entry for `plugin` in the pack's own Tools
/// documents that carries one: what the pack's author reviewed.
async fn reviewed_job(
    txn: &gents::config_client::ConfigApplyTxn<'_>,
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
