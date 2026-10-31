use std::path::PathBuf;

use anyhow::Result;
use gents::ConfigAccess;
use serde_json::Value;

use crate::cli::args::{JobPromoteArgs, PluginBindArgs, PluginCommand, PluginUnbindArgs};
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

/// Promotes a finished dynamic run into the installed, wired tool
/// `gents/<name>`; the `PromoteReport` as JSON.
pub async fn promote(
    home: PathBuf,
    selector: String,
    name: Option<String>,
    agent: Option<String>,
) -> Result<Value> {
    capture_report(crate::commands::job::job_promote(JobPromoteArgs {
        scope: scope(home),
        selector,
        name,
        agent,
        output: OutputFormat::Json,
    }))
    .await
}

/// Every dynamic script with a completed run, newest first: its completed
/// runs and the tools it was promoted to. Runs that failed, were cancelled or
/// timed out are not promotable (`promote::gate`) and are left out.
pub async fn dynamic_workflows(home: PathBuf) -> Result<Value> {
    use gents::tools::background::job::view::{list_jobs, JobFilter, StateFilter};
    use gents_protocol::job::{JobState, JobView};
    let (access, home) = crate::resolve_config_access(Some(&home), None).await?;
    let views = list_jobs(
        &access,
        &JobFilter {
            session_id: None,
            requester: None,
            state: StateFilter::States(vec![JobState::Completed]),
            plugin: None,
        },
    )
    .await
    .map_err(crate::commands::job::read_error)?;
    let promotions = gents::tools::background::job::promote::list_promotions(&home)?;
    let mut scripts: std::collections::BTreeMap<&str, Vec<&JobView>> = Default::default();
    for view in views
        .iter()
        .filter(|view| gents::tools::background::job::promote::gate(view).is_ok())
    {
        scripts.entry(&view.pin.script_id).or_default().push(view);
    }
    let mut workflows = Vec::new();
    for (script_id, runs) in scripts {
        let latest = runs
            .iter()
            .max_by_key(|view| view.created_at)
            .expect("a group has a run");
        let agent_id = gents::session::origin::load_session(
            &access,
            &latest.session_id,
            latest.scope.requester_did.as_deref(),
        )
        .await?
        .map(|session| session.agent_id);
        workflows.push(serde_json::json!({
            "script_id": script_id,
            "handle": latest.handle,
            "runs": runs.len(),
            "last_run_at": latest.created_at,
            "agent_id": agent_id,
            "latest_address": latest.address,
            "promoted_as": promoted_as(&promotions, script_id),
        }));
    }
    workflows.sort_by_key(|workflow| std::cmp::Reverse(workflow["last_run_at"].as_u64()));
    // Installed workflows are the home's job plugins, as Grok's `/workflows` lists them.
    let installed: Vec<Value> = gents::plugin::store::list_records(&home)?
        .into_iter()
        .filter(|record| record.declaration.job.0)
        .map(|record| {
            serde_json::json!({
                "plugin": format!("{}/{}", record.namespace, record.name),
                "version": record.version,
                "description": record.declaration.description,
                "pack": record.owner_pack_coordinate,
            })
        })
        .collect();
    Ok(serde_json::json!({ "workflows": workflows, "installed": installed }))
}

/// One dynamic run, selected by its address (SESSION_ID:TOOL_CALL_ID) or tool
/// call id: its view, script and promotions.
pub async fn dynamic_workflow(home: PathBuf, selector: String) -> Result<Value> {
    let actor = crate::home_state::resolve_node_did(Some(&home), None)?;
    let (access, home) = crate::resolve_config_access(Some(&home), None).await?;
    let view = crate::commands::job::resolve_job(&access, &actor, &selector, None).await?;
    let run = gents::tools::background::job::promote::load_dynamic_run(
        &access,
        gents::tools::background::job::view::JobKey::RowDocId(view.row_doc_id),
    )
    .await?;
    let promotions = gents::tools::background::job::promote::list_promotions(&home)?;
    Ok(serde_json::json!({
        "promoted_as": promoted_as(&promotions, &run.view.pin.script_id),
        "view": run.view,
        "script": run.script,
    }))
}

fn promoted_as(
    promotions: &[gents::tools::background::job::promote::PromotionRecord],
    script_id: &str,
) -> Vec<String> {
    promotions
        .iter()
        .filter(|record| record.script_id == script_id)
        .map(|record| record.name.clone())
        .collect()
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
            .filter(|(collection, _)| {
                *collection == gents::store::collection::Collection::AgentTarget
            })
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
pub fn ux_list(home: &std::path::Path) -> Result<Vec<gents::plugin::ux::UxListing>> {
    gents::plugin::ux::list(home)
}

/// The module for the UX plugin `id`, produced from its file or by running
/// the pack's own `.afb` producer under the installed plugin's bounds.
pub async fn ux_module(home: PathBuf, id: String) -> Result<gents::plugin::ux::UxModule> {
    let listing = gents::plugin::ux::find(&home, &id)?;
    let home_for_run = home.clone();
    gents::plugin::ux::resolve_module(&home, &listing, |namespace, plugin| async move {
        crate::commands::plugin::call_installed(
            &home_for_run,
            &namespace,
            &plugin,
            gents::plugin::ux::producer_input(),
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
    let listing = gents::plugin::ux::find(home, ux)?;
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

pub fn plugin_call_uses_data(home: &std::path::Path, ux: &str, name: &str) -> Result<bool> {
    let coordinate = plugin_call_target(home, ux, name)?;
    let (namespace, _) = gents::pack::registry::split_pack_coordinate(&coordinate);
    Ok(gents::plugin::store::read_record(home, namespace, name)?
        .data_grant()
        .is_some())
}

pub fn ux_installing_node(home: &std::path::Path) -> Result<String> {
    let config = crate::home_state::read_init_config(home)?.ok_or_else(|| {
        anyhow::anyhow!("the local node that installed this app is not initialized")
    })?;
    let node_did = config.node_did.trim();
    anyhow::ensure!(
        !node_did.is_empty(),
        "the local node that installed this app has no identity"
    );
    Ok(node_did.to_owned())
}

/// Runs the `.afb` plugin `name` of the pack behind the UX plugin listed as
/// `ux`, with `input`, for that UX plugin's `ctx.plugin` (see
/// [`plugin_call_target`]). The call is a person's action in the desktop,
/// so a folder the input names outside the allowed ones is asked of them
/// (the plugin access prompt) rather than refused, as an interactive chat's
/// tool call is.
pub async fn plugin_call(
    home: PathBuf,
    ux: String,
    name: String,
    input: Value,
    operator: Option<(gents::ConfigAccess, String)>,
) -> Result<Value> {
    let coordinate = plugin_call_target(&home, &ux, &name)?;
    let (namespace, _) = gents::pack::registry::split_pack_coordinate(&coordinate);
    tracing::info!(ux = %ux, pack = %coordinate, plugin = %name, "ux plugin call");
    gents::plugin::approval::scope_interactive(
        true,
        crate::commands::plugin::call_installed_for_operator(
            &home, namespace, &name, input, operator,
        ),
    )
    .await
}

/* ---- UX plugins: a page's data path ---- */

/// Runs one datastore tool the UX plugin listed as `request.ux` declares
/// (`ux[].data`) on the agent `request.node_did`, through `access`: the
/// operator's access to that agent's node (see `gents::observe::ux_data`).
pub async fn ux_data_call(
    home: PathBuf,
    access: ConfigAccess,
    request: gents::observe::ux_data::UxDataCall,
) -> Result<Value> {
    gents::observe::ux_data::call(&home, &access, request).await
}

/// The surfaces and tools the UX plugin listed as `ux` may call on the
/// agent `node_did`, as the node has them.
pub async fn ux_data_surfaces(
    home: PathBuf,
    access: ConfigAccess,
    ux: String,
    node_did: String,
) -> Result<Value> {
    gents::observe::ux_data::surfaces(&home, &access, &ux, &node_did).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ux_plugin_data_uses_the_installing_home_node() {
        let home = tempfile::tempdir().unwrap();
        assert!(ux_installing_node(home.path()).is_err());
        crate::home_state::write_init_config(
            home.path(),
            &crate::shared::StoredInitConfig {
                home: home.path().display().to_string(),
                node_name: "local".into(),
                node_did: "did:key:local".into(),
                key_path: None,
                identity_backend: None,
                keychain_label: None,
                secure_enclave_label: None,
                tool_package: None,
                tool_ceiling: crate::cli::args::ToolCeilingArg::Readonly,
                tool_root: None,
                store_encryption: None,
            },
        )
        .unwrap();
        assert_eq!(ux_installing_node(home.path()).unwrap(), "did:key:local");
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
            .join(gents::store::local::home::UX_PLUGINS_DIR_NAME)
            .join("loose");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(dev.join(gents::plugin::ux::DEV_ENTRY), "export default {}").unwrap();
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
                dependency_assets: Vec::new(),
                installed_via: None,
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
        let mut record = gents::pack::read_home_install(home.path(), "examples/board_pack")
            .unwrap()
            .unwrap();
        record.kind = PackKind::Documents;
        write_home_install(home.path(), &record).unwrap();
        assert_eq!(
            plugin_call_target(home.path(), "examples/board_pack/board", "board_store").unwrap(),
            "examples/board_pack"
        );
        assert!(refused("examples/board_pack/board", "other").contains("ships no plugin"));
    }
}
