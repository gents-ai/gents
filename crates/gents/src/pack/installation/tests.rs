use std::sync::Arc;

use super::*;

const OWNER: &str = "did:key:zPackInstallOwner";

async fn access() -> ConfigAccess {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
    ConfigAccess::local(node)
}

fn config(tools: &[(&str, &str)]) -> PackConfig {
    serde_json::from_value(json!({
        "node": {"node_did": OWNER},
        "tools": tools
            .iter()
            .map(|(id, name)| json!({"tools_id": id, "display_name": name, "node_did": OWNER, "tags": ["gents:pack:demo"]}))
            .collect::<Vec<_>>(),
    }))
    .unwrap()
}

fn identity(version: &str) -> PackIdentity {
    PackIdentity {
        coordinate: "acme/demo".into(),
        version: version.into(),
        digest: format!(
            "sha256:{}",
            version.repeat(64).chars().take(64).collect::<String>()
        ),
        plugins: vec![InstalledPackPlugin {
            name: "echo".into(),
            digest: format!("sha256:{}", "e".repeat(64)),
        }],
        dependencies: Vec::new(),
    }
}

async fn install(
    access: &ConfigAccess,
    version: &str,
    config: &PackConfig,
    policy: DriftPolicy,
) -> Result<InstallReport> {
    super::super::install_pack_documents(access, OWNER, &identity(version), config, policy).await
}

async fn edit_tools(access: &ConfigAccess, id: &str, name: &str) {
    let edit = crate::config_client::DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Tools,
        add: json!({"tools_id": id, "display_name": name, "node_did": OWNER}),
        update: json!({"tools_id": id, "display_name": name, "node_did": OWNER}),
    }])
    .unwrap();
    access
        .transact_write("test.edit", |txn| {
            let edit = &edit;
            Box::pin(async move { crate::config_client::apply_desired_state_plan(txn, edit).await })
        })
        .await
        .unwrap();
}

async fn tools_ids(access: &ConfigAccess) -> Vec<String> {
    let response = access.execute("{ Tools { tools_id } }").await.unwrap();
    let mut ids: Vec<String> = response["data"]["Tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["tools_id"].as_str().unwrap().to_owned())
        .collect();
    ids.sort();
    ids
}

#[tokio::test]
async fn install_then_remove_leaves_no_pack_documents_and_keeps_adopted_ones() {
    let access = access().await;
    edit_tools(&access, "shared", "Shared before the pack").await;

    let report = install(
        &access,
        "1",
        &config(&[("alpha", "Alpha"), ("shared", "Shared")]),
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();
    assert_eq!(report.created, vec!["Tools/alpha"]);
    assert_eq!(report.adopted, vec!["Tools/shared"]);

    assert_eq!(
        list_installed_packs(&access, OWNER).await.unwrap(),
        vec![InstalledPack {
            coordinate: "acme/demo".into(),
            version: "1".into(),
            digest: identity("1").digest,
        }]
    );
    assert_eq!(
        read_installed_pack(&access, OWNER, "acme/demo")
            .await
            .unwrap(),
        Some(InstalledPack {
            coordinate: "acme/demo".into(),
            version: "1".into(),
            digest: identity("1").digest,
        })
    );
    assert_eq!(
        read_installed_pack(&access, OWNER, "acme/missing")
            .await
            .unwrap(),
        None,
        "an uninstalled coordinate resolves to nothing, not an error"
    );
    let removed = remove_pack(&access, OWNER, "acme/demo", DriftPolicy::Refuse)
        .await
        .unwrap();
    assert_eq!(removed.documents.removed, vec!["Tools/alpha"]);
    assert_eq!(
        removed.documents.plugins,
        identity("1").plugins,
        "removal releases what install stored"
    );
    assert_eq!(
        tools_ids(&access).await,
        vec!["shared"],
        "an adopted document stays"
    );
    let record = access
        .execute("{ PackInstallation { _docID } }")
        .await
        .unwrap();
    assert_eq!(record["data"]["PackInstallation"], json!([]));
    assert_eq!(
        read_installed_pack(&access, OWNER, "acme/demo")
            .await
            .unwrap(),
        None,
        "removal leaves no installed record behind"
    );
    assert!(
        remove_pack(&access, OWNER, "acme/demo", DriftPolicy::Refuse)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn an_edited_document_stops_an_upgrade_by_name() {
    let access = access().await;
    let v1 = config(&[("alpha", "Alpha"), ("beta", "Beta")]);
    install(&access, "1", &v1, DriftPolicy::Refuse)
        .await
        .unwrap();
    let again = install(&access, "1", &v1, DriftPolicy::Refuse)
        .await
        .unwrap();
    assert_eq!(again.replaced, vec!["Tools/alpha", "Tools/beta"]);

    edit_tools(&access, "alpha", "Alpha, edited by the operator").await;
    let v2 = config(&[("alpha", "Alpha 2"), ("beta", "Beta 2")]);
    let error = install(&access, "2", &v2, DriftPolicy::Refuse)
        .await
        .unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("Tools/alpha"), "{message}");
    assert!(!message.contains("Tools/beta"), "{message}");

    let kept = install(&access, "2", &v2, DriftPolicy::Keep).await.unwrap();
    assert_eq!(kept.kept, vec!["Tools/alpha"]);
    assert_eq!(kept.replaced, vec!["Tools/beta"]);
    let alpha = access
        .execute(r#"{ Tools(filter: {tools_id: {_eq: "alpha"}}) { display_name } }"#)
        .await
        .unwrap();
    assert_eq!(
        alpha["data"]["Tools"][0]["display_name"],
        "Alpha, edited by the operator"
    );

    let overwritten = install(&access, "2", &v2, DriftPolicy::Overwrite)
        .await
        .unwrap();
    assert_eq!(overwritten.replaced, vec!["Tools/alpha", "Tools/beta"]);
}

#[tokio::test]
async fn a_document_the_new_version_drops_is_removed_and_history_is_kept() {
    let access = access().await;
    install(
        &access,
        "1",
        &config(&[("alpha", "Alpha"), ("beta", "Beta")]),
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();
    let report = install(
        &access,
        "2",
        &config(&[("alpha", "Alpha")]),
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();
    assert_eq!(report.removed, vec!["Tools/beta"]);
    assert_eq!(tools_ids(&access).await, vec!["alpha"]);
    let record = access
        .execute("{ PackInstallation { version digest history } }")
        .await
        .unwrap();
    let row = &record["data"]["PackInstallation"][0];
    assert_eq!(row["version"], "2");
    assert_eq!(row["history"], json!([identity("1").digest]));
}

#[tokio::test]
async fn a_plugin_store_record_is_not_a_pack_install() {
    let access = access().await;
    let home = tempfile::tempdir().unwrap();
    record_plugin_store_change(
        &access,
        OWNER,
        home.path(),
        "acme/demo",
        Some(&identity("1")),
    )
    .await
    .unwrap();
    installation_doc_id(&access).await;
    assert!(list_installed_packs(&access, OWNER)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        read_installed_pack(&access, OWNER, "acme/demo")
            .await
            .unwrap(),
        None
    );
    let refused = remove_pack(&access, OWNER, "acme/demo", DriftPolicy::Refuse)
        .await
        .unwrap_err();
    assert!(
        format!("{refused:#}").contains("gents plugin remove"),
        "{refused:#}"
    );

    record_plugin_store_change(&access, OWNER, home.path(), "acme/demo", None)
        .await
        .unwrap();
    let rows = access
        .execute("{ PackInstallation { _docID } }")
        .await
        .unwrap();
    assert_eq!(rows["data"]["PackInstallation"], json!([]));
}

#[tokio::test]
async fn a_plugin_store_change_keeps_a_documents_install_record() {
    let access = access().await;
    let home = tempfile::tempdir().unwrap();
    install(
        &access,
        "1",
        &config(&[("alpha", "Alpha")]),
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();
    let extra = InstalledPackPlugin {
        name: "extra".into(),
        digest: format!("sha256:{}", "a".repeat(64)),
    };
    let plugin_only = PackIdentity {
        plugins: vec![extra.clone()],
        ..identity("2")
    };
    record_plugin_store_change(&access, OWNER, home.path(), "acme/demo", Some(&plugin_only))
        .await
        .unwrap();
    record_plugin_store_change(&access, OWNER, home.path(), "acme/demo", None)
        .await
        .unwrap();

    assert_eq!(
        read_installed_pack(&access, OWNER, "acme/demo")
            .await
            .unwrap(),
        Some(InstalledPack {
            coordinate: "acme/demo".into(),
            version: "1".into(),
            digest: identity("1").digest,
        })
    );
    let removed = remove_pack(&access, OWNER, "acme/demo", DriftPolicy::Refuse)
        .await
        .unwrap();
    assert_eq!(removed.documents.removed, vec!["Tools/alpha"]);
    assert_eq!(
        removed.documents.plugins,
        vec![identity("1").plugins[0].clone(), extra]
    );
}

#[tokio::test]
async fn a_documentless_pack_install_is_not_a_plugin_store_record() {
    let access = access().await;
    let home = tempfile::tempdir().unwrap();
    install(&access, "1", &config(&[]), DriftPolicy::Refuse)
        .await
        .unwrap();
    record_plugin_store_change(&access, OWNER, home.path(), "acme/demo", None)
        .await
        .unwrap();
    assert_eq!(list_installed_packs(&access, OWNER).await.unwrap().len(), 1);
    remove_pack(&access, OWNER, "acme/demo", DriftPolicy::Refuse)
        .await
        .unwrap();
}

async fn installation_doc_id(access: &ConfigAccess) -> String {
    access
        .execute("{ PackInstallation { _docID } }")
        .await
        .unwrap()["data"]["PackInstallation"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn corrupt_installation_field(
    access: &ConfigAccess,
    doc_id: &str,
    field: &str,
    value: Value,
) {
    let mut input = serde_json::Map::new();
    input.insert(field.to_owned(), value);
    let variables = json!({ "input": Value::Object(input) });
    let variables = &variables;
    let response = access
        .transact_write("test.corrupt_installation_field", |txn| {
            Box::pin(async move {
                txn.execute_with_variables(
                    &format!(
                        r#"mutation($input: {RECORD}MutationInputArg!) {{ update_{RECORD}(docID: "{}", input: $input) {{ _docID }} }}"#,
                        doc_id
                    ),
                    variables,
                )
                .await
            })
        })
        .await
        .unwrap();
    assert!(
        response.get("errors").is_none_or(Value::is_null),
        "{response}"
    );
}

#[tokio::test]
async fn a_malformed_plugins_field_fails_loudly_instead_of_reading_as_empty() {
    let access = access().await;
    install(
        &access,
        "1",
        &config(&[("alpha", "Alpha")]),
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();
    let doc_id = installation_doc_id(&access).await;
    corrupt_installation_field(&access, &doc_id, "plugins", json!({"not": "an array"})).await;

    let error = install(
        &access,
        "2",
        &config(&[("alpha", "Alpha 2")]),
        DriftPolicy::Refuse,
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("malformed plugins field"),
        "{error:#}"
    );
}

#[tokio::test]
async fn list_installed_packs_fails_loudly_on_a_malformed_record() {
    let access = access().await;
    install(
        &access,
        "1",
        &config(&[("alpha", "Alpha")]),
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();
    let doc_id = installation_doc_id(&access).await;
    corrupt_installation_field(&access, &doc_id, "version", Value::Null).await;

    let error = list_installed_packs(&access, OWNER).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("malformed version field"),
        "{error:#}"
    );
}

#[tokio::test]
async fn read_installed_pack_fails_loudly_on_a_malformed_record() {
    let access = access().await;
    install(
        &access,
        "1",
        &config(&[("alpha", "Alpha")]),
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();
    let doc_id = installation_doc_id(&access).await;
    corrupt_installation_field(&access, &doc_id, "digest", Value::Null).await;

    let error = read_installed_pack(&access, OWNER, "acme/demo")
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("malformed digest field"),
        "{error:#}"
    );
}

#[tokio::test]
async fn read_installed_pack_fails_loudly_on_more_than_one_record() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    let schema = gents_protocol::schemas::PACK_INSTALLATION.replace(
        r#"@index(fields: ["node_did", "coordinate"], unique: true)"#,
        "",
    );
    node.add_schema(&schema).await.unwrap();
    let access = ConfigAccess::local(node);

    for digest_seed in ["1", "2"] {
        let input = json!({
            "node_did": OWNER,
            "coordinate": "acme/duplicated",
            "version": "1",
            "digest": format!("sha256:{}", digest_seed.repeat(64)),
        });
        access
            .write(
                "test.duplicate_installation",
                &format!(
                    "mutation {{ create_{RECORD}(input: {}) {{ _docID }} }}",
                    gents_protocol::graphql::graphql_input_literal(&input).unwrap()
                ),
            )
            .await
            .unwrap();
    }

    let error = read_installed_pack(&access, OWNER, "acme/duplicated")
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("has more than one installation record"),
        "{error:#}"
    );
}

async fn dependency_fixture() -> (
    Arc<defra_node::EmbeddedNode>,
    ConfigAccess,
    crate::graph_package::GraphPackageInstallBindings,
) {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    crate::document_config::ensure_node(&node, OWNER)
        .await
        .unwrap();
    for profile in ["claude", "glm", "grok"] {
        crate::test_support::install_test_agent(&node, OWNER, profile).await;
    }
    let options = crate::graph_package::GraphPackageInstallBindings {
        node_did: OWNER.into(),
        inference_slots: std::collections::BTreeMap::from([
            ("coordinator".into(), "claude:inference".into()),
            ("worker".into(), "glm:inference".into()),
            ("verifier".into(), "grok:inference".into()),
        ]),
    };
    let access = ConfigAccess::local(node.clone());
    (node, access, options)
}

fn demo_identity(dependencies: Vec<String>) -> PackIdentity {
    PackIdentity {
        coordinate: "acme/demo".into(),
        version: "1".into(),
        digest: format!("sha256:{}", "d".repeat(64)),
        plugins: vec![],
        dependencies,
    }
}

async fn activate(access: &ConfigAccess, graph_id: &str, digest: &str) {
    crate::graph_pipeline::activate_graph_revision_with_access(
        access, OWNER, graph_id, digest, None,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn a_dependency_is_released_with_its_last_non_explicit_dependent() {
    let (_node, access, options) = dependency_fixture().await;
    let receipt = crate::test_support::install_test_graph_package_explicit(
        &access,
        OWNER,
        "review_graph",
        &options,
        false,
    )
    .await
    .unwrap();
    activate(&access, &receipt.graph_id, &receipt.revision_digest).await;

    super::super::install_pack_documents(
        &access,
        OWNER,
        &demo_identity(vec!["fixture/review_graph".into()]),
        &config(&[("alpha", "Alpha")]),
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();

    let error = remove_pack(&access, OWNER, "fixture/review_graph", DriftPolicy::Refuse)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("acme/demo"), "{error:#}");

    let report = remove_pack(&access, OWNER, "acme/demo", DriftPolicy::Refuse)
        .await
        .unwrap();
    assert_eq!(report.dependencies.len(), 1);
    assert_eq!(report.dependencies[0].pack, "fixture/review_graph");
    assert!(report.dependencies[0]
        .documents
        .removed
        .iter()
        .any(|name| name.starts_with("GraphDefinition/")));

    let remaining = access
        .execute("{ PackInstallation { coordinate } }")
        .await
        .unwrap();
    assert_eq!(remaining["data"]["PackInstallation"], json!([]));
}

#[tokio::test]
async fn an_explicit_dependency_survives_its_dependent() {
    let (_node, access, options) = dependency_fixture().await;
    let receipt =
        crate::test_support::install_test_graph_package(&access, OWNER, "review_graph", &options)
            .await
            .unwrap();
    activate(&access, &receipt.graph_id, &receipt.revision_digest).await;

    super::super::install_pack_documents(
        &access,
        OWNER,
        &demo_identity(vec!["fixture/review_graph".into()]),
        &config(&[("alpha", "Alpha")]),
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();

    let report = remove_pack(&access, OWNER, "acme/demo", DriftPolicy::Refuse)
        .await
        .unwrap();
    assert!(
        report.dependencies.is_empty(),
        "an explicit install is never auto-released"
    );

    let remaining = access
        .execute("{ PackInstallation { coordinate required_by } }")
        .await
        .unwrap();
    let rows = remaining["data"]["PackInstallation"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["coordinate"], "fixture/review_graph");
    assert_eq!(
        rows[0]["required_by"],
        Value::Null,
        "an explicit install is never auto-released"
    );

    remove_pack(&access, OWNER, "fixture/review_graph", DriftPolicy::Refuse)
        .await
        .unwrap();
}

#[tokio::test]
async fn an_upgrade_that_drops_a_dependency_releases_its_claim() {
    let (_node, access, options) = dependency_fixture().await;
    let receipt = crate::test_support::install_test_graph_package_explicit(
        &access,
        OWNER,
        "review_graph",
        &options,
        false,
    )
    .await
    .unwrap();
    activate(&access, &receipt.graph_id, &receipt.revision_digest).await;

    super::super::install_pack_documents(
        &access,
        OWNER,
        &demo_identity(vec!["fixture/review_graph".into()]),
        &config(&[("alpha", "Alpha")]),
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();
    let after_first = access
        .execute("{ PackInstallation { coordinate required_by } }")
        .await
        .unwrap();
    let rows = after_first["data"]["PackInstallation"].as_array().unwrap();
    let review_graph = rows
        .iter()
        .find(|row| row["coordinate"] == "fixture/review_graph")
        .unwrap();
    assert_eq!(review_graph["required_by"], json!(["acme/demo"]));

    super::super::install_pack_documents(
        &access,
        OWNER,
        &demo_identity(vec![]),
        &config(&[("alpha", "Alpha 2")]),
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();

    let after_upgrade = access
        .execute("{ PackInstallation { coordinate required_by } }")
        .await
        .unwrap();
    let rows = after_upgrade["data"]["PackInstallation"]
        .as_array()
        .unwrap();
    assert!(
        !rows
            .iter()
            .any(|row| row["coordinate"] == "fixture/review_graph"),
        "the dropped, non-explicit dependency is released outright: {rows:?}"
    );
}

#[tokio::test]
async fn a_legacy_record_without_dependency_fields_reads_as_explicit() {
    let (_node, access, options) = dependency_fixture().await;
    let receipt = crate::test_support::install_test_graph_package_explicit(
        &access,
        OWNER,
        "review_graph",
        &options,
        false,
    )
    .await
    .unwrap();
    activate(&access, &receipt.graph_id, &receipt.revision_digest).await;

    let doc_id = installation_doc_id(&access).await;
    corrupt_installation_field(&access, &doc_id, "explicit", Value::Null).await;

    super::super::install_pack_documents(
        &access,
        OWNER,
        &demo_identity(vec!["fixture/review_graph".into()]),
        &config(&[("alpha", "Alpha")]),
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();
    remove_pack(&access, OWNER, "acme/demo", DriftPolicy::Refuse)
        .await
        .unwrap();

    let remaining = access
        .execute("{ PackInstallation { coordinate } }")
        .await
        .unwrap();
    let rows = remaining["data"]["PackInstallation"].as_array().unwrap();
    assert!(
        rows.iter()
            .any(|row| row["coordinate"] == "fixture/review_graph"),
        "a legacy record with no explicit field must never be auto-released: {rows:?}"
    );
}

#[tokio::test]
async fn installing_a_pack_keeps_the_users_default_behavior() {
    let access = access().await;
    let home = |collection, value: Value| DesiredStateApplyDocument {
        collection,
        add: value.clone(),
        update: value,
    };
    let seed = crate::config_client::DesiredStateApplyPlan::new(vec![
        home(
            Collection::InferenceBackend,
            json!({"node_did":OWNER,"backend_id":"backend","name":"Local","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:8000/v1","auth":{"kind":"unauthenticated"}}),
        ),
        home(
            Collection::InferenceProfile,
            json!({"node_did":OWNER,"profile_id":"profile","backend_id":"backend","model_name":"model"}),
        ),
        home(
            Collection::Agent,
            json!({"node_did":OWNER,"agent_id":"chosen","inference_profile_id":"profile"}),
        ),
        home(
            Collection::Node,
            json!({"node_did":OWNER,"default_agent_id":"chosen"}),
        ),
    ])
    .unwrap();
    access
        .transact_write("test.seed", |txn| {
            let seed = &seed;
            Box::pin(async move { crate::config_client::apply_desired_state_plan(txn, seed).await })
        })
        .await
        .unwrap();

    let manifest: crate::pack::PackManifest = serde_json::from_value(json!({
        "manifest_version":1,"name":"demo","version":"1","description":"Demo",
        "kind":"documents","authors":["Demo"],
        "assets":["README.md","pack_config.json"],"config":"pack_config.json",
    }))
    .unwrap();
    let authored = json!({
        "node": {},
        "tools": [{"tools_id":"pack-tools","display_name":"Pack tools"}],
    });
    let pack = crate::pack::load_pack_config(
        &manifest,
        &crate::pack::PackInstallOptions {
            node_did: OWNER.into(),
        },
        &|_| Ok(serde_json::to_vec(&authored)?),
        &|_| None,
    )
    .unwrap();
    install(&access, "1", &pack, DriftPolicy::Refuse)
        .await
        .unwrap();

    let principal = access
        .execute("{ Node { default_agent_id } }")
        .await
        .unwrap();
    assert_eq!(
        principal["data"]["Node"],
        json!([{"default_agent_id": "chosen"}])
    );
    assert_eq!(tools_ids(&access).await, ["pack-tools"]);
}

#[tokio::test]
async fn a_delayed_plugin_notification_projects_the_current_home_pin() {
    let access = access().await;
    let home = tempfile::tempdir().unwrap();
    let current = identity("2");
    super::super::write_home_install(
        home.path(),
        &super::super::HomePackInstall {
            coordinate: current.coordinate.clone(),
            version: current.version.clone(),
            digest: current.digest.clone(),
            kind: super::super::PackKind::Plugins,
            assets: "packs/.materialized/acme/demo/current".into(),
            dependency_assets: Vec::new(),
            plugins: current.plugins.clone(),
            installed_at: "2026-01-01T00:00:00Z".into(),
            installed_via: None,
        },
    )
    .unwrap();
    record_plugin_store_change(
        &access,
        OWNER,
        home.path(),
        "acme/demo",
        Some(&identity("1")),
    )
    .await
    .unwrap();
    let response = access
        .execute("{ PackInstallation { version digest } }")
        .await
        .unwrap();
    assert_eq!(
        response["data"]["PackInstallation"][0]["version"],
        current.version
    );
    assert_eq!(
        response["data"]["PackInstallation"][0]["digest"],
        current.digest
    );
    super::super::forget_home_install(home.path(), "acme/demo").unwrap();
    record_plugin_store_change(&access, OWNER, home.path(), "acme/demo", None)
        .await
        .unwrap();
    let response = access
        .execute("{ PackInstallation { _docID } }")
        .await
        .unwrap();
    assert_eq!(response["data"]["PackInstallation"], json!([]));
}

const NOTE_SDL: &str = "type FixtureNote @index(fields: [\"text\", \"job_row_doc_id\"], unique: true) {\n  text: String\n  job_row_doc_id: String\n  chain_root_doc_id: String\n}\ntype Other { key: String @index(unique: true) }\n";

/// Installs the job fixture pack's plugins into `home`, with its SDL and
/// plugin declaration rewritten by the caller.
fn install_fixture_plugins_into(
    home: &Path,
    sdl: &str,
    edit: impl FnOnce(&mut serde_json::Value),
) -> Result<Vec<crate::plugin::store::InstalledPlugin>> {
    let (_guard, root) = crate::test_support::fixture_pack_copy("job_plugin_fixture", &json!({}));
    std::fs::write(root.join("schemas/fixture_note.graphql"), sdl).unwrap();
    let path = root.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    edit(&mut manifest["plugins"][0]);
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let (bytes, _) = crate::pack_archive::pack_dir(&root).unwrap();
    let archive = crate::pack_archive::PackArchive::from_bytes(&bytes).unwrap();
    crate::plugin::install::install_pack_plugins(
        home,
        archive.manifest(),
        archive.digest(),
        |path| archive.asset(path),
        false,
    )
}

/// Installs the job fixture pack's plugins into a fresh plugin home.
fn install_fixture_plugins(
    sdl: &str,
    edit: impl FnOnce(&mut serde_json::Value),
) -> (
    tempfile::TempDir,
    Result<Vec<crate::plugin::store::InstalledPlugin>>,
) {
    let home = tempfile::tempdir().unwrap();
    let installed = install_fixture_plugins_into(home.path(), sdl, edit);
    (home, installed)
}

fn refusal(result: Result<Vec<crate::plugin::store::InstalledPlugin>>) -> String {
    format!("{:#}", result.expect_err("the install must be refused"))
}

#[test]
fn data_collection_missing_from_own_sdl_refuses_the_install() {
    let (_home, installed) = install_fixture_plugins("type Other { key: String }", |_| {});
    let message = refusal(installed);
    assert!(message.contains("FixtureNote"), "{message}");
    assert!(message.contains("schemas"), "{message}");
}

#[test]
fn install_records_data_unique_keys() {
    let (home, installed) = install_fixture_plugins(NOTE_SDL, |plugin| {
        plugin["data"]["collections"] = json!(["FixtureNote", "Other"]);
    });
    installed.unwrap();
    let record = crate::plugin::store::read_record(home.path(), "fixture", "job_fixture").unwrap();
    assert_eq!(
        record.data_unique_keys,
        std::collections::BTreeMap::from([
            (
                "FixtureNote".to_owned(),
                vec![vec!["text".to_owned(), "job_row_doc_id".to_owned()]]
            ),
            ("Other".to_owned(), vec![vec!["key".to_owned()]]),
        ])
    );
}

#[test]
fn install_records_data_collection_fields() {
    let (home, installed) = install_fixture_plugins(NOTE_SDL, |plugin| {
        plugin["data"]["collections"] = json!(["FixtureNote", "Other"]);
    });
    installed.unwrap();
    let record = crate::plugin::store::read_record(home.path(), "fixture", "job_fixture").unwrap();
    assert_eq!(
        record.data_collection_fields.keys().collect::<Vec<_>>(),
        ["FixtureNote", "Other"]
    );
    let fields = &record.data_collection_fields;
    for field in ["text", "job_row_doc_id", "chain_root_doc_id"] {
        assert!(fields["FixtureNote"].iter().any(|f| f == field), "{field}");
    }
    assert!(fields["Other"].iter().any(|f| f == "key"));
    assert_eq!(
        record.data_unique_keys,
        std::collections::BTreeMap::from([
            (
                "FixtureNote".to_owned(),
                vec![vec!["text".to_owned(), "job_row_doc_id".to_owned()]]
            ),
            ("Other".to_owned(), vec![vec!["key".to_owned()]]),
        ])
    );
}

#[test]
fn reinstall_with_changed_sdl_rewrites_both_maps() {
    let home = tempfile::tempdir().unwrap();
    let both = |plugin: &mut serde_json::Value| {
        plugin["data"]["collections"] = json!(["FixtureNote", "Other"]);
    };
    install_fixture_plugins_into(home.path(), NOTE_SDL, both).unwrap();
    let changed = NOTE_SDL
        .replace(
            "chain_root_doc_id: String",
            "chain_root_doc_id: String\n  node_did: String",
        )
        .replace("[\"text\", \"job_row_doc_id\"]", "[\"text\"]");
    install_fixture_plugins_into(home.path(), &changed, both).unwrap();
    let record = crate::plugin::store::read_record(home.path(), "fixture", "job_fixture").unwrap();
    assert!(record.data_collection_fields["FixtureNote"]
        .iter()
        .any(|field| field == "node_did"));
    assert_eq!(
        record.data_unique_keys["FixtureNote"],
        vec![vec!["text".to_owned()]]
    );
}

#[test]
fn stamped_key_outside_declared_collections_is_refused() {
    let (_home, installed) = install_fixture_plugins(NOTE_SDL, |plugin| {
        plugin["data"]["stamped"] = json!({"missing_field": "node_did"});
    });
    let message = refusal(installed);
    assert!(message.contains("missing_field"), "{message}");
    assert!(message.contains("data.stamped"), "{message}");
}

#[test]
fn asset_mount_class_without_shipped_asset_is_refused() {
    let (_home, installed) = install_fixture_plugins(NOTE_SDL, |plugin| {
        plugin["asset_mounts"] = json!([{"class": "unshipped_assets"}]);
    });
    let message = refusal(installed);
    assert!(message.contains("unshipped_assets"), "{message}");
    assert!(message.contains("asset_mounts"), "{message}");
}

#[test]
fn installed_plugin_record_without_data_unique_keys_decodes() {
    let (home, installed) = install_fixture_plugins(NOTE_SDL, |_| {});
    installed.unwrap();
    let mut record = serde_json::to_value(
        crate::plugin::store::read_record(home.path(), "fixture", "job_fixture").unwrap(),
    )
    .unwrap();
    record.as_object_mut().unwrap().remove("data_unique_keys");
    let record: crate::plugin::store::InstalledPlugin = serde_json::from_value(record).unwrap();
    assert!(record.data_unique_keys.is_empty());
}

fn documents_archive() -> crate::pack_archive::PackArchive {
    let (_guard, root) = crate::test_support::fixture_pack_copy("documents_fixture", &json!({}));
    let path = root.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest.as_object_mut().unwrap().remove("schemas");
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let (bytes, _) = crate::pack_archive::pack_dir(&root).unwrap();
    crate::pack_archive::PackArchive::from_bytes(&bytes).unwrap()
}

fn prepared() -> PreparedDocumentPackInstall {
    PreparedDocumentPackInstall {
        config: config(&[("self-config-tools", "Self-config tools")]),
        inference: crate::pack::PackInferenceBindingPreview {
            slots: Vec::new(),
            profiles: Vec::new(),
            bindings: Default::default(),
            automatic: true,
        },
        dependency_inference: BTreeMap::new(),
        dependencies: Vec::new(),
    }
}

fn operator_record(archive: &crate::pack_archive::PackArchive) -> crate::pack::HomePackInstall {
    crate::pack::HomePackInstall {
        coordinate: format!(
            "{}/{}",
            archive.manifest().metadata.namespace,
            archive.manifest().name
        ),
        version: "0.9.0".into(),
        digest: format!("sha256:{}", "0".repeat(64)),
        kind: archive.manifest().metadata.kind.clone(),
        assets: String::new(),
        dependency_assets: Vec::new(),
        plugins: Vec::new(),
        installed_at: "2026-01-01T00:00:00Z".into(),
        installed_via: Some(crate::pack::InstalledVia::Operator),
    }
}

async fn self_config_install(archive: &crate::pack_archive::PackArchive, home: Option<&Path>) {
    install_prepared_document_pack(
        &access().await,
        OWNER,
        archive,
        &[],
        &prepared(),
        home,
        false,
        crate::pack::InstalledVia::SelfConfig,
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn non_explicit_self_config_record_never_lowers_operator_provenance() {
    let home = tempfile::tempdir().unwrap();
    let archive = documents_archive();
    let engine = operator_record(&archive);
    crate::pack::write_home_install(home.path(), &engine).unwrap();
    let identity = PackIdentity::new(archive.manifest(), archive.digest(), Vec::new());
    documents::record_home_install(
        home.path(),
        archive.manifest(),
        &identity,
        String::new(),
        Vec::new(),
        crate::pack::InstalledVia::SelfConfig,
        false,
    )
    .unwrap();
    let stored = crate::pack::read_home_install(home.path(), &engine.coordinate)
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.installed_via,
        Some(crate::pack::InstalledVia::Operator)
    );
}

#[tokio::test]
async fn explicit_self_config_reinstall_lowers_the_pack_record() {
    let home = tempfile::tempdir().unwrap();
    let archive = documents_archive();
    let prior = operator_record(&archive);
    crate::pack::write_home_install(home.path(), &prior).unwrap();
    self_config_install(&archive, Some(home.path())).await;
    let stored = crate::pack::read_home_install(home.path(), &prior.coordinate)
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.installed_via,
        Some(crate::pack::InstalledVia::SelfConfig)
    );
    assert_eq!(stored.digest, archive.digest());
}

fn documents_archive_shipping(files: &[(&str, &str)]) -> crate::pack_archive::PackArchive {
    let (_guard, root) = crate::test_support::fixture_pack_copy("documents_fixture", &json!({}));
    let path = root.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let object = manifest.as_object_mut().unwrap();
    object.remove("schemas");
    let mut assets = vec!["README.md".to_owned(), "pack_config.json".to_owned()];
    for (asset, body) in files {
        let target = root.join(asset);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, body).unwrap();
        assets.push((*asset).to_owned());
    }
    object.insert("assets".into(), json!(assets));
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let (bytes, _) = crate::pack_archive::pack_dir(&root).unwrap();
    crate::pack_archive::PackArchive::from_bytes(&bytes).unwrap()
}

fn recorded(
    home: &Path,
    archive: &crate::pack_archive::PackArchive,
) -> crate::pack::HomePackInstall {
    crate::pack::read_home_install(home, &operator_record(archive).coordinate)
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn documents_pack_shipping_workflows_is_materialized_with_no_plugin_installed() {
    let home = tempfile::tempdir().unwrap();
    let archive = documents_archive_shipping(&[("workflows/hello.rhai", "1")]);
    assert!(archive.manifest().metadata.plugins.is_empty());
    self_config_install(&archive, Some(home.path())).await;
    let root = crate::pack::materialize::cache_root(
        home.path(),
        &archive.manifest().metadata.namespace,
        &archive.manifest().name,
        archive.digest(),
    )
    .unwrap();
    let stored = recorded(home.path(), &archive);
    assert_eq!(
        Path::new(&stored.assets),
        root.strip_prefix(home.path()).unwrap()
    );
    assert!(root.join("workflows/hello.rhai").is_file());
}

#[tokio::test]
async fn dependency_install_materializes_the_same_way() {
    let (_node, access, options) = dependency_fixture().await;
    let (_guard, dir) = crate::test_support::fixture_pack_copy("review_graph", &json!({}));
    let (bytes, _) = crate::pack_archive::pack_dir(&dir).unwrap();
    let dependency = crate::pack_archive::PackArchive::from_bytes(&bytes).unwrap();
    let package = crate::graph_package::load_archive_graph_package_with_environment(
        &dependency,
        &crate::pack::PackInstallOptions {
            node_did: OWNER.into(),
        },
        &|name| (name == "GENTS_REVIEW_MODEL").then(|| "test-model".to_owned()),
    )
    .unwrap();
    let mut prepared = prepared();
    prepared.dependencies = vec![(package, options)];
    let home = tempfile::tempdir().unwrap();
    let archive = documents_archive_shipping(&[]);
    install_prepared_document_pack(
        &access,
        OWNER,
        &archive,
        &[&dependency],
        &prepared,
        Some(home.path()),
        false,
        crate::pack::InstalledVia::SelfConfig,
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();
    let root = crate::pack::materialize::cache_root(
        home.path(),
        &dependency.manifest().metadata.namespace,
        &dependency.manifest().name,
        dependency.digest(),
    )
    .unwrap();
    assert!(root.join("agents/review_recon/system_prompt.md").is_file());
    let stored = recorded(home.path(), &archive);
    assert_eq!(stored.assets, "");
    assert_eq!(
        stored.dependency_assets,
        vec![(
            "fixture/review_graph".to_owned(),
            root.strip_prefix(home.path())
                .unwrap()
                .to_string_lossy()
                .into_owned()
        )]
    );
}

#[tokio::test]
async fn pack_with_no_asset_class_is_not_materialized() {
    let home = tempfile::tempdir().unwrap();
    let archive = documents_archive_shipping(&[]);
    self_config_install(&archive, Some(home.path())).await;
    let stored = recorded(home.path(), &archive);
    assert_eq!(stored.assets, "");
    assert!(stored.dependency_assets.is_empty());
    assert!(!home.path().join("packs/.materialized").exists());
}

#[tokio::test]
async fn self_config_install_succeeds_without_a_home() {
    let archive = documents_archive();
    self_config_install(&archive, None).await;
}

fn job_tools(job: serde_json::Value) -> crate::document_config::Tools {
    serde_json::from_value(json!({
        "tools_id": "engine-tools",
        "node_did": OWNER,
        "integrations": {"plugins": [{"plugin": "acme/runner", "job": job}]},
    }))
    .unwrap()
}

fn shipping(targets: &[&str]) -> PackConfig {
    serde_json::from_value(json!({
        "node": {"node_did": OWNER},
        "agent_targets": targets
            .iter()
            .map(|id| json!({
                "target_id": id, "node_did": OWNER, "target_node_did": OWNER,
                "agent_id": id, "name": id,
            }))
            .collect::<Vec<_>>(),
    }))
    .unwrap()
}

fn engine_admission(job: serde_json::Value, via: crate::pack::InstalledVia) -> Result<()> {
    let mut metadata = documents_archive().manifest().metadata.clone();
    metadata.dependencies = vec!["acme/base".into()];
    let own = shipping(&["wf-run"]);
    let base = shipping(&["wf-read-only"]);
    crate::plugin_job::settings::pack_job_block_admissible(
        &job_tools(job),
        &metadata,
        "acme/engine",
        &[("acme/engine".into(), &own), ("acme/base".into(), &base)],
        via,
    )
}

fn operator_refusal(job: serde_json::Value) {
    let message = engine_admission(job, crate::pack::InstalledVia::Operator)
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("job block this install cannot grant"),
        "{message}"
    );
}

#[tokio::test]
async fn pack_job_block_is_refused_on_a_self_config_install() {
    let manifest: crate::pack::PackManifest = serde_json::from_value(json!({
        "manifest_version": 1, "name": "engine", "version": "1", "description": "Engine",
        "kind": "documents", "authors": ["Demo"],
        "assets": ["README.md", "pack_config.json"], "config": "pack_config.json",
    }))
    .unwrap();
    let mut config = config(&[]);
    config.tools = vec![job_tools(json!({"agent_targets": ["wf-run"]}))];
    config.agent_targets = shipping(&["wf-run"]).agent_targets;
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    crate::document_config::ensure_node(&node, OWNER)
        .await
        .unwrap();
    let access = ConfigAccess::local(node);
    let requested = Default::default();
    let prepare = |via| {
        prepare_document_pack_install(
            &access,
            OWNER,
            &manifest,
            &config,
            &requested,
            &[],
            via,
            &|_| None,
        )
    };
    let refused = prepare(crate::pack::InstalledVia::SelfConfig).await;
    let message = refused.err().unwrap().to_string();
    assert!(message.contains("operator install"), "{message}");
    prepare(crate::pack::InstalledVia::Operator).await.unwrap();
}

#[test]
fn pack_job_block_targets_outside_pack_and_dependencies_are_refused() {
    operator_refusal(json!({"agent_targets": ["wf-run", "elsewhere"]}));
}

#[test]
fn pack_job_block_trusted_packs_outside_self_and_dependencies_are_refused() {
    operator_refusal(json!({
        "agent_targets": ["wf-run"],
        "policy": {"trusted_packs": ["acme/base", "rogue/pack"]},
    }));
}

#[test]
fn pack_job_block_dynamic_run_is_refused() {
    operator_refusal(json!({"agent_targets": ["wf-run"], "policy": {"dynamic": "run"}}));
}

#[test]
fn pack_job_block_project_roots_must_be_empty() {
    operator_refusal(json!({
        "agent_targets": ["wf-run"],
        "policy": {"project_roots": ["/work/repo"]},
    }));
}

#[test]
fn engine_style_pack_job_block_installs() {
    let policy = json!({
        "dynamic": "check",
        "dynamic_targets": ["wf-read-only"],
        "trusted_packs": ["acme/engine", "acme/base"],
    });
    let job = json!({"agent_targets": ["wf-run", "wf-read-only"], "policy": policy});
    engine_admission(job, crate::pack::InstalledVia::Operator).unwrap();
}

/// A graph fixture renamed to `fixture/<name>` that also ships `workflows/`.
fn graph_archive_shipping_workflows(
    fixture: &str,
    name: &str,
    edit: impl FnOnce(&mut serde_json::Value),
) -> crate::pack_archive::PackArchive {
    let (_guard, root) = crate::test_support::fixture_pack_copy(fixture, &json!({}));
    let path = root.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["name"] = json!(name);
    manifest["assets"]
        .as_array_mut()
        .unwrap()
        .push(json!("workflows/flow.json"));
    edit(&mut manifest);
    std::fs::create_dir_all(root.join("workflows")).unwrap();
    std::fs::write(root.join("workflows/flow.json"), name).unwrap();
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let (bytes, _) = crate::pack_archive::pack_dir(&root).unwrap();
    crate::pack_archive::PackArchive::from_bytes(&bytes).unwrap()
}

#[tokio::test]
async fn content_then_engine_dependency_order_mounts_the_content_workflows() {
    let (_node, access, options) = dependency_fixture().await;
    let content = graph_archive_shipping_workflows("review_graph", "content", |_| {});
    let engine = graph_archive_shipping_workflows("prepared_graph", "engine", |manifest| {
        let plugin = manifest["plugins"][0].as_object_mut().unwrap();
        plugin.remove("limits");
        plugin.insert("asset_mounts".into(), json!([{"class": "workflows"}]));
    });
    let load = |archive: &crate::pack_archive::PackArchive| {
        crate::graph_package::load_archive_graph_package_with_environment(
            archive,
            &crate::pack::PackInstallOptions {
                node_did: OWNER.into(),
            },
            &|name| (name == "GENTS_REVIEW_MODEL").then(|| "test-model".to_owned()),
        )
        .unwrap()
    };
    let engine_options = crate::graph_package::GraphPackageInstallBindings {
        node_did: OWNER.into(),
        inference_slots: BTreeMap::from([("worker".into(), "glm:inference".into())]),
    };
    let mut prepared = prepared();
    prepared.dependencies = vec![(load(&content), options), (load(&engine), engine_options)];
    let home = tempfile::tempdir().unwrap();
    install_prepared_document_pack(
        &access,
        OWNER,
        &documents_archive_shipping(&[]),
        &[&content, &engine],
        &prepared,
        Some(home.path()),
        false,
        crate::pack::InstalledVia::SelfConfig,
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();
    let record = crate::plugin::store::read_record(home.path(), "fixture", "prepare_fixture")
        .expect("the engine plugin is installed");
    let mounts = crate::plugin::mounts::resolve(home.path(), &record, None, None).unwrap();
    let packs: Vec<&str> = mounts.iter().map(|mount| mount.pack.as_str()).collect();
    assert_eq!(packs, ["fixture/content", "fixture/engine"]);
    assert!(mounts.iter().all(|mount| mount.class == "workflows"));
    assert!(Path::new(&mounts[0].path).join("flow.json").is_file());
}
