//! One package-facing CLI; install writes stay with their existing owners.
pub(crate) mod account;
mod build;
mod check;
mod cli_process;
mod edit;
mod inspect;
mod local;
pub(crate) mod registry;
mod scaffold;
mod scenario;
mod secscan;
mod server;
mod test;
mod update;
use crate::cli::*;
use anyhow::{Context, Result};
use gents::pack::{pack_catalog, resolve_pack, PackKind, PackManifest, ResolvedPack};
use serde_json::json;
use std::collections::BTreeMap;

const CACHE_MARKER: &str = ".gents-pack-cache-v1";
const CACHE_LOCK: &str = ".cache.lock";

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
        PackCommand::List => {
            let entries: Vec<_> = pack_catalog()?.into_iter().map(|pack| json!({
                "name":pack.name,"version":pack.version,"description":pack.description,
                "kind":pack.metadata.kind,"authors":pack.metadata.authors,"tags":pack.metadata.tags,
                "inference_slots":pack.metadata.inference_slots
            })).collect();
            crate::print_json(&json!({"packs":entries}))
        }
        PackCommand::Show(args) => inspect::show(args).await,
        PackCommand::Verify(args) => inspect::verify(args),
        PackCommand::New(args) => scaffold::new(args),
        PackCommand::Init(args) => scaffold::init(args),
        PackCommand::Add(args) => edit::add(args),
        PackCommand::RemovePart(args) => edit::remove_part(args),
        PackCommand::Fmt(args) => edit::fmt(args),
        PackCommand::Diff(args) => inspect::diff(args).await,
        PackCommand::Test(args) => test::test(args).await,
        PackCommand::Check(args) => check::check(args).await,
        PackCommand::Graph(args) => check::graph(args),
        PackCommand::Install(args) => install(args).await,
        PackCommand::Remove(args) => remove(args).await,
        PackCommand::Outdated(args) => update::outdated(args).await,
        PackCommand::Update(args) => update::update(args).await,
        PackCommand::Prune(args) => prune(args),
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

/// A pack, wherever it came from: compiled into this binary, or downloaded
/// from the registry and verified. Everything past resolution (materialize,
/// cache, install) works the same either way, so it is written once against
/// this instead of twice against `ResolvedPack` and a registry type.
enum PackSource {
    Bundled(ResolvedPack),
    Registry(registry::RegistryPack),
    /// Named by digest or path, and opened from the home's pack store.
    Stored(gents::pack_archive::PackArchive),
}

impl PackSource {
    fn manifest(&self) -> &PackManifest {
        match self {
            Self::Bundled(pack) => &pack.manifest,
            Self::Registry(pack) => pack.archive.manifest(),
            Self::Stored(pack) => pack.manifest(),
        }
    }

    /// The pack's content digest: identical whichever way it arrived, which
    /// is what makes it safe to use as the asset-cache key either way.
    fn digest(&self) -> &str {
        match self {
            Self::Bundled(pack) => &pack.digest,
            Self::Registry(pack) => &pack.digest,
            Self::Stored(pack) => pack.digest(),
        }
    }

    fn asset(&self, path: &str) -> Result<&[u8]> {
        match self {
            Self::Bundled(pack) => pack.asset(path),
            Self::Registry(pack) => pack.archive.asset(path),
            Self::Stored(pack) => pack.asset(path),
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Bundled(_) => "bundled",
            Self::Registry(_) => "registry",
            Self::Stored(_) => "local",
        }
    }

    /// A human-readable resolution note: which coordinate on the registry
    /// this pack came from, when it did.
    fn describe(&self) -> String {
        match self {
            Self::Bundled(_) => self.label().to_owned(),
            Self::Stored(pack) => format!("{} ({})", self.label(), pack.digest()),
            Self::Registry(pack) => format!(
                "{} ({}/{}@{})",
                self.label(),
                pack.namespace,
                pack.name,
                pack.version
            ),
        }
    }
}

/// `{namespace}/{name}`, defaulting to the `gents` namespace when the given
/// name carries none (every bundled pack name is bare, so this only matters
/// for a registry lookup).
pub(crate) fn split_namespace(name: &str) -> (&str, &str) {
    gents::pack_registry::split_pack_coordinate(name)
}

/// Resolves a pack named by digest or path from the home's store; otherwise
/// a pack compiled into this binary first, and only when that fails the
/// registry, downloading, verifying, and storing the result. Both bundled and
/// registry failures are reported together so a real problem with the
/// bundled lookup is never masked by a registry error.
async fn resolve_pack_source(
    name: &str,
    registry_override: Option<&str>,
    home: &std::path::Path,
) -> Result<PackSource> {
    if let Some(local) = local::classify(name) {
        return local::open(&local, home).map(PackSource::Stored);
    }
    match resolve_pack(name) {
        Ok(pack) => Ok(PackSource::Bundled(pack)),
        Err(bundled_error) => {
            let (namespace, pack_name) = split_namespace(name);
            let base_url = registry::resolve_registry_url(registry_override);
            let client = registry::RegistryClient::new(base_url.clone());
            let fetched = registry::fetch_pack(&client, Some(home), namespace, pack_name)
                .await
                .map_err(|registry_error| {
                    anyhow::anyhow!(
                        "{name} is not compiled into this binary ({bundled_error}) and fetching \
                         it from the registry at {base_url} also failed: {registry_error}"
                    )
                })?;
            Ok(PackSource::Registry(fetched))
        }
    }
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

fn write_cache_marker(root: &std::path::Path) -> Result<()> {
    let marker = root.join(CACHE_MARKER);
    if marker.exists() {
        return Ok(());
    }
    let staged = tempfile::NamedTempFile::new_in(root)?;
    set_distribution_permissions(staged.as_file())?;
    match staged.persist_noclobber(&marker) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.error.into()),
    }
}

fn cache_lock(parent: &std::path::Path) -> Result<std::fs::File> {
    std::fs::create_dir_all(parent)?;
    Ok(std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(parent.join(CACHE_LOCK))?)
}

fn prune_stale_asset_cache(
    parent: &std::path::Path,
    current: &std::path::Path,
) -> Result<Vec<String>> {
    let mut removed = Vec::new();
    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        let root = entry.path();
        if root == current || !root.is_dir() {
            continue;
        }
        let Some(name) = root.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.len() != 64 || !name.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        if !root.join(CACHE_MARKER).is_file() {
            continue;
        }
        // `runs/` is operator-owned history. The caller holds the exclusive
        // per-pack lock, so no Gents scenario can acquire or use any sibling
        // cache root while the probe and removal occur.
        if !root.join("runs").exists() {
            std::fs::remove_dir_all(&root)
                .with_context(|| format!("pruning stale pack cache {}", root.display()))?;
            removed.push(name.to_owned());
        }
    }
    removed.sort();
    Ok(removed)
}

fn materialize_cached_pack(
    home: &std::path::Path,
    pack: &PackSource,
) -> Result<(std::path::PathBuf, std::fs::File)> {
    let root = asset_cache_root(home, pack)?;
    let lock = cache_lock(root.parent().context("pack cache parent")?)?;
    lock.lock_shared()?;
    materialize(pack, &root)?;
    write_cache_marker(&root)?;
    Ok((root, lock))
}

pub(crate) fn materialize_named_pack(
    name: &str,
) -> Result<(std::path::PathBuf, std::fs::File, gents::pack::PackManifest)> {
    let pack = resolve_pack(name)?;
    let manifest = pack.manifest.clone();
    let (root, lease) = materialize_cached_pack(
        &crate::home_state::resolve_home_dir(None),
        &PackSource::Bundled(pack),
    )?;
    Ok((root, lease, manifest))
}

fn asset_cache_root(home: &std::path::Path, pack: &PackSource) -> Result<std::path::PathBuf> {
    // Keep the shared sha256: digest representation out of filesystem names.
    let hash = pack
        .digest()
        .strip_prefix("sha256:")
        .context("invalid pack digest")?;
    Ok(home.join("packs").join(&pack.manifest().name).join(hash))
}

fn prune(args: PackPruneArgs) -> Result<()> {
    let pack = PackSource::Bundled(resolve_pack(&args.package)?);
    anyhow::ensure!(
        pack.manifest().metadata.kind == PackKind::Assets
            || pack
                .manifest()
                .metadata
                .assets
                .iter()
                .any(|asset| asset == "experiment.json"),
        "pack {} has no materialized asset cache",
        pack.manifest().name
    );
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let current = asset_cache_root(&home, &pack)?;
    let parent = current.parent().context("pack cache parent")?;
    if !parent.is_dir() {
        return crate::print_json(&json!({
            "pack": pack.manifest().name,
            "current_digest": pack.digest(),
            "removed_digests": [],
        }));
    }
    let lock = cache_lock(parent)?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            anyhow::bail!("pack cache is in use; stop active pack operations and retry")
        }
        Err(std::fs::TryLockError::Error(error)) => {
            return Err(error).context("locking pack cache for pruning")
        }
    }
    let removed = prune_stale_asset_cache(parent, &current)?;
    crate::print_json(&json!({
        "pack": pack.manifest().name,
        "current_digest": pack.digest(),
        "removed_digests": removed,
    }))
}

/// Admits and stores every plugin a pack ships in `home`'s plugin store, the
/// one `gents plugin install` uses, so a plugin that arrived inside a pack
/// runs by name like one installed alone.
pub(crate) fn install_pack_plugins<'a>(
    home: &std::path::Path,
    manifest: &PackManifest,
    asset: impl Fn(&str) -> Result<&'a [u8]>,
    consent: bool,
) -> Result<Vec<super::plugin::store::InstalledPlugin>> {
    manifest
        .metadata
        .plugins
        .iter()
        .map(|plugin| {
            super::plugin::install_from_pack(
                home,
                &manifest.metadata.namespace,
                &manifest.version,
                plugin,
                asset(&plugin.artifact)?,
                consent,
            )
        })
        .collect()
}

/// The node and the owner a pack command acts for.
pub(crate) async fn resolve_scope_owner(
    scope: &GraphScopeArgs,
) -> Result<(gents::config_client::ConfigAccess, String)> {
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

/// `gents pack remove`: deletes what the pack's install created, keeping
/// documents it adopted, and forgets the install.
async fn remove(args: PackRemoveArgs) -> Result<()> {
    let (namespace, name) = split_namespace(&args.package);
    let (access, owner) = resolve_scope_owner(&args.scope).await?;
    let report = gents::pack::remove_pack(
        &access,
        &owner,
        &format!("{namespace}/{name}"),
        args.drift.policy(),
    )
    .await?;
    if !report.plugins.is_empty() {
        let home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
        for plugin in &report.plugins {
            // Another install may have replaced it since; only this pack's
            // own artifact is forgotten.
            if super::plugin::store::read_record(&home, namespace, &plugin.name)
                .is_ok_and(|record| record.digest == plugin.digest)
            {
                super::plugin::store::remove_record(&home, namespace, &plugin.name)?;
            }
        }
    }
    crate::print_json(
        &json!({ "pack": format!("{namespace}/{name}"), "owner": owner, "removed": report }),
    )
}

async fn install(args: PackInstallArgs) -> Result<()> {
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
            anyhow::ensure!(
                matches!(pack, PackSource::Bundled(_)),
                "{} is a graph pack; only a graph pack compiled into this binary can be \
                 installed today, so it cannot be installed from the registry yet",
                args.package
            );
            super::graph::install(args, true).await
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
            let dependencies = pack
                .manifest()
                .metadata
                .dependencies
                .iter()
                .map(|name| resolve_pack(name))
                .collect::<Result<Vec<_>>>()?;
            for dependency in &dependencies {
                anyhow::ensure!(
                    matches!(dependency.manifest.metadata.kind, PackKind::Graph),
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
                        .manifest
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
                            .manifest
                            .metadata
                            .inference_slots
                            .iter()
                            .any(|declared| declared.name.as_str() == slot.as_str())
                    })
                    .map(|(slot, profile)| (slot.clone(), profile.clone()))
                    .collect();
                let preview = gents::pack::preview_pack_inference_bindings(
                    &access,
                    &dependency.manifest,
                    &owner,
                    &dependency_requested,
                )
                .await?;
                dependency_inference.insert(dependency.manifest.name.clone(), preview);
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
            for dependency in dependencies {
                let dependency_slots = dependency_inference[&dependency.manifest.name]
                    .bindings
                    .iter()
                    .map(|(slot, profile)| format!("{slot}={profile}"))
                    .collect();
                super::graph::install(
                    PackInstallArgs {
                        package: dependency.manifest.name,
                        bindings: None,
                        inference_slots: dependency_slots,
                        preview: false,
                        scope: args.scope.clone(),
                        output: args.output,
                        force_rebind_concrete_did: false,
                        registry: args.registry.clone(),
                        drift: args.drift,
                        grant_authority: args.grant_authority,
                    },
                    false,
                )
                .await?;
            }
            let schemas = super::schema::apply_pack_schemas_if_present(&access, temp.path())
                .await
                .context("pack install schemas")?;
            let plugins = if pack.manifest().metadata.plugins.is_empty() {
                Vec::new()
            } else {
                // A plugin runs on the host of the node that calls it; a
                // remote node's host is not reachable from here.
                anyhow::ensure!(
                    args.scope.graphql.is_none(),
                    "{} ships plugins, which install on the node's own host; run the install \
                     there with --home",
                    pack.manifest().name
                );
                let home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
                install_pack_plugins(
                    &home,
                    pack.manifest(),
                    |path| pack.asset(path),
                    args.grant_authority,
                )?
            };
            let identity = gents::pack::PackIdentity {
                coordinate: format!(
                    "{}/{}",
                    pack.manifest().metadata.namespace,
                    pack.manifest().name
                ),
                version: pack.manifest().version.clone(),
                digest: pack.digest().to_owned(),
                plugins: plugins
                    .iter()
                    .map(|plugin| gents::pack::InstalledPackPlugin {
                        name: plugin.name.clone(),
                        digest: plugin.digest.clone(),
                    })
                    .collect(),
            };
            let apply = gents::pack::install_pack_documents(
                &access,
                &owner,
                &identity,
                &desired,
                args.drift.policy(),
            )
            .await?;
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
            anyhow::ensure!(
                args.bindings.is_none()
                    && args.inference_slots.is_empty()
                    && args.scope.graphql.is_none()
                    && args.scope.agent_did.is_none()
                    && !args.force_rebind_concrete_did,
                "asset and plugins packs install locally with --home; identity and graph binding flags do not apply"
            );
            if args.preview {
                return crate::print_json(&json!({
                    "pack": pack.manifest().name,
                    "source": pack.label(),
                    "digest": pack.digest(),
                    "installed_plugins": pack.manifest().metadata.plugins,
                    "would_write": false,
                }));
            }
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
            // own.
            let installed_plugins = install_pack_plugins(
                &home,
                pack.manifest(),
                |path| pack.asset(path),
                args.grant_authority,
            )?;
            crate::print_json(&json!({
                "pack": pack.manifest().name,
                "digest": pack.digest(),
                "installed_assets": root,
                "installed_plugins": installed_plugins,
                "source": pack.label(),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_materialization_publishes_complete_assets() {
        let pack = resolve_pack("mailbox").unwrap();
        let source = PackSource::Bundled(resolve_pack("mailbox").unwrap());
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
            .chain(pack.manifest.metadata.assets.iter().map(String::as_str))
        {
            assert_eq!(
                std::fs::read(root.path().join(path)).unwrap(),
                pack.asset(path).unwrap()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn materialized_assets_are_readable_like_distribution_files() {
        use std::os::unix::fs::PermissionsExt;

        let pack = resolve_pack("mailbox").unwrap();
        let source = PackSource::Bundled(resolve_pack("mailbox").unwrap());
        let root = tempfile::tempdir().unwrap();
        materialize(&source, root.path()).unwrap();
        for path in std::iter::once("manifest.json")
            .chain(pack.manifest.metadata.assets.iter().map(String::as_str))
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
    fn cache_pruning_removes_only_owned_versions_without_runs() {
        let parent = tempfile::tempdir().unwrap();
        let current = parent.path().join("a".repeat(64));
        let stale = parent.path().join("b".repeat(64));
        let active = parent.path().join("c".repeat(64));
        let unowned = parent.path().join("d".repeat(64));
        for root in [&current, &stale, &active, &unowned] {
            std::fs::create_dir_all(root).unwrap();
        }
        write_cache_marker(&current).unwrap();
        write_cache_marker(&stale).unwrap();
        write_cache_marker(&active).unwrap();
        std::fs::create_dir(active.join("runs")).unwrap();

        prune_stale_asset_cache(parent.path(), &current).unwrap();

        assert!(current.exists());
        assert!(!stale.exists());
        assert!(active.exists(), "run artifacts retain their source version");
        assert!(
            unowned.exists(),
            "directories without our marker are not ours"
        );
    }

    #[test]
    fn scenario_cache_lease_excludes_pruning() {
        let home = tempfile::tempdir().unwrap();
        let pack = PackSource::Bundled(resolve_pack("pipeline").unwrap());
        let (root, lease) = materialize_cached_pack(home.path(), &pack).unwrap();
        let exclusive = cache_lock(root.parent().unwrap()).unwrap();
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
    fn graph_pack_prune_rejects_without_creating_a_cache_tree() {
        let parent = tempfile::tempdir().unwrap();
        let home = parent.path().join("missing-home");
        let error = prune(PackPruneArgs {
            package: "code_review".to_owned(),
            home: Some(home.clone()),
        })
        .unwrap_err();
        assert!(error.to_string().contains("no materialized asset cache"));
        assert!(!home.exists());
    }

    #[test]
    fn abandoned_staging_file_does_not_poison_install_or_allow_overwrite() {
        use std::io::Write;
        let pack = PackSource::Bundled(resolve_pack("mailbox").unwrap());
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

    #[test]
    fn every_bundled_document_pack_materializes_a_valid_configuration() {
        for manifest in pack_catalog().unwrap() {
            if manifest.metadata.kind != PackKind::Documents {
                continue;
            }
            let pack = resolve_pack(&manifest.name).unwrap();
            let root = tempfile::tempdir().unwrap();
            materialize(
                &PackSource::Bundled(resolve_pack(&manifest.name).unwrap()),
                root.path(),
            )
            .unwrap();
            let config = gents::pack::load_pack_config(
                &pack.manifest,
                &gents::pack::PackInstallOptions {
                    agent_did: "did:key:zPackCatalogValidationOwner".into(),
                },
                &|path| pack.asset(path).map(Vec::from),
                &|_| None,
            )
            .unwrap_or_else(|error| panic!("{}: {error:#}", manifest.name));
            gents::config_client::DesiredStateApplyPlan::from_pack_config(&config)
                .unwrap_or_else(|error| panic!("{}: {error:#}", manifest.name));
        }
    }

    #[tokio::test]
    async fn resolution_prefers_a_pack_compiled_into_this_binary() {
        // An unroutable registry: if resolution incorrectly fell through to
        // it for a bundled pack, this fails fast instead of hanging on a
        // real network call or silently succeeding some other way.
        let source = resolve_pack_source(
            "mailbox",
            Some("http://127.0.0.1:1"),
            tempfile::tempdir().unwrap().path(),
        )
        .await
        .expect("a bundled pack must resolve without touching the registry");
        assert!(matches!(source, PackSource::Bundled(_)));
        assert_eq!(source.label(), "bundled");
    }

    #[tokio::test]
    async fn resolution_falls_back_to_the_registry_and_reports_both_failures() {
        // `ResolvedPack`/`PackSource` are not `Debug`, so this checks the
        // `Err` case by hand rather than via `expect_err`.
        let result = resolve_pack_source(
            "definitely_not_a_bundled_pack",
            Some("http://127.0.0.1:1"),
            tempfile::tempdir().unwrap().path(),
        )
        .await;
        let Err(error) = result else {
            panic!("neither bundled nor registry has this pack");
        };
        let message = format!("{error:#}");
        assert!(
            message.contains("not compiled into this binary"),
            "{message}"
        );
        assert!(
            message.contains("definitely_not_a_bundled_pack"),
            "{message}"
        );
    }

    #[test]
    fn split_namespace_defaults_to_gents() {
        assert_eq!(split_namespace("mailbox"), ("gents", "mailbox"));
        assert_eq!(split_namespace("acme/widget"), ("acme", "widget"));
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
}
