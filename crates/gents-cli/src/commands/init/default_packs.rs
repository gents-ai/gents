use std::path::Path;

use anyhow::{Context, Result};
use gents::core_packs::{not_carried, release_archive, Archives, Consent, CorePackTarget};
use gents_pack::pack::core_packs::{self, CorePack, EnableRoute, Wiring};
use gents_store::local::home::SETUP_UNFINISHED_FILE_NAME;
use serde_json::{json, Value};

use crate::cli::ScopeArgs;
use crate::commands::pack as command;

/// What a core pack is for and what the Engineer lacks without it, as the
/// init notice, `gents config pack starter` and the desktop's card say it.
fn describe(pack: CorePack) -> (&'static str, &'static str) {
    match pack {
        CorePack::PackBuilder => (
            "skills for building and sharing packs",
            "Without it the Engineer has no guide for packaging what it builds.",
        ),
        CorePack::Web => (
            "searching and reading the web",
            "Without it the Engineer cannot search or read the web.",
        ),
        CorePack::DynamicWorkflows => (
            "workflows that put several agents on one job",
            "Without it the Engineer cannot run workflows.",
        ),
        CorePack::Charts => (
            "charts on the pages it builds",
            "Without it the Engineer's pages have no charts.",
        ),
        CorePack::DataTables => (
            "data tables on the pages it builds",
            "Without it the Engineer's pages have no data tables.",
        ),
        CorePack::Script => (
            "short scripts over its own tools: waiting, trying logic, bulk changes",
            "Without it the Engineer makes every tool call one at a time.",
        ),
    }
}

fn enable_command(home: &Path, pack: CorePack) -> String {
    format!(
        "gents config pack starter --enable {} --home {}",
        pack.coordinate(),
        home.display()
    )
}

/// How to stop the node serving `home` for each way it runs, then `then`,
/// then start it again.
fn stop_the_node(home: &Path, then: &str) -> String {
    let home = home.display();
    format!(
        "Stop the node first: a background service with `gents runtime service stop --home {home}`, or a `gents runtime server --home {home}` you started by stopping that process (Ctrl-C). Then run `{then}`, and start the node again with `gents runtime service start --home {home}` or `gents runtime server --home {home}`"
    )
}

/// The core pack `name` names, written `ns/name` or `name`.
pub(crate) fn core_pack(home: &Path, name: &str) -> Result<CorePack> {
    CorePack::of_coordinate(name).with_context(|| {
        format!(
            "{name} is not one of the Engineer's core packs ({}). Install any other pack with: gents config pack install {name} --home {}",
            CorePack::ALL.map(CorePack::coordinate).join(", "),
            home.display()
        )
    })
}

/// Who asked for the home, which decides the remedy a refusal can offer: only
/// the CLI can pass `--no-default-packs`.
#[derive(Debug, Clone, Copy)]
pub(crate) enum InitEntryPoint {
    Cli,
    Start,
    Desktop,
}

#[derive(Debug, thiserror::Error)]
#[error(
    "this build cannot initialize the Engineer: missing core pack archives: {missing}; {remedy}"
)]
pub(crate) struct MissingCorePackArchives {
    missing: String,
    remedy: &'static str,
}

/// Lean `Pack.CorePacks.refuseInit`, applied to the archives this build carries.
pub(crate) fn admit_init(
    archives: Archives<'_>,
    entry: InitEntryPoint,
    fresh: bool,
    engineer: bool,
    defaults: bool,
) -> Result<(), MissingCorePackArchives> {
    let missing: Vec<_> = CorePack::ALL
        .into_iter()
        .filter(|&pack| release_archive(archives, pack).is_none())
        .map(CorePack::coordinate)
        .collect();
    if !core_packs::refuse_init(fresh, engineer, defaults, missing.is_empty()) {
        return Ok(());
    }
    Err(MissingCorePackArchives {
        missing: missing.join(", "),
        remedy: match entry {
            InitEntryPoint::Cli => "run `make core-packs && make build-cli` and retry, or pass `--no-default-packs` to opt out, leaving the Engineer without skills",
            InitEntryPoint::Start => "run `make core-packs && make build-cli` and retry",
            InitEntryPoint::Desktop => "this desktop dev build is incomplete; run `make desktop-native-dev`",
        },
    })
}

fn scope(home: &Path) -> ScopeArgs {
    ScopeArgs {
        home: Some(home.to_owned()),
        graphql: None,
        node_did: None,
    }
}

/// Returns once the store this access held is released, its node handles
/// let go first (`tla/StoreHandoff.tla` `AckedHandoff`).
async fn release(access: gents_store::config_client::ConfigAccess) {
    let store_release = access.begin_store_release();
    if let Some(node) = access.local_node() {
        if let Err(error) = node.shutdown().await {
            tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(&error), "failed to shut down embedded node");
        }
    }
    drop(access);
    if let Some(store_release) = store_release {
        store_release.released().await;
    }
}

/// Installs the core packs `archives` carries into the fresh home `home` for
/// its Engineer `engineer`, each left as Lean `initWiring` says. Installing
/// Gents is the consent to what they request, so nothing is asked. A pack
/// that cannot be installed is reported with the command that installs it
/// later; it never fails the init.
pub(crate) async fn install(
    home: &Path,
    engineer: &str,
    archives: Archives<'_>,
    registry: Option<&str>,
) -> Value {
    install_each(home, engineer, archives, &CorePack::ALL, registry).await
}

/// Records that `home`'s setup still has its core packs to install. Written
/// before init finishes the home's settings and removed by
/// [`record_installed`], so a setup stopped in between is found by
/// `gents_store::local::home::unfinished_setup`.
pub(crate) fn record_installing(home: &Path) -> Result<()> {
    let record = home.join(SETUP_UNFINISHED_FILE_NAME);
    std::fs::write(
        &record,
        serde_json::to_vec(&CorePack::ALL.map(CorePack::coordinate))?,
    )
    .with_context(|| format!("recording setup progress in {}", record.display()))
}

pub(crate) fn record_installed(home: &Path) -> Result<()> {
    let record = home.join(SETUP_UNFINISHED_FILE_NAME);
    match std::fs::remove_file(&record) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(error).with_context(|| format!("removing {}", record.display()))
        }
        _ => Ok(()),
    }
}

/// The core packs `home` has no install record of.
pub(crate) fn not_installed(home: &Path) -> Result<Vec<CorePack>> {
    let mut missing = Vec::new();
    for pack in CorePack::ALL {
        if gents_pack::pack::read_home_install(home, pack.coordinate())?.is_none() {
            missing.push(pack);
        }
    }
    Ok(missing)
}

/// Continues a setup that stopped before it installed every core pack: each
/// one the home lacks is installed as init leaves it, under init's consent.
/// The home's own store is opened, so its node is stopped first.
pub(crate) async fn install_missing(
    home: &Path,
    archives: Archives<'_>,
    registry: Option<&str>,
) -> Result<Value> {
    let missing = not_installed(home)?;
    if missing.is_empty() {
        return Ok(json!({"installed": [], "not_installed": []}));
    }
    let (access, owner) = command::resolve_scope_owner(&scope(home)).await?;
    let engineer = match access.local_node() {
        Some(_) => engineer_of(&access, &owner).await,
        None => Err(anyhow::anyhow!(
            "a runtime is serving this home, and a core pack is installed through the home's own store. Nothing was changed. {}",
            stop_the_node(home, &format!("gents --home {}", home.display()))
        )),
    };
    release(access).await;
    let engineer = engineer?
        .context("this home's default agent is not the Engineer, so it has no core packs")?;
    Ok(install_each(home, &engineer, archives, &missing, registry).await)
}

async fn install_each(
    home: &Path,
    engineer: &str,
    archives: Archives<'_>,
    packs: &[CorePack],
    registry: Option<&str>,
) -> Value {
    eprintln!("\nThe Engineer starts with the core packs this release carries:");
    for &pack in packs {
        eprintln!("  {}: {}", pack.coordinate(), describe(pack).0);
    }
    let mut installed = Vec::new();
    let mut not_installed = Vec::new();
    for &pack in packs {
        let wiring = core_packs::init_wiring(pack);
        match apply(
            home,
            engineer,
            pack,
            archives,
            wiring,
            Consent::Init,
            registry,
        )
        .await
        {
            Ok(mut report) if wiring != core_packs::enabled_wiring(pack) => {
                let command = enable_command(home, pack);
                eprintln!(
                    "  installed {}, not turned on for the Engineer. Turn it on with: {command}",
                    pack.coordinate()
                );
                report["enable_with"] = command.into();
                installed.push(report);
            }
            Ok(report) => {
                eprintln!("  installed {}", pack.coordinate());
                installed.push(report);
            }
            Err(error) => {
                let command = enable_command(home, pack);
                eprintln!(
                    "  {} was not installed: {error:#}. Install it later with: {command}",
                    pack.coordinate()
                );
                not_installed.push(json!({
                    "pack": pack.coordinate(),
                    "reason": format!("{error:#}"),
                    "install_with": command,
                }));
            }
        }
    }
    json!({"installed": installed, "not_installed": not_installed})
}

/// One core pack as `home` has it: what the desktop's card and
/// `gents config pack starter` list.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct PackStatus {
    pub(crate) pack: &'static str,
    pub(crate) purpose: &'static str,
    /// What the Engineer lacks while the pack is not enabled.
    pub(crate) without: &'static str,
    pub(crate) state: PackState,
    pub(crate) version: Option<String>,
    /// Why an `unavailable` pack cannot be installed, or why an installed
    /// pack's plugins do not run on this runtime.
    pub(crate) reason: Option<String>,
    /// The call that installs or turns on a `missing` or `not_enabled` pack.
    pub(crate) enable_with: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PackState {
    /// Installed and on the Engineer as Lean `enabledWiring` leaves it.
    Installed,
    /// Installed and not on the Engineer as enabling leaves it: dynamic
    /// workflows after init.
    NotEnabled,
    /// Not installed, and this build carries its archive.
    Missing,
    /// Not installed, and this build carries no archive of it.
    Unavailable,
    /// Installed, and a record of a plugin it ships cannot be read, so what
    /// it leaves on the Engineer is unknown; `reason` says why.
    Unknown,
}

/// The Engineer of `home`'s node: its default agent, when that agent is the
/// Engineer. `None` on a home with another default agent, which has no core
/// packs.
async fn engineer_of(
    access: &gents_store::config_client::ConfigAccess,
    owner: &str,
) -> Result<Option<String>> {
    let Ok(agent) = crate::commands::start::default_agent(access, owner).await else {
        return Ok(None);
    };
    let engineer = access
        .read_view(
            &gents_documents::views::agents::AgentList::new(owner)
                .agent_did(&agent)
                .limit(1),
        )
        .await?
        .first()
        .is_some_and(|row| {
            row.agent
                .tags
                .iter()
                .any(|tag| tag == gents_tools::builtin::self_config::ENGINEER_AGENT_TAG)
        });
    Ok(engineer.then_some(agent))
}

/// What the installed `record` leaves on the Engineer's Tools and Context, as
/// a Lean `Wiring`: wired when every plugin it ships is selected and every
/// skill attached, with the consents those selections carry. Its dynamic
/// consent holds when every job plugin of `job_plugins` is selected with a
/// block that is `PluginWiring.dynamicEnabled`, the meaning the enable uses.
///
/// Its root readers are not observed and read `false`: a reader is a DefraDB
/// relationship on the WorkspaceRoot document, listed only per document to an
/// actor that manages it, and no owner reads whether one agent holds it. An
/// enable whose grant failed names the command that makes the grant, and
/// enabling the pack again makes it.
async fn observed_wiring(
    access: &gents_store::config_client::ConfigAccess,
    owner: &str,
    record: &gents_pack::pack::HomePackInstall,
    context: &gents_documents::AgentContext,
    tools: &gents_documents::Tools,
    job_plugins: &[String],
) -> Result<Wiring> {
    let (namespace, _) = gents_pack::pack::registry::split_pack_coordinate(&record.coordinate);
    let selected: Vec<_> = tools
        .integrations
        .iter()
        .flat_map(|integrations| integrations.plugins.iter().flatten())
        .collect();
    let selections: Vec<_> = record
        .plugins
        .iter()
        .map(|shipped| {
            let coordinate = format!("{namespace}/{}", shipped.name);
            selected
                .iter()
                .find(|plugin| plugin.plugin == coordinate)
                .copied()
        })
        .collect();
    let skills_attached =
        gents_pack::pack::installed_pack_documents(access, owner, &record.coordinate)
            .await?
            .into_iter()
            .filter(|(collection, _)| *collection == gents_store::collection::Collection::Skill)
            .all(|(_, skill)| context.skill_ids.contains(&skill));
    let wired = skills_attached && selections.iter().all(Option::is_some);
    let selections: Vec<_> = selections.into_iter().flatten().collect();
    let agents_active = gents::core_packs::pack_agents(access, owner, &record.coordinate)
        .await?
        .iter()
        .all(|(_, enabled)| *enabled);
    Ok(Wiring {
        wired,
        tool_calls: !selections.is_empty() && selections.iter().all(|plugin| plugin.tool_calls),
        consent_dynamic: !job_plugins.is_empty()
            && job_plugins.iter().all(|coordinate| {
                selected.iter().any(|plugin| {
                    plugin.plugin == *coordinate
                        && plugin
                            .job
                            .as_ref()
                            .is_some_and(gents_pack::pack::dynamic_enabled)
                })
            }),
        root_readers: false,
        agents_active,
    })
}

/// The part of `wiring` a read of the Engineer's documents observes.
fn on_the_engineer(wiring: Wiring) -> Wiring {
    Wiring {
        root_readers: false,
        ..wiring
    }
}

/// An installed core pack's record and what it leaves on the Engineer, or
/// why a plugin record it ships cannot be read.
type Installed = (
    gents_pack::pack::HomePackInstall,
    std::result::Result<Wiring, String>,
);

/// The job plugins the installed `record` ships, from their plugin records.
fn job_plugins(home: &Path, record: &gents_pack::pack::HomePackInstall) -> Result<Vec<String>> {
    let (namespace, _) = gents_pack::pack::registry::split_pack_coordinate(&record.coordinate);
    let mut jobs = Vec::new();
    for shipped in &record.plugins {
        let plugin = gents_pack::plugin::store::read_record(home, namespace, &shipped.name)?;
        if plugin.declaration.job.0 {
            jobs.push(format!("{namespace}/{}", shipped.name));
        }
    }
    Ok(jobs)
}

/// Each core pack as `home`'s Engineer has it: its install record and what
/// it leaves on the Engineer, or `None` when it is not installed. Read
/// through whatever serves the home. `None` for a home whose default agent is
/// not the Engineer.
async fn engineer_packs(home: &Path) -> Result<Option<Vec<(CorePack, Option<Installed>)>>> {
    let (access, owner) = command::resolve_scope_owner(&scope(home)).await?;
    let read = async {
        let Some(engineer) = engineer_of(&access, &owner).await? else {
            return Ok(None);
        };
        let (context, tools) = access
            .transact_readonly("cli.core_packs.status", |txn| {
                let (owner, engineer) = (&owner, &engineer);
                Box::pin(async move {
                    gents_documents::load_agent_context_and_tools_in_txn(txn, owner, engineer).await
                })
            })
            .await?;
        let context = context.context("the Engineer has no Context document")?;
        let tools = tools.context("the Engineer has no Tools document")?;
        let mut packs = Vec::new();
        for pack in CorePack::ALL {
            let installed = match gents_pack::pack::read_home_install(home, pack.coordinate())? {
                Some(record) => {
                    let wiring = match job_plugins(home, &record) {
                        Ok(jobs) => Ok(observed_wiring(
                            &access, &owner, &record, &context, &tools, &jobs,
                        )
                        .await?),
                        Err(error) => Err(format!("{error:#}")),
                    };
                    Some((record, wiring))
                }
                None => None,
            };
            packs.push((pack, installed));
        }
        Ok::<_, anyhow::Error>(Some(packs))
    }
    .await;
    release(access).await;
    read
}

/// Why the plugins of `pack`, installed as `record`, do not run on this
/// runtime: the first plugin record refused for the release its pack was
/// built for.
fn interface_refusal(
    home: &Path,
    pack: CorePack,
    record: &gents_pack::pack::HomePackInstall,
) -> Option<String> {
    let (namespace, _) = gents_pack::pack::registry::split_pack_coordinate(pack.coordinate());
    record.plugins.iter().find_map(|plugin| {
        gents_pack::plugin::store::read_record(home, namespace, &plugin.name)
            .ok()?
            .check_interface()
            .err()
            .map(|refusal| refusal.to_string())
    })
}

/// Each core pack as `home` has it, with `archives` the release's. A home
/// whose default agent is not the Engineer has none.
pub(crate) async fn status(home: &Path, archives: Archives<'_>) -> Result<Vec<PackStatus>> {
    let Some(packs) = engineer_packs(home).await? else {
        return Ok(Vec::new());
    };
    Ok(packs
        .into_iter()
        .map(|(pack, installed)| {
            let (state, version, reason) = match installed {
                Some((record, Ok(observed))) => {
                    let reason = interface_refusal(home, pack, &record).or_else(|| {
                        (!observed.agents_active && core_packs::enabled_wiring(pack).agents_active)
                            .then(|| "an agent it ships is disabled".to_owned())
                    });
                    (
                        if on_the_engineer(observed)
                            == on_the_engineer(core_packs::enabled_wiring(pack))
                        {
                            PackState::Installed
                        } else {
                            PackState::NotEnabled
                        },
                        Some(record.version),
                        reason,
                    )
                }
                Some((record, Err(reason))) => {
                    (PackState::Unknown, Some(record.version), Some(reason))
                }
                None => match release_archive(archives, pack).map(|bytes| {
                    gents_pack::pack::archive::peek_header(
                        bytes,
                        gents_pack::pack::archive::Bounds::default(),
                    )
                }) {
                    Some(Ok(header)) => (PackState::Missing, Some(header.version), None),
                    Some(Err(error)) => (PackState::Unavailable, None, Some(format!("{error:#}"))),
                    None => (PackState::Unavailable, None, Some(not_carried(pack))),
                },
            };
            let (purpose, without) = describe(pack);
            PackStatus {
                pack: pack.coordinate(),
                purpose,
                without,
                state,
                version,
                reason,
                enable_with: matches!(state, PackState::Missing | PackState::NotEnabled)
                    .then(|| enable_command(home, pack)),
            }
        })
        .collect())
}

/// Enables each of `packs` for `home`'s Engineer: the user's explicit
/// operation. A pack the home lacks is installed from `archives`, without
/// asking, and each is left as Lean `enabledWiring` says. Each is written
/// where Lean `enableRoute` says: through the runtime serving the home, or
/// through the home's own store when none does; a pack the home lacks while
/// a runtime serves it is refused before anything is written. Returns
/// `{enabled, failed}`; a pack that fails leaves the others enabled and is
/// retried by enabling it again.
pub(crate) async fn enable(
    home: &Path,
    archives: Archives<'_>,
    packs: &[CorePack],
    registry: Option<&str>,
) -> Result<Value> {
    let (access, owner) = command::resolve_scope_owner(&scope(home)).await?;
    let served = access.local_node().is_none();
    let engineer = engineer_of(&access, &owner).await;
    release(access).await;
    let engineer = engineer?
        .context("this home's default agent is not the Engineer, so it has no core packs")?;
    for &pack in packs {
        let installed = gents_pack::pack::read_home_install(home, pack.coordinate())?.is_some();
        if core_packs::enable_route(served, installed) == EnableRoute::StopFirst {
            anyhow::bail!(
                "{} is not installed on this home, and it is installed as the Engineer through the home's own store, which the running node holds. Nothing was changed. {}",
                pack.coordinate(),
                stop_the_node(home, &enable_command(home, pack))
            );
        }
    }
    let mut enabled = Vec::new();
    let mut failed = Vec::new();
    for &pack in packs {
        let wiring = core_packs::enabled_wiring(pack);
        match apply(
            home,
            &engineer,
            pack,
            archives,
            wiring,
            Consent::Explicit,
            registry,
        )
        .await
        {
            Ok(report) => enabled.push(report),
            // A pack that installed and then failed a later step is on the
            // node; enabling it again finishes it.
            Err(error) => failed.push(json!({
                "pack": pack.coordinate(),
                "error": format!("{error:#}"),
                "installed": gents_pack::pack::read_home_install(home, pack.coordinate())
                    .ok()
                    .flatten()
                    .is_some(),
            })),
        }
    }
    Ok(json!({"enabled": enabled, "failed": failed}))
}

/// Leaves `pack` on `home`'s Engineer as `wiring` says through the core
/// pack owner. The archive is resolved from the home's files first, so a
/// build without it opens no store; then one held access installs the pack,
/// grants its agents, wires its plugins and attaches its skills, and its
/// node is drained before the next pack opens the home.
async fn apply(
    home: &Path,
    engineer: &str,
    pack: CorePack,
    archives: Archives<'_>,
    wiring: Wiring,
    consent: Consent,
    registry: Option<&str>,
) -> Result<Value> {
    let source = gents::core_packs::resolve(home, pack, archives, registry).await?;
    let (access, owner) = command::resolve_scope_owner(&scope(home)).await?;
    let runtime = runtime_identity(home);
    let applied = gents::core_packs::apply(
        &target(&access, &owner, home, engineer, registry, pack, &runtime),
        &source,
        wiring,
        consent,
    )
    .await;
    release(access).await;
    applied
}

/// The runtime identity `home`'s init recorded, read only when a pack is
/// installed: loading it reads the node's key.
fn runtime_identity(
    home: &Path,
) -> impl Fn() -> Result<std::sync::Arc<dyn gents_store::identity::NodeIdentity>> + Send + Sync + '_
{
    move || {
        let stored = gents_service::home_state::read_init_config(home)?
            .context("home is not initialized")?;
        gents_service::home_state::load_initialized_home_identity(home, &stored)
    }
}

/// The home's Engineer as the core pack owner leaves a pack on it, signed by
/// the User the home holds.
fn target<'a>(
    access: &'a gents_store::config_client::ConfigAccess,
    owner: &'a str,
    home: &'a Path,
    engineer: &'a str,
    registry: Option<&'a str>,
    pack: CorePack,
    runtime: gents::core_packs::RuntimeIdentity<'a>,
) -> CorePackTarget<'a> {
    CorePackTarget {
        access,
        owner,
        home,
        engineer,
        runtime,
        signers: gents_host::http::pack_wire_client::SIGNERS,
        registry,
        enable_with: enable_command(home, pack),
    }
}

/// `gents config pack starter`: the Engineer's core packs as this home has
/// them, or with `--enable` the named ones installed from this release where
/// missing and turned on for the Engineer.
pub(crate) async fn starter(args: crate::cli::PackStarterArgs) -> Result<()> {
    let home = gents_service::home_state::resolve_home_dir(args.home.as_deref());
    let archives = gents_core_packs::archives();
    if args.enable.is_empty() {
        return crate::print_json(&json!({
            "packs": status(&home, archives).await?,
        }));
    }
    let packs = args
        .enable
        .iter()
        .map(|name| core_pack(&home, name))
        .collect::<Result<Vec<_>>>()?;
    let report = enable(&home, archives, &packs, args.registry.as_deref()).await?;
    crate::print_json(&report)?;
    anyhow::ensure!(
        report["failed"].as_array().is_none_or(Vec::is_empty),
        "a core pack was not enabled; its error is in the report above"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;
    use gents_lean_vocab::families;

    fn write_pack(dir: &Path, manifest: Value, files: &[(&str, Vec<u8>)]) {
        std::fs::create_dir_all(dir.join("plugins")).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        std::fs::write(dir.join("README.md"), "# fixture").unwrap();
        for (path, bytes) in files {
            std::fs::write(dir.join(path), bytes).unwrap();
        }
    }

    /// The pack directory `dir` as release archive bytes.
    fn archive(dir: &Path) -> Vec<u8> {
        let mut bytes = Vec::new();
        gents_pack::pack::archive::write_pack(dir, &mut bytes).unwrap();
        bytes
    }

    fn borrowed(archives: &[(String, Vec<u8>)]) -> Vec<(&str, &[u8])> {
        archives
            .iter()
            .map(|(coordinate, bytes)| (coordinate.as_str(), bytes.as_slice()))
            .collect()
    }

    fn plugin_entry(name: &str, env: Value, job: bool) -> Value {
        json!({
            "name": name, "description": "test", "artifact": format!("plugins/{name}.afb"),
            "language": "rust", "input_schema": {"type": "object"}, "job": job,
            "manifold": {"fs": "None", "net": "None", "crypto": false,
                "child_process": false, "env": env,
                "allow_exit": false, "http_timeout_ms": null, "listen": "None"},
        })
    }

    /// Fixture core packs `gents/<name>`, each shipping one skill and what
    /// makes the rest of its Lean `Wiring` observable: `gents/web` a plugin
    /// that requests a permission, `gents/script` a plugin its consent lets
    /// call the Engineer's tools, and `gents/dynamic_workflows` a job plugin
    /// whose pack Tools ship its block at `check`, with a worker agent whose
    /// rootless host Tools take the Engineer's root.
    async fn fixture_archives() -> Vec<(String, Vec<u8>)> {
        let build = |name: &'static str| {
            tokio::task::spawn_blocking(move || {
                crate::commands::plugin::testing::build_plugin_afb(
                    name,
                    b"fn main() { println!(\"{{}}\"); }",
                )
            })
        };
        let (web, workflow, script) =
            tokio::join!(build("web_fetch"), build("workflow"), build("script"));
        let (web, workflow, script) = (web.unwrap(), workflow.unwrap(), script.unwrap());
        let packs = tempfile::tempdir().unwrap();
        CorePack::ALL
            .into_iter()
            .map(|pack| {
                let (_, name) =
                    gents_pack::pack::registry::split_pack_coordinate(pack.coordinate());
                let mut manifest = json!({
                    "manifest_version": 1, "name": name, "namespace": "gents",
                    "version": "1.0.0", "description": "A core pack fixture",
                    "authors": ["tests"], "tags": [], "kind": "documents",
                    "assets": ["README.md", "pack_config.json"], "dependencies": [],
                    "config": "pack_config.json",
                });
                let mut config = json!({
                    "node": {"node_did": "${GENTS_PACK_NODE_DID}"},
                    "skills": [{"skill_id": format!("core-{name}"),
                        "node_did": "${GENTS_PACK_NODE_DID}", "name": name,
                        "description": "A core skill.", "instructions": "Use it."}],
                });
                let shipped = match pack {
                    CorePack::Web => Some((
                        "web_fetch",
                        &web,
                        json!({"AllowList": ["CORE_FIXTURE"]}),
                        false,
                    )),
                    CorePack::Script => Some(("script", &script, json!("None"), false)),
                    CorePack::DynamicWorkflows => {
                        Some(("workflow", &workflow, json!("None"), true))
                    }
                    CorePack::PackBuilder | CorePack::Charts | CorePack::DataTables => None,
                };
                let mut files = Vec::new();
                if let Some((plugin, afb, env, job)) = shipped {
                    manifest["assets"]
                        .as_array_mut()
                        .unwrap()
                        .push(format!("plugins/{plugin}.afb").into());
                    manifest["plugins"] = json!([plugin_entry(plugin, env, job)]);
                    files.push((format!("plugins/{plugin}.afb"), afb.clone()));
                }
                match pack {
                    CorePack::DynamicWorkflows => {
                        manifest["inference_slots"] = json!([{"name": "worker",
                            "description": "The worker.", "agents": ["wf-read-only"]}]);
                        config["agents"] = json!([{"agent_slot": "wf-read-only",
                            "context_id": "wf-read-only-context",
                            "display_name": "Workflow read-only worker",
                            "description": "Does one read-only task.",
                            "inference_profile_id": "gents:inference-slot:worker"}]);
                        config["contexts"] = json!([{"context_id": "wf-read-only-context",
                            "system_prompt": "Work.",
                            "tools_id": "${GENTS_PACK_NODE_DID}-wf-read-only"}]);
                        config["tools"] = json!([
                            {"tools_id": "${GENTS_PACK_NODE_DID}-wf-read-only",
                                "display_name": "Read-only files",
                                "host": {"files": {"mode": "ReadOnly"}}},
                            {"tools_id": "workflow-caller", "display_name": "Workflow caller",
                                "agents": {"target_ids": ["wf-read-only"]},
                                "integrations": {"plugins": [{"plugin": "gents/workflow",
                                    "job": {"agent_targets": ["wf-read-only"],
                                        "policy": {"dynamic": "check"}}}]}},
                        ]);
                        config["agent_targets"] = json!([{
                            "agent_did": "gents:agent-slot:wf-read-only",
                            "name": "wf-read-only", "target_id": "wf-read-only",
                            "target_node_did": "${GENTS_PACK_NODE_DID}",
                            "description": "A read-only worker."}]);
                    }
                    CorePack::PackBuilder
                    | CorePack::Web
                    | CorePack::Charts
                    | CorePack::DataTables
                    | CorePack::Script => {}
                }
                files.push((
                    "pack_config.json".to_owned(),
                    serde_json::to_vec(&config).unwrap(),
                ));
                let dir = packs.path().join(name);
                let files: Vec<(&str, Vec<u8>)> = files
                    .iter()
                    .map(|(path, bytes)| (path.as_str(), bytes.clone()))
                    .collect();
                write_pack(&dir, manifest, &files);
                (pack.coordinate().to_owned(), archive(&dir))
            })
            .collect()
    }

    fn init_args(home: &Path, extra: &[&str]) -> crate::cli::InitArgs {
        let mut argv = vec![
            "gents",
            "init",
            "--store-key-custody",
            "file",
            "--engineer",
            "--model-name",
            "test-model",
            "--inference-url",
            "http://127.0.0.1:9/v1",
            "--home",
            home.to_str().unwrap(),
        ];
        argv.extend_from_slice(extra);
        let cli = crate::cli::Cli::try_parse_from(argv).unwrap();
        let Some(crate::cli::Command::Init(args)) = cli.command else {
            panic!("expected init")
        };
        args
    }

    /// Each core pack as the Engineer of `home` has it, as a Lean `Wiring`.
    /// Its root readers are what the handler's report `reports` says each of
    /// its agents was granted: true when it ships none.
    async fn observed(home: &Path, reports: &Value) -> Vec<(CorePack, Wiring)> {
        engineer_packs(home)
            .await
            .unwrap()
            .expect("the home's default agent is the Engineer")
            .into_iter()
            .map(|(pack, installed)| {
                let (_, wiring) =
                    installed.unwrap_or_else(|| panic!("{} is not installed", pack.coordinate()));
                let wiring = wiring.expect("every plugin record of an installed pack reads");
                let report = reports
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|report| report["pack"] == pack.coordinate())
                    .unwrap_or_else(|| panic!("no report for {}: {reports}", pack.coordinate()));
                let root_readers = report["host_tool_roots"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .all(|row| row["reader_on"].is_string());
                (
                    pack,
                    Wiring {
                        root_readers,
                        ..wiring
                    },
                )
            })
            .collect()
    }

    fn family_output<T>(wiring: Wiring) -> T
    where
        T: serde::de::DeserializeOwned,
    {
        serde_json::from_value(serde_json::to_value(wiring).unwrap()).unwrap()
    }

    fn wiring_of(observed: &[(CorePack, Wiring)], pack: CorePack) -> Wiring {
        observed
            .iter()
            .find(|(observed, _)| *observed == pack)
            .map(|(_, wiring)| *wiring)
            .unwrap()
    }

    /// Lean `Pack.CorePacks.initWiring` replayed against a home's core packs
    /// right after init, with or without inference.
    fn replay_init_wiring(at_init: &[(CorePack, Wiring)]) {
        for case in families::pack_core_packs_init_wiring::cases() {
            let pack: CorePack = case.input.core_pack;
            case.check(family_output(wiring_of(at_init, pack)));
        }
    }

    #[tokio::test]
    async fn a_fresh_engineer_without_archives_is_refused_before_its_bootstrap() {
        let home = tempfile::tempdir().unwrap();
        let error = crate::commands::init::init_home_with_core_packs(
            init_args(home.path(), &[]),
            &[],
            InitEntryPoint::Cli,
        )
        .await
        .unwrap_err();
        let refusal = error.downcast_ref::<MissingCorePackArchives>().unwrap();
        assert_eq!(
            refusal.missing,
            CorePack::ALL.map(CorePack::coordinate).join(", ")
        );
        assert_eq!(
            refusal.to_string(),
            "this build cannot initialize the Engineer: missing core pack archives: gents/pack_builder, gents/web, gents/dynamic_workflows, gents/charts, gents/data_tables, gents/script; run `make core-packs && make build-cli` and retry, or pass `--no-default-packs` to opt out, leaving the Engineer without skills"
        );
        assert!(!home
            .path()
            .join(gents_store::local::home::BOOTSTRAP_CONSUMED_FILE_NAME)
            .exists());
        let initialized = crate::commands::init::init_home_with_core_packs(
            init_args(home.path(), &["--no-default-packs"]),
            &[],
            InitEntryPoint::Cli,
        )
        .await
        .unwrap();
        assert_eq!(initialized["status"], "initialized");
    }

    #[tokio::test]
    async fn an_existing_home_never_needs_archives() {
        let home = tempfile::tempdir().unwrap();
        crate::commands::init::init_home_with_core_packs(
            init_args(home.path(), &["--no-default-packs"]),
            &[],
            InitEntryPoint::Cli,
        )
        .await
        .unwrap();
        let initialized = crate::commands::init::init_home_with_core_packs(
            init_args(home.path(), &[]),
            &[],
            InitEntryPoint::Cli,
        )
        .await
        .unwrap();
        assert_eq!(initialized["status"], "initialized");
    }

    #[test]
    fn a_refusal_names_only_the_missing_archives_and_a_remedy_its_caller_can_take() {
        let archives = [
            ("gents/pack_builder", &b"pack"[..]),
            ("gents/web", &b"pack"[..]),
        ];
        let refused = |entry| admit_init(&archives, entry, true, true, true).unwrap_err();
        assert_eq!(
            refused(InitEntryPoint::Start).to_string(),
            "this build cannot initialize the Engineer: missing core pack archives: gents/dynamic_workflows, gents/charts, gents/data_tables, gents/script; run `make core-packs && make build-cli` and retry"
        );
        assert_eq!(
            refused(InitEntryPoint::Desktop).to_string(),
            "this build cannot initialize the Engineer: missing core pack archives: gents/dynamic_workflows, gents/charts, gents/data_tables, gents/script; this desktop dev build is incomplete; run `make desktop-native-dev`"
        );
    }

    /// The archive is resolved from the home's files before its store is
    /// opened, so a build that does not carry a pack reports that without
    /// opening and draining the store: on a home with no store at all the
    /// answer is the missing archive, not the store.
    #[tokio::test]
    async fn a_pack_the_build_does_not_carry_is_reported_before_the_store_opens() {
        let home = tempfile::tempdir().unwrap();
        let error = apply(
            home.path(),
            "did:key:zEngineer",
            CorePack::Web,
            &[],
            core_packs::init_wiring(CorePack::Web),
            Consent::Init,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(format!("{error:#}"), not_carried(CorePack::Web));
    }

    /// Only installing a pack loads the node's runtime key, so re-applying
    /// an installed one, as enabling it does on a served home, never reads
    /// the key.
    #[tokio::test]
    async fn reapplying_an_installed_pack_never_loads_the_runtime_key() {
        let archives = gents_pack::test_support::skill_only_core_pack_archives();
        let archives = borrowed(&archives);
        let home = tempfile::tempdir().unwrap();
        let initialized = crate::commands::init::init_home_with_core_packs(
            init_args(home.path(), &[]),
            &archives,
            InitEntryPoint::Cli,
        )
        .await
        .unwrap();
        let engineer = initialized["default_agent_did"]
            .as_str()
            .unwrap()
            .to_owned();
        let source = gents::core_packs::resolve(home.path(), CorePack::Web, &archives, None)
            .await
            .unwrap();
        let (access, owner) = command::resolve_scope_owner(&scope(home.path()))
            .await
            .unwrap();
        let unloadable = || -> Result<std::sync::Arc<dyn gents_store::identity::NodeIdentity>> {
            anyhow::bail!("the runtime key was loaded")
        };
        let applied = gents::core_packs::apply(
            &target(
                &access,
                &owner,
                home.path(),
                &engineer,
                None,
                CorePack::Web,
                &unloadable,
            ),
            &source,
            core_packs::enabled_wiring(CorePack::Web),
            Consent::Explicit,
        )
        .await;
        release(access).await;
        assert_eq!(applied.unwrap()["pack"], CorePack::Web.coordinate());
    }

    /// The real `init --engineer` and `gents config pack starter --enable`
    /// replay Lean `Pack.CorePacks`: init installs every core pack the
    /// release carries without asking and leaves each as `initWiring` says;
    /// enabling leaves each as `enabledWiring` says.
    #[tokio::test]
    async fn first_run_installs_packs_and_releases_the_home_for_its_service() {
        let archives = gents_pack::test_support::skill_only_core_pack_archives();
        let archives = borrowed(&archives);
        let home = tempfile::tempdir().unwrap();
        let initialized = crate::commands::init::init_home_with_core_packs(
            init_args(home.path(), &[]),
            &archives,
            InitEntryPoint::Cli,
        )
        .await
        .unwrap();
        let report = &initialized["default_packs"];
        assert_eq!(report["not_installed"], json!([]), "{report}");
        assert_eq!(
            report["installed"].as_array().unwrap().len(),
            CorePack::ALL.len()
        );
        let installed = status(home.path(), &archives).await.unwrap();
        assert_eq!(installed.len(), CorePack::ALL.len());
        assert!(
            installed
                .iter()
                .all(|pack| matches!(pack.state, PackState::Installed | PackState::NotEnabled)),
            "{installed:?}"
        );
        let data = gents_service::home_state::default_data_dir(home.path());
        let claim = gents_store::local::home::lock_store(home.path(), &data)
            .expect("provisioning and all pack readers released the service's home");
        drop(claim);
    }

    #[tokio::test]
    async fn init_and_enable_leave_the_core_packs_as_the_model_says() {
        let archives = fixture_archives().await;
        let archives = borrowed(&archives);
        let home = tempfile::tempdir().unwrap();
        let initialized = crate::commands::init::init_home_with_core_packs(
            init_args(home.path(), &[]),
            &archives,
            InitEntryPoint::Cli,
        )
        .await
        .unwrap();
        let report = &initialized["default_packs"];
        assert_eq!(report["not_installed"], json!([]), "{report}");
        let at_init = observed(home.path(), &report["installed"]).await;
        replay_init_wiring(&at_init);

        let listed = status(home.path(), &archives).await.unwrap();
        for pack in &listed {
            let expected = if pack.pack == CorePack::DynamicWorkflows.coordinate() {
                PackState::NotEnabled
            } else {
                PackState::Installed
            };
            assert_eq!(pack.state, expected, "{listed:?}");
        }

        let enabled = enable(home.path(), &archives, &CorePack::ALL, None)
            .await
            .unwrap();
        assert_eq!(enabled["failed"], json!([]), "{enabled}");
        let after_enable = observed(home.path(), &enabled["enabled"]).await;
        for case in families::pack_core_packs_enabled_wiring::cases() {
            let pack: CorePack = case.input.core_pack;
            case.check(family_output(wiring_of(&after_enable, pack)));
        }
        let listed = status(home.path(), &archives).await.unwrap();
        assert!(
            listed.iter().all(|pack| pack.state == PackState::Installed),
            "{listed:?}"
        );
        let workflows = enabled["enabled"]
            .as_array()
            .unwrap()
            .iter()
            .find(|report| report["pack"] == CorePack::DynamicWorkflows.coordinate())
            .unwrap();
        let receipts = workflows["dynamic_enabled"].as_array().unwrap();
        assert!(
            !receipts.is_empty()
                && receipts.iter().all(|receipt| {
                    receipt["before"]["dynamic"] == "check" && receipt["after"]["dynamic"] == "run"
                }),
            "the enable receipt shows each block before and after: {workflows}"
        );
    }

    /// Status reads a disabled agent as not enabled, saying so, and a plugin
    /// record it cannot read as unknown, without failing.
    #[tokio::test]
    async fn status_names_a_disabled_agent_and_an_unreadable_record() {
        let archives = fixture_archives().await;
        let archives = borrowed(&archives);
        let home = tempfile::tempdir().unwrap();
        crate::commands::init::init_home_with_core_packs(
            init_args(home.path(), &[]),
            &archives,
            InitEntryPoint::Cli,
        )
        .await
        .unwrap();
        enable(home.path(), &archives, &[CorePack::DynamicWorkflows], None)
            .await
            .unwrap();
        let state_of = |listed: &[PackStatus], pack: CorePack| {
            listed
                .iter()
                .find(|status| status.pack == pack.coordinate())
                .cloned()
                .unwrap()
        };
        let (access, owner) = command::resolve_scope_owner(&scope(home.path()))
            .await
            .unwrap();
        gents::core_packs::set_pack_agents_active(
            &access,
            &owner,
            CorePack::DynamicWorkflows.coordinate(),
            false,
        )
        .await
        .unwrap();
        release(access).await;
        let workflows = state_of(
            &status(home.path(), &archives).await.unwrap(),
            CorePack::DynamicWorkflows,
        );
        assert_eq!(workflows.state, PackState::NotEnabled, "{workflows:?}");
        assert_eq!(
            workflows.reason.as_deref(),
            Some("an agent it ships is disabled")
        );

        let record = home
            .path()
            .join(gents_store::local::home::PLUGINS_DIR_NAME)
            .join("installed/gents/workflow.json");
        std::fs::write(&record, b"not a record").unwrap();
        status(home.path(), &archives)
            .await
            .expect_err("a plugin store that does not open fails status");
        std::fs::remove_file(&record).unwrap();
        let listed = status(home.path(), &archives).await.unwrap();
        let workflows = state_of(&listed, CorePack::DynamicWorkflows);
        assert_eq!(workflows.state, PackState::Unknown, "{workflows:?}");
        assert!(workflows.reason.is_some(), "{workflows:?}");
        assert_eq!(
            state_of(&listed, CorePack::Web).state,
            PackState::Installed,
            "{listed:?}"
        );
    }

    /// A home set up before any inference provider exists, as the desktop
    /// sets one up: every core pack is installed, a required inference slot
    /// bound to the Engineer's own profile while its backend is disabled
    /// (Lean `Pack.CorePacks.slotBinding`), and dynamic workflows waits to be
    /// turned on.
    #[tokio::test]
    async fn init_without_inference_installs_every_core_pack() {
        let archives = fixture_archives().await;
        let archives = borrowed(&archives);
        let home = tempfile::tempdir().unwrap();
        let cli = crate::cli::Cli::try_parse_from([
            "gents",
            "init",
            "--store-key-custody",
            "file",
            "--engineer",
            "--home",
            home.path().to_str().unwrap(),
        ])
        .unwrap();
        let Some(crate::cli::Command::Init(args)) = cli.command else {
            panic!("expected init")
        };
        let initialized =
            crate::commands::init::init_home_with_core_packs(args, &archives, InitEntryPoint::Cli)
                .await
                .unwrap();
        let report = &initialized["default_packs"];
        assert_eq!(report["not_installed"], json!([]), "{report}");
        let at_init = observed(home.path(), &report["installed"]).await;
        replay_init_wiring(&at_init);
        let listed = status(home.path(), &archives).await.unwrap();
        for pack in &listed {
            let expected = if pack.pack == CorePack::DynamicWorkflows.coordinate() {
                PackState::NotEnabled
            } else {
                PackState::Installed
            };
            assert_eq!(pack.state, expected, "{listed:?}");
        }
    }

    /// A core pack is installed as the Engineer, so what it ships is the
    /// Engineer's to use and to remove (gents-ai/packs#61): the pack's agent
    /// is in its agent list, it targets and wires that agent, and it removes
    /// the pack with everything the pack left.
    #[tokio::test]
    async fn the_engineer_owns_the_core_packs_it_starts_with() {
        let home = tempfile::tempdir().unwrap();
        let initialized =
            crate::commands::init::init_home(init_args(home.path(), &["--no-default-packs"]))
                .await
                .unwrap();
        let engineer = initialized["default_agent_did"]
            .as_str()
            .unwrap()
            .to_owned();
        let afb = tokio::task::spawn_blocking(|| {
            crate::commands::plugin::testing::build_plugin_afb(
                "probe",
                b"fn main() { println!(\"{{}}\"); }",
            )
        })
        .await
        .unwrap();
        let packs = tempfile::tempdir().unwrap();
        let crew = packs.path().join("web");
        write_pack(
            &crew,
            json!({
                "manifest_version": 1, "name": "web", "namespace": "gents",
                "version": "1.0.0", "description": "An agent, its target and a plugin",
                "authors": ["tests"], "tags": [], "kind": "documents",
                "assets": ["README.md", "pack_config.json", "plugins/probe.afb"],
                "dependencies": [], "config": "pack_config.json",
                "inference_slots": [{"name": "worker", "description": "The worker.",
                    "agents": ["crew-worker"]}],
                "plugins": [plugin_entry("probe", json!({"AllowList": ["SOME_VAR"]}), false)],
            }),
            &[
                ("plugins/probe.afb", afb),
                (
                    "pack_config.json",
                    serde_json::to_vec(&json!({
                        "node": {},
                        "agents": [{"agent_slot": "crew-worker", "context_id": "crew-context",
                            "display_name": "Crew worker", "description": "Does one task.",
                            "inference_profile_id": "gents:inference-slot:worker"}],
                        "contexts": [{"context_id": "crew-context", "system_prompt": "Work.",
                            "tools_id": "crew-tools"}],
                        "tools": [{"tools_id": "crew-tools", "display_name": "Read-only files",
                            "host": {"files": {"mode": "ReadOnly"}}}],
                        "agent_targets": [{"agent_did": "gents:agent-slot:crew-worker",
                            "name": "crew-worker", "target_id": "crew-worker",
                            "target_node_did": "${GENTS_PACK_NODE_DID}",
                            "description": "The crew worker."}],
                    }))
                    .unwrap(),
                ),
            ],
        );
        let archives = [("gents/web".to_owned(), archive(&crew))];
        let archives = borrowed(&archives);

        let listed = status(home.path(), &archives).await.unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|pack| (pack.pack, pack.state))
                .collect::<Vec<_>>(),
            CorePack::ALL.map(|pack| (
                pack.coordinate(),
                if pack == CorePack::Web {
                    PackState::Missing
                } else {
                    PackState::Unavailable
                }
            )),
            "{listed:?}"
        );
        let report = enable(home.path(), &archives, &[CorePack::Web], None)
            .await
            .unwrap();
        assert_eq!(report["failed"], json!([]), "{report}");
        let report = &report["enabled"];
        assert_eq!(report[0]["wired"][0]["plugin"], "gents/probe", "{report}");
        assert_eq!(
            gents_pack::pack::read_home_install(home.path(), "gents/web")
                .unwrap()
                .unwrap()
                .installed_via,
            Some(gents_pack::pack::InstalledVia::Operator),
            "an install under the Engineer's pack authority records operator"
        );

        let (access, owner) = command::resolve_scope_owner(&scope(home.path()))
            .await
            .unwrap();
        let runtime = runtime_identity(home.path());
        let config = gents::core_packs::engineer_config(&target(
            &access,
            &owner,
            home.path(),
            &engineer,
            None,
            CorePack::Web,
            &runtime,
        ))
        .await
        .unwrap();

        // The pack's agent runs host tools under the Engineer's tool root:
        // its Tools took that root at install, and reading as itself it finds
        // the published root that covers it, which is what the runtime's
        // execution boundary asks for.
        let engineer_root = initialized["tool_root"].as_str().unwrap();
        let granted = &report[0]["host_tool_roots"];
        assert_eq!(granted.as_array().map(Vec::len), Some(1), "{report}");
        let worker = granted[0]["agent"].as_str().unwrap().to_owned();
        let node = access.local_node().unwrap().clone();
        let stored = gents_service::home_state::read_init_config(home.path())
            .unwrap()
            .unwrap();
        let runtime =
            gents_service::home_state::load_initialized_home_identity(home.path(), &stored)
                .unwrap();
        let binding = gents::core_packs::agent_identity_binding(&access, &owner, &worker)
            .await
            .unwrap();
        let worker_identity =
            gents_store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap();
        let as_worker = gents_store::config_client::ConfigAccess::local_managed_agent(
            node,
            identity::Did::new(&worker).unwrap(),
            worker_identity.as_ref(),
        )
        .unwrap();
        let reader = gents_tools::surface::agent_tool_root_reader(&as_worker, &owner, &worker)
            .await
            .unwrap();
        let gents_tools::surface::ToolRootReader::Published(published) = reader else {
            panic!("the pack's agent reads no published root covering its tool root: {reader:?}")
        };
        assert_eq!(granted[0]["reader_on"], published.as_str(), "{report}");
        assert!(
            std::path::Path::new(engineer_root).starts_with(&published)
                || std::fs::canonicalize(engineer_root)
                    .unwrap()
                    .starts_with(&published),
            "{engineer_root} is not under {published}"
        );
        drop(as_worker);

        let call = |call: Value| {
            let config = &config;
            async move {
                let answer = config
                    .call(
                        call.to_string(),
                        &gents_loop::tool::ToolCallEnvelope::default(),
                    )
                    .await;
                let answer = answer.unwrap_or_else(|error| panic!("{call}: {error}"));
                serde_json::from_str::<Value>(&answer).unwrap()
            }
        };
        let agents = call(json!({"argv": ["agent", "list"]})).await;
        let worker = agents["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|agent| agent["display_name"] == "Crew worker")
            .unwrap_or_else(|| panic!("the Engineer lists the pack's agent: {agents}"))
            ["agent_did"]
            .clone();
        call(
            json!({"argv": ["agent-target", "create"], "target_id": "crew-helper",
            "set": {"agent_did": worker, "name": "crew-helper"}}),
        )
        .await;
        call(json!({"argv": ["tools", "update"],
            "set": {"agents": {"enabled": true, "target_ids": ["engineer", "crew-helper", "crew-worker"]}}}))
        .await;
        let tools = call(json!({"argv": ["tools", "get"]})).await;
        assert_eq!(
            tools["document"]["agents"]["target_ids"],
            json!(["engineer", "crew-helper", "crew-worker"]),
            "{tools}"
        );
        let refused = |call: Value| {
            let config = &config;
            async move {
                config
                    .call(
                        call.to_string(),
                        &gents_loop::tool::ToolCallEnvelope::default(),
                    )
                    .await
                    .unwrap_err()
                    .to_string()
            }
        };
        // The Engineer wires and unwires a pack itself, through the owner the
        // operator's command uses; a preview writes nothing.
        let unwired = call(json!({"argv": ["pack", "wire", "gents/web", "--remove"]})).await;
        assert_eq!(unwired["committed"], true, "{unwired}");
        assert_eq!(
            unwired["wired"][0]["changed"][0], "removed plugin gents/probe",
            "{unwired}"
        );
        let preview = call(json!({"argv": ["pack", "preview", "wire", "gents/web"]})).await;
        assert_eq!(preview["committed"], false, "{preview}");
        assert_eq!(
            preview["apply_with"]["argv"],
            json!(["pack", "wire", "gents/web"]),
            "{preview}"
        );
        let unselected = call(json!({"argv": ["tools", "get"]})).await;
        assert_eq!(
            unselected["document"]["integrations"]["plugins"]
                .as_array()
                .map_or(0, Vec::len),
            0,
            "{unselected}"
        );
        let rewired = call(preview["apply_with"].clone()).await;
        assert!(
            rewired["wired"][0]["changed"][0]
                .as_str()
                .unwrap()
                .starts_with("added plugin gents/probe at sha256:"),
            "{rewired}"
        );
        let selected = call(json!({"argv": ["tools", "get"]})).await;
        assert_eq!(
            selected["document"]["integrations"]["plugins"][0]["plugin"], "gents/probe",
            "{selected}"
        );

        let still_wired = refused(json!({"argv": ["pack", "remove", "gents/web"]})).await;
        assert!(
            still_wired.contains("gents/web was not removed")
                && still_wired.contains("references missing AgentTarget")
                && still_wired.contains("crew-worker"),
            "{still_wired}"
        );

        // The Engineer removes the pack it was given. What it made around
        // the pack is its own to delete first: a target of its own that names
        // the pack's agent would be left dangling. A plugin its Tools still
        // select refuses the retirement and names the unwiring call; unwired,
        // the pack goes with its agent, its target and its plugin.
        call(
            json!({"argv": ["tools", "update"], "options": {"allow-drop": "agents"},
            "set": {"agents": {"enabled": true, "target_ids": ["engineer"]}}}),
        )
        .await;
        let delete =
            call(json!({"argv": ["agent-target", "preview", "delete", "crew-helper"]})).await;
        call(delete["apply_with"].clone()).await;
        let selected = refused(json!({"argv": ["pack", "remove", "gents/web"]})).await;
        assert!(
            selected.contains("gents/web was not removed (gents/probe): selected by Tools")
                && selected.contains(r#""argv":["pack","wire","gents/web","--remove"]"#),
            "{selected}"
        );
        call(json!({"argv": ["pack", "wire", "gents/web", "--remove"]})).await;
        let removed = call(json!({"argv": ["pack", "remove", "gents/web"]})).await;
        let agents = call(json!({"argv": ["agent", "list"]})).await;
        assert!(
            !agents.to_string().contains("Crew worker"),
            "{removed}\n{agents}"
        );
        let targets = call(json!({"argv": ["agent-target", "list"]})).await;
        assert_eq!(
            targets["items"].as_array().map(Vec::len),
            Some(1),
            "only the Engineer's own target is left: {targets}"
        );
        assert!(
            gents_pack::plugin::store::read_record(home.path(), "gents", "probe").is_err(),
            "the retired plugin has no record: {removed}"
        );
        assert_eq!(
            gents_pack::pack::read_home_install(home.path(), "gents/web").unwrap(),
            None,
            "{removed}"
        );
        drop(config);
        if let Some(node) = access.local_node() {
            node.shutdown().await.expect("node shutdown");
        }
    }
}
