use super::install::{
    check_expected_digest, check_wire_target, install_documents_command_with_local,
};
use super::scope::resolve_plugin_slot_owner;
use super::source::names_a_directory;
use super::*;
use crate::packs::EditedDocuments;
use gents::pack::{PackChangeError, PackKind, PackManifest};
use gents::plugin::authority::GrantError;

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
    let config = gents::pack::load_pack_authoring_config(
        pack.manifest(),
        &gents::pack::PackInstallOptions {
            node_did: "did:key:zPackCatalogValidationOwner".into(),
        },
        &|path| pack.asset(path).map(Vec::from),
        &|_| None,
    )
    .unwrap();
    assert!(config.agent("fixture-worker").is_some());
    gents::store::config_client::DesiredStateApplyPlan::from_pack_config(&config.documents)
        .unwrap();
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

#[test]
fn document_pack_preview_shows_the_install_variable_and_its_target() {
    let dir = documents_pack_dir("1.0.0");
    let config_path = dir.path().join("pack_config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["tasks"] =
        json!([{ "task_id": "research", "prompt_template": "${GENTS_TEST_3317_UNSET:-fallback}" }]);
    std::fs::write(config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let (bytes, _) = gents::pack::archive::pack_dir(dir.path()).unwrap();
    let archive = gents::pack::archive::PackArchive::from_bytes(&bytes).unwrap();
    let source =
        super::source::PackSource::received(archive, gents::pack::PackSource::Installed).unwrap();
    let preview = super::install::install_variable_preview(&source, "did:key:node").unwrap();
    assert_eq!(preview["kind"], "install_literal");
    assert_eq!(preview["uses"][0]["path"], "/node/node_did");
    assert_eq!(
        preview["uses"][0]["document"],
        json!({"collection": "Node", "id": "did:key:node"})
    );
    assert_eq!(
        preview["uses"][0]["variables"],
        json!([{"name": "GENTS_PACK_NODE_DID", "source": "runtime", "present": true}])
    );
    let task = preview["uses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == "/tasks/0/prompt_template")
        .unwrap();
    assert_eq!(
        task["document"],
        json!({"collection": "Task", "id": "research"})
    );
    assert_eq!(
        task["variables"],
        json!([{"name": "GENTS_TEST_3317_UNSET", "source": "default", "present": false}])
    );
    for item in preview["uses"].as_array().unwrap() {
        assert!(item.get("resolved").is_none(), "{item}");
    }
}

#[test]
fn dependency_preview_reads_installed_literals_instead_of_the_current_environment() {
    let dir = documents_pack_dir("1.0.0");
    let config_path = dir.path().join("pack_config.json");
    let mut authored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    authored["tools"][0]["display_name"] = json!("${DISPLAY_LABEL:-fallback}");
    std::fs::write(config_path, serde_json::to_vec(&authored).unwrap()).unwrap();
    let (bytes, _) = gents::pack::archive::pack_dir(dir.path()).unwrap();
    let source = super::source::PackSource::received(
        gents::pack::archive::PackArchive::from_bytes(&bytes).unwrap(),
        gents::pack::PackSource::Installed,
    )
    .unwrap();
    let config: gents::documents::PackConfig = serde_json::from_value(json!({
        "node": {"node_did": "did:key:node"},
        "tools": [{"node_did": "did:key:node", "tools_id": "demo-tools", "display_name": "installed value"}]
    })).unwrap();
    let preview = super::install::reused_variable_preview(&source, &config).unwrap();
    let node = preview["uses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == "/node/node_did")
        .unwrap();
    assert!(node.get("resolved").is_none(), "{node}");
    let target = preview["uses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == "/tools/0/display_name")
        .unwrap();
    assert_eq!(
        target["variables"],
        json!([{"name": "DISPLAY_LABEL", "source": "installed", "present": true}])
    );
    assert!(
        !preview.to_string().contains("installed value"),
        "a preview shows an installed variable's value: {preview}"
    );
    assert_eq!(
        target["document"],
        json!({"collection": "Tools", "id": "demo-tools"})
    );
}

#[test]
fn dependency_variable_overrides_are_scoped_and_reject_duplicates() {
    let parse = super::install::parse_dependency_variables;
    let values = parse(&["acme/alpha:HOST=one=two".into(), "acme/beta:HOST=".into()]).unwrap();
    assert_eq!(values["acme/alpha"]["HOST"].as_deref(), Some("one=two"));
    assert_eq!(values["acme/beta"]["HOST"].as_deref(), Some(""));
    assert!(parse(&["acme/alpha:HOST=one".into(), "acme/alpha:HOST=two".into()]).is_err());
    assert!(parse(&["HOST=one".into()]).is_err());
}

#[test]
fn remote_plugin_routing_does_not_resolve_install_variables() {
    let config = json!({"tools": [{"integrations": {"plugins": [{"plugin": "${UNSET_PLUGIN}"}]}}]});
    assert!(super::install::has_plugin_references(&config));
    assert!(!super::install::has_plugin_references(
        &json!({"tools": [{"integrations": {"plugins": []}}]})
    ));
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
    let Some(crate::cli::Command::Init(init_args)) = cli.command else {
        panic!("expected init")
    };
    crate::commands::init::init(init_args).await.unwrap();
    home
}

#[tokio::test]
async fn plugin_store_record_error_releases_the_owned_offline_store() {
    let home = initialized_home().await;
    let error = record_plugin_retirement(home.path(), "invalid coordinate")
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("invalid pack coordinate"));
    let (reopened, _) = crate::resolve_config_access(Some(home.path()), None)
        .await
        .expect("a failed record must drain its owned store before returning");
    let records = reopened
        .execute("{ PackInstallation(limit: 1) { coordinate } }")
        .await;
    reopened
        .local_node()
        .unwrap()
        .shutdown()
        .await
        .expect("node shutdown");
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
        dependency_variables: Vec::new(),
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
) -> Result<serde_json::Value, PackChangeError> {
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
        let (access, _, local_endpoint) =
            crate::resolve_pack_config_access(Some(home.path()), None)
                .await
                .unwrap();
        let query = "{ Tools(limit: 1) { _docID } }";
        let before = access.execute(query).await.unwrap();
        assert!(before["data"]["Tools"].is_array(), "{before}");
        let retained_node = access.local_node().unwrap().clone();
        let outcome = crate::request_helpers::capture_report(install_documents_command_with_local(
            access,
            &pack,
            &args,
            local_endpoint,
        ))
        .await;
        assert_eq!(outcome.is_err(), invalid_binding, "{outcome:?}");
        if let Err(error) = outcome {
            assert!(
                format!("{error:#}").contains("expected NAME=PROFILE_ID"),
                "{error:#}"
            );
        }
        let after = gents::store::config_client::ConfigAccess::local(retained_node.clone())
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
        reopened
            .local_node()
            .unwrap()
            .shutdown()
            .await
            .expect("node shutdown");
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

async fn edit_demo_tools(home: &std::path::Path) {
    let (access, _) = crate::resolve_config_access(Some(home), None)
        .await
        .unwrap();
    // The home's client edits the installed document as an operator
    // would, through its signed desired-state apply.
    let node_did = access
        .local_node()
        .unwrap()
        .node_identity_did()
        .unwrap()
        .to_owned();
    access
        .transact_write("test.edit_pack_document", |txn| {
            let node_did = &node_did;
            Box::pin(async move {
                use gents::documents::apply_desired_state_plan;
                use gents::store::config_client::{
                    read_desired_state_record_in_txn, DesiredStateApplyDocument,
                    DesiredStateApplyPlan,
                };
                let (_, mut tools) = read_desired_state_record_in_txn(
                    txn,
                    gents::Collection::Tools,
                    node_did,
                    "demo-tools",
                )
                .await?
                .expect("installed pack tools");
                tools["display_name"] = "edited".into();
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: gents::Collection::Tools,
                    add: tools.clone(),
                    update: tools,
                }])?;
                apply_desired_state_plan(txn, &plan).await.map(|_| ())
            })
        })
        .await
        .unwrap();
    access
        .local_node()
        .unwrap()
        .shutdown()
        .await
        .expect("node shutdown");
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
    edit_demo_tools(home.path()).await;

    let second = documents_pack_dir("1.1.0");
    let error = install_from(home.path(), second.path(), Vec::new())
        .await
        .unwrap_err();
    assert!(
        matches!(&error, PackChangeError::Edited { edited, .. } if !edited.is_empty()),
        "{error:#}"
    );
    assert!(format!("{error:#}").contains("were edited"), "{error:#}");

    assert_eq!(
        gents::pack::read_home_install(home.path(), "gents/demo_pack").unwrap(),
        Some(before)
    );
}

#[tokio::test]
async fn a_plugins_pack_asking_for_authority_is_a_typed_refusal() {
    let home = tempfile::tempdir().unwrap();
    let pack = tempfile::tempdir().unwrap();
    std::fs::create_dir(pack.path().join("plugins")).unwrap();
    std::fs::write(
        pack.path().join("plugins/echo.afb"),
        super::super::plugin::testing::build_echo_plugin(),
    )
    .unwrap();
    std::fs::write(pack.path().join("README.md"), "# widget").unwrap();
    std::fs::write(
        pack.path().join("manifest.json"),
        serde_json::to_vec(&json!({
            "manifest_version": 1, "name": "widget", "namespace": "acme", "version": "1.0.0",
            "description": "test plugin pack", "authors": ["acme"], "tags": [], "kind": "plugins",
            "assets": ["README.md", "plugins/echo.afb"],
            "plugins": [{
                "name": "echo", "description": "test", "artifact": "plugins/echo.afb",
                "language": "rust", "input_schema": {"type": "object"},
                "manifold": {
                    "fs": "None", "net": {"OutboundHttp": ["api.example.com"]}, "env": "None",
                    "crypto": false, "child_process": false,
                },
            }],
        }))
        .unwrap(),
    )
    .unwrap();
    let install = |grant_authority| {
        crate::packs::install(
            home.path().to_owned(),
            pack.path().to_str().unwrap().to_owned(),
            None,
            Vec::new(),
            EditedDocuments::Refuse,
            grant_authority,
            false,
            None,
        )
    };

    let error = install(false).await.unwrap_err();
    assert!(
        matches!(
            &error,
            PackChangeError::Grant(GrantError::Ungranted { plugin, asks })
                if plugin == "echo" && asks.contains("HTTP to api.example.com")
        ),
        "{error:#}"
    );
    assert!(gents::plugin::store::list_records(home.path())
        .unwrap()
        .is_empty());
    install(true)
        .await
        .expect("granting the authority installs the pack");
}

#[tokio::test]
async fn update_returns_the_edited_refusal_for_one_package() {
    let home = initialized_home().await;
    let first = documents_pack_dir("1.0.0");
    install_from(home.path(), first.path(), Vec::new())
        .await
        .unwrap();
    edit_demo_tools(home.path()).await;
    let newer = documents_pack_dir("1.1.0");
    let (bytes, _) = gents::pack::archive::pack_dir(newer.path()).unwrap();
    let digest = {
        use sha2::Digest;
        format!("{:x}", sha2::Sha256::digest(&bytes))
    };
    let (registry, _state) =
        super::registry::tests::serve_fake_pack("demo_pack", "1.1.0", bytes, digest).await;
    let update = |edited| {
        crate::packs::update(
            home.path().to_owned(),
            Some("gents/demo_pack".to_owned()),
            Some(registry.clone()),
            edited,
        )
    };

    let error = update(EditedDocuments::Refuse).await.unwrap_err();
    assert!(
        matches!(
            &error,
            PackChangeError::Edited { coordinate, edited }
                if coordinate == "gents/demo_pack" && !edited.is_empty()
        ),
        "{error:#}"
    );

    let report = update(EditedDocuments::Keep).await.unwrap();
    assert_eq!(report["updated"], json!(["gents/demo_pack"]), "{report}");
    let (access, _) = crate::resolve_config_access(Some(home.path()), None)
        .await
        .unwrap();
    let tools = access
        .execute(r#"{ Tools(filter: {tools_id: {_eq: "demo-tools"}}) { display_name } }"#)
        .await;
    access
        .local_node()
        .unwrap()
        .shutdown()
        .await
        .expect("node shutdown");
    assert_eq!(
        tools.unwrap()["data"]["Tools"][0]["display_name"],
        "edited",
        "keep leaves the edited document as it was"
    );
}

#[tokio::test]
async fn remove_returns_the_edited_refusal() {
    let home = initialized_home().await;
    let pack = documents_pack_dir("1.0.0");
    install_from(home.path(), pack.path(), Vec::new())
        .await
        .unwrap();
    edit_demo_tools(home.path()).await;

    let error = crate::packs::remove(
        home.path().to_owned(),
        "gents/demo_pack".to_owned(),
        EditedDocuments::Refuse,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&error, PackChangeError::Edited { edited, .. } if !edited.is_empty()),
        "{error:#}"
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
    for needle in ["sha256:expected", "sha256:actual", "gents config pack show"] {
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

/// A documents pack `fixture/NAME` shipping one agent whose Tools select
/// read-only files, with `host` as its Tools author it.
fn host_tools_pack_dir(name: &str, host: serde_json::Value) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("manifest.json"),
        serde_json::to_vec(&json!({
            "manifest_version": 1, "name": name, "namespace": "fixture", "version": "1.0.0",
            "description": "An agent with host tools", "authors": ["tests"], "tags": [],
            "kind": "documents", "assets": ["README.md", "pack_config.json"],
            "dependencies": [], "config": "pack_config.json",
            "inference_slots": [{"name": "worker", "description": "The worker.",
                "agents": [format!("{name}-worker")]}],
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(dir.path().join("README.md"), "# fixture").unwrap();
    std::fs::write(
        dir.path().join("pack_config.json"),
        serde_json::to_vec(&json!({
            "node": {},
            "agents": [{"agent_slot": format!("{name}-worker"),
                "context_id": format!("{name}-context"),
                "display_name": format!("{name} worker"), "description": "Does one task.",
                "inference_profile_id": "gents:inference-slot:worker"}],
            "contexts": [{"context_id": format!("{name}-context"), "system_prompt": "Work.",
                "tools_id": format!("{name}-tools")}],
            "tools": [{"tools_id": format!("{name}-tools"), "display_name": "Read-only files",
                "host": host}],
        }))
        .unwrap(),
    )
    .unwrap();
    dir
}

/// The reader grant a User install makes is the installer's consent to its
/// own root: a pack that authors a root is refused, a root an install
/// variable set is never granted, and the installer's root is granted only
/// with `--grant-authority`, after a preview that lists it.
#[tokio::test]
async fn an_install_grants_reader_only_on_its_own_root_and_only_with_the_flag() {
    use clap::Parser;
    let home = tempfile::tempdir().unwrap();
    let cli = crate::cli::Cli::try_parse_from([
        "gents",
        "init",
        "--store-key-custody",
        "file",
        "--engineer",
        "--no-default-packs",
        "--model-name",
        "test-model",
        "--inference-url",
        "http://127.0.0.1:9/v1",
        "--home",
        home.path().to_str().unwrap(),
    ])
    .unwrap();
    let Some(crate::cli::Command::Init(init_args)) = cli.command else {
        panic!("expected init")
    };
    let initialized = crate::commands::init::init_home(init_args).await.unwrap();
    let engineer_root = initialized["tool_root"].as_str().unwrap().to_owned();

    // Another folder the node publishes, which no pack was given.
    let other = tempfile::tempdir().unwrap();
    let other_root = std::fs::canonicalize(other.path()).unwrap();
    let other_root = other_root.to_str().unwrap();
    let (access, owner) = super::resolve_scope_owner(&ScopeArgs {
        home: Some(home.path().to_owned()),
        graphql: None,
        node_did: None,
    })
    .await
    .unwrap();
    gents::tools::surface::set_workspace_root(&access, &owner, other_root, None, true)
        .await
        .unwrap();
    access.local_node().unwrap().shutdown().await.unwrap();
    drop(access);

    let install_with = |dir: &std::path::Path, grant_authority: bool, preview: bool| {
        let mut args = document_install_args(home.path(), dir, Vec::new());
        args.grant_authority = grant_authority;
        args.preview = preview;
        crate::request_helpers::capture_report(install(args))
    };
    let files = json!({"files": {"mode": "ReadOnly"}});

    let mut literal = files.clone();
    literal["root"] = other_root.into();
    let literal = host_tools_pack_dir("literal", literal);
    let refused = install_with(literal.path(), true, false).await.unwrap_err();
    let refused = format!("{:#}", refused.into_anyhow());
    assert!(
        refused.contains("ships a host tool root")
            && refused.contains("Tools literal-tools")
            && refused.contains("Nothing was installed"),
        "{refused}"
    );
    assert_eq!(
        gents::pack::read_home_install(home.path(), "fixture/literal").unwrap(),
        None
    );

    let mut variable = files.clone();
    variable["root"] = format!("${{GENTS_FIXTURE_UNSET_ROOT:-{other_root}}}").into();
    let variable = host_tools_pack_dir("variable", variable);
    let report = install_with(variable.path(), true, false).await.unwrap();
    let roots = report["host_tool_roots"].as_array().unwrap();
    assert_eq!(roots.len(), 1, "{report}");
    assert_eq!(roots[0]["root"], other_root, "{report}");
    assert_eq!(roots[0].get("reader_on"), None, "{report}");
    assert!(
        roots[0]["not_granted"]
            .as_str()
            .unwrap()
            .contains("not the one this install supplied")
            && roots[0]["next"]
                .as_str()
                .unwrap()
                .starts_with("gents config workspace-root grant-reader "),
        "{report}"
    );

    let rootless = host_tools_pack_dir("rootless", files);
    let preview = install_with(rootless.path(), false, true).await.unwrap();
    assert_eq!(
        preview["host_tool_roots"]["tools"],
        json!(["rootless-tools"]),
        "{preview}"
    );
    assert_eq!(
        preview["host_tool_roots"]["root"], engineer_root,
        "{preview}"
    );
    assert_eq!(
        preview["host_tool_roots"]["granted_with"], "--grant-authority",
        "{preview}"
    );
    let published = preview["host_tool_roots"]["reader_on"].clone();
    assert!(published.is_string(), "{preview}");

    let unflagged = install_with(rootless.path(), false, false)
        .await
        .unwrap_err();
    let unflagged = format!("{:#}", unflagged.into_anyhow());
    assert!(
        unflagged.contains("rootless-tools")
            && unflagged.contains("Nothing was installed")
            && unflagged.contains("--grant-authority"),
        "{unflagged}"
    );
    assert_eq!(
        gents::pack::read_home_install(home.path(), "fixture/rootless").unwrap(),
        None
    );

    let report = install_with(rootless.path(), true, false).await.unwrap();
    let roots = report["host_tool_roots"].as_array().unwrap();
    assert_eq!(roots.len(), 1, "{report}");
    assert_eq!(roots[0]["root"], engineer_root, "{report}");
    assert_eq!(roots[0]["reader_on"], published, "{report}");

    // A reinstall keeps what was granted and grants nothing new unflagged.
    let again = install_with(rootless.path(), false, false).await.unwrap();
    assert_eq!(
        again["host_tool_roots"][0].get("reader_on"),
        None,
        "{again}"
    );
    assert!(again["host_tool_roots"][0]["next"].is_string(), "{again}");
}
