//! Pack operations for embedding callers such as the desktop: the same
//! commands `gents pack` runs, returning their reports instead of printing
//! them, so there is one install path, not two.

use std::path::PathBuf;

use anyhow::Result;
use serde_json::Value;

use crate::cli::args::{PluginBindArgs, PluginCommand, PluginUnbindArgs};
use crate::cli::{
    GraphScopeArgs, PackAccountArgs, PackDriftArgs, PackInfoArgs, PackInstallArgs, PackLoginArgs,
    PackOutdatedArgs, PackRemoveArgs, PackSearchArgs, PackUpdateArgs,
};
use crate::commands::pack as command;
use crate::output_format::OutputFormat;
use crate::request_helpers::capture_report;

/// What to do with pack documents someone edited since they were installed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EditedDocuments {
    /// Stop and name them.
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

fn scope(home: PathBuf) -> GraphScopeArgs {
    GraphScopeArgs {
        home: Some(home),
        graphql: None,
        agent_did: None,
    }
}

/// Installed packs with the registry's latest version of each.
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

/// Installs `package` (a registry name, `sha256:<hex>`, a `.pack` file or a
/// directory), or with `preview` reports what it would write.
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
        explicit: true,
    }))
    .await
}

/// The installed plugins that can call a model, which profile each is bound
/// to (`null` when none), and the profiles a slot may be bound to.
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

/// Binds the installed `plugin`'s model slot to `profile`, or leaves it
/// unbound when `profile` is `None`.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_captured_command_returns_its_report_instead_of_printing() {
        let report = capture_report(async {
            crate::print_json(&serde_json::json!({"first": true}))?;
            crate::print_json(&serde_json::json!({"last": true}))
        })
        .await
        .unwrap();
        assert_eq!(report, serde_json::json!({"last": true}));
        assert!(capture_report(async { Ok(()) }).await.is_err());
    }

    #[tokio::test]
    async fn a_registry_error_reaches_the_caller() {
        let error = search(Some("http://127.0.0.1:1".into()), "x".into(), 1)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("could not reach the registry"),
            "{error:#}"
        );
    }
}
