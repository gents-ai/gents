use super::*;
use crate::cli::{GraphScopeArgs, PackDriftArgs, PackInstallArgs};
use crate::commands::pack::test_support::fixture_dir;
use crate::output_format::OutputFormat;
use serde_json::json;

fn scope(home: &Path) -> GraphScopeArgs {
    GraphScopeArgs {
        home: Some(home.to_path_buf()),
        graphql: None,
        agent_did: None,
    }
}

async fn install(home: &Path, package: String) -> anyhow::Result<Value> {
    crate::request_helpers::capture_report(super::super::install(PackInstallArgs {
        package,
        bindings: None,
        inference_slots: Vec::new(),
        preview: false,
        scope: scope(home),
        output: OutputFormat::Json,
        force_rebind_concrete_did: false,
        registry: None,
        drift: PackDriftArgs::default(),
        grant_authority: false,
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

/// The fixture assets pack, as a spec string `install` accepts.
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
    let store = gents::pack_store::PackStore::new(home.path());
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
    let lock = super::super::lock_exclusive(parent).unwrap();

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

/// A minimal `plugins`-kind pack directory: one plugin, `name`'s bytes.
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
