pub(crate) mod account;
pub(crate) mod build;
mod cache;
mod check;
mod edit;
mod export;
mod import;
mod inspect;
mod install;
mod local;
pub(crate) mod registry;
mod remove;
pub(crate) use remove::{ensure_home_plugins_unreferenced, remove, remove_home_install_with_guard};
mod scaffold;
mod scenario;
mod scope;
mod server;
mod source;
mod test;
pub(crate) mod update;
pub(crate) mod wire;
use crate::cli::*;
use anyhow::Result;
pub(crate) use install::{install, install_documents_pack};
pub(crate) use scope::{
    plugin_notification_scope, record_plugin_retirement, resolve_home_owner_under_plugin_store,
    resolve_scope_owner, warn_on_unrecorded_removal,
};
use serde_json::json;
use source::asset_cache_root;
pub(crate) use source::{resolve_subject_pack, single_slot, single_slot_agent, SubjectPack};

use gents_pack::pack::materialize::CacheRelease;
use gents_pack::pack::PackChangeError;

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
        PackCommand::Export(args) => export::export(args).await,
        PackCommand::Diff(args) => inspect::diff(args).await,
        PackCommand::Test(args) => test::test(args).await,
        PackCommand::Check(args) => check::check(args).await,
        PackCommand::Install(args) => install(args).await.map_err(PackChangeError::into_anyhow),
        PackCommand::Wire(args) => wire::wire(args).await,
        PackCommand::Starter(args) => crate::commands::init::default_packs::starter(args).await,
        PackCommand::Remove(args) => remove::remove(args)
            .await
            .map_err(PackChangeError::into_anyhow),
        PackCommand::Outdated(args) => update::outdated(args).await,
        PackCommand::Update(args) => update::update(args)
            .await
            .map_err(PackChangeError::into_anyhow),
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
        PackCommand::Plugin { command } => crate::commands::plugin::dispatch(command).await,
        PackCommand::Ux { command } => crate::commands::ux::dispatch(command).await,
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

    pub(crate) fn build_unbuilt_plugins(dir: &Path, manifest: &gents_pack::pack::PackManifest) {
        for plugin in &manifest.metadata.plugins {
            if plugin.source.is_some() && !dir.join(&plugin.artifact).is_file() {
                super::build::build_plugin(dir, manifest, plugin)
                    .unwrap_or_else(|error| panic!("building plugin {}: {error:#}", plugin.name));
            }
        }
    }

    pub(crate) fn local_pack_source(
        dir: &Path,
        home: &Path,
    ) -> gents_pack::pack::resolved_pack::ResolvedPack {
        let archive = super::local::open(&super::local::LocalName::Path(dir), home)
            .unwrap_or_else(|error| panic!("opening {}: {error:#}", dir.display()));
        gents_pack::pack::resolved_pack::ResolvedPack::received(
            archive,
            gents_pack::pack::PackSource::Local { file_name: None },
        )
        .unwrap_or_else(|error| panic!("receiving {}: {error:#}", dir.display()))
    }

    pub(crate) fn fixture_pack_source(
        name: &str,
        home: &Path,
    ) -> gents_pack::pack::resolved_pack::ResolvedPack {
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

async fn list(args: PackListArgs) -> Result<()> {
    let home = gents_service::home_state::resolve_home_dir(args.home.as_deref());
    crate::print_json(&list_packs(&home)?)
}

fn list_packs(home: &std::path::Path) -> Result<serde_json::Value> {
    let store = gents_pack::pack::store::PackStore::new(home);
    let (records, skipped) = gents_pack::pack::list_home_installs_with_skips(home)?;
    let installed: std::collections::BTreeMap<_, _> = records
        .into_iter()
        .map(|record| (record.coordinate.clone(), record))
        .collect();
    let mut names: std::collections::BTreeMap<_, _> = store.names()?.into_iter().collect();
    for coordinate in installed.keys() {
        names.entry(coordinate.clone()).or_default();
    }
    let mut entries = Vec::new();
    for (coordinate, versions) in names {
        let record = installed.get(&coordinate);
        let listing = gents_pack::pack::store::list_pack_versions(
            &versions,
            record.map(|record| gents_pack::pack::store::StoredName {
                version: record.version.clone(),
                digest: record.digest.clone(),
            }),
        );
        let preferred = versions.first();
        let display = listing.installed.as_ref().or(preferred);
        let archive = display.map(|entry| store.open(&entry.digest)).transpose();
        let (manifest, metadata_error) = match &archive {
            Ok(Some(archive)) => (Some(archive.manifest()), None),
            Ok(None) => (None, None),
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        entries.push(json!({
            "pack": coordinate,
            "versions": versions.iter().map(|entry| &entry.version).collect::<Vec<_>>(),
            "version": listing.installed.as_ref().map(|entry| &entry.version),
            "digest": listing.installed.as_ref().map(|entry| &entry.digest),
            "available_version": preferred.map(|entry| &entry.version),
            "available_digest": preferred.map(|entry| &entry.digest),
            "other_stored_versions": listing.other_stored.iter().map(|entry| json!({"version": entry.version, "digest": entry.digest})).collect::<Vec<_>>(),
            "kind": manifest.map(|manifest| &manifest.metadata.kind).or_else(|| record.map(|record| &record.kind)),
            "description": manifest.map(|manifest| &manifest.description),
            "inference_slots": manifest.map(|manifest| &manifest.metadata.inference_slots),
            "metadata_error": metadata_error,
            "installed": record.is_some(),
        }));
    }
    let mut output = json!({ "packs": entries });
    if !skipped.is_empty() {
        output["skipped_installs"] = skipped
            .iter()
            .map(|skip| json!({ "path": skip.path.display().to_string(), "reason": skip.reason }))
            .collect();
    }
    Ok(output)
}
/// A refused pack operation as the terminal shows it: what happened, then
/// the command its error names.
pub(crate) fn refusal(
    operation: gents_protocol::operations::pack::PackOperation,
    package: &str,
    error: gents_protocol::operations::pack::PackOperationError,
) -> anyhow::Error {
    use gents_protocol::operations::NextCall;
    if error.passthrough().is_some() {
        return anyhow::Error::new(error);
    }
    let next = match error.recovery(operation) {
        NextCall::Preview { resource, .. } => format!("gents config {resource} show {package}"),
        NextCall::List { resource } => format!("gents config {resource} list"),
        NextCall::Verb { resource, verb } => format!("gents config {resource} {verb}"),
        NextCall::VerbHelp { resource, verb } => format!("gents config {resource} {verb} --help"),
        NextCall::ResourceHelp { resource } | NextCall::Inspect { resource } => {
            format!("gents config {resource} --help")
        }
    };
    anyhow::anyhow!("{error}. Next: {next}")
}

#[cfg(test)]
mod tests;
