use std::path::PathBuf;

use anyhow::Result;
use serde_json::Value;

use crate::cli::args::{PluginBindArgs, PluginCommand, PluginUnbindArgs};
use crate::cli::{
    PackAccountArgs, PackDriftArgs, PackInfoArgs, PackInstallArgs, PackLoginArgs, PackOutdatedArgs,
    PackRemoveArgs, PackSearchArgs, PackUpdateArgs, PackWireArgs, ScopeArgs,
};
use crate::commands::pack as command;
use crate::output_format::OutputFormat;
use crate::request_helpers::capture_report;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum EditedDocuments {
    #[default]
    Refuse,
    Overwrite,
    Keep,
}

fn drift(edited: EditedDocuments) -> PackDriftArgs {
    PackDriftArgs {
        overwrite: edited == EditedDocuments::Overwrite,
        keep: edited == EditedDocuments::Keep,
    }
}

fn scope(home: PathBuf) -> ScopeArgs {
    ScopeArgs {
        home: Some(home),
        graphql: None,
        node_did: None,
    }
}

pub async fn installed(home: PathBuf, registry: Option<String>) -> Result<Value> {
    capture_report(command::update::outdated(PackOutdatedArgs {
        scope: scope(home),
        registry,
    }))
    .await
}

pub async fn search(registry: Option<String>, query: String, page: u32) -> Result<Value> {
    capture_report(command::registry::search(PackSearchArgs {
        query: Some(query),
        page,
        registry,
    }))
    .await
}

pub async fn info(registry: Option<String>, package: String) -> Result<Value> {
    capture_report(command::account::info(PackInfoArgs { package, registry })).await
}

#[allow(clippy::too_many_arguments)]
pub async fn install(
    home: PathBuf,
    package: String,
    registry: Option<String>,
    inference_slots: Vec<String>,
    edited: EditedDocuments,
    grant_authority: bool,
    preview: bool,
) -> Result<Value> {
    capture_report(command::install(PackInstallArgs {
        package,
        bindings: None,
        inference_slots,
        preview,
        scope: scope(home),
        output: OutputFormat::Json,
        force_rebind_concrete_did: false,
        registry,
        drift: drift(edited),
        grant_authority,
        wire: Vec::new(),
        expect_digest: None,
        explicit: true,
    }))
    .await
}

/// Wires the installed `plugin` into `agent`'s Tools and Context (or with
/// `remove`, takes it back out), or with `preview` reports what that would
/// write.
pub async fn wire(
    home: PathBuf,
    plugin: String,
    agent: String,
    targets_from: Vec<String>,
    remove: bool,
    preview: bool,
) -> Result<Value> {
    capture_report(command::wire::wire(PackWireArgs {
        plugin,
        agent,
        targets_from,
        remove,
        preview,
        scope: scope(home),
    }))
    .await
}

pub async fn plugin_slots(home: PathBuf) -> Result<Value> {
    let plugins = gents::plugin::store::list_records(&home)?
        .into_iter()
        .filter_map(|record| {
            let slot = record.declaration.model_slot.clone()?;
            Some(serde_json::json!({
                "plugin": format!("{}/{}", record.namespace, record.name),
                "slot": slot,
                "profile": record.model_binding.map(|binding| binding.profile_id),
            }))
        })
        .collect::<Vec<_>>();
    if plugins.is_empty() {
        return Ok(serde_json::json!({"plugins": plugins, "profiles": []}));
    }
    let (access, owner) = command::resolve_scope_owner(&scope(home)).await?;
    let profiles = gents::pack::inference_profile_options(&access, &owner).await?;
    Ok(serde_json::json!({"plugins": plugins, "profiles": profiles}))
}

/// What a person can wire: every installed plugin with its pack and whether
/// it declares jobs, and the installed packs that ship AgentTargets.
pub async fn wire_sources(home: PathBuf) -> Result<Value> {
    let plugins = gents::plugin::store::list_records(&home)?
        .into_iter()
        .map(|record| {
            serde_json::json!({
                "plugin": format!("{}/{}", record.namespace, record.name),
                "pack": record.owner_pack_coordinate,
                "job": record.declaration.job.0,
            })
        })
        .collect::<Vec<_>>();
    let (access, owner) = command::resolve_scope_owner(&scope(home.clone())).await?;
    let mut target_packs = Vec::new();
    for pack in gents::pack::installed_packs(Some(&home), Some((&access, &owner))).await? {
        let targets = gents::pack::installed_pack_documents(&access, &owner, &pack.coordinate)
            .await?
            .into_iter()
            .filter(|(collection, _)| *collection == gents::collection::Collection::AgentTarget)
            .map(|(_, id)| id)
            .collect::<Vec<_>>();
        if !targets.is_empty() {
            target_packs.push(serde_json::json!({"pack": pack.coordinate, "targets": targets}));
        }
    }
    Ok(serde_json::json!({"plugins": plugins, "target_packs": target_packs}))
}

pub async fn bind_plugin_slot(
    home: PathBuf,
    plugin: String,
    profile: Option<String>,
) -> Result<Value> {
    let command = match profile {
        Some(profile) => PluginCommand::Bind(PluginBindArgs {
            name: plugin,
            profile,
            scope: scope(home),
        }),
        None => PluginCommand::Unbind(PluginUnbindArgs {
            name: plugin,
            home: Some(home),
        }),
    };
    capture_report(crate::commands::plugin::dispatch(command)).await
}

pub async fn update(
    home: PathBuf,
    package: Option<String>,
    registry: Option<String>,
    edited: EditedDocuments,
) -> Result<Value> {
    capture_report(command::update::update(PackUpdateArgs {
        package,
        bindings: None,
        inference_slots: Vec::new(),
        scope: scope(home),
        registry,
        drift: drift(edited),
    }))
    .await
}

pub async fn remove(home: PathBuf, package: String, edited: EditedDocuments) -> Result<Value> {
    capture_report(command::remove(PackRemoveArgs {
        package,
        scope: scope(home),
        drift: drift(edited),
    }))
    .await
}

pub async fn login(home: PathBuf, registry: Option<String>, token: String) -> Result<Value> {
    capture_report(command::account::login(PackLoginArgs {
        username: None,
        password_stdin: false,
        token: Some(token),
        token_stdin: false,
        registry,
        home: Some(home),
    }))
    .await
}

pub async fn logout(home: PathBuf, registry: Option<String>) -> Result<Value> {
    capture_report(async {
        command::account::logout(PackAccountArgs {
            registry,
            home: Some(home),
        })
    })
    .await
}

pub async fn whoami(home: PathBuf, registry: Option<String>) -> Result<Value> {
    capture_report(command::account::whoami(PackAccountArgs {
        registry,
        home: Some(home),
    }))
    .await
}
