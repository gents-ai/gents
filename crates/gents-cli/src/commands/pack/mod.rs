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
    crate::print_json(&list_packs(&home)?)
}

fn list_packs(home: &std::path::Path) -> Result<serde_json::Value> {
    let store = gents::pack::store::PackStore::new(home);
    let (records, skipped) = gents::pack::list_home_installs_with_skips(home)?;
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
        let listing = gents::pack::store::list_pack_versions(
            &versions,
            record.map(|record| gents::pack::store::StoredName {
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
#[cfg(test)]
mod tests;

<<<<<<< HEAD
=======
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
    #[expect(
        clippy::disallowed_methods,
        reason = "synchronous test retrying a file lock that has no release notification"
    )]
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

    #[tokio::test]
    async fn plugin_store_record_error_releases_the_owned_offline_store() {
        let home = initialized_home().await;
        let error = record_plugin_store_change(home.path(), "invalid coordinate", None)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("invalid pack coordinate"));
        let (reopened, _) = crate::resolve_config_access(Some(home.path()), None)
            .await
            .expect("a failed record must drain its owned store before returning");
        let records = reopened
            .execute("{ PackInstallation(limit: 1) { coordinate } }")
            .await;
        reopened.local_node().unwrap().shutdown().await;
        assert_eq!(
            records.unwrap()["data"]["PackInstallation"],
            serde_json::json!([])
        );
    }

    fn document_install_args(
        home: &std::path::Path,
        spec: &std::path::Path,
        inference_slots: Vec<String>,
    ) -> PackInstallArgs {
        PackInstallArgs {
            package: spec.to_str().unwrap().to_owned(),
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

        let staged = documents_pack_dir("1.1.0");
        let mut preview = document_install_args(home.path(), staged.path(), Vec::new());
        preview.preview = true;
        crate::request_helpers::capture_report(install(preview))
            .await
            .unwrap();
        let listed = list_packs(home.path()).unwrap();
        let entry = listed["packs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["pack"] == "gents/demo_pack")
            .unwrap();
        assert_eq!(entry["installed"], true);
        assert_eq!(entry["version"], "1.0.0");
        assert_eq!(entry["digest"], record.digest);
        assert_eq!(entry["available_version"], "1.1.0");
        assert_eq!(entry["other_stored_versions"][0]["version"], "1.1.0");
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
    fn wire_refuses_a_pluginless_pack() {
        let wire = vec!["agent".to_owned()];
        let plugins = plugin_manifest("acme", "echo_pack", "echo", "1.0.0");
        check_wire_target(&plugins, &wire).unwrap();
        check_wire_target(&plugins, &[]).unwrap();

        let mut pluginless = plugins;
        pluginless.metadata.plugins.clear();
        let error = check_wire_target(&pluginless, &wire).unwrap_err();
        assert!(
            format!("{error:#}").contains("--wire needs a pack that ships a plugin"),
            "{error:#}"
        );
    }
}
>>>>>>> 6532f7bfd (Exercise pack previews in version reporting)
