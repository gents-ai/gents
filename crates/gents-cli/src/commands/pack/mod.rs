pub(crate) mod account;
pub(crate) mod build;
mod cache;
mod check;
mod cli_process;
mod edit;
mod import;
mod inspect;
mod local;
pub(crate) mod registry;
mod remove;
pub(crate) use remove::{ensure_home_plugins_unreferenced, remove, remove_home_install_with_guard};
mod scaffold;
mod scenario;
mod server;
mod test;
pub(crate) mod update;
pub(crate) mod wire;
use crate::cli::*;
use anyhow::{Context, Result};
use futures_util::StreamExt;
use gents::pack::{PackKind, PackManifest};
use gents::pack_archive::PackArchive;
use gents::pack_resolve::{resolve_named, ResolveOptions, ResolvedFrom};
use serde_json::json;
use std::collections::BTreeMap;

use gents::pack::materialize::CacheRelease;

pub(crate) fn parse_inference_slot_bindings(
    values: &[String],
) -> Result<gents::pack::PackInferenceBindings> {
    let mut bindings = BTreeMap::new();
    for value in values {
        let (slot, profile_id) = value.split_once('=').with_context(|| {
            format!("invalid inference slot binding {value:?}; expected NAME=PROFILE_ID")
        })?;
        anyhow::ensure!(
            !slot.trim().is_empty() && !profile_id.trim().is_empty(),
            "invalid inference slot binding {value:?}; slot and profile ID must not be blank"
        );
        anyhow::ensure!(
            bindings
                .insert(slot.trim().to_owned(), profile_id.trim().to_owned())
                .is_none(),
            "inference slot {slot:?} was bound more than once"
        );
    }
    Ok(bindings)
}

pub(crate) async fn dispatch(command: PackCommand) -> Result<()> {
    match command {
        PackCommand::List(args) => list(args).await,
        PackCommand::Show(args) => inspect::show(args).await,
        PackCommand::Verify(args) => inspect::verify(args),
        PackCommand::New(args) => scaffold::new(args),
        PackCommand::Init(args) => scaffold::init(args),
        PackCommand::Import(args) => import::import(args).await,
        PackCommand::Add(args) => edit::add(args),
        PackCommand::RemovePart(args) => edit::remove_part(args),
        PackCommand::Fmt(args) => edit::fmt(args),
        PackCommand::Diff(args) => inspect::diff(args).await,
        PackCommand::Test(args) => test::test(args).await,
        PackCommand::Check(args) => check::check(args).await,
        PackCommand::Install(args) => install(args).await,
        PackCommand::Wire(args) => wire::wire(args).await,
        PackCommand::Remove(args) => remove::remove(args).await,
        PackCommand::Outdated(args) => update::outdated(args).await,
        PackCommand::Update(args) => update::update(args).await,
        PackCommand::Prune(args) => cache::prune(args).await,
        PackCommand::Scenario(PackScenarioCommand::Run(args)) => scenario::run(args).await,
        PackCommand::Scenario(PackScenarioCommand::Init(args)) => scenario::init_pack(args).await,
        PackCommand::Scenario(PackScenarioCommand::Seed(args)) => scenario::seed(args).await,
        PackCommand::Build(args) => build::dispatch(args),
        PackCommand::Search(args) => registry::search(args).await,
        PackCommand::Info(args) => account::info(args).await,
        PackCommand::Login(args) => account::login(args).await,
        PackCommand::Logout(args) => account::logout(args),
        PackCommand::Whoami(args) => account::whoami(args).await,
        PackCommand::Yank(args) => account::yank(args).await,
        PackCommand::Owner(args) => account::owner(args).await,
        PackCommand::Publish(args) => registry::publish(args).await,
        PackCommand::Fetch(args) => registry::fetch(args).await,
    }
}

pub(crate) struct PackSource {
    archive: std::sync::Arc<PackArchive>,
    from: Source,
}

enum Source {
    Local,
    Installed,
    Store,
    Registry {
        namespace: String,
        name: String,
        version: String,
        artifact_digest: String,
    },
}

impl PackSource {
    /// A verified archive the runtime received from its operator.
    pub(crate) fn received(archive: PackArchive) -> Self {
        Self {
            archive: archive.into(),
            from: Source::Local,
        }
    }

    pub(crate) fn shared_archive(&self) -> std::sync::Arc<PackArchive> {
        self.archive.clone()
    }

    pub(crate) fn manifest(&self) -> &PackManifest {
        self.archive.manifest()
    }

    pub(crate) fn digest(&self) -> &str {
        self.archive.digest()
    }

    pub(crate) fn asset(&self, path: &str) -> Result<&[u8]> {
        self.archive.asset(path)
    }

    pub(crate) fn archive(&self) -> &PackArchive {
        &self.archive
    }

    pub(crate) fn label(&self) -> &'static str {
        match &self.from {
            Source::Local => "local",
            Source::Installed => "installed",
            Source::Store => "store",
            Source::Registry { .. } => "registry",
        }
    }

    pub(crate) fn describe(&self) -> String {
        match &self.from {
            Source::Registry {
                namespace,
                name,
                version,
                artifact_digest,
            } => format!(
                "{} ({namespace}/{name}@{version}, artifact sha256:{artifact_digest})",
                self.label()
            ),
            _ => format!("{} ({})", self.label(), self.digest()),
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};

    pub(crate) fn fixture_dir(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../gents/tests/fixtures/packs")
            .join(name)
    }

    pub(crate) fn every_fixture_dir() -> Vec<PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../gents/tests/fixtures/packs");
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
            .unwrap_or_else(|error| panic!("reading {}: {error:#}", root.display()))
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.join("manifest.json").is_file())
            .collect();
        dirs.sort();
        dirs
    }

    pub(crate) fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let target = to.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy_tree(&entry.path(), &target)?;
            } else {
                std::fs::copy(entry.path(), &target)?;
            }
        }
        Ok(())
    }

    pub(crate) fn build_unbuilt_plugins(dir: &Path, manifest: &super::PackManifest) {
        for plugin in &manifest.metadata.plugins {
            if plugin.source.is_some() && !dir.join(&plugin.artifact).is_file() {
                super::build::build_plugin(dir, manifest, plugin)
                    .unwrap_or_else(|error| panic!("building plugin {}: {error:#}", plugin.name));
            }
        }
    }

    pub(crate) fn local_pack_source(dir: &Path, home: &Path) -> super::PackSource {
        let archive = super::local::open(&super::local::LocalName::Path(dir), home)
            .unwrap_or_else(|error| panic!("opening {}: {error:#}", dir.display()));
        super::PackSource {
            archive: archive.into(),
            from: super::Source::Local,
        }
    }

    pub(crate) fn fixture_pack_source(name: &str, home: &Path) -> super::PackSource {
        local_pack_source(&fixture_dir(name), home)
    }

    pub(crate) fn assets_pack_dir(namespace: &str, name: &str, version: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        copy_tree(&fixture_dir("assets_fixture"), dir.path()).unwrap();
        let path = dir.path().join("manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        manifest["namespace"] = namespace.into();
        manifest["name"] = name.into();
        manifest["version"] = version.into();
        std::fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        dir
    }
}

pub(crate) fn split_namespace(name: &str) -> (&str, &str) {
    gents::pack_registry::split_pack_coordinate(name)
}

async fn list(args: PackListArgs) -> Result<()> {
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let store = gents::pack_store::PackStore::new(&home);
    let (records, skipped) = gents::pack::list_home_installs_with_skips(&home)?;
    let installed: std::collections::BTreeSet<String> = records
        .into_iter()
        .map(|record| record.coordinate)
        .collect();
    let mut entries = Vec::new();
    for (coordinate, versions) in store.names()? {
        let (namespace, name) = coordinate
            .split_once('/')
            .context("malformed pack store name index entry")?;
        let Some(preferred) = store.lookup(namespace, name, None)? else {
            continue;
        };
        let archive = store.open(&preferred.digest)?;
        let manifest = archive.manifest();
        entries.push(json!({
            "pack": coordinate,
            "versions": versions.into_iter().map(|entry| entry.version).collect::<Vec<_>>(),
            "version": preferred.version,
            "digest": preferred.digest,
            "kind": manifest.metadata.kind,
            "description": manifest.description,
            "inference_slots": manifest.metadata.inference_slots,
            "installed": installed.contains(&coordinate),
        }));
    }
    let mut output = json!({ "packs": entries });
    if !skipped.is_empty() {
        output["skipped_installs"] = skipped
            .iter()
            .map(|skip| json!({ "path": skip.path.display().to_string(), "reason": skip.reason }))
            .collect();
    }
    crate::print_json(&output)
}

pub(crate) async fn resolve_pack_source(
    spec: &str,
    registry_override: Option<&str>,
    home: &std::path::Path,
) -> Result<PackSource> {
    if let Some(local) = local::classify(spec) {
        let archive = local::open(&local, home)?;
        return Ok(PackSource {
            archive: archive.into(),
            from: Source::Local,
        });
    }
    let installed: Vec<gents::pack::InstalledPack> = gents::pack::list_home_installs(home)?
        .into_iter()
        .map(|record| gents::pack::InstalledPack {
            coordinate: record.coordinate,
            version: record.version,
            digest: record.digest,
        })
        .collect();
    let options = ResolveOptions {
        home: Some(home),
        registry_url: registry::resolve_registry_url(registry_override),
        installed: &installed,
    };
    let resolved = resolve_named(spec, &options).await?;
    let from = match resolved.from {
        ResolvedFrom::Installed => Source::Installed,
        ResolvedFrom::Store => Source::Store,
        ResolvedFrom::Registry {
            artifact_digest,
            version,
        } => {
            let parsed = gents::pack_resolve::parse_pack_spec(spec)?;
            Source::Registry {
                namespace: parsed.namespace.to_owned(),
                name: parsed.name.to_owned(),
                version,
                artifact_digest,
            }
        }
    };
    Ok(PackSource {
        archive: resolved.archive.into(),
        from,
    })
}

fn materialize_cached_pack(
    home: &std::path::Path,
    pack: &PackSource,
) -> Result<(std::path::PathBuf, gents::file_lock::FileLock)> {
    gents::pack::materialize::cached(home, &pack.archive)
}

pub(crate) struct SubjectPack {
    pub(crate) source: gents::eval::runner::CellSource,
    pub(crate) manifest: PackManifest,
    _lease: Option<gents::file_lock::FileLock>,
}

impl SubjectPack {
    pub(crate) fn directory(&self) -> &std::path::Path {
        let gents::eval::runner::CellSource::Directory(dir) = &self.source;
        dir
    }

    pub(crate) fn default_agent(&self) -> Result<String> {
        let mut agents: Vec<&String> = self
            .manifest
            .metadata
            .inference_slots
            .iter()
            .flat_map(|slot| slot.agents.iter())
            .collect();
        agents.sort();
        agents.dedup();
        match agents.as_slice() {
            [only] => Ok((*only).clone()),
            _ => anyhow::bail!(
                "pack {} declares {} agents in its inference slots; name one as <pack>:<agent>",
                self.manifest.name,
                agents.len()
            ),
        }
    }
}

pub(crate) async fn resolve_subject_pack(
    home: &std::path::Path,
    spec: &str,
    registry: Option<&str>,
) -> Result<SubjectPack> {
    let path = std::path::Path::new(spec);
    if names_a_directory(spec) {
        let manifest_path = path.join("manifest.json");
        let manifest: PackManifest = serde_json::from_slice(
            &std::fs::read(&manifest_path)
                .with_context(|| format!("reading {}", manifest_path.display()))?,
        )
        .with_context(|| format!("parsing {}", manifest_path.display()))?;
        return Ok(SubjectPack {
            source: gents::eval::runner::CellSource::Directory(path.to_path_buf()),
            manifest,
            _lease: None,
        });
    }
    let pack = resolve_pack_source(spec, registry, home).await?;
    let manifest = pack.manifest().clone();
    let (root, lease) = materialize_cached_pack(home, &pack)?;
    Ok(SubjectPack {
        source: gents::eval::runner::CellSource::Directory(root),
        manifest,
        _lease: Some(lease),
    })
}

pub(crate) fn single_slot(manifest: &PackManifest) -> Result<&gents::pack::PackInferenceSlot> {
    match manifest.metadata.inference_slots.as_slice() {
        [only] => Ok(only),
        slots => anyhow::bail!(
            "pack {} declares {} inference slots; expected exactly one",
            manifest.name,
            slots.len()
        ),
    }
}

pub(crate) fn single_slot_agent(manifest: &PackManifest) -> Result<(String, String)> {
    let slot = single_slot(manifest)?;
    match slot.agents.as_slice() {
        [only] => Ok((slot.name.clone(), only.clone())),
        agents => anyhow::bail!(
            "pack {} slot {} declares {} agents; expected exactly one",
            manifest.name,
            slot.name,
            agents.len()
        ),
    }
}

fn names_a_directory(spec: &str) -> bool {
    spec.starts_with(['.', '/', '~'])
        || (spec.contains(std::path::is_separator) && std::path::Path::new(spec).is_dir())
}

fn asset_cache_root(home: &std::path::Path, pack: &PackSource) -> Result<std::path::PathBuf> {
    gents::pack::materialize::cache_root(
        home,
        &pack.manifest().metadata.namespace,
        &pack.manifest().name,
        pack.digest(),
    )
}

/// An uninitialized home has no node, so no runtime to wake: the next start
/// resolves the plugin store as it is.
pub(crate) async fn plugin_notification_scope(
    home: &std::path::Path,
) -> Result<Option<(gents::ConfigAccess, String)>> {
    if !gents::home::init_config_path(home).is_file() {
        return Ok(None);
    }
    resolve_scope_owner(&ScopeArgs {
        home: Some(home.to_owned()),
        graphql: None,
        node_did: None,
    })
    .await
    .map(Some)
}

pub(crate) async fn record_plugin_store_change(
    home: &std::path::Path,
    coordinate: &str,
    installed: Option<&gents::pack::PackIdentity>,
) -> Result<()> {
    if !gents::home::init_config_path(home).is_file() {
        return Ok(());
    }
    let (access, owner) = resolve_scope_owner(&ScopeArgs {
        home: Some(home.to_owned()),
        graphql: None,
        node_did: None,
    })
    .await?;
    let result =
        gents::pack::record_plugin_store_change(&access, &owner, home, coordinate, installed).await;
    if let Some(node) = access.local_node() {
        node.shutdown().await;
    }
    result.map_err(Into::into)
}

pub(crate) fn warn_on_unrecorded_removal(coordinate: &str, recorded: Result<()>) {
    if let Err(error) = recorded {
        tracing::warn!(
            coordinate,
            error = %error,
            "plugins removed, but the node was not told: a running runtime keeps the removed \
             tools listed (their calls fail) until it restarts or its configuration changes",
        );
    }
}

async fn resolve_plugin_slot_owner(
    scope: &ScopeArgs,
    manifest: &PackManifest,
    requested: &BTreeMap<String, String>,
) -> Result<Option<String>> {
    if requested.is_empty() {
        return Ok(None);
    }
    for slot in requested.keys() {
        anyhow::ensure!(
            manifest
                .metadata
                .inference_slots
                .iter()
                .any(|declared| declared.name == *slot),
            "pack {} has no inference slot {slot:?}",
            manifest.name
        );
    }
    let (access, owner) = resolve_scope_owner(scope).await?;
    let result =
        gents::pack::preview_pack_inference_bindings(&access, manifest, &owner, requested).await;
    if let Some(node) = access.local_node() {
        node.shutdown().await;
    }
    result?;
    Ok(Some(owner))
}

pub(crate) async fn resolve_scope_owner(
    scope: &ScopeArgs,
) -> Result<(gents::config_client::ConfigAccess, String)> {
    let (access, _) =
        crate::resolve_config_access(scope.home.as_deref(), scope.graphql.as_deref()).await?;
    let owner = super::config::binding::resolve_target_node_did(
        scope.node_did.as_deref(),
        if scope.node_did.is_some() {
            None
        } else if scope.graphql.is_some() {
            Some(ManifestNodeDidBindingArg::Live)
        } else {
            Some(ManifestNodeDidBindingArg::Home)
        },
        scope.home.as_deref(),
        scope.graphql.as_deref(),
        Some(&access),
    )
    .await?;
    Ok((access, owner))
}

/// The documents pack install: the CLI's single site that writes a documents
/// pack and its dependencies. `explicit` is false only when the pack is
/// installed as another pack's dependency.
pub(crate) async fn install_documents_pack(
    access: &gents::config_client::ConfigAccess,
    owner: &str,
    pack: &PackSource,
    args: &PackInstallArgs,
    explicit: bool,
) -> Result<serde_json::Value> {
    let home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
    let dependencies: Vec<PackSource> =
        futures_util::stream::iter(pack.manifest().metadata.dependencies.iter())
            .map(|coordinate| resolve_pack_source(coordinate, args.registry.as_deref(), &home))
            .buffered(4)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
    if let gents::config_client::ConfigAccess::Graphql(endpoint) = access {
        let mut needs_runtime_store = false;
        if args.scope.graphql.is_some() {
            for source in std::iter::once(pack).chain(dependencies.iter()) {
                if !source.manifest().metadata.plugins.is_empty() {
                    needs_runtime_store = true;
                    break;
                }
                let config = gents::pack::load_pack_config(
                    source.manifest(),
                    &gents::pack::PackInstallOptions {
                        node_did: owner.to_owned(),
                    },
                    &|path| Ok(source.asset(path)?.to_vec()),
                    &|name| std::env::var(name).ok(),
                )?;
                if config.tools.iter().any(|tools| {
                    tools
                        .integrations
                        .as_ref()
                        .and_then(|integrations| integrations.plugins.as_ref())
                        .is_some_and(|plugins| !plugins.is_empty())
                }) {
                    needs_runtime_store = true;
                    break;
                }
            }
        }
        if needs_runtime_store {
            return crate::http::pack_install::install_remote(
                endpoint,
                owner,
                pack,
                &dependencies,
                args,
                explicit,
            )
            .await;
        }
    }
    install_documents_pack_resolved(access, owner, pack, args, explicit, dependencies).await
}

pub(crate) async fn install_documents_pack_resolved(
    access: &gents::config_client::ConfigAccess,
    owner: &str,
    pack: &PackSource,
    args: &PackInstallArgs,
    explicit: bool,
    dependencies: Vec<PackSource>,
) -> Result<serde_json::Value> {
    let home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
    let requested = parse_inference_slot_bindings(&args.inference_slots)?;
    let temp = tempfile::tempdir()?;
    gents::pack::materialize::materialize(pack.archive(), temp.path())?;
    let (mut authored, mut report) = crate::desired_state::load_manifest_root(temp.path());
    if authored.is_none() {
        (authored, report) =
            crate::desired_state::load_manifest_root_for_owner(temp.path(), Some(owner));
    }
    anyhow::ensure!(
        authored.is_some(),
        "invalid pack configuration: {:?}",
        report.errors
    );
    let mut authored = authored.expect("checked pack configuration");
    super::config::binding::rebind_manifest_to_node(
        &mut authored,
        owner,
        args.force_rebind_concrete_did,
    )?;
    let prepared = gents::pack::prepare_document_pack_install(
        access,
        owner,
        pack.manifest(),
        &authored,
        &requested,
        &dependencies
            .iter()
            .map(PackSource::archive)
            .collect::<Vec<_>>(),
        gents::pack::InstalledVia::Operator,
        args.scope.graphql.is_none().then_some(home.as_path()),
        Some(pack.archive()),
        &|name| std::env::var(name).ok(),
    )
    .await?;
    let inference = prepared.inference.clone();
    let dependency_inference = prepared.dependency_inference.clone();
    let origin_tag = gents::pack::pack_origin_tag(&pack.manifest().name)?;
    if args.preview {
        return Ok(json!({
            "pack": pack.manifest().name,
            "source": pack.label(),
            "digest": pack.digest(),
            "owner": owner,
            "inference": inference,
            "dependency_inference": dependency_inference,
            "origin_tag": origin_tag,
            "dependencies": pack.manifest().metadata.dependencies,
            "would_write": false,
        }));
    }
    // A dependency installed on its own behalf (`explicit` false) writes no
    // home record, and a remote node has no local home to record in.
    let record_home = explicit && args.scope.graphql.is_none();
    // Every pack that ships an asset class is materialized before any
    // write; the leases are held until the home record is written.
    let mut cache_leases = Vec::new();
    let mut materialized_assets = String::new();
    let mut dependency_assets = Vec::new();
    if record_home {
        for dependency in &dependencies {
            if let Some((assets, lease)) =
                gents::pack::home_install::materialize_asset_pack(&home, dependency.archive())?
            {
                dependency_assets.push((
                    format!(
                        "{}/{}",
                        dependency.manifest().metadata.namespace,
                        dependency.manifest().name
                    ),
                    assets,
                ));
                cache_leases.push(lease);
            }
        }
        if let Some((assets, lease)) =
            gents::pack::home_install::materialize_asset_pack(&home, pack.archive())?
        {
            materialized_assets = assets;
            cache_leases.push(lease);
        }
    }
    let dependency_coordinates: Vec<String> = dependencies
        .iter()
        .map(|dependency| {
            format!(
                "{}/{}",
                dependency.manifest().metadata.namespace,
                dependency.manifest().name
            )
        })
        .collect();
    gents::pack::apply_prepared_dependency_schemas(access, &prepared).await?;
    let schemas = super::schema::apply_pack_schemas_if_present(access, temp.path())
        .await
        .context("pack install schemas")?;
    let (apply, plugins) = gents::pack::install_prepared_document_graph(
        access,
        owner,
        pack.archive(),
        &prepared,
        args.drift.policy(),
        args.scope.graphql.is_none().then_some(home.as_path()),
        args.grant_authority,
        explicit,
    )
    .await?;
    let root_coordinate = format!(
        "{}/{}",
        pack.manifest().metadata.namespace,
        pack.manifest().name
    );
    let mut identity = gents::pack::PackIdentity::new(
        pack.manifest(),
        pack.digest(),
        plugins
            .iter()
            .filter(|plugin| {
                plugin.owner_pack_coordinate.as_deref() == Some(root_coordinate.as_str())
            })
            .map(|plugin| gents::pack::InstalledPackPlugin {
                name: plugin.name.clone(),
                digest: plugin.digest.clone(),
            })
            .collect(),
    );
    identity.dependencies = dependency_coordinates;
    identity.explicit = explicit;
    if record_home {
        gents::pack::record_after_install(
            &home,
            gents::pack::HomePackInstall {
                coordinate: identity.coordinate.clone(),
                version: identity.version.clone(),
                digest: identity.digest.clone(),
                kind: PackKind::Documents,
                assets: materialized_assets,
                dependency_assets,
                plugins: identity.plugins.clone(),
                ux: pack.manifest().metadata.ux.clone(),
                installed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                installed_via: Some(gents::pack::InstalledVia::Operator),
            },
            gents::pack::InstalledVia::Operator,
            explicit,
        )
        .context("the pack documents were written, but the home install record was not")?;
    }
    Ok(json!({
        "pack": pack.manifest().name,
        "source": pack.label(),
        "digest": pack.digest(),
        "owner": owner,
        "inference": inference,
        "dependency_inference": dependency_inference,
        "origin_tag": origin_tag,
        "dependencies": pack.manifest().metadata.dependencies,
        "schemas": schemas,
        "apply": apply,
    }))
}

/// Refuses, before any write, a pack whose content digest is not the one the
/// operator reviewed with `gents pack show`.
fn check_expected_digest(expected: Option<&str>, actual: &str) -> Result<()> {
    match expected {
        Some(expected) if expected != actual => anyhow::bail!(
            "the pack's digest is {actual}, not the expected {expected}. Nothing was installed. \
             Next: gents pack show ./dir"
        ),
        _ => Ok(()),
    }
}

/// `--wire` acts on plugins this install places, so a pack that cannot ship
/// one refuses it before any write.
fn check_wire_target(manifest: &PackManifest, wire: &[String]) -> Result<()> {
    if wire.is_empty() {
        return Ok(());
    }
    anyhow::ensure!(
        matches!(
            manifest.metadata.kind,
            PackKind::Documents | PackKind::Plugins
        ),
        "--wire applies only to documents and plugins packs; {} is not one",
        manifest.name
    );
    anyhow::ensure!(
        !manifest.metadata.plugins.is_empty(),
        "--wire needs a pack that ships a plugin; {} ships none",
        manifest.name
    );
    Ok(())
}

/// Wires each plugin `pack` shipped into each of `agents`, agent by agent,
/// through the same writer as `gents pack wire`.
pub(crate) async fn wire_pack_plugins(
    access: &gents::config_client::ConfigAccess,
    owner: &str,
    home: &std::path::Path,
    pack: &PackSource,
    agents: &[String],
) -> Result<Vec<serde_json::Value>> {
    let namespace = &pack.manifest().metadata.namespace;
    let mut wired = Vec::new();
    for agent in agents {
        for plugin in &pack.manifest().metadata.plugins {
            let request = wire::WireRequest {
                plugin: format!("{namespace}/{}", plugin.name),
                agent: agent.clone(),
                targets_from: Vec::new(),
                remove: false,
                preview: false,
            };
            wired.push(wire::wire_in(access, owner, home, &request).await?);
        }
    }
    Ok(wired)
}

/// Offline command access owns its node and store claim. DefraDB background
/// tasks may retain the database after a handle drops, so shutdown must finish
/// before this command releases its access. HTTP access has no owned node here.
async fn install_documents_command(
    access: gents::config_client::ConfigAccess,
    pack: &PackSource,
    args: &PackInstallArgs,
) -> Result<()> {
    let result = async {
        let home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
        let bind_mode = if args.scope.graphql.is_some() {
            Some(ManifestNodeDidBindingArg::Live)
        } else {
            Some(ManifestNodeDidBindingArg::Home)
        };
        let owner = super::config::binding::resolve_target_node_did(
            args.scope.node_did.as_deref(),
            if args.scope.node_did.is_some() {
                None
            } else {
                bind_mode
            },
            args.scope.home.as_deref(),
            args.scope.graphql.as_deref(),
            Some(&access),
        )
        .await?;
        let mut output = install_documents_pack(&access, &owner, pack, args, args.explicit).await?;
        if !args.wire.is_empty() && !args.preview && args.scope.graphql.is_none() {
            output["wired"] = wire_pack_plugins(&access, &owner, &home, pack, &args.wire)
                .await?
                .into();
        }
        Ok::<_, anyhow::Error>(output)
    }
    .await;
    if let Some(node) = access.local_node() {
        node.shutdown().await;
    }
    crate::print_json(&result?)
}

pub(crate) async fn install(args: PackInstallArgs) -> Result<()> {
    let home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
    let pack = resolve_pack_source(&args.package, args.registry.as_deref(), &home).await?;
    tracing::info!(package = %args.package, source = %pack.describe(), "resolved pack");
    check_expected_digest(args.expect_digest.as_deref(), pack.digest())?;
    check_wire_target(pack.manifest(), &args.wire)?;
    args.output.ensure_supported(
        "pack install",
        &[crate::cli::output_format::OutputFormat::Json],
    )?;
    match pack.manifest().metadata.kind {
        PackKind::Documents => {
            let (access, _) = crate::resolve_config_access(
                args.scope.home.as_deref(),
                args.scope.graphql.as_deref(),
            )
            .await?;
            install_documents_command(access, &pack, &args).await
        }
        PackKind::Assets | PackKind::Plugins => {
            let requested = parse_inference_slot_bindings(&args.inference_slots)?;
            anyhow::ensure!(
                args.scope.graphql.is_none()
                    && (args.scope.node_did.is_none() || !requested.is_empty())
                    && !args.force_rebind_concrete_did,
                "asset and plugins packs install locally with --home; identity flags do not apply"
            );
            if args.preview {
                return crate::print_json(&json!({
                    "pack": pack.manifest().name,
                    "source": pack.label(),
                    "digest": pack.digest(),
                    "installed_plugins": pack.manifest().metadata.plugins,
                    "ux_plugins": pack.manifest().metadata.ux,
                    "inference_slots": pack.manifest().metadata.inference_slots,
                    "would_write": false,
                }));
            }
            let slot_owner =
                resolve_plugin_slot_owner(&args.scope, pack.manifest(), &requested).await?;
            let home = args
                .scope
                .home
                .clone()
                .context("asset and plugins packs require --home")?;
            let notification_scope = plugin_notification_scope(&home).await?;
            let notify_scope = notification_scope.clone();
            let notify_home = home.clone();
            let result = gents::pack::plugin_lifecycle::install(
                &home,
                &pack.archive,
                args.grant_authority,
                slot_owner.as_deref(),
                &requested,
                move |identity| {
                    let home = notify_home;
                    async move {
                        if let Some((access, owner)) = notify_scope {
                            gents::pack::record_plugin_store_change(
                                &access,
                                &owner,
                                &home,
                                &identity.coordinate,
                                Some(&identity),
                            )
                            .await
                        } else {
                            Ok(())
                        }
                    }
                },
            )
            .await;
            if let Some((access, _)) = &notification_scope {
                if let Some(node) = access.local_node() {
                    node.shutdown().await;
                }
            }
            let (record, installed_plugins) = result?;
            let root = home.join(&record.assets);
            let mut output = json!({
                "pack": pack.manifest().name,
                "digest": pack.digest(),
                "installed_assets": root,
                "installed_plugins": installed_plugins,
                "record": record,
                "source": pack.label(),
            });
            if !args.wire.is_empty() {
                let (access, owner) = resolve_scope_owner(&args.scope).await?;
                output["wired"] = wire_pack_plugins(&access, &owner, &home, &pack, &args.wire)
                    .await?
                    .into();
            }
            crate::print_json(&output)
        }
    }
}

#[cfg(test)]
mod tests;
