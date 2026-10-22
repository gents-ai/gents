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
pub(crate) use remove::{remove, remove_home_install};
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
        PackCommand::Graph(args) => check::graph(args),
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
    archive: PackArchive,
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
            archive,
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
    let installed: std::collections::BTreeSet<String> =
        gents::pack::installed_packs(Some(&home), None)
            .await?
            .into_iter()
            .map(|pack| pack.coordinate)
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
    crate::print_json(&json!({ "packs": entries }))
}

pub(crate) async fn resolve_pack_source(
    spec: &str,
    registry_override: Option<&str>,
    home: &std::path::Path,
) -> Result<PackSource> {
    if let Some(local) = local::classify(spec) {
        let archive = local::open(&local, home)?;
        return Ok(PackSource {
            archive,
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
        archive: resolved.archive,
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
    gents::pack::record_plugin_store_change(&access, &owner, home, coordinate, installed).await
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
    gents::pack::preview_pack_inference_bindings(&access, manifest, &owner, requested).await?;
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
async fn install_documents_pack(
    access: &gents::config_client::ConfigAccess,
    owner: &str,
    pack: &PackSource,
    args: &PackInstallArgs,
    explicit: bool,
) -> Result<serde_json::Value> {
    let home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
    let requested = parse_inference_slot_bindings(&args.inference_slots)?;
    let dependencies: Vec<PackSource> =
        futures_util::stream::iter(pack.manifest().metadata.dependencies.iter())
            .map(|coordinate| resolve_pack_source(coordinate, args.registry.as_deref(), &home))
            .buffered(4)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
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
        &|name| std::env::var(name).ok(),
    )
    .await?;
    let desired = prepared.config;
    let inference = prepared.inference;
    let dependency_inference = prepared.dependency_inference;
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
    for dependency in &dependencies {
        let dependency_slots = dependency_inference[dependency.manifest().name.as_str()]
            .bindings
            .iter()
            .map(|(slot, profile)| format!("{slot}={profile}"))
            .collect();
        let dependency_args = PackInstallArgs {
            package: dependency.manifest().name.clone(),
            bindings: None,
            inference_slots: dependency_slots,
            preview: false,
            scope: args.scope.clone(),
            output: args.output,
            force_rebind_concrete_did: false,
            registry: args.registry.clone(),
            drift: args.drift,
            grant_authority: args.grant_authority,
            wire: Vec::new(),
            expect_digest: None,
            explicit: false,
        };
        match dependency.manifest().metadata.kind {
            PackKind::Graph => {
                super::graph::install_with_access(
                    access,
                    owner,
                    dependency,
                    dependency_args,
                    false,
                )
                .await?;
            }
            // A dependency installs exactly as `gents pack install <dep>`
            // would; the boxed future breaks the async recursion.
            PackKind::Documents => {
                Box::pin(install_documents_pack(
                    access,
                    owner,
                    dependency,
                    &dependency_args,
                    false,
                ))
                .await?;
            }
            // `prepare_document_pack_install` above already refused these.
            PackKind::Assets | PackKind::Plugins => {
                unreachable!("dependency preparation refuses asset and plugin dependencies")
            }
        }
    }
    let schemas = super::schema::apply_pack_schemas_if_present(access, temp.path())
        .await
        .context("pack install schemas")?;
    let plugins = if pack.manifest().metadata.plugins.is_empty() {
        Vec::new()
    } else {
        anyhow::ensure!(
            args.scope.graphql.is_none(),
            "{} ships plugins, which install on the node's own host; run the install \
             there with --home",
            pack.manifest().name
        );
        gents::plugin::install::install_pack_plugins_with_bindings(
            &home,
            pack.manifest(),
            pack.digest(),
            |path| pack.asset(path),
            args.grant_authority,
            Some(owner),
            &inference.bindings,
            |_| Ok(()),
        )
        .context("publishing document-pack plugins locally")?
    };
    let mut identity = gents::pack::PackIdentity::new(
        pack.manifest(),
        pack.digest(),
        plugins
            .iter()
            .map(|plugin| gents::pack::InstalledPackPlugin {
                name: plugin.name.clone(),
                digest: plugin.digest.clone(),
            })
            .collect(),
    );
    identity.dependencies = dependency_coordinates;
    identity.explicit = explicit;
    // Local plugin publication is complete before the node write.
    // A later transaction failure leaves those files installed for
    // the next explicit reconciliation attempt.
    let apply = gents::pack::install_pack_documents(
        access,
        owner,
        &identity,
        &desired,
        args.drift.policy(),
    )
    .await
    .context("document pack write failed after local plugins were published; the plugins remain installed")?;
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
async fn wire_pack_plugins(
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
        if !args.wire.is_empty() && !args.preview {
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
    let supported_outputs: &[crate::cli::output_format::OutputFormat] =
        if pack.manifest().metadata.kind == PackKind::Graph {
            &[
                crate::cli::output_format::OutputFormat::Text,
                crate::cli::output_format::OutputFormat::Json,
            ]
        } else {
            &[crate::cli::output_format::OutputFormat::Json]
        };
    args.output
        .ensure_supported("pack install", supported_outputs)?;
    match pack.manifest().metadata.kind {
        PackKind::Graph => {
            anyhow::ensure!(
                !args.force_rebind_concrete_did,
                "--force-rebind-concrete-did applies only to document packs"
            );
            super::graph::install(&pack, args, true).await
        }
        PackKind::Documents => {
            anyhow::ensure!(
                args.bindings.is_none(),
                "document packs use --inference-slot; --bindings is a graph installation option"
            );
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
                args.bindings.is_none()
                    && args.scope.graphql.is_none()
                    && (args.scope.node_did.is_none() || !requested.is_empty())
                    && !args.force_rebind_concrete_did,
                "asset and plugins packs install locally with --home; identity and graph binding flags do not apply"
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
            let (record, installed_plugins) = gents::pack::plugin_lifecycle::install(
                &home,
                &pack.archive,
                args.grant_authority,
                slot_owner.as_deref(),
                &requested,
                |identity| {
                    let home = &home;
                    async move {
                        record_plugin_store_change(home, &identity.coordinate, Some(&identity))
                            .await
                    }
                },
            )
            .await?;
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
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_bare_subject_spec_is_a_pack_name_even_when_the_cwd_has_that_directory() {
        assert!(std::path::Path::new("src").is_dir());
        assert!(!names_a_directory("src"));
        assert!(names_a_directory("./src"));
        assert!(names_a_directory("/packs/monitor"));
        assert!(names_a_directory("~/packs/monitor"));
        assert!(names_a_directory("src/commands"));
        assert!(!names_a_directory("acme/widget"));

        let home = tempfile::tempdir().unwrap();
        let unreachable = Some("http://127.0.0.1:9");
        let bare = resolve_subject_pack(home.path(), "src", unreachable)
            .await
            .err()
            .expect("no pack is named src");
        assert!(
            format!("{bare:#}").contains("is not in the pack store of"),
            "{bare:#}"
        );
        let dotted = resolve_subject_pack(home.path(), "./src", unreachable)
            .await
            .err()
            .expect("./src has no manifest");
        assert_eq!(dotted.to_string(), "reading ./src/manifest.json");
    }

    #[test]
    fn scenario_cache_lease_excludes_pruning() {
        let home = tempfile::tempdir().unwrap();
        let pack = test_support::fixture_pack_source("documents_fixture", home.path());
        let (root, lease) = materialize_cached_pack(home.path(), &pack).unwrap();
        let exclusive = gents::pack::materialize::cache_lock(root.parent().unwrap()).unwrap();
        assert!(exclusive.try_lock().is_err());
        drop(lease);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut backoff = std::time::Duration::from_millis(1);
        while let Err(error) = exclusive.try_lock() {
            assert!(
                std::time::Instant::now() < deadline,
                "the exclusive lock stayed blocked after the lease was dropped: {error:?}"
            );
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(std::time::Duration::from_millis(100));
        }
    }

    #[test]
    fn a_document_pack_materializes_a_valid_configuration() {
        let home = tempfile::tempdir().unwrap();
        let pack = test_support::fixture_pack_source("documents_fixture", home.path());
        let root = tempfile::tempdir().unwrap();
        gents::pack::materialize::materialize(pack.archive(), root.path()).unwrap();
        let config = gents::pack::load_pack_config(
            pack.manifest(),
            &gents::pack::PackInstallOptions {
                node_did: "did:key:zPackCatalogValidationOwner".into(),
            },
            &|path| pack.asset(path).map(Vec::from),
            &|_| None,
        )
        .unwrap();
        gents::config_client::DesiredStateApplyPlan::from_pack_config(&config).unwrap();
    }

    #[tokio::test]
    async fn resolution_prefers_the_home_store_without_the_registry() {
        let home = tempfile::tempdir().unwrap();
        let _ = test_support::fixture_pack_source("assets_fixture", home.path());
        let source = resolve_pack_source(
            "fixture/assets_fixture",
            Some("http://127.0.0.1:1"),
            home.path(),
        )
        .await
        .expect("a stored pack must resolve without touching the registry");
        assert_eq!(source.label(), "store");
    }

    #[tokio::test]
    async fn resolution_falls_back_to_the_registry_and_reports_the_offline_sentence() {
        let result = resolve_pack_source(
            "definitely_not_a_stored_pack",
            Some("http://127.0.0.1:1"),
            tempfile::tempdir().unwrap().path(),
        )
        .await;
        let Err(error) = result else {
            panic!("neither the store nor the registry has this pack");
        };
        let message = format!("{error:#}");
        assert!(
            message.contains("is not in the pack store of")
                && message.contains("could not be reached"),
            "{message}"
        );
        assert!(
            message.contains("definitely_not_a_stored_pack"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn plugin_slot_bindings_need_a_declared_slot_and_open_no_store_without_one() {
        let manifest: PackManifest = serde_json::from_value(json!({
            "manifest_version": 1, "name": "ocr", "version": "1.0.0", "description": "d",
            "authors": ["t"], "kind": "plugins", "assets": ["README.md"],
            "inference_slots": [{"name": "remote_ocr", "description": "d", "optional": true}],
        }))
        .unwrap();
        let scope = ScopeArgs {
            home: Some(tempfile::tempdir().unwrap().path().to_owned()),
            graphql: None,
            node_did: None,
        };
        let none = resolve_plugin_slot_owner(&scope, &manifest, &BTreeMap::new()).await;
        assert_eq!(none.unwrap(), None);
        let unknown = BTreeMap::from([("other".to_owned(), "p".to_owned())]);
        let error = resolve_plugin_slot_owner(&scope, &manifest, &unknown)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("no inference slot"),
            "{error:#}"
        );
    }

    fn plugin_manifest(
        namespace: &str,
        pack_name: &str,
        plugin_name: &str,
        version: &str,
    ) -> PackManifest {
        serde_json::from_value(json!({
            "manifest_version": 1, "name": pack_name, "namespace": namespace, "version": version,
            "description": "test plugin pack", "authors": [namespace], "tags": [], "kind": "plugins",
            "assets": ["README.md", format!("plugins/{plugin_name}.afb")],
            "plugins": [{
                "name": plugin_name, "description": "test", "artifact": format!("plugins/{plugin_name}.afb"),
                "language": "rust", "input_schema": {"type": "object"},
            }],
        }))
        .unwrap()
    }

    fn documents_pack_dir(version: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("manifest.json"),
            serde_json::to_vec(&json!({
                "manifest_version": 1, "name": "demo_pack", "version": version,
                "description": "A minimal document pack", "authors": ["tests"], "tags": [],
                "kind": "documents", "assets": ["README.md", "pack_config.json"],
                "dependencies": [], "config": "pack_config.json",
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(dir.path().join("README.md"), "# demo_pack").unwrap();
        std::fs::write(
            dir.path().join("pack_config.json"),
            serde_json::to_vec(&json!({
                "node": { "node_did": "${GENTS_PACK_NODE_DID}" },
                "tools": [{ "tools_id": "demo-tools", "display_name": "No tools" }],
            }))
            .unwrap(),
        )
        .unwrap();
        dir
    }

    async fn initialized_home() -> tempfile::TempDir {
        use clap::Parser;
        let home = tempfile::tempdir().unwrap();
        let cli = crate::cli::Cli::try_parse_from([
            "gents",
            "init",
            "--store-key-custody",
            "file",
            "--node-name",
            "installer",
            "--home",
            home.path().to_str().unwrap(),
        ])
        .unwrap();
        let crate::cli::Command::Init(init_args) = cli.command else {
            panic!("expected init")
        };
        crate::commands::init::init(init_args).await.unwrap();
        home
    }

    fn document_install_args(
        home: &std::path::Path,
        spec: &std::path::Path,
        inference_slots: Vec<String>,
    ) -> PackInstallArgs {
        PackInstallArgs {
            package: spec.to_str().unwrap().to_owned(),
            bindings: None,
            inference_slots,
            preview: false,
            scope: ScopeArgs {
                home: Some(home.to_path_buf()),
                graphql: None,
                node_did: None,
            },
            output: crate::cli::output_format::OutputFormat::Json,
            force_rebind_concrete_did: false,
            registry: None,
            drift: PackDriftArgs::default(),
            grant_authority: false,
            wire: Vec::new(),
            expect_digest: None,
            explicit: true,
        }
    }

    async fn install_from(
        home: &std::path::Path,
        spec: &std::path::Path,
        inference_slots: Vec<String>,
    ) -> Result<serde_json::Value> {
        crate::request_helpers::capture_report(install(document_install_args(
            home,
            spec,
            inference_slots,
        )))
        .await
    }

    #[tokio::test]
    async fn documents_command_closes_its_owned_store_on_success_and_failure() {
        for invalid_binding in [false, true] {
            let home = initialized_home().await;
            let directory = documents_pack_dir("1.0.0");
            let slots = if invalid_binding {
                vec!["malformed".into()]
            } else {
                Vec::new()
            };
            let args = document_install_args(home.path(), directory.path(), slots);
            let pack = resolve_pack_source(&args.package, None, home.path())
                .await
                .unwrap();
            let (access, _) = crate::resolve_config_access(Some(home.path()), None)
                .await
                .unwrap();
            let query = "{ Tools(limit: 1) { _docID } }";
            let before = access.execute(query).await.unwrap();
            assert!(before["data"]["Tools"].is_array(), "{before}");
            let retained_node = access.local_node().unwrap().clone();
            let outcome = crate::request_helpers::capture_report(install_documents_command(
                access, &pack, &args,
            ))
            .await;
            assert_eq!(outcome.is_err(), invalid_binding, "{outcome:?}");
            if let Err(error) = outcome {
                assert!(
                    format!("{error:#}").contains("expected NAME=PROFILE_ID"),
                    "{error:#}"
                );
            }
            let after = gents::config_client::ConfigAccess::local(retained_node.clone())
                .execute(query)
                .await;
            assert!(
                after.is_err(),
                "the command left its owned database running: {after:?}"
            );
            drop(retained_node);
            let (reopened, _) = crate::resolve_config_access(Some(home.path()), None)
                .await
                .expect("command shutdown and handle release must allow immediate store reopen");
            reopened.local_node().unwrap().shutdown().await;
            drop(reopened);
        }
    }

    #[tokio::test]
    async fn documents_install_writes_an_operator_home_record() {
        let home = initialized_home().await;
        let pack = documents_pack_dir("1.0.0");
        install_from(home.path(), pack.path(), Vec::new())
            .await
            .unwrap();

        let record = gents::pack::read_home_install(home.path(), "gents/demo_pack")
            .unwrap()
            .expect("a documents install records itself in the home");
        assert_eq!(record.kind, PackKind::Documents);
        assert_eq!(
            record.installed_via,
            Some(gents::pack::InstalledVia::Operator)
        );
    }

    #[tokio::test]
    async fn documents_install_rolls_back_its_record_on_failure() {
        let home = initialized_home().await;
        let first = documents_pack_dir("1.0.0");
        install_from(home.path(), first.path(), Vec::new())
            .await
            .unwrap();
        let before = gents::pack::read_home_install(home.path(), "gents/demo_pack")
            .unwrap()
            .unwrap();
        {
            let (access, _) = crate::resolve_config_access(Some(home.path()), None)
                .await
                .unwrap();
            access
                .write(
                    "test.edit_pack_document",
                    r#"mutation { update_Tools(filter: {tools_id: {_eq: "demo-tools"}}, input: {display_name: "edited"}) { _docID } }"#,
                )
                .await
                .unwrap();
            access.local_node().unwrap().shutdown().await;
        }

        let second = documents_pack_dir("1.1.0");
        let error = install_from(home.path(), second.path(), Vec::new())
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("were edited"), "{error:#}");

        assert_eq!(
            gents::pack::read_home_install(home.path(), "gents/demo_pack").unwrap(),
            Some(before)
        );
    }

    #[tokio::test]
    async fn dependency_install_keeps_an_existing_operator_record() {
        let home = tempfile::tempdir().unwrap();
        let fixture = test_support::fixture_dir("assets_fixture");
        let installed = install_from(home.path(), &fixture, Vec::new())
            .await
            .unwrap();
        let record: gents::pack::HomePackInstall =
            serde_json::from_value(installed["record"].clone()).unwrap();
        let via =
            gents::pack::home_install::recorded_provenance(record.kind, Some(record.installed_via));
        assert_eq!(via, gents::pack::InstalledVia::Operator);
    }

    #[tokio::test]
    async fn graph_install_writes_no_home_record() {
        let home = initialized_home().await;
        let owner = crate::resolve_node_did(Some(home.path()), None).unwrap();
        let slots = ["coordinator", "worker", "verifier"]
            .map(|slot| format!("{slot}={owner}:default-profile"))
            .to_vec();
        install_from(
            home.path(),
            &test_support::fixture_dir("review_graph"),
            slots,
        )
        .await
        .unwrap();

        let record = gents::pack::read_home_install(home.path(), "fixture/review_graph");
        assert!(record.unwrap().is_none());
    }

    #[test]
    fn local_publication_failure_restores_exactly_the_previous_records() {
        let home = tempfile::tempdir().unwrap();
        let echo_v1 = super::super::plugin::testing::build_plugin_afb(
            "echo",
            b"fn main() { println!(\"{{}}\"); }",
        );
        let asset_v1 = |path: &str| -> Result<&[u8]> {
            (path == "plugins/echo.afb")
                .then_some(echo_v1.as_slice())
                .context("unexpected asset")
        };

        let v1 = plugin_manifest("acme", "widget", "echo", "1.0.0");
        gents::plugin::install::install_pack_plugins_with_bindings(
            home.path(),
            &v1,
            "sha256:v1",
            asset_v1,
            false,
            None,
            &BTreeMap::new(),
            |_| Ok(()),
        )
        .unwrap();
        let before = super::super::plugin::store::read_record(home.path(), "acme", "echo").unwrap();

        // A rejected synchronous local publication rolls records back under
        // the same guard; later node transaction failures do not use this path.
        let echo_v2 = super::super::plugin::testing::build_plugin_afb(
            "echo",
            b"fn main() { println!(\"{{\\\"v\\\":2}}\"); }",
        );
        let asset_v2 = |path: &str| -> Result<&[u8]> {
            (path == "plugins/echo.afb")
                .then_some(echo_v2.as_slice())
                .context("unexpected asset")
        };
        let v2 = plugin_manifest("acme", "widget", "echo", "1.1.0");
        let error = gents::plugin::install::install_pack_plugins_with_bindings(
            home.path(),
            &v2,
            "sha256:v2",
            asset_v2,
            false,
            None,
            &BTreeMap::new(),
            |_| anyhow::bail!("local publication rejected"),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("local publication rejected"));
        assert_eq!(
            super::super::plugin::store::read_record(home.path(), "acme", "echo").unwrap(),
            before
        );

        let new_plugin = super::super::plugin::testing::build_plugin_afb(
            "brandnew",
            b"fn main() { println!(\"{{}}\"); }",
        );
        let asset_new = |path: &str| -> Result<&[u8]> {
            (path == "plugins/brandnew.afb")
                .then_some(new_plugin.as_slice())
                .context("unexpected asset")
        };
        let first_time = plugin_manifest("acme", "brand_new", "brandnew", "1.0.0");
        let error = gents::plugin::install::install_pack_plugins_with_bindings(
            home.path(),
            &first_time,
            "sha256:new",
            asset_new,
            false,
            None,
            &BTreeMap::new(),
            |_| anyhow::bail!("local publication rejected"),
        );
        assert!(error.is_err());
        assert!(super::super::plugin::store::read_record(home.path(), "acme", "brandnew").is_err());
    }

    #[test]
    fn expect_digest_match_passes() {
        check_expected_digest(Some("sha256:abc"), "sha256:abc").unwrap();
        check_expected_digest(None, "sha256:abc").unwrap();
    }

    #[test]
    fn expect_digest_mismatch_names_both_digests_and_pack_show() {
        let error = format!(
            "{:#}",
            check_expected_digest(Some("sha256:expected"), "sha256:actual").unwrap_err()
        );
        for needle in ["sha256:expected", "sha256:actual", "gents pack show"] {
            assert!(error.contains(needle), "{needle} missing from {error}");
        }
    }

    #[test]
    fn wire_refuses_a_graph_or_pluginless_pack() {
        let wire = vec!["agent".to_owned()];
        let plugins = plugin_manifest("acme", "echo_pack", "echo", "1.0.0");
        check_wire_target(&plugins, &wire).unwrap();
        check_wire_target(&plugins, &[]).unwrap();

        let mut graph = plugins.clone();
        graph.metadata.kind = PackKind::Graph;
        assert!(check_wire_target(&graph, &wire).is_err());

        let mut pluginless = plugins;
        pluginless.metadata.plugins.clear();
        let error = check_wire_target(&pluginless, &wire).unwrap_err();
        assert!(
            format!("{error:#}").contains("--wire needs a pack that ships a plugin"),
            "{error:#}"
        );
    }
}
