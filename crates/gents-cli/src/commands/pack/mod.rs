//! One package-facing CLI; install writes stay with their existing owners.
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
pub(crate) use remove::remove;
mod scaffold;
mod scenario;
mod server;
mod test;
pub(crate) mod update;
use crate::cli::*;
use anyhow::{Context, Result};
use futures_util::StreamExt;
use gents::pack::{PackKind, PackManifest};
use gents::pack_archive::PackArchive;
use gents::pack_resolve::{resolve_named, ResolveOptions, ResolvedFrom};
use serde_json::json;
use std::collections::BTreeMap;

use cache::{asset_cache_root_for, lock_exclusive, release_cache_root, CacheRelease};

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

/// A pack, wherever it came from: an explicit local path or digest, an
/// already-installed coordinate, the home's pack store, or the registry.
/// Everything past resolution (materialize, cache, install) works the same
/// regardless, so it is written once against this instead of once per source.
pub(crate) struct PackSource {
    archive: PackArchive,
    from: Source,
}

enum Source {
    /// Named by digest or path (`sha256:`, `./x.pack`, `./dir`).
    Local,
    /// The coordinate's installed record named this digest, and the home's
    /// store still holds it.
    Installed,
    /// The home's pack store held it; no network call was made.
    Store,
    /// Fetched from the registry, which stored and indexed it for next time.
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

    /// The pack's content digest: identical whichever way it arrived, which
    /// is what makes it safe to use as the asset-cache key either way.
    pub(crate) fn digest(&self) -> &str {
        self.archive.digest()
    }

    pub(crate) fn asset(&self, path: &str) -> Result<&[u8]> {
        self.archive.asset(path)
    }

    /// The underlying archive, for a caller (the graph installer) that needs
    /// more than one asset's bytes at a time.
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

    /// A human-readable resolution note: which coordinate on the registry
    /// this pack came from, when it did.
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

/// Shared by every gents-cli unit test that needs a resolved pack: the
/// fixture packs live in the gents crate and open into a store
/// the same way any local pack spec does, so a test never has to spin up a
/// fake registry just to get a [`PackSource`].
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};

    /// A gents-crate fixture pack directory:
    /// `crates/gents/tests/fixtures/packs/<name>`.
    pub(crate) fn fixture_dir(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../gents/tests/fixtures/packs")
            .join(name)
    }

    /// Every fixture pack directory under `crates/gents/tests/fixtures/packs`.
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

    /// Recursively copies `from` into `to`, so a test can mutate a fixture
    /// (build its plugin, edit a config) without touching the checked-in
    /// copy other tests share.
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

    /// Builds every plugin `manifest` declares from source whose artifact is
    /// not already on disk at `dir`, the way `gents pack build` does.
    pub(crate) fn build_unbuilt_plugins(dir: &Path, manifest: &super::PackManifest) {
        for plugin in &manifest.metadata.plugins {
            if plugin.source.is_some() && !dir.join(&plugin.artifact).is_file() {
                super::build::build_plugin(dir, manifest, plugin)
                    .unwrap_or_else(|error| panic!("building plugin {}: {error:#}", plugin.name));
            }
        }
    }

    /// Opens `dir` into `home`'s store, the way a local path spec resolves.
    pub(crate) fn local_pack_source(dir: &Path, home: &Path) -> super::PackSource {
        let archive = super::local::open(&super::local::LocalName::Path(dir), home)
            .unwrap_or_else(|error| panic!("opening {}: {error:#}", dir.display()));
        super::PackSource {
            archive,
            from: super::Source::Local,
        }
    }

    /// Opens a fixture pack directory into `home`'s store, the way a local
    /// path spec resolves.
    pub(crate) fn fixture_pack_source(name: &str, home: &Path) -> super::PackSource {
        local_pack_source(&fixture_dir(name), home)
    }
}

/// `{namespace}/{name}`, defaulting to the `gents` namespace when the given
/// name carries none (a store or registry name may be bare, so this only
/// matters for a registry lookup).
pub(crate) fn split_namespace(name: &str) -> (&str, &str) {
    gents::pack_registry::split_pack_coordinate(name)
}

/// `gents pack list`: every pack the home's store holds, by name index, with
/// the versions on hand, the preferred one's digest and manifest summary,
/// and whether it is currently installed. No catalog ships in the binary, so
/// an empty, unused home lists nothing.
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
            // Every entry has at least one version whose archive exists, or
            // `lookup` would not have indexed it; this is unreachable in
            // practice and simply skipped rather than failing the listing.
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

/// Resolves a pack named by digest or path, an already-installed coordinate,
/// the home's pack store, or the registry, in that order. Local forms
/// (`sha256:`, `./x.pack`, `./dir`, an absolute path) never reach the
/// registry; everything else goes through [`resolve_named`], which stores
/// and indexes a registry fetch so the next resolution of that version is
/// free of the network too.
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

fn materialize(pack: &PackSource, root: &std::path::Path) -> Result<()> {
    use std::io::Write;
    for path in std::iter::once("manifest.json")
        .chain(pack.manifest().metadata.assets.iter().map(String::as_str))
    {
        let destination = root.join(path);
        std::fs::create_dir_all(destination.parent().context("asset parent")?)?;
        let bytes = pack.asset(path)?;
        // Stage beside the destination and publish without replacing it. Readers
        // never see a partial body, and an interrupted staging write cannot
        // poison the digest-addressed destination or overwrite operator edits.
        let mut staged = tempfile::NamedTempFile::new_in(destination.parent().unwrap())?;
        staged.write_all(bytes)?;
        set_distribution_permissions(staged.as_file())?;
        staged.as_file().sync_all()?;
        match staged.persist_noclobber(&destination) {
            Ok(_) => {
                #[cfg(unix)]
                std::fs::File::open(destination.parent().unwrap())?.sync_all()?;
            }
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                anyhow::ensure!(
                    std::fs::read(&destination)? == bytes,
                    "installed asset was modified: {}",
                    destination.display()
                );
                set_distribution_permissions(&std::fs::File::open(&destination)?)?;
            }
            Err(error) => return Err(error.error.into()),
        }
    }
    Ok(())
}

fn set_distribution_permissions(file: &std::fs::File) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o644))?;
    }
    Ok(())
}

fn materialize_cached_pack(
    home: &std::path::Path,
    pack: &PackSource,
) -> Result<(std::path::PathBuf, gents::file_lock::FileLock)> {
    let root = asset_cache_root(home, pack)?;
    let lock = gents::file_lock::FileLock::shared(cache::cache_lock(
        root.parent().context("pack cache parent")?,
    )?)?;
    materialize(pack, &root)?;
    cache::write_cache_marker(&root)?;
    Ok((root, lock))
}

/// A subject pack for `gents eval run` and `gents optimization run`: a
/// directory on disk, or a name resolved the way `gents pack install`
/// resolves one (installed, the home's store, else the registry) and
/// materialized into a directory.
pub(crate) struct SubjectPack {
    pub(crate) source: gents::eval::runner::CellSource,
    pub(crate) manifest: PackManifest,
    /// The shared lock on a materialized cache entry, held while it is read.
    _lease: Option<gents::file_lock::FileLock>,
}

impl SubjectPack {
    /// The pack's directory: the one the operator named, or the cache entry
    /// a resolved name was materialized into.
    pub(crate) fn directory(&self) -> &std::path::Path {
        let gents::eval::runner::CellSource::Directory(dir) = &self.source;
        dir
    }

    /// The pack's one inference-slot behavior; refused when it declares
    /// none or several.
    pub(crate) fn default_behavior(&self) -> Result<String> {
        let mut behaviors: Vec<&String> = self
            .manifest
            .metadata
            .inference_slots
            .iter()
            .flat_map(|slot| slot.behaviors.iter())
            .collect();
        behaviors.sort();
        behaviors.dedup();
        match behaviors.as_slice() {
            [only] => Ok((*only).clone()),
            _ => anyhow::bail!(
                "pack {} declares {} behaviors in its inference slots; name one as <pack>:<behavior>",
                self.manifest.name,
                behaviors.len()
            ),
        }
    }
}

/// Resolve `spec`. A spec that names a path is a directory used in place:
/// one that starts with `.`, `/` or `~`, or one with a path separator that
/// is a directory (`acme/widget` is otherwise a namespaced pack name). Any
/// other spec is a pack name, even when a directory of that name is in the
/// working directory: it resolves as `gents pack install` resolves one
/// (local, installed, store, then registry) and materializes into
/// `<home>/packs/<name>/<digest>`.
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

/// The one inference slot a pack declares: refused when it declares none or
/// several, with a sentence naming the pack.
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

/// The `(slot, behavior)` of a pack with one inference slot holding one
/// behavior, which is how an eval author pack names the behavior it runs.
pub(crate) fn single_slot_behavior(manifest: &PackManifest) -> Result<(String, String)> {
    let slot = single_slot(manifest)?;
    match slot.behaviors.as_slice() {
        [only] => Ok((slot.name.clone(), only.clone())),
        behaviors => anyhow::bail!(
            "pack {} slot {} declares {} behaviors; expected exactly one",
            manifest.name,
            slot.name,
            behaviors.len()
        ),
    }
}

/// Whether a subject spec names a directory rather than a pack: see
/// [`resolve_subject_pack`].
fn names_a_directory(spec: &str) -> bool {
    spec.starts_with(['.', '/', '~'])
        || (spec.contains(std::path::is_separator) && std::path::Path::new(spec).is_dir())
}

fn asset_cache_root(home: &std::path::Path, pack: &PackSource) -> Result<std::path::PathBuf> {
    cache::asset_cache_root_for(home, &pack.manifest().name, pack.digest())
}

/// Thin caller over [`gents::plugin::install::install_pack_plugins`]; the
/// implementation moved into the runtime crate so `self_config` can install
/// a graph pack's plugins itself, without asking this CLI to do it.
pub(crate) fn install_pack_plugins<'a>(
    home: &std::path::Path,
    manifest: &PackManifest,
    pack_digest: &str,
    asset: impl Fn(&str) -> Result<&'a [u8]>,
    consent: bool,
) -> Result<Vec<super::plugin::store::InstalledPlugin>> {
    gents::plugin::install::install_pack_plugins(home, manifest, pack_digest, asset, consent)
}

/// Thin caller over [`gents::plugin::install::snapshot_pack_plugin_records`].
pub(crate) fn snapshot_pack_plugin_records(
    home: &std::path::Path,
    manifest: &PackManifest,
) -> Vec<(
    String,
    String,
    Option<super::plugin::store::InstalledPlugin>,
)> {
    gents::plugin::install::snapshot_pack_plugin_records(home, manifest)
}

/// Thin caller over [`gents::plugin::install::rollback_pack_plugin_records`].
pub(crate) fn rollback_pack_plugin_records(
    home: &std::path::Path,
    previous: &[(
        String,
        String,
        Option<super::plugin::store::InstalledPlugin>,
    )],
) {
    gents::plugin::install::rollback_pack_plugin_records(home, previous)
}

/// The owner whose profiles `requested` names for a plugins pack's model
/// slots, after checking each slot exists and its profile can serve a plugin;
/// `None` when nothing is requested, which opens no store.
async fn resolve_plugin_slot_owner(
    scope: &GraphScopeArgs,
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

/// Binds each plugin's model slot to the profile `requested` names for it;
/// a no-op when nothing is requested.
fn bind_plugin_slots(
    home: &std::path::Path,
    manifest: &PackManifest,
    owner: Option<&str>,
    requested: &BTreeMap<String, String>,
) -> Result<()> {
    match owner {
        Some(owner) => {
            gents::plugin::install::bind_plugin_slots(home, manifest, owner, requested).map(|_| ())
        }
        None => Ok(()),
    }
}

/// The node and the owner a pack command acts for.
pub(crate) async fn resolve_scope_owner(
    scope: &GraphScopeArgs,
) -> Result<(crate::CommandAccess, String)> {
    let (access, _) =
        crate::resolve_config_access(scope.home.as_deref(), scope.graphql.as_deref()).await?;
    let owner = super::config::binding::resolve_target_agent_did(
        scope.agent_did.as_deref(),
        if scope.agent_did.is_some() {
            None
        } else if scope.graphql.is_some() {
            Some(ManifestAgentDidBindingArg::Live)
        } else {
            Some(ManifestAgentDidBindingArg::Home)
        },
        scope.home.as_deref(),
        scope.graphql.as_deref(),
        Some(&access),
    )
    .await?;
    Ok((access, owner))
}

pub(crate) async fn install(args: PackInstallArgs) -> Result<()> {
    let home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
    let pack = resolve_pack_source(&args.package, args.registry.as_deref(), &home).await?;
    tracing::info!(package = %args.package, source = %pack.describe(), "resolved pack");
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
            let bind_mode = if args.scope.graphql.is_some() {
                Some(ManifestAgentDidBindingArg::Live)
            } else {
                Some(ManifestAgentDidBindingArg::Home)
            };
            let owner = super::config::binding::resolve_target_agent_did(
                args.scope.agent_did.as_deref(),
                if args.scope.agent_did.is_some() {
                    None
                } else {
                    bind_mode
                },
                args.scope.home.as_deref(),
                args.scope.graphql.as_deref(),
                Some(&access),
            )
            .await?;
            let requested = parse_inference_slot_bindings(&args.inference_slots)?;
            // Resolve and preview the complete dependency closure before any
            // schema or configuration write. Reused slot names intentionally
            // share one user selection across the root and dependency pack.
            // Resolution never overlaps a write: up to 4 dependencies resolve
            // concurrently (store first, then the registry) before this.
            let dependencies: Vec<PackSource> =
                futures_util::stream::iter(pack.manifest().metadata.dependencies.iter())
                    .map(|coordinate| {
                        resolve_pack_source(coordinate, args.registry.as_deref(), &home)
                    })
                    .buffered(4)
                    .collect::<Vec<_>>()
                    .await
                    .into_iter()
                    .collect::<Result<Vec<_>>>()?;
            for dependency in &dependencies {
                anyhow::ensure!(
                    dependency.manifest().metadata.kind == PackKind::Graph,
                    "only graph dependencies are currently installable"
                );
            }
            for slot in requested.keys() {
                let declared_by_root = pack
                    .manifest()
                    .metadata
                    .inference_slots
                    .iter()
                    .any(|declared| declared.name.as_str() == slot.as_str());
                let declared_by_dependency = dependencies.iter().any(|dependency| {
                    dependency
                        .manifest()
                        .metadata
                        .inference_slots
                        .iter()
                        .any(|declared| declared.name.as_str() == slot.as_str())
                });
                anyhow::ensure!(
                    declared_by_root || declared_by_dependency,
                    "pack {} and its dependencies have no inference slot {slot:?}",
                    pack.manifest().name
                );
            }
            let root_requested = requested
                .iter()
                .filter(|(slot, _)| {
                    pack.manifest()
                        .metadata
                        .inference_slots
                        .iter()
                        .any(|declared| declared.name.as_str() == slot.as_str())
                })
                .map(|(slot, profile)| (slot.clone(), profile.clone()))
                .collect();
            let inference = gents::pack::preview_pack_inference_bindings(
                &access,
                pack.manifest(),
                &owner,
                &root_requested,
            )
            .await?;
            let mut dependency_inference = BTreeMap::new();
            for dependency in &dependencies {
                let dependency_requested = requested
                    .iter()
                    .filter(|(slot, _)| {
                        dependency
                            .manifest()
                            .metadata
                            .inference_slots
                            .iter()
                            .any(|declared| declared.name.as_str() == slot.as_str())
                    })
                    .map(|(slot, profile)| (slot.clone(), profile.clone()))
                    .collect();
                let preview = gents::pack::preview_pack_inference_bindings(
                    &access,
                    dependency.manifest(),
                    &owner,
                    &dependency_requested,
                )
                .await?;
                dependency_inference.insert(dependency.manifest().name.clone(), preview);
            }
            let temp = tempfile::tempdir()?;
            materialize(&pack, temp.path())?;
            let (mut authored, mut report) = crate::desired_state::load_manifest_root(temp.path());
            if authored.is_none() {
                (authored, report) =
                    crate::desired_state::load_manifest_root_for_owner(temp.path(), Some(&owner));
            }
            anyhow::ensure!(
                authored.is_some(),
                "invalid pack configuration: {:?}",
                report.errors
            );
            let mut authored = authored.expect("checked pack configuration");
            super::config::binding::rebind_manifest_to_agent(
                &mut authored,
                &owner,
                args.force_rebind_concrete_did,
            )?;
            let desired = gents::pack::bind_pack_install_config(
                pack.manifest(),
                &authored,
                &inference.bindings,
            )?;
            let origin_tag = gents::pack::pack_origin_tag(&pack.manifest().name)?;
            if args.preview {
                return crate::print_json(&json!({
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
                super::graph::install_with_access(
                    &access,
                    &owner,
                    dependency,
                    PackInstallArgs {
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
                        explicit: false,
                    },
                    false,
                )
                .await?;
            }
            let schemas = super::schema::apply_pack_schemas_if_present(&access, temp.path())
                .await
                .context("pack install schemas")?;
            let plugin_home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
            let (plugins, plugin_rollback) = if pack.manifest().metadata.plugins.is_empty() {
                (Vec::new(), Vec::new())
            } else {
                // A plugin runs on the host of the node that calls it; a
                // remote node's host is not reachable from here.
                anyhow::ensure!(
                    args.scope.graphql.is_none(),
                    "{} ships plugins, which install on the node's own host; run the install \
                     there with --home",
                    pack.manifest().name
                );
                let rollback = snapshot_pack_plugin_records(&plugin_home, pack.manifest());
                let installed = install_pack_plugins(
                    &plugin_home,
                    pack.manifest(),
                    pack.digest(),
                    |path| pack.asset(path),
                    args.grant_authority,
                )
                .inspect_err(|_| rollback_pack_plugin_records(&plugin_home, &rollback))?;
                bind_plugin_slots(
                    &plugin_home,
                    pack.manifest(),
                    Some(&owner),
                    &inference.bindings,
                )
                .inspect_err(|_| rollback_pack_plugin_records(&plugin_home, &rollback))?;
                (installed, rollback)
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
            // From here, a document-transaction failure must not leave the
            // plugins installed above orphaned: the operator sees this pack
            // install as one atomic step, so its filesystem side effect is
            // undone along with the write that never landed.
            let apply = match gents::pack::install_pack_documents(
                &access,
                &owner,
                &identity,
                &desired,
                args.drift.policy(),
            )
            .await
            {
                Ok(apply) => apply,
                Err(error) => {
                    rollback_pack_plugin_records(&plugin_home, &plugin_rollback);
                    return Err(error);
                }
            };
            crate::print_json(&json!({
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
        // A plugins pack carries no documents either, just files to
        // materialize (its compiled artifacts), so it installs the same
        // way an assets pack does: into the home's content-addressed
        // cache, where the artifacts become admissible by digest.
        PackKind::Assets | PackKind::Plugins => {
            let requested = parse_inference_slot_bindings(&args.inference_slots)?;
            anyhow::ensure!(
                args.bindings.is_none()
                    && args.scope.graphql.is_none()
                    && (args.scope.agent_did.is_none() || !requested.is_empty())
                    && !args.force_rebind_concrete_did,
                "asset and plugins packs install locally with --home; identity and graph binding flags do not apply"
            );
            if args.preview {
                return crate::print_json(&json!({
                    "pack": pack.manifest().name,
                    "source": pack.label(),
                    "digest": pack.digest(),
                    "installed_plugins": pack.manifest().metadata.plugins,
                    "inference_slots": pack.manifest().metadata.inference_slots,
                    "would_write": false,
                }));
            }
            let slot_owner =
                resolve_plugin_slot_owner(&args.scope, pack.manifest(), &requested).await?;
            let home = args
                .scope
                .home
                .context("asset and plugins packs require --home")?;
            let (root, _cache_lease) = materialize_cached_pack(&home, &pack)?;
            // A pack's plugins travel inside it (pack_archive's own doc),
            // so installing the pack installs each one into the same
            // content-addressed plugin store `gents plugin install` uses:
            // a plugin that arrived bundled in a pack is just as runnable
            // by name (`gents plugin run <name>`) as one installed on its
            // own. A failure past this point (the record write) must not
            // leave the plugins installed above orphaned.
            let rollback = snapshot_pack_plugin_records(&home, pack.manifest());
            let installed_plugins = install_pack_plugins(
                &home,
                pack.manifest(),
                pack.digest(),
                |path| pack.asset(path),
                args.grant_authority,
            )
            .inspect_err(|_| rollback_pack_plugin_records(&home, &rollback))?;
            let record = gents::pack::HomePackInstall {
                coordinate: format!(
                    "{}/{}",
                    pack.manifest().metadata.namespace,
                    pack.manifest().name
                ),
                version: pack.manifest().version.clone(),
                digest: pack.digest().to_owned(),
                kind: pack.manifest().metadata.kind.clone(),
                assets: root
                    .strip_prefix(&home)
                    .unwrap_or(root.as_path())
                    .to_string_lossy()
                    .into_owned(),
                plugins: installed_plugins
                    .iter()
                    .map(|plugin| gents::pack::InstalledPackPlugin {
                        name: plugin.name.clone(),
                        digest: plugin.digest.clone(),
                    })
                    .collect(),
                installed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            };
            if let Err(error) =
                bind_plugin_slots(&home, pack.manifest(), slot_owner.as_deref(), &requested)
                    .and_then(|()| gents::pack::write_home_install(&home, &record))
            {
                rollback_pack_plugin_records(&home, &rollback);
                return Err(error);
            }
            crate::print_json(&json!({
                "pack": pack.manifest().name,
                "digest": pack.digest(),
                "installed_assets": root,
                "installed_plugins": installed_plugins,
                "record": record,
                "source": pack.label(),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare subject spec is a pack name even when a directory of that name
    /// is in the working directory (a test runs in the crate root, which has
    /// `src`); only a spec that looks like a path is read as a directory.
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
    fn concurrent_materialization_publishes_complete_assets() {
        let home = tempfile::tempdir().unwrap();
        let source = test_support::fixture_pack_source("assets_fixture", home.path());
        let root = tempfile::tempdir().unwrap();
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    barrier.wait();
                    materialize(&source, root.path()).unwrap();
                });
            }
        });
        for path in std::iter::once("manifest.json")
            .chain(source.manifest().metadata.assets.iter().map(String::as_str))
        {
            assert_eq!(
                std::fs::read(root.path().join(path)).unwrap(),
                source.asset(path).unwrap()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn materialized_assets_are_readable_like_distribution_files() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().unwrap();
        let source = test_support::fixture_pack_source("assets_fixture", home.path());
        let root = tempfile::tempdir().unwrap();
        materialize(&source, root.path()).unwrap();
        for path in std::iter::once("manifest.json")
            .chain(source.manifest().metadata.assets.iter().map(String::as_str))
        {
            assert_eq!(
                std::fs::metadata(root.path().join(path))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o644,
                "{path}"
            );
        }

        let cached = root.path().join("README.md");
        std::fs::set_permissions(&cached, std::fs::Permissions::from_mode(0o600)).unwrap();
        materialize(&source, root.path()).unwrap();
        assert_eq!(
            std::fs::metadata(cached).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn scenario_cache_lease_excludes_pruning() {
        let home = tempfile::tempdir().unwrap();
        let pack = test_support::fixture_pack_source("documents_fixture", home.path());
        let (root, lease) = materialize_cached_pack(home.path(), &pack).unwrap();
        let exclusive = cache::cache_lock(root.parent().unwrap()).unwrap();
        assert!(exclusive.try_lock().is_err());
        drop(lease);
        // A process another test forks inherits every open descriptor until it
        // execs, so the shared lock can outlive `drop` for that window.
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
    fn abandoned_staging_file_does_not_poison_install_or_allow_overwrite() {
        use std::io::Write;
        let home = tempfile::tempdir().unwrap();
        let pack = test_support::fixture_pack_source("assets_fixture", home.path());
        let root = tempfile::tempdir().unwrap();
        // Model process death before publication: a partial temporary file
        // remains, but no destination has been exposed.
        let mut staged = tempfile::NamedTempFile::new_in(root.path()).unwrap();
        staged.write_all(b"partial").unwrap();
        let (_file, abandoned) = staged.keep().unwrap();
        materialize(&pack, root.path()).unwrap();
        materialize(&pack, root.path()).unwrap();
        assert_eq!(std::fs::read(abandoned).unwrap(), b"partial");
        std::fs::write(root.path().join("README.md"), "operator edit").unwrap();
        assert!(materialize(&pack, root.path())
            .unwrap_err()
            .to_string()
            .contains("installed asset was modified"));
        assert_eq!(
            std::fs::read_to_string(root.path().join("README.md")).unwrap(),
            "operator edit"
        );
    }

    /// The CI gate for real (packs-repo) packs moves to packs CI
    /// (`gents pack test`); this keeps only the gents-side machinery honest,
    /// over the checked-in fixture.
    #[test]
    fn a_document_pack_materializes_a_valid_configuration() {
        let home = tempfile::tempdir().unwrap();
        let pack = test_support::fixture_pack_source("documents_fixture", home.path());
        let root = tempfile::tempdir().unwrap();
        materialize(&pack, root.path()).unwrap();
        let config = gents::pack::load_pack_config(
            pack.manifest(),
            &gents::pack::PackInstallOptions {
                agent_did: "did:key:zPackCatalogValidationOwner".into(),
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
        // Store it first, the way `gents pack fetch --store` or a prior
        // install would have.
        let _ = test_support::fixture_pack_source("assets_fixture", home.path());
        // An unroutable registry: if resolution incorrectly fell through to
        // it, this fails fast instead of hanging on a real network call or
        // silently succeeding some other way.
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

    #[test]
    fn split_namespace_defaults_to_gents() {
        assert_eq!(split_namespace("mailbox"), ("gents", "mailbox"));
        assert_eq!(split_namespace("acme/widget"), ("acme", "widget"));
    }

    #[tokio::test]
    async fn plugin_slot_bindings_need_a_declared_slot_and_open_no_store_without_one() {
        let manifest: PackManifest = serde_json::from_value(json!({
            "manifest_version": 1, "name": "ocr", "version": "1.0.0", "description": "d",
            "authors": ["t"], "kind": "plugins", "assets": ["README.md"],
            "inference_slots": [{"name": "remote_ocr", "description": "d", "optional": true}],
        }))
        .unwrap();
        let scope = GraphScopeArgs {
            home: Some(tempfile::tempdir().unwrap().path().to_owned()),
            graphql: None,
            agent_did: None,
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

    #[test]
    fn inference_slot_flags_are_complete_unique_pairs() {
        let bindings =
            parse_inference_slot_bindings(&["coordinator=claude".into(), "worker=glm".into()])
                .unwrap();
        assert_eq!(bindings["coordinator"], "claude");
        assert_eq!(bindings["worker"], "glm");
        for invalid in [
            vec!["missing-separator".into()],
            vec!["worker=".into()],
            vec!["worker=one".into(), "worker=two".into()],
        ] {
            assert!(parse_inference_slot_bindings(&invalid).is_err());
        }
    }

    /// A `plugins`-kind manifest with one plugin, named after `pack_name`
    /// and shipping under `plugin_name`.
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

    /// #1721 item 1: a step after plugin install fails (here, standing in
    /// for `install_pack_documents` failing) must leave the plugin store
    /// exactly as it was before this install attempt, not orphan whatever
    /// it just wrote.
    #[test]
    fn a_failure_after_plugin_install_rolls_back_to_exactly_the_previous_records() {
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

        // A prior install this pack coordinate already owns.
        let v1 = plugin_manifest("acme", "widget", "echo", "1.0.0");
        install_pack_plugins(home.path(), &v1, "sha256:v1", asset_v1, false).unwrap();
        let before = super::super::plugin::store::read_record(home.path(), "acme", "echo").unwrap();

        // An update whose install fails after the plugin step must roll all
        // the way back to `before`, exactly as `install()` does when
        // `install_pack_documents` errors.
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
        let rollback = snapshot_pack_plugin_records(home.path(), &v2);
        install_pack_plugins(home.path(), &v2, "sha256:v2", asset_v2, false).unwrap();
        let mid = super::super::plugin::store::read_record(home.path(), "acme", "echo").unwrap();
        assert_ne!(
            mid, before,
            "the reinstall must actually have changed the record"
        );
        rollback_pack_plugin_records(home.path(), &rollback);
        assert_eq!(
            super::super::plugin::store::read_record(home.path(), "acme", "echo").unwrap(),
            before
        );

        // A pack installing a name for the first time leaves nothing behind
        // when the same kind of failure happens: rollback removes it.
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
        let rollback = snapshot_pack_plugin_records(home.path(), &first_time);
        install_pack_plugins(home.path(), &first_time, "sha256:new", asset_new, false).unwrap();
        assert!(super::super::plugin::store::read_record(home.path(), "acme", "brandnew").is_ok());
        rollback_pack_plugin_records(home.path(), &rollback);
        assert!(super::super::plugin::store::read_record(home.path(), "acme", "brandnew").is_err());
    }
}
