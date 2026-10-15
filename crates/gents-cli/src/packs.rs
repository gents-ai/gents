//! Pack operations for embedding callers such as the desktop: the same
//! commands `gents pack` runs, returning their reports instead of printing
//! them, so there is one install path, not two.

use std::path::PathBuf;

use anyhow::Result;
use gents::ConfigAccess;
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
        node_did: None,
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

/* ---- UX plugins: what the desktop's doors read ---- */

/// Every UX plugin on disk under `home` (dev folder and installed packs).
pub fn ux_list(home: &std::path::Path) -> Result<Vec<gents::ux_plugin::UxListing>> {
    gents::ux_plugin::list(home)
}

/// The module for the UX plugin `id`, produced from its file or by running
/// the pack's own `.afb` producer under the installed plugin's bounds.
pub async fn ux_module(home: PathBuf, id: String) -> Result<gents::ux_plugin::UxModule> {
    let listing = gents::ux_plugin::find(&home, &id)?;
    let home_for_run = home.clone();
    gents::ux_plugin::resolve_module(&home, &listing, |namespace, plugin| async move {
        crate::commands::plugin::call_installed(
            &home_for_run,
            &namespace,
            &plugin,
            gents::ux_plugin::producer_input(),
        )
        .await
    })
    .await
}

/// The pack behind the UX plugin listed as `ux`, once it ships an `.afb`
/// plugin named `name`: the `namespace/name` coordinate `ctx.plugin` may
/// run under. A UX plugin outside a pack has nothing to call. The webview
/// names the listing and nothing crossing that boundary says which code
/// asked, so this binds a call to a pack, not to a caller; the webview
/// keeps a plugin to its own listing by offering it no other verb.
pub fn plugin_call_target(home: &std::path::Path, ux: &str, name: &str) -> Result<String> {
    let listing = gents::ux_plugin::find(home, ux)?;
    let coordinate = listing.pack.ok_or_else(|| {
        anyhow::anyhow!(
            "ux plugin {ux:?} ships in no pack; ctx.plugin reaches a pack's own .afb plugins only"
        )
    })?;
    let record = gents::pack::read_home_install(home, &coordinate)?
        .ok_or_else(|| anyhow::anyhow!("pack {coordinate} is not installed"))?;
    anyhow::ensure!(
        record.plugins.iter().any(|p| p.name == name),
        "pack {coordinate} ships no plugin named {name:?}; ux plugin {ux:?} may call only its own pack's plugins"
    );
    Ok(coordinate)
}

/// Runs the `.afb` plugin `name` of the pack behind the UX plugin listed as
/// `ux`, with `input`, for that UX plugin's `ctx.plugin` (see
/// [`plugin_call_target`]). The call is a person's action in the desktop,
/// so a folder the input names outside the allowed ones is asked of them
/// (the plugin access prompt) rather than refused, as an interactive chat's
/// tool call is.
pub async fn plugin_call(home: PathBuf, ux: String, name: String, input: Value) -> Result<Value> {
    let coordinate = plugin_call_target(&home, &ux, &name)?;
    let (namespace, _) = gents::pack_registry::split_pack_coordinate(&coordinate);
    tracing::info!(ux = %ux, pack = %coordinate, plugin = %name, "ux plugin call");
    gents::plugin::approval::scope_interactive(
        true,
        crate::commands::plugin::call_installed(&home, namespace, &name, input),
    )
    .await
}

/* ---- UX plugins: a page's data path ---- */

/// Runs one datastore tool the UX plugin listed as `request.ux` declares
/// (`ux[].data`) on the agent `request.node_did`, through `access`: the
/// operator's access to that agent's node (see `gents::ux_data`).
pub async fn ux_data_call(
    home: PathBuf,
    access: ConfigAccess,
    request: gents::ux_data::UxDataCall,
) -> Result<Value> {
    gents::ux_data::call(&home, &access, request).await
}

/// The surfaces and tools the UX plugin listed as `ux` may call on the
/// agent `node_did`, as the node has them.
pub async fn ux_data_surfaces(
    home: PathBuf,
    access: ConfigAccess,
    ux: String,
    node_did: String,
) -> Result<Value> {
    gents::ux_data::surfaces(&home, &access, &ux, &node_did).await
}

/* ---- registries: the index the Packs panel's picker reads ---- */

pub fn registry_list(home: &std::path::Path) -> Result<Value> {
    crate::commands::registry::list(home)
}

pub async fn registry_refresh(home: PathBuf) -> Result<Value> {
    crate::commands::registry::refresh(&home, None).await
}

pub fn registry_add(
    home: &std::path::Path,
    id: &str,
    url: &str,
    label: Option<&str>,
) -> Result<Value> {
    crate::commands::registry::add(home, id, url, label)
}

pub fn registry_remove(home: &std::path::Path, id: &str) -> Result<Value> {
    crate::commands::registry::remove(home, id)
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

    /// `ctx.plugin` resolves its pack from the caller's own listing: an
    /// unknown listing, a plugin outside a pack and a plugin the pack does
    /// not ship are each refused by name; the pack's own plugin resolves.
    #[test]
    fn a_plugin_call_resolves_the_pack_from_the_callers_listing() {
        use gents::pack::{
            write_home_install, HomePackInstall, InstalledPackPlugin, PackKind, PackUxPlugin,
            UxContributions,
        };
        let home = tempfile::tempdir().unwrap();
        let dev = home
            .path()
            .join(gents::home::UX_PLUGINS_DIR_NAME)
            .join("loose");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(dev.join(gents::ux_plugin::DEV_ENTRY), "export default {}").unwrap();
        let assets = "packs/.materialized/examples/board_pack/deadbeef";
        std::fs::create_dir_all(home.path().join(assets).join("ux/board")).unwrap();
        std::fs::write(
            home.path().join(assets).join("ux/board/plugin.js"),
            "export default {}",
        )
        .unwrap();
        write_home_install(
            home.path(),
            &HomePackInstall {
                coordinate: "examples/board_pack".into(),
                version: "0.1.0".into(),
                digest: format!("sha256:{}", "b".repeat(64)),
                kind: PackKind::Plugins,
                assets: assets.into(),
                plugins: vec![InstalledPackPlugin {
                    name: "board_store".into(),
                    digest: format!("sha256:{}", "c".repeat(64)),
                }],
                installed_at: "2026-01-01T00:00:00Z".into(),
                ux: vec![PackUxPlugin {
                    name: "board".into(),
                    description: "the board".into(),
                    entry: Some("ux/board/plugin.js".into()),
                    css: None,
                    plugin: None,
                    contributes: UxContributions {
                        areas: vec!["nav".into()],
                        directives: vec![],
                    },
                    default_enabled: None,
                    data: vec![],
                }],
                registry: None,
            },
        )
        .unwrap();

        let refused = |ux: &str, name: &str| {
            format!(
                "{:#}",
                plugin_call_target(home.path(), ux, name).unwrap_err()
            )
        };
        assert!(refused("nope", "board_store").contains("no ux plugin \"nope\""));
        assert!(refused("loose", "board_store").contains("ships in no pack"));
        let other = refused("examples/board_pack/board", "other");
        assert!(other.contains("ships no plugin named \"other\""), "{other}");
        assert!(other.contains("examples/board_pack/board"), "{other}");
        assert_eq!(
            plugin_call_target(home.path(), "examples/board_pack/board", "board_store").unwrap(),
            "examples/board_pack"
        );
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
