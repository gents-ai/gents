//! One package-facing CLI; install writes stay with their existing owners.
mod build;
mod cli_process;
mod registry;
mod scenario;
mod secscan;
mod server;
use crate::cli::*;
use anyhow::{Context, Result};
use gents::pack::{pack_catalog, resolve_pack, PackKind, PackManifest, ResolvedPack};
use gents::pack_archive::DEFAULT_NAMESPACE;
use serde_json::json;

const CACHE_MARKER: &str = ".gents-pack-cache-v1";
const CACHE_LOCK: &str = ".cache.lock";

pub(crate) async fn dispatch(command: PackCommand) -> Result<()> {
    match command {
        PackCommand::List => {
            let entries: Vec<_> = pack_catalog()?.into_iter().map(|pack| json!({
                "name":pack.name,"version":pack.version,"description":pack.description,
                "kind":pack.metadata.kind,"authors":pack.metadata.authors,"tags":pack.metadata.tags
            })).collect();
            crate::print_json(&json!({"packs":entries}))
        }
        PackCommand::Show(args) => {
            let pack = resolve_pack(&args.package)?;
            let graph = if matches!(pack.manifest.metadata.kind, PackKind::Graph) {
                Some(gents::graph_package::load_resolved_graph_package(&pack)?.catalog_entry())
            } else {
                None
            };
            crate::print_json(
                &json!({"manifest": pack.manifest, "digest": pack.digest, "graph": graph}),
            )
        }
        PackCommand::Install(args) => install(args).await,
        PackCommand::Prune(args) => prune(args),
        PackCommand::Run(args) => scenario::run(args).await,
        PackCommand::Init(args) => scenario::init_pack(args).await,
        PackCommand::Seed(args) => scenario::seed(args).await,
        PackCommand::Build(args) => build::dispatch(args),
        PackCommand::Search(args) => registry::search(args).await,
        PackCommand::Publish(args) => registry::publish(args).await,
    }
}

/// A pack, wherever it came from: compiled into this binary, or downloaded
/// from the registry and verified. Everything past resolution (materialize,
/// cache, install) works the same either way, so it is written once against
/// this instead of twice against `ResolvedPack` and a registry type.
enum PackSource {
    Bundled(ResolvedPack),
    Registry(registry::RegistryPack),
}

impl PackSource {
    fn manifest(&self) -> &PackManifest {
        match self {
            Self::Bundled(pack) => &pack.manifest,
            Self::Registry(pack) => pack.afb.manifest(),
        }
    }

    /// The pack's content digest: identical whichever way it arrived, which
    /// is what makes it safe to use as the asset-cache key either way.
    fn digest(&self) -> &str {
        match self {
            Self::Bundled(pack) => &pack.digest,
            Self::Registry(pack) => &pack.digest,
        }
    }

    fn asset(&self, path: &str) -> Result<&[u8]> {
        match self {
            Self::Bundled(pack) => pack.asset(path),
            Self::Registry(pack) => pack.afb.asset(path),
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Bundled(_) => "bundled",
            Self::Registry(_) => "registry",
        }
    }

    /// A human-readable resolution note: which coordinate on the registry
    /// this pack came from, when it did.
    fn describe(&self) -> String {
        match self {
            Self::Bundled(_) => self.label().to_owned(),
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
fn split_namespace(name: &str) -> (&str, &str) {
    name.split_once('/').unwrap_or((DEFAULT_NAMESPACE, name))
}

/// Resolves a pack compiled into this binary first; only when that fails
/// does it fall back to the registry, downloading, verifying, and caching
/// the result. Both failures are reported together so a real problem with
/// the bundled lookup is never masked by a registry error.
async fn resolve_pack_source(name: &str, registry_override: Option<&str>) -> Result<PackSource> {
    match resolve_pack(name) {
        Ok(pack) => Ok(PackSource::Bundled(pack)),
        Err(bundled_error) => {
            let (namespace, pack_name) = split_namespace(name);
            let base_url = registry::resolve_registry_url(registry_override);
            let client = registry::RegistryClient::new(base_url.clone());
            let home = crate::home_state::resolve_home_dir(None);
            let fetched = registry::fetch_pack(&client, &home, namespace, pack_name)
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
        exclusive.try_lock().unwrap();
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
            materialize(&PackSource::Bundled(pack), root.path()).unwrap();
            let (_, report) = crate::desired_state::load_manifest_root(root.path());
            assert!(
                report.errors.is_empty(),
                "{}: {:?}",
                manifest.name,
                report.errors
            );
        }
    }

    #[tokio::test]
    async fn resolution_prefers_a_pack_compiled_into_this_binary() {
        // An unroutable registry: if resolution incorrectly fell through to
        // it for a bundled pack, this fails fast instead of hanging on a
        // real network call or silently succeeding some other way.
        let source = resolve_pack_source("mailbox", Some("http://127.0.0.1:1"))
            .await
            .expect("a bundled pack must resolve without touching the registry");
        assert!(matches!(source, PackSource::Bundled(_)));
        assert_eq!(source.label(), "bundled");
    }

    #[tokio::test]
    async fn resolution_falls_back_to_the_registry_and_reports_both_failures() {
        // `ResolvedPack`/`PackSource` are not `Debug`, so this checks the
        // `Err` case by hand rather than via `expect_err`.
        let result =
            resolve_pack_source("definitely_not_a_bundled_pack", Some("http://127.0.0.1:1")).await;
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

async fn install(args: PackInstallArgs) -> Result<()> {
    let pack = resolve_pack_source(&args.package, args.registry.as_deref()).await?;
    eprintln!(
        "gents pack install: resolved {} from {}",
        args.package,
        pack.describe()
    );
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
            anyhow::ensure!(args.bindings.is_none() && args.scope.agent_did.is_none(), "document packs bind to the target node; --bindings/--agent-did are graph installation options");
            // Resolve every dependency before writing any configuration.
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
            let temp = tempfile::tempdir()?;
            materialize(&pack, temp.path())?;
            let (_, report) = crate::desired_state::load_manifest_root(temp.path());
            anyhow::ensure!(
                report.errors.is_empty(),
                "invalid pack configuration: {:?}",
                report.errors
            );
            for dependency in dependencies {
                super::graph::install(
                    PackInstallArgs {
                        package: dependency.manifest.name,
                        bindings: None,
                        scope: args.scope.clone(),
                        output: args.output,
                        force_rebind_concrete_did: false,
                        registry: args.registry.clone(),
                    },
                    false,
                )
                .await?;
            }
            let binding = if args.scope.graphql.is_some() {
                ManifestAgentDidBindingArg::Live
            } else {
                ManifestAgentDidBindingArg::Home
            };
            super::config::dispatch(ConfigCommand::Apply(ConfigApplyArgs {
                root: temp.path().to_owned(),
                home: args.scope.home,
                graphql: args.scope.graphql,
                bind_agent_did: Some(binding),
                force_rebind_concrete_did: args.force_rebind_concrete_did,
                prune: false,
            }))
            .await
        }
        // A tools pack carries no documents either, just files to
        // materialize (its compiled modules), so it installs the same way
        // an assets pack does: into the home's content-addressed cache,
        // where the modules become admissible by digest.
        PackKind::Assets | PackKind::Tools => {
            anyhow::ensure!(
                args.bindings.is_none()
                    && args.scope.graphql.is_none()
                    && args.scope.agent_did.is_none()
                    && !args.force_rebind_concrete_did,
                "asset and tools packs install locally with --home; identity and graph binding flags do not apply"
            );
            let home = args
                .scope
                .home
                .context("asset and tools packs require --home")?;
            let (root, _cache_lease) = materialize_cached_pack(&home, &pack)?;
            crate::print_json(&json!({
                "pack": pack.manifest().name,
                "digest": pack.digest(),
                "installed_assets": root,
                "source": pack.label(),
            }))
        }
    }
}
