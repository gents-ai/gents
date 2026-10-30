use super::install::{check_expected_digest, check_wire_target, install_documents_command};
use super::scope::resolve_plugin_slot_owner;
use super::source::names_a_directory;
use super::*;
use gents::pack::{PackKind, PackManifest};

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
        message.contains("is not in the pack store of") && message.contains("could not be reached"),
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
        let outcome =
            crate::request_helpers::capture_report(install_documents_command(access, &pack, &args))
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
