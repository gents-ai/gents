use super::*;
use crate::cli::{PackDriftArgs, PackInstallArgs, ScopeArgs};
use crate::commands::pack::test_support::{assets_pack_dir, fixture_dir};
use crate::output_format::OutputFormat;
use gents::pack::HomePackInstall;
use serde_json::json;

fn scope(home: &Path) -> ScopeArgs {
    ScopeArgs {
        home: Some(home.to_path_buf()),
        graphql: None,
        node_did: None,
    }
}

async fn install(home: &Path, package: String) -> anyhow::Result<Value> {
    crate::request_helpers::capture_report(super::super::install(PackInstallArgs {
        package,
        inference_slots: Vec::new(),
        preview: false,
        scope: scope(home),
        output: OutputFormat::Json,
        force_rebind_concrete_did: false,
        registry: None,
        drift: PackDriftArgs::default(),
        grant_authority: false,
        wire: Vec::new(),
        expect_digest: None,
        explicit: true,
    }))
    .await
}

async fn remove_pkg(home: &Path, package: &str) -> anyhow::Result<Value> {
    crate::request_helpers::capture_report(remove(PackRemoveArgs {
        package: package.to_owned(),
        scope: scope(home),
        drift: PackDriftArgs::default(),
    }))
    .await
}

fn assets_fixture_spec() -> String {
    fixture_dir("assets_fixture").to_str().unwrap().to_owned()
}

#[tokio::test]
async fn a_bundled_assets_pack_removes_without_a_node() {
    let home = tempfile::tempdir().unwrap();
    let installed = install(home.path(), assets_fixture_spec()).await.unwrap();
    let assets = Path::new(installed["installed_assets"].as_str().unwrap());
    assert!(assets.is_dir());
    assert!(
        !home.path().join("data").exists(),
        "install never opens a node"
    );

    let removed = remove_pkg(home.path(), "fixture/assets_fixture")
        .await
        .unwrap();
    assert!(!assets.exists());
    assert!(
        !home.path().join("data").exists(),
        "removal never opens a node"
    );
    assert_eq!(
        removed["removed"]["assets"].as_array().unwrap().len(),
        1,
        "{removed}"
    );

    let error = remove_pkg(home.path(), "fixture/assets_fixture")
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("is not installed"));
}

#[tokio::test]
async fn an_assets_pack_from_a_directory_releases_its_archive() {
    let home = tempfile::tempdir().unwrap();
    let installed = install(home.path(), assets_fixture_spec()).await.unwrap();
    let digest = installed["digest"].as_str().unwrap().to_owned();
    let store = gents::pack::store::PackStore::new(home.path());
    assert!(store.contains(&digest).unwrap());

    remove_pkg(home.path(), "fixture/assets_fixture")
        .await
        .unwrap();
    assert!(
        !store.contains(&digest).unwrap(),
        "the imported archive is released once nothing references it"
    );
}

#[tokio::test]
async fn a_cache_version_with_runs_is_retained_and_reported() {
    let home = tempfile::tempdir().unwrap();
    let installed = install(home.path(), assets_fixture_spec()).await.unwrap();
    let assets = Path::new(installed["installed_assets"].as_str().unwrap());
    std::fs::create_dir(assets.join("runs")).unwrap();

    let removed = remove_pkg(home.path(), "fixture/assets_fixture")
        .await
        .unwrap();
    assert!(
        assets.exists(),
        "a cache version with run history is kept, not deleted"
    );
    let retained = removed["removed"]["retained"].as_array().unwrap();
    assert_eq!(retained.len(), 1, "{retained:?}");
    assert_eq!(retained[0]["reason"], "holds run history");

    let error = remove_pkg(home.path(), "fixture/assets_fixture")
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("is not installed"));
}

#[tokio::test]
async fn a_busy_cache_lock_fails_removal_loudly() {
    let home = tempfile::tempdir().unwrap();
    let installed = install(home.path(), assets_fixture_spec()).await.unwrap();
    let assets = Path::new(installed["installed_assets"].as_str().unwrap());
    let parent = assets.parent().unwrap();
    let lock = gents::pack::materialize::lock_exclusive(parent).unwrap();

    let error = remove_pkg(home.path(), "fixture/assets_fixture")
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("pack cache is in use"),
        "{error:#}"
    );

    drop(lock);
    remove_pkg(home.path(), "fixture/assets_fixture")
        .await
        .expect("the record was untouched by the failed attempt");
}

#[tokio::test]
async fn same_name_assets_have_independent_removal_locks() {
    let home = tempfile::tempdir().unwrap();
    let mut roots = Vec::new();
    for namespace in ["acme", "zeta"] {
        let dir = assets_pack_dir(namespace, "tools", "1.0.0");
        let installed = install(home.path(), dir.path().to_str().unwrap().to_owned())
            .await
            .unwrap();
        roots.push(std::path::PathBuf::from(
            installed["installed_assets"].as_str().unwrap(),
        ));
    }
    let _lease = gents::file_lock::FileLock::shared(
        gents::pack::materialize::cache_lock(roots[1].parent().unwrap()).unwrap(),
    )
    .unwrap();

    remove_pkg(home.path(), "acme/tools").await.unwrap();
    assert!(!roots[0].exists());
    assert!(roots[1].exists());
    let error = remove_pkg(home.path(), "zeta/tools").await.unwrap_err();
    assert!(format!("{error:#}").contains("pack cache is in use"));
    assert!(gents::pack::read_home_install(home.path(), "zeta/tools")
        .unwrap()
        .is_some());
}

async fn install_unscoped_assets(home: &Path, package: String) -> HomePackInstall {
    let installed = install(home, package).await.unwrap();
    let root = Path::new(installed["installed_assets"].as_str().unwrap());
    let mut record: HomePackInstall = serde_json::from_value(installed["record"].clone()).unwrap();
    let (_, name) = super::super::split_namespace(&record.coordinate);
    record.assets = format!(
        "packs/{name}/{}",
        gents::pack::archive::digest_hex(&record.digest).unwrap()
    );
    let recorded_root = home.join(&record.assets);
    super::super::test_support::copy_tree(root, &recorded_root).unwrap();
    std::fs::remove_dir_all(root).unwrap();
    gents::pack::write_home_install(home, &record).unwrap();
    record
}

#[tokio::test]
async fn recorded_unscoped_assets_use_their_original_lock_and_retention_rules() {
    let home = tempfile::tempdir().unwrap();
    let record = install_unscoped_assets(home.path(), assets_fixture_spec()).await;
    let root = home.path().join(&record.assets);
    let _lease = gents::file_lock::FileLock::shared(
        gents::pack::materialize::cache_lock(root.parent().unwrap()).unwrap(),
    )
    .unwrap();
    let error = remove_pkg(home.path(), &record.coordinate)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("pack cache is in use"));
    assert!(root.exists());
}

#[tokio::test]
async fn recorded_unscoped_runs_are_retained_without_relocating_them() {
    let home = tempfile::tempdir().unwrap();
    let record = install_unscoped_assets(home.path(), assets_fixture_spec()).await;
    let root = home.path().join(&record.assets);
    std::fs::create_dir(root.join("runs")).unwrap();
    let removed = remove_pkg(home.path(), &record.coordinate).await.unwrap();
    assert_eq!(
        removed["removed"]["retained"][0]["reason"],
        "holds run history"
    );
    assert!(root.join("runs").exists());
}

#[tokio::test]
async fn recorded_unscoped_assets_are_released_after_verifying_their_contents() {
    let home = tempfile::tempdir().unwrap();
    let record = install_unscoped_assets(home.path(), assets_fixture_spec()).await;
    remove_pkg(home.path(), &record.coordinate).await.unwrap();
    assert!(!home.path().join(&record.assets).exists());
}

#[tokio::test]
async fn removing_a_materialized_pack_preserves_a_digest_named_namespace() {
    let home = tempfile::tempdir().unwrap();
    let dir = (0..64)
        .find_map(|version| {
            let dir = assets_pack_dir("fixture", "materialized", &format!("1.0.{version}"));
            let pack = super::super::test_support::local_pack_source(dir.path(), home.path());
            let namespace = gents::pack::archive::digest_hex(pack.digest()).unwrap();
            gents::pack::is_valid_pack_name(namespace).then_some(dir)
        })
        .expect("a pack digest beginning with a letter");
    let record =
        install_unscoped_assets(home.path(), dir.path().to_str().unwrap().to_owned()).await;
    let namespace = gents::pack::archive::digest_hex(&record.digest).unwrap();
    let other = assets_pack_dir(namespace, "tools", "1.0.0");
    let installed = install(home.path(), other.path().to_str().unwrap().to_owned())
        .await
        .unwrap();
    let root = Path::new(installed["installed_assets"].as_str().unwrap());
    std::fs::create_dir(root.join("runs")).unwrap();
    let _lease = gents::file_lock::FileLock::shared(
        gents::pack::materialize::cache_lock(root.parent().unwrap()).unwrap(),
    )
    .unwrap();

    remove_pkg(home.path(), &record.coordinate).await.unwrap();
    assert!(root.join("runs").is_dir());
    assert!(root.join("manifest.json").is_file());
    assert!(
        gents::pack::read_home_install(home.path(), &format!("{namespace}/tools"))
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn releasing_an_unpacked_pack_preserves_recorded_runs_on_every_release() {
    let home = tempfile::tempdir().unwrap();
    let dir = assets_pack_dir("fixture", "unpacked", "1.0.0");
    let record =
        install_unscoped_assets(home.path(), dir.path().to_str().unwrap().to_owned()).await;
    let root = home.path().join(&record.assets);
    std::fs::create_dir(root.join("runs")).unwrap();
    std::fs::write(root.join("runs/receipt.json"), "retained").unwrap();

    let removed = remove_pkg(home.path(), &record.coordinate).await.unwrap();
    assert_eq!(
        removed["removed"]["retained"][0]["reason"],
        "holds run history"
    );
    assert_eq!(removed["removed"]["archives"], json!([record.digest]));
    let store = gents::pack::store::PackStore::new(home.path());
    assert!(!store.contains(&record.digest).unwrap());
    assert!(store.lookup("fixture", "unpacked", None).unwrap().is_none());
    assert!(!store.release(&record.digest).unwrap());
    assert_eq!(
        std::fs::read_to_string(root.join("runs/receipt.json")).unwrap(),
        "retained"
    );
    assert!(root.join("manifest.json").is_file());
}

#[tokio::test]
async fn unscoped_receipts_cannot_release_another_namespace_or_modified_assets() {
    let home = tempfile::tempdir().unwrap();
    let record = install_unscoped_assets(home.path(), assets_fixture_spec()).await;
    let mut forged = record.clone();
    forged.coordinate = "other/assets_fixture".to_owned();
    gents::pack::write_home_install(home.path(), &forged).unwrap();
    let error = remove_pkg(home.path(), &forged.coordinate)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("digest belongs to a different pack"));
    assert!(
        gents::pack::read_home_install(home.path(), &forged.coordinate)
            .unwrap()
            .is_some()
    );

    let root = home.path().join(&record.assets);
    std::fs::write(root.join("README.md"), "changed").unwrap();
    let error = remove_pkg(home.path(), &record.coordinate)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("does not match"));
    assert!(root.exists());
}

#[tokio::test]
async fn forged_asset_paths_do_not_release_files_or_install_receipts() {
    let home = tempfile::tempdir().unwrap();
    install(home.path(), assets_fixture_spec()).await.unwrap();
    let record = gents::pack::read_home_install(home.path(), "fixture/assets_fixture")
        .unwrap()
        .unwrap();
    for assets in [
        home.path()
            .join(&record.assets)
            .to_string_lossy()
            .into_owned(),
        record.assets.replace("/fixture/", "/other/"),
        format!("../{}", record.assets),
        "packs/store".to_owned(),
    ] {
        let mut forged = record.clone();
        forged.assets = assets;
        gents::pack::write_home_install(home.path(), &forged).unwrap();
        let error = remove_pkg(home.path(), &record.coordinate)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("recorded asset path"));
        assert!(home.path().join(&record.assets).exists());
        assert!(
            gents::pack::read_home_install(home.path(), &record.coordinate)
                .unwrap()
                .is_some()
        );
    }
}

fn plugin_pack_dir(
    namespace: &str,
    pack_name: &str,
    plugin_name: &str,
    afb: &[u8],
) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let manifest = json!({
        "manifest_version": 1,
        "name": pack_name,
        "namespace": namespace,
        "version": "1.0.0",
        "description": "a test plugins pack",
        "authors": [namespace],
        "tags": [],
        "kind": "plugins",
        "assets": ["README.md", format!("plugins/{plugin_name}.afb")],
        "plugins": [{
            "name": plugin_name,
            "description": "test",
            "artifact": format!("plugins/{plugin_name}.afb"),
            "language": "rust",
            "input_schema": {"type": "object"},
        }],
    });
    std::fs::write(
        dir.path().join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.path().join("README.md"), "# test plugins pack").unwrap();
    std::fs::create_dir_all(dir.path().join("plugins")).unwrap();
    std::fs::write(
        dir.path()
            .join("plugins")
            .join(format!("{plugin_name}.afb")),
        afb,
    )
    .unwrap();
    dir
}

#[tokio::test]
async fn a_plugins_pack_releases_its_plugins_and_unreferenced_bytes() {
    let home = tempfile::tempdir().unwrap();
    let afb = crate::commands::plugin::testing::build_plugin_afb(
        "shared",
        b"fn main() { println!(\"{{}}\"); }",
    );
    let digest_hex = {
        use sha2::Digest;
        format!("{:x}", sha2::Sha256::digest(&afb))
    };

    let acme = plugin_pack_dir("acme", "tools", "shared", &afb);
    let zeta = plugin_pack_dir("zeta", "tools", "shared", &afb);
    install(home.path(), acme.path().to_str().unwrap().to_owned())
        .await
        .unwrap();
    install(home.path(), zeta.path().to_str().unwrap().to_owned())
        .await
        .unwrap();
    assert!(gents::plugin::store::read_bytes(home.path(), &digest_hex).is_ok());

    let removed = remove_pkg(home.path(), "acme/tools").await.unwrap();
    assert!(
        removed["removed"]["plugin_bytes"]
            .as_array()
            .unwrap()
            .is_empty(),
        "bytes zeta/tools still references are kept: {removed}"
    );
    assert!(gents::plugin::store::read_bytes(home.path(), &digest_hex).is_ok());
    assert!(gents::plugin::store::read_record(home.path(), "acme", "shared").is_err());
    assert!(gents::plugin::store::read_record(home.path(), "zeta", "shared").is_ok());

    let removed = remove_pkg(home.path(), "zeta/tools").await.unwrap();
    assert_eq!(
        removed["removed"]["plugin_bytes"],
        json!([format!("sha256:{digest_hex}")]),
        "{removed}"
    );
    assert!(
        gents::plugin::store::read_bytes(home.path(), &digest_hex).is_err(),
        "nothing references the bytes any more"
    );
}

fn documents_pack_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("manifest.json"),
        serde_json::to_vec(&json!({
            "manifest_version": 1, "name": "demo_pack", "version": "1.0.0",
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

async fn demo_tools_rows(home: &Path) -> usize {
    let (access, _) = crate::resolve_config_access(Some(home), None)
        .await
        .unwrap();
    let response = access
        .execute(r#"{ Tools(filter: {tools_id: {_eq: "demo-tools"}}) { _docID } }"#)
        .await
        .unwrap();
    let count = response["data"]["Tools"].as_array().unwrap().len();
    if let Some(node) = access.local_node() {
        node.shutdown().await;
    }
    count
}

#[tokio::test]
async fn failed_document_removal_releases_its_node_before_the_next_command() {
    let home = initialized_home().await;
    let error = remove_pkg(home.path(), "gents/missing").await.unwrap_err();
    assert!(format!("{error:#}").contains("gents/missing is not installed"));
    assert_eq!(demo_tools_rows(home.path()).await, 0);
}

#[tokio::test]
async fn documents_pack_with_a_home_record_is_removed_from_the_node_and_forgotten() {
    let home = initialized_home().await;
    let pack = documents_pack_dir();
    install(home.path(), pack.path().to_str().unwrap().to_owned())
        .await
        .unwrap();
    let record = gents::pack::read_home_install(home.path(), "gents/demo_pack")
        .unwrap()
        .expect("a documents install records itself in the home");
    assert_eq!(record.kind, gents::pack::PackKind::Documents);
    assert_eq!(demo_tools_rows(home.path()).await, 1);

    remove_pkg(home.path(), "gents/demo_pack").await.unwrap();

    assert_eq!(
        demo_tools_rows(home.path()).await,
        0,
        "the pack's documents leave the node"
    );
    assert!(
        gents::pack::read_home_install(home.path(), "gents/demo_pack")
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn assets_pack_still_takes_the_early_path() {
    let home = tempfile::tempdir().unwrap();
    install(home.path(), assets_fixture_spec()).await.unwrap();
    let record = gents::pack::read_home_install(home.path(), "fixture/assets_fixture")
        .unwrap()
        .unwrap();
    assert_eq!(record.kind, gents::pack::PackKind::Assets);

    remove_pkg(home.path(), "fixture/assets_fixture")
        .await
        .unwrap();

    assert!(!home.path().join("data").exists(), "no node was opened");
    assert!(
        gents::pack::read_home_install(home.path(), "fixture/assets_fixture")
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn referenced_plugin_removal_refuses_before_changing_pack_or_store_records() {
    let afb = tokio::task::spawn_blocking(|| {
        crate::commands::plugin::testing::build_plugin_afb(
            "shared",
            b"fn main() { println!(\"{{}}\"); }",
        )
    })
    .await
    .unwrap();
    for documents in [true, false] {
        let home = initialized_home().await;
        let pack = plugin_pack_dir("acme", "tools", "shared", &afb);
        if documents {
            let path = pack.path().join("manifest.json");
            let mut manifest: Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            manifest["kind"] = json!("documents");
            manifest["config"] = json!("pack_config.json");
            manifest["assets"]
                .as_array_mut()
                .unwrap()
                .push(json!("pack_config.json"));
            std::fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
            std::fs::write(
                pack.path().join("pack_config.json"),
                serde_json::to_vec(&json!({
                    "node": {"node_did": "${GENTS_PACK_NODE_DID}"},
                    "tools": [{"tools_id": "owned-tools"}],
                }))
                .unwrap(),
            )
            .unwrap();
        }
        install(home.path(), pack.path().to_str().unwrap().to_owned())
            .await
            .unwrap();
        let (access, owner) = super::super::resolve_scope_owner(&scope(home.path()))
            .await
            .unwrap();
        let mut request = crate::commands::pack::wire::WireRequest {
            plugin: "acme/shared".into(),
            agent: gents::default_agent_id_for_node(&owner),
            targets_from: Vec::new(),
            remove: false,
            preview: false,
        };
        crate::commands::pack::wire::wire_in(&access, &owner, home.path(), &request)
            .await
            .unwrap();
        access.local_node().unwrap().shutdown().await;
        drop(access);
        if !documents {
            let error = crate::request_helpers::capture_report(crate::commands::plugin::dispatch(
                crate::cli::PluginCommand::Remove(crate::cli::PluginRemoveArgs {
                    name: "acme/shared".into(),
                    home: Some(home.path().to_path_buf()),
                }),
            ))
            .await
            .unwrap_err();
            assert!(format!("{error:#}").contains("acme/shared"));
        }
        let before = gents::pack::read_home_install(home.path(), "acme/tools").unwrap();
        let plugin = gents::plugin::store::read_record(home.path(), "acme", "shared").unwrap();
        let error = remove_pkg(home.path(), "acme/tools").await.unwrap_err();
        let error = format!("{error:#}");
        assert!(
            error.contains("acme/shared") && error.contains("--remove"),
            "{error}"
        );
        assert_eq!(
            gents::pack::read_home_install(home.path(), "acme/tools").unwrap(),
            before
        );
        assert_eq!(
            gents::plugin::store::read_record(home.path(), "acme", "shared").unwrap(),
            plugin
        );
        let (access, owner) = super::super::resolve_scope_owner(&scope(home.path()))
            .await
            .unwrap();
        request.remove = true;
        crate::commands::pack::wire::wire_in(&access, &owner, home.path(), &request)
            .await
            .unwrap();
        access.local_node().unwrap().shutdown().await;
        drop(access);
        remove_pkg(home.path(), "acme/tools").await.unwrap();
        assert!(gents::plugin::store::read_record(home.path(), "acme", "shared").is_err());
    }
}

async fn build_shared_plugin(source: &'static [u8]) -> Vec<u8> {
    tokio::task::spawn_blocking(move || {
        crate::commands::plugin::testing::build_plugin_afb("shared", source)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn wiring_holds_store_lock_through_pin_commit() {
    let home = initialized_home().await;
    let old = build_shared_plugin(b"fn main() { println!(\"old\"); }").await;
    let new = build_shared_plugin(b"fn main() { println!(\"new\"); }").await;
    let original = plugin_pack_dir("acme", "tools", "shared", &old);
    let replacement = plugin_pack_dir("acme", "tools", "shared", &new);
    install(home.path(), original.path().to_str().unwrap().to_owned())
        .await
        .unwrap();
    let (access, owner) = super::super::resolve_scope_owner(&scope(home.path()))
        .await
        .unwrap();
    let expected_digest = gents::plugin::store::read_record(home.path(), "acme", "shared")
        .unwrap()
        .digest;
    let pause = std::sync::Arc::new(super::super::wire::WireTestPause {
        home: home.path().to_owned(),
        reached: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    *super::super::wire::WIRE_TEST_PAUSE.lock().unwrap() = Some(pause.clone());
    let writer = tokio::spawn({
        let access = access.clone();
        let owner = owner.clone();
        let path = home.path().to_owned();
        async move {
            let request = super::super::wire::WireRequest {
                plugin: "acme/shared".into(),
                agent: gents::default_agent_id_for_node(&owner),
                targets_from: Vec::new(),
                remove: false,
                preview: false,
            };
            super::super::wire::wire_in(&access, &owner, &path, &request).await
        }
    });
    pause.reached.notified().await;
    let (archive_bytes, _) = gents::pack::archive::pack_dir(replacement.path()).unwrap();
    let archive = gents::pack::archive::PackArchive::from_bytes(&archive_bytes).unwrap();
    let notify_access = access.clone();
    let notify_owner = owner.clone();
    let notify_home = home.path().to_owned();
    let result = gents::pack::plugin_lifecycle::install(
        home.path(),
        &archive,
        false,
        Some(&owner),
        &std::collections::BTreeMap::new(),
        move |identity| async move {
            gents::pack::record_plugin_store_change(
                &notify_access,
                &notify_owner,
                &notify_home,
                &identity.coordinate,
                Some(&identity),
            )
            .await
        },
    )
    .await;
    let snapshot: Value =
        tokio::task::spawn_blocking(|| gents_lean_contract::load_contract_snapshot().unwrap())
            .await
            .unwrap();
    let allowed = snapshot["pack_wire_lock_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["held"] == true)
        .unwrap()["publish_allowed"]
        .as_bool()
        .unwrap();
    assert_eq!(
        result.is_ok(),
        allowed,
        "replacement must contend with the wire lock: {result:?}"
    );
    assert!(format!("{:#}", result.unwrap_err()).contains("plugin store is busy"));
    pause.release.notify_one();
    writer.await.unwrap().unwrap();
    *super::super::wire::WIRE_TEST_PAUSE.lock().unwrap() = None;
    let agent = gents::default_agent_id_for_node(&owner);
    let (_, tools) = access
        .transact_write("test.wire.pin", |txn| {
            let owner = &owner;
            let agent = &agent;
            Box::pin(async move {
                gents::documents::load_agent_context_and_tools_in_txn(txn, owner, agent).await
            })
        })
        .await
        .unwrap();
    let tools = tools.unwrap();
    let pin = tools
        .integrations
        .unwrap()
        .plugins
        .unwrap()
        .into_iter()
        .find(|entry| entry.plugin == "acme/shared")
        .unwrap()
        .digest;
    assert_eq!(pin.as_deref(), Some(expected_digest.as_str()));
    access.local_node().unwrap().shutdown().await;
}

#[tokio::test]
async fn documents_pack_with_schema_patch_installs_after_schema_application() {
    let home = initialized_home().await;
    let afb = build_shared_plugin(b"fn main() { println!(\"{{}}\"); }").await;
    let pack = plugin_pack_dir("acme", "schema_patch", "shared", &afb);
    let manifest_path = pack.path().join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["kind"] = json!("documents");
    manifest["config"] = json!("pack_config.json");
    manifest["assets"].as_array_mut().unwrap().extend([
        json!("pack_config.json"),
        json!("schemas/patch_probe.graphql"),
        json!("schemas/patch_probe.patch.json"),
    ]);
    std::fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    std::fs::write(
        pack.path().join("pack_config.json"),
        serde_json::to_vec(&json!({
            "node": {"node_did": "${GENTS_PACK_NODE_DID}"},
            "tools": [{"tools_id": "patch-fixture-tools"}]
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::create_dir_all(pack.path().join("schemas")).unwrap();
    std::fs::write(
        pack.path().join("schemas/patch_probe.graphql"),
        "type PackPatchProbe { original: String }",
    )
    .unwrap();
    std::fs::write(pack.path().join("schemas/patch_probe.patch.json"), serde_json::to_vec(&json!([
        {"op":"add", "path":"/PackPatchProbe/Fields/-", "value":{"Name":"added", "Kind":"String"}}
    ])).unwrap()).unwrap();
    install(home.path(), pack.path().to_str().unwrap().to_owned())
        .await
        .unwrap();
    let (access, _) = super::super::resolve_scope_owner(&scope(home.path()))
        .await
        .unwrap();
    let response = access
        .execute("{ PackPatchProbe { added } }")
        .await
        .unwrap();
    assert_eq!(response["data"]["PackPatchProbe"], json!([]));
    let tools = access
        .execute("{ Tools(filter: {tools_id: {_eq: \"patch-fixture-tools\"}}) { tools_id } }")
        .await
        .unwrap();
    assert_eq!(tools["data"]["Tools"].as_array().unwrap().len(), 1);
    access.local_node().unwrap().shutdown().await;
}

#[tokio::test]
async fn remote_wire_refuses_new_pin_before_http_access_but_allows_preview_and_remove() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new().fallback({
        let calls = calls.clone();
        move || {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                axum::http::StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let home = tempfile::tempdir().unwrap();
    let access = gents::store::config_client::ConfigAccess::graphql(format!(
        "http://{address}/api/v0/graphql"
    ));
    let cases: Value =
        tokio::task::spawn_blocking(|| gents_lean_contract::load_contract_snapshot().unwrap())
            .await
            .unwrap();
    for (preview, remove) in [(false, false), (true, false), (false, true)] {
        let request = super::super::wire::WireRequest {
            plugin: "acme/missing".into(),
            agent: "engineer".into(),
            targets_from: vec![],
            remove,
            preview,
        };
        let error = super::super::wire::wire_in(&access, "did:key:remote", home.path(), &request)
            .await
            .unwrap_err();
        let text = format!("{error:#}");
        let allowed = cases["pack_wire_target_cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| {
                case["local"] == false && case["preview"] == preview && case["remove"] == remove
            })
            .unwrap()["allowed"]
            .as_bool()
            .unwrap();
        assert_eq!(
            text.contains("cannot publish a plugin pin through GraphQL"),
            !allowed,
            "{text}"
        );
        if !allowed {
            assert!(
                text.contains("stop the runtime")
                    && text.contains("--home <node-home>")
                    && text.contains("without --graphql"),
                "{text}"
            );
        } else {
            assert!(text.contains("not installed"), "{text}");
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn wire_refuses_even_the_initialized_homes_served_endpoint() {
    let home = initialized_home().await;
    let init = crate::home_state::read_init_config(home.path())
        .unwrap()
        .unwrap();
    let url = "http://127.0.0.1:56789/api/v0/graphql";
    crate::home_state::write_runtime_state(
        home.path(),
        &crate::shared::StoredRuntimeState {
            home: home.path().to_string_lossy().into_owned(),
            graphql: url.into(),
            node_name: init.node_name.clone(),
            node_did: init.node_did.clone(),
            default_agent_id: gents::default_agent_id_for_node(&init.node_did),
            p2p_transport: "iroh".into(),
            p2p_peer_id: None,
            p2p_listen_addresses: vec![],
            p2p_admission: None,
        },
    )
    .unwrap();
    let access = gents::store::config_client::ConfigAccess::graphql(url);
    let request = super::super::wire::WireRequest {
        plugin: "acme/missing".into(),
        agent: "engineer".into(),
        targets_from: vec![],
        remove: false,
        preview: false,
    };
    let error = super::super::wire::wire_in(&access, &init.node_did, home.path(), &request)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("stop the runtime"));
}

#[tokio::test]
async fn removal_holds_plugin_lock_after_reference_check_until_deletion() {
    let home = initialized_home().await;
    let afb = build_shared_plugin(b"fn main() { println!(\"{{}}\"); }").await;
    let pack = plugin_pack_dir("acme", "tools", "shared", &afb);
    install(home.path(), pack.path().to_str().unwrap().to_owned())
        .await
        .unwrap();
    let pause = std::sync::Arc::new(super::RemovalTestPause {
        home: home.path().to_owned(),
        reached: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    *super::REMOVAL_TEST_PAUSE.lock().unwrap() = Some(pause.clone());
    let remover = tokio::spawn({
        let home = home.path().to_owned();
        async move { super::remove_home_install(&home, "acme/tools").await }
    });
    pause.reached.notified().await;
    let (access, owner) = super::super::resolve_scope_owner(&scope(home.path()))
        .await
        .unwrap();
    let request = super::super::wire::WireRequest {
        plugin: "acme/shared".into(),
        agent: gents::default_agent_id_for_node(&owner),
        targets_from: vec![],
        remove: false,
        preview: false,
    };
    let result = super::super::wire::wire_in(&access, &owner, home.path(), &request).await;
    let snapshot: Value =
        tokio::task::spawn_blocking(|| gents_lean_contract::load_contract_snapshot().unwrap())
            .await
            .unwrap();
    let allowed = snapshot["pack_wire_lock_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["held"] == true)
        .unwrap()["publish_allowed"]
        .as_bool()
        .unwrap();
    assert_eq!(result.is_ok(), allowed, "{result:?}");
    assert!(format!("{:#}", result.unwrap_err()).contains("plugin store is busy"));
    access.local_node().unwrap().shutdown().await;
    drop(access);
    pause.release.notify_one();
    remover.await.unwrap().unwrap();
    *super::REMOVAL_TEST_PAUSE.lock().unwrap() = None;
    assert!(gents::plugin::store::read_record(home.path(), "acme", "shared").is_err());
}

#[tokio::test]
async fn delegated_home_removal_uses_the_existing_store_guard() {
    let home = initialized_home().await;
    let afb = build_shared_plugin(b"fn main() { println!(\"{{}}\"); }").await;
    let pack = plugin_pack_dir("acme", "tools", "shared", &afb);
    install(home.path(), pack.path().to_str().unwrap().to_owned())
        .await
        .unwrap();
    let guard = gents::plugin::store::try_lock_store(home.path()).unwrap();
    super::remove_home_install_with_guard(home.path(), "acme/tools", guard)
        .await
        .unwrap();
    assert!(gents::plugin::store::read_record(home.path(), "acme", "shared").is_err());
}
