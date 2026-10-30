pub(crate) mod account;
pub(crate) mod build;
mod cache;
mod check;
mod cli_process;
mod edit;
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
use anyhow::{Context, Result};
pub(crate) use install::{
    install, install_documents_pack, install_documents_pack_resolved, wire_pack_plugins,
};
pub(crate) use scope::{
    plugin_notification_scope, record_plugin_store_change, resolve_scope_owner,
    warn_on_unrecorded_removal,
};
use serde_json::json;
use source::{asset_cache_root, materialize_cached_pack};
pub(crate) use source::{
    resolve_pack_source, resolve_subject_pack, single_slot, single_slot_agent, PackSource,
    SubjectPack,
};
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

    pub(crate) fn build_unbuilt_plugins(dir: &Path, manifest: &gents::pack::PackManifest) {
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
            from: super::source::Source::Local,
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
    gents::pack::registry::split_pack_coordinate(name)
}

async fn list(args: PackListArgs) -> Result<()> {
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let store = gents::pack::store::PackStore::new(&home);
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
#[cfg(test)]
mod tests;
