use std::sync::Arc;

use super::*;

const OWNER: &str = "did:key:zPackInstallOwner";

#[test]
fn edited_pack_error_identifies_the_operator_cli() {
    let error = edited_error("acme/demo", &["Tools/t1".to_owned()]).to_string();
    assert!(error.contains("Tools/t1"));
    assert!(error.contains("operator CLI `gents pack install`"));
    assert!(error.contains("--overwrite"));
    assert!(error.contains("--keep"));
    assert!(error.contains("agent pack command does not accept these flags"));
}

async fn access() -> ConfigAccess {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
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
        explicit: true,
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
async fn installed_pack_documents_lists_what_the_record_holds() {
    let access = access().await;
    install(&access, "1", &config(&[("t1", "T1")]), DriftPolicy::Refuse)
        .await
        .unwrap();

    assert_eq!(
        installed_pack_documents(&access, OWNER, "acme/demo")
            .await
            .unwrap(),
        vec![(Collection::Tools, "t1".to_owned())]
    );
    assert!(installed_pack_documents(&access, OWNER, "acme/other")
        .await
        .unwrap()
        .is_empty());
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

async fn installation_explicit(access: &ConfigAccess) -> bool {
    access
        .execute("{ PackInstallation { explicit } }")
        .await
        .unwrap()["data"]["PackInstallation"][0]["explicit"]
        .as_bool()
        .unwrap()
}

#[tokio::test]
async fn a_non_explicit_documents_install_becomes_explicit_when_installed_explicitly() {
    let access = access().await;
    let config = config(&[("alpha", "Alpha")]);
    let dependency = PackIdentity {
        explicit: false,
        ..identity("1")
    };
    super::super::install_pack_documents(&access, OWNER, &dependency, &config, DriftPolicy::Refuse)
        .await
        .unwrap();
    assert!(!installation_explicit(&access).await);

    super::super::install_pack_documents(
        &access,
        OWNER,
        &identity("1"),
        &config,
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();
    assert!(installation_explicit(&access).await);

    super::super::install_pack_documents(&access, OWNER, &dependency, &config, DriftPolicy::Refuse)
        .await
        .unwrap();
    assert!(
        installation_explicit(&access).await,
        "an explicit install is sticky"
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

async fn create_raw_tools(access: &ConfigAccess, id: &str) {
    let response = access
        .transact_write("test.create_raw_tools", |txn| {
            Box::pin(async move {
                txn.execute(&format!(
                    r#"mutation {{ create_Tools(input: {{ tools_id: "{id}", display_name: "Leftover", node_did: "{OWNER}" }}) {{ _docID }} }}"#
                ))
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

fn recorded_entry(collection: &str, id: &str) -> Value {
    json!({"collection": collection, "id": id, "digest": "sha256:stale", "created": true})
}

async fn record_with_unmanaged_entries(access: &ConfigAccess) {
    create_raw_tools(access, "pkg-leftover").await;
    let doc_id = installation_doc_id(access).await;
    let current = access
        .execute("{ PackInstallation { documents } }")
        .await
        .unwrap();
    let mut documents: Vec<Value> =
        serde_json::from_value(current["data"]["PackInstallation"][0]["documents"].clone())
            .unwrap();
    documents.push(recorded_entry("Tools", "pkg-leftover"));
    documents.push(recorded_entry("RetiredFixture", "old"));
    corrupt_installation_field(access, &doc_id, "documents", json!(documents)).await;
}

#[tokio::test]
async fn remove_tolerates_unmanaged_collections() {
    let access = access().await;
    install(&access, "1", &config(&[("t1", "T1")]), DriftPolicy::Refuse)
        .await
        .unwrap();
    record_with_unmanaged_entries(&access).await;

    let report = remove_pack(&access, OWNER, "acme/demo", DriftPolicy::Refuse)
        .await
        .unwrap();
    assert!(report
        .documents
        .removed
        .contains(&"Tools/pkg-leftover".to_owned()));
    assert_eq!(report.retained.len(), 1);
    assert_eq!(report.retained[0].item, "RetiredFixture old");
    assert_eq!(
        report.retained[0].reason,
        "collection is no longer managed; the document stays dormant"
    );
    assert!(tools_ids(&access).await.is_empty());
    let record = access
        .execute("{ PackInstallation { _docID } }")
        .await
        .unwrap();
    assert_eq!(record["data"]["PackInstallation"], json!([]));
}

#[tokio::test]
async fn update_over_unmanaged_record() {
    let access = access().await;
    install(&access, "1", &config(&[("t1", "T1")]), DriftPolicy::Refuse)
        .await
        .unwrap();
    record_with_unmanaged_entries(&access).await;

    install(&access, "2", &config(&[("t1", "T1")]), DriftPolicy::Refuse)
        .await
        .unwrap();
    assert_eq!(tools_ids(&access).await, vec!["t1"]);
    let record = access
        .execute("{ PackInstallation { documents } }")
        .await
        .unwrap();
    let documents = record["data"]["PackInstallation"][0]["documents"].to_string();
    assert!(!documents.contains("RetiredFixture"), "{documents}");
    assert!(!documents.contains("pkg-leftover"), "{documents}");
}

#[tokio::test]
async fn installed_pack_documents_skips_unmanaged_collections() {
    let access = access().await;
    install(&access, "1", &config(&[("t1", "T1")]), DriftPolicy::Refuse)
        .await
        .unwrap();
    let doc_id = installation_doc_id(&access).await;
    corrupt_installation_field(
        &access,
        &doc_id,
        "documents",
        json!([
            recorded_entry("RetiredFixture", "old"),
            recorded_entry("Trigger", "trig"),
        ]),
    )
    .await;

    assert_eq!(
        installed_pack_documents(&access, OWNER, "acme/demo")
            .await
            .unwrap(),
        vec![(Collection::Trigger, "trig".to_owned())]
    );
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
    node.add_collection(&schema).await.unwrap();
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

async fn dependency_fixture() -> (Arc<defra_node::EmbeddedNode>, ConfigAccess) {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    crate::document_config::ensure_node(&node, OWNER)
        .await
        .unwrap();
    for profile in ["claude", "glm", "grok"] {
        crate::test_support::install_test_agent(&node, OWNER, profile).await;
    }
    let access = ConfigAccess::local(node.clone());
    (node, access)
}

/// Installs `dependee_fixture` the way a front-end installs a documents pack.
async fn install_dependee(access: &ConfigAccess, explicit: bool) {
    let (_guard, dir) = crate::test_support::fixture_pack_copy("dependee_fixture", &json!({}));
    let (bytes, _) = crate::pack_archive::pack_dir(&dir).unwrap();
    let archive = crate::pack_archive::PackArchive::from_bytes(&bytes).unwrap();
    let authored = crate::pack::load_pack_config(
        archive.manifest(),
        &crate::pack::PackInstallOptions {
            node_did: OWNER.into(),
        },
        &|path| Ok(archive.asset(path)?.to_vec()),
        &|_| None,
    )
    .unwrap();
    let requested = BTreeMap::from([("worker".to_owned(), "glm:inference".to_owned())]);
    let prepared = prepare_document_pack_install(
        access,
        OWNER,
        archive.manifest(),
        &authored,
        &requested,
        &[],
        crate::pack::InstalledVia::Operator,
        None,
        &|_| None,
    )
    .await
    .unwrap();
    super::super::install_pack_documents(
        access,
        OWNER,
        &PackIdentity {
            coordinate: "fixture/dependee_fixture".into(),
            version: archive.manifest().version.clone(),
            digest: archive.digest().to_owned(),
            plugins: vec![],
            dependencies: vec![],
            explicit,
        },
        &prepared.config,
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();
}

fn demo_identity(dependencies: Vec<String>) -> PackIdentity {
    PackIdentity {
        coordinate: "acme/demo".into(),
        version: "1".into(),
        digest: format!("sha256:{}", "d".repeat(64)),
        plugins: vec![],
        dependencies,
        explicit: true,
    }
}

#[tokio::test]
async fn a_dependency_is_released_with_its_last_non_explicit_dependent() {
    let (_node, access) = dependency_fixture().await;
    install_dependee(&access, false).await;

    super::super::install_pack_documents(
        &access,
        OWNER,
        &demo_identity(vec!["fixture/dependee_fixture".into()]),
        &config(&[("alpha", "Alpha")]),
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();

    let error = remove_pack(
        &access,
        OWNER,
        "fixture/dependee_fixture",
        DriftPolicy::Refuse,
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("acme/demo"), "{error:#}");

    let report = remove_pack(&access, OWNER, "acme/demo", DriftPolicy::Refuse)
        .await
        .unwrap();
    assert_eq!(report.dependencies.len(), 1);
    assert_eq!(report.dependencies[0].pack, "fixture/dependee_fixture");
    assert!(report.dependencies[0]
        .documents
        .removed
        .iter()
        .any(|name| name.starts_with("Agent/")));

    let remaining = access
        .execute("{ PackInstallation { coordinate } }")
        .await
        .unwrap();
    assert_eq!(remaining["data"]["PackInstallation"], json!([]));
}

#[tokio::test]
async fn an_explicit_dependency_survives_its_dependent() {
    let (_node, access) = dependency_fixture().await;
    install_dependee(&access, true).await;

    super::super::install_pack_documents(
        &access,
        OWNER,
        &demo_identity(vec!["fixture/dependee_fixture".into()]),
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
    assert_eq!(rows[0]["coordinate"], "fixture/dependee_fixture");
    assert_eq!(
        rows[0]["required_by"],
        Value::Null,
        "an explicit install is never auto-released"
    );

    remove_pack(
        &access,
        OWNER,
        "fixture/dependee_fixture",
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn an_upgrade_that_drops_a_dependency_releases_its_claim() {
    let (_node, access) = dependency_fixture().await;
    install_dependee(&access, false).await;

    super::super::install_pack_documents(
        &access,
        OWNER,
        &demo_identity(vec!["fixture/dependee_fixture".into()]),
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
    let dependee = rows
        .iter()
        .find(|row| row["coordinate"] == "fixture/dependee_fixture")
        .unwrap();
    assert_eq!(dependee["required_by"], json!(["acme/demo"]));

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
            .any(|row| row["coordinate"] == "fixture/dependee_fixture"),
        "the dropped, non-explicit dependency is released outright: {rows:?}"
    );
}

#[tokio::test]
async fn a_legacy_record_without_dependency_fields_reads_as_explicit() {
    let (_node, access) = dependency_fixture().await;
    install_dependee(&access, false).await;

    let doc_id = installation_doc_id(&access).await;
    corrupt_installation_field(&access, &doc_id, "explicit", Value::Null).await;

    super::super::install_pack_documents(
        &access,
        OWNER,
        &demo_identity(vec!["fixture/dependee_fixture".into()]),
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
            .any(|row| row["coordinate"] == "fixture/dependee_fixture"),
        "a legacy record with no explicit field must never be auto-released: {rows:?}"
    );
}

#[tokio::test]
async fn an_explicitly_installed_documents_dependency_survives_its_dependent() {
    let (_node, access) = dependency_fixture().await;
    install_dependee(&access, false).await;
    super::super::install_pack_documents(
        &access,
        OWNER,
        &demo_identity(vec!["fixture/dependee_fixture".into()]),
        &config(&[("alpha", "Alpha")]),
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();

    install_dependee(&access, true).await;

    let report = remove_pack(&access, OWNER, "acme/demo", DriftPolicy::Refuse)
        .await
        .unwrap();
    assert!(
        report.dependencies.is_empty(),
        "an explicit install is sticky, so it is never auto-released"
    );
    let remaining = access
        .execute("{ PackInstallation { coordinate required_by } }")
        .await
        .unwrap();
    let rows = remaining["data"]["PackInstallation"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["coordinate"], "fixture/dependee_fixture");
    assert_eq!(rows[0]["required_by"], Value::Null);
}

#[tokio::test]
async fn an_upgrade_that_drops_a_documents_dependency_releases_it() {
    let (_node, access) = dependency_fixture().await;
    let (_guards, dependee, dependent) = documents_dependency_pair(false, false);
    install_dependent_with_dependee(&access, &dependee, &dependent, None).await;
    let agents = access.execute("{ Agent { agent_id } }").await.unwrap();
    let before = agents["data"]["Agent"].as_array().unwrap().len();

    super::super::install_pack_documents(
        &access,
        OWNER,
        &PackIdentity {
            coordinate: "fixture/dependent_fixture".into(),
            version: "2".into(),
            digest: format!("sha256:{}", "2".repeat(64)),
            plugins: vec![],
            dependencies: vec![],
            explicit: true,
        },
        &config(&[("alpha", "Alpha")]),
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();

    let rows = access
        .execute("{ PackInstallation { coordinate } }")
        .await
        .unwrap();
    let rows = rows["data"]["PackInstallation"].as_array().unwrap();
    assert!(
        !rows
            .iter()
            .any(|row| row["coordinate"] == "fixture/dependee_fixture"),
        "the dropped documents dependency is released outright: {rows:?}"
    );
    let agents = access.execute("{ Agent { agent_id } }").await.unwrap();
    assert!(
        agents["data"]["Agent"].as_array().unwrap().len() < before,
        "the released dependency's own documents are removed: {agents}"
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
            ux: Vec::new(),
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
        ux: Vec::new(),
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
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
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
            None,
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
fn engine_style_pack_job_block_installs() {
    let policy = json!({
        "dynamic": "check",
        "dynamic_targets": ["wf-read-only"],
        "trusted_packs": ["acme/engine", "acme/base"],
    });
    let job = json!({"agent_targets": ["wf-run", "wf-read-only"], "policy": policy});
    engine_admission(job, crate::pack::InstalledVia::Operator).unwrap();
}

// --- Documents dependencies ---------------------------------------------

/// `dependent_fixture` rewired to depend on `dependee_fixture`, and
/// `dependee_fixture` with the plugin `echo` when `dependee_plugin`; both
/// packed the way an install reads them. With `broken_dependent_plugin` the
/// dependent also ships an `echo` whose artifact is not a plugin, so its
/// install fails at its own plugin step, after its dependency installed.
fn documents_dependency_pair(
    dependee_plugin: bool,
    broken_dependent_plugin: bool,
) -> (
    [tempfile::TempDir; 2],
    crate::pack_archive::PackArchive,
    crate::pack_archive::PackArchive,
) {
    let archive = |dir: &std::path::Path| {
        let (bytes, _) = crate::pack_archive::pack_dir(dir).unwrap();
        crate::pack_archive::PackArchive::from_bytes(&bytes).unwrap()
    };
    let edit = |dir: &std::path::Path, change: &dyn Fn(&mut Value)| {
        let path = dir.join("manifest.json");
        let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        change(&mut manifest);
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    };
    let (dependee_guard, dependee_dir) =
        crate::test_support::fixture_pack_copy("dependee_fixture", &json!({}));
    if dependee_plugin {
        std::fs::create_dir_all(dependee_dir.join("plugins")).unwrap();
        let wat = crate::plugin::tests::constant_output_wat(b"{}");
        std::fs::write(
            dependee_dir.join("plugins/echo.afb"),
            crate::plugin::tests::build_plugin_afb(&wat),
        )
        .unwrap();
        edit(&dependee_dir, &|manifest| {
            manifest["assets"]
                .as_array_mut()
                .unwrap()
                .push(json!("plugins/echo.afb"));
            manifest["plugins"] = json!([{
                "name": "echo",
                "description": "Echoes nothing.",
                "artifact": "plugins/echo.afb",
                "language": "rust",
                "input_schema": {"type": "object"},
            }]);
        });
    }
    let (dependent_guard, dependent_dir) =
        crate::test_support::fixture_pack_copy("dependent_fixture", &json!({}));
    edit(&dependent_dir, &|manifest| {
        manifest["dependencies"] = json!(["fixture/dependee_fixture"]);
    });
    if broken_dependent_plugin {
        std::fs::create_dir_all(dependent_dir.join("plugins")).unwrap();
        std::fs::write(dependent_dir.join("plugins/echo.afb"), b"not a plugin").unwrap();
        edit(&dependent_dir, &|manifest| {
            manifest["assets"]
                .as_array_mut()
                .unwrap()
                .push(json!("plugins/echo.afb"));
            manifest["plugins"] = json!([{
                "name": "echo",
                "description": "Echoes nothing.",
                "artifact": "plugins/echo.afb",
                "language": "rust",
                "input_schema": {"type": "object"},
            }]);
        });
    }
    (
        [dependee_guard, dependent_guard],
        archive(&dependee_dir),
        archive(&dependent_dir),
    )
}

async fn install_dependent_with_dependee(
    access: &ConfigAccess,
    dependee: &crate::pack_archive::PackArchive,
    dependent: &crate::pack_archive::PackArchive,
    plugin_home: Option<&std::path::Path>,
) {
    try_install_dependent_with_dependee(access, dependee, dependent, plugin_home)
        .await
        .unwrap();
}

async fn try_install_dependent_with_dependee(
    access: &ConfigAccess,
    dependee: &crate::pack_archive::PackArchive,
    dependent: &crate::pack_archive::PackArchive,
    plugin_home: Option<&std::path::Path>,
) -> Result<InstallReport> {
    let authored = crate::pack::load_pack_config(
        dependent.manifest(),
        &crate::pack::PackInstallOptions {
            node_did: OWNER.into(),
        },
        &|path| Ok(dependent.asset(path)?.to_vec()),
        &|_| None,
    )
    .unwrap();
    let requested = BTreeMap::from([("worker".to_owned(), "glm:inference".to_owned())]);
    let prepared = prepare_document_pack_install(
        access,
        OWNER,
        dependent.manifest(),
        &authored,
        &requested,
        &[dependee],
        crate::pack::InstalledVia::Operator,
        plugin_home,
        &|_| None,
    )
    .await
    .unwrap();
    install_prepared_document_pack(
        access,
        OWNER,
        dependent,
        &[dependee],
        &prepared,
        plugin_home,
        false,
        crate::pack::InstalledVia::Operator,
        DriftPolicy::Refuse,
    )
    .await
}

#[tokio::test]
async fn a_documents_dependency_installs_non_explicitly_before_its_dependent() {
    let (_node, access) = dependency_fixture().await;
    let (_guards, dependee, dependent) = documents_dependency_pair(false, false);
    install_dependent_with_dependee(&access, &dependee, &dependent, None).await;

    let rows = access
        .execute("{ PackInstallation { coordinate explicit required_by } }")
        .await
        .unwrap();
    let rows = rows["data"]["PackInstallation"].as_array().unwrap();
    let row = |coordinate: &str| {
        rows.iter()
            .find(|row| row["coordinate"] == coordinate)
            .unwrap_or_else(|| panic!("no record for {coordinate}: {rows:?}"))
    };
    let dependency = row("fixture/dependee_fixture");
    assert_eq!(dependency["explicit"], false, "{rows:?}");
    assert_eq!(
        dependency["required_by"],
        json!(["fixture/dependent_fixture"])
    );
    assert_eq!(row("fixture/dependent_fixture")["explicit"], true);
}

#[tokio::test]
async fn a_failing_dependent_leaves_its_documents_dependency_installed() {
    let (_node, access) = dependency_fixture().await;
    let (_guards, dependee, dependent) = documents_dependency_pair(false, true);
    let home = tempfile::tempdir().unwrap();
    try_install_dependent_with_dependee(&access, &dependee, &dependent, Some(home.path()))
        .await
        .unwrap_err();

    let rows = access
        .execute("{ PackInstallation { coordinate explicit } }")
        .await
        .unwrap();
    assert_eq!(
        rows["data"]["PackInstallation"],
        json!([{"coordinate": "fixture/dependee_fixture", "explicit": false}])
    );
}

#[tokio::test]
async fn a_documents_dependency_plugin_installs_before_the_dependent_documents() {
    let (_node, access) = dependency_fixture().await;
    let (_guards, dependee, dependent) = documents_dependency_pair(true, false);
    let home = tempfile::tempdir().unwrap();
    install_dependent_with_dependee(&access, &dependee, &dependent, Some(home.path())).await;

    let record = crate::plugin::store::read_record(home.path(), "fixture", "echo").unwrap();
    assert_eq!(
        record.owner_pack_coordinate.as_deref(),
        Some("fixture/dependee_fixture")
    );
    let installed = read_installed_pack(&access, OWNER, "fixture/dependee_fixture")
        .await
        .unwrap();
    assert_eq!(installed.unwrap().digest, dependee.digest());
}

#[tokio::test]
async fn a_documents_dependency_plugin_outlives_a_failing_dependent() {
    let (_node, access) = dependency_fixture().await;
    let (_guards, dependee, dependent) = documents_dependency_pair(true, true);
    let home = tempfile::tempdir().unwrap();
    try_install_dependent_with_dependee(&access, &dependee, &dependent, Some(home.path()))
        .await
        .unwrap_err();

    let record = crate::plugin::store::read_record(home.path(), "fixture", "echo").unwrap();
    assert_eq!(
        record.owner_pack_coordinate.as_deref(),
        Some("fixture/dependee_fixture")
    );
    let dependent = read_installed_pack(&access, OWNER, "fixture/dependent_fixture")
        .await
        .unwrap();
    assert!(dependent.is_none(), "the dependent installed nothing");
}

/// The error `prepare_document_pack_install` gives `dependent_fixture` when
/// it names `dependee_fixture` after `change` edited its manifest; with
/// `dependee_plugin` the dependee also ships the plugin `echo`.
async fn refused_dependency(dependee_plugin: bool, change: &dyn Fn(&mut Value)) -> String {
    let archive = |dir: &std::path::Path| {
        let (bytes, _) = crate::pack_archive::pack_dir(dir).unwrap();
        crate::pack_archive::PackArchive::from_bytes(&bytes).unwrap()
    };
    let edit = |dir: &std::path::Path, change: &dyn Fn(&mut Value)| {
        let path = dir.join("manifest.json");
        let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        change(&mut manifest);
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    };
    let (_dependee_guard, dependee_dir) =
        crate::test_support::fixture_pack_copy("dependee_fixture", &json!({}));
    if dependee_plugin {
        std::fs::create_dir_all(dependee_dir.join("plugins")).unwrap();
        let wat = crate::plugin::tests::constant_output_wat(b"{}");
        std::fs::write(
            dependee_dir.join("plugins/echo.afb"),
            crate::plugin::tests::build_plugin_afb(&wat),
        )
        .unwrap();
        edit(&dependee_dir, &|manifest| {
            manifest["assets"]
                .as_array_mut()
                .unwrap()
                .push(json!("plugins/echo.afb"));
            manifest["plugins"] = json!([{
                "name": "echo",
                "description": "Echoes nothing.",
                "artifact": "plugins/echo.afb",
                "language": "rust",
                "input_schema": {"type": "object"},
            }]);
        });
    }
    edit(&dependee_dir, change);
    let (_dependent_guard, dependent_dir) =
        crate::test_support::fixture_pack_copy("dependent_fixture", &json!({}));
    edit(&dependent_dir, &|manifest| {
        manifest["dependencies"] = json!(["fixture/dependee_fixture"]);
    });
    let (dependee, dependent) = (archive(&dependee_dir), archive(&dependent_dir));
    let authored = crate::pack::load_pack_config(
        dependent.manifest(),
        &crate::pack::PackInstallOptions {
            node_did: OWNER.into(),
        },
        &|path| Ok(dependent.asset(path)?.to_vec()),
        &|_| None,
    )
    .unwrap();
    let (_node, access) = dependency_fixture().await;
    let error = prepare_document_pack_install(
        &access,
        OWNER,
        dependent.manifest(),
        &authored,
        &BTreeMap::from([("worker".to_owned(), "glm:inference".to_owned())]),
        &[&dependee],
        crate::pack::InstalledVia::Operator,
        None,
        &|_| None,
    )
    .await
    .err()
    .expect("the dependency is refused");
    format!("{error:#}")
}

const DEPENDENCY_NEXT_STEP: &str = "install it with `gents pack install` or `gents plugin install`, then remove it from `dependencies`";

#[tokio::test]
async fn an_assets_dependency_is_refused_with_a_next_step() {
    let error = refused_dependency(false, &|manifest| {
        manifest["kind"] = json!("assets");
        manifest.as_object_mut().unwrap().remove("config");
        manifest["inference_slots"] = json!([]);
    })
    .await;
    assert!(error.contains(DEPENDENCY_NEXT_STEP), "{error}");
}

#[tokio::test]
async fn a_plugins_dependency_is_refused_with_a_next_step() {
    let error = refused_dependency(true, &|manifest| {
        manifest["kind"] = json!("plugins");
        manifest.as_object_mut().unwrap().remove("config");
        manifest["inference_slots"] = json!([]);
    })
    .await;
    assert!(error.contains(DEPENDENCY_NEXT_STEP), "{error}");
}

#[tokio::test]
async fn a_documents_dependency_with_its_own_dependencies_is_refused() {
    let error = refused_dependency(false, &|manifest| {
        manifest["dependencies"] = json!(["fixture/other_fixture"]);
    })
    .await;
    assert!(
        error.contains("declares dependencies of its own"),
        "{error}"
    );
    assert!(
        error.contains("install it with `gents pack install`, then remove it from `dependencies`"),
        "{error}"
    );
}

fn plugin_pack_manifest(name: &str, kind: &str, plugins: &[&str]) -> crate::pack::PackManifest {
    let mut manifest: crate::pack::PackManifest = serde_json::from_value(json!({
        "manifest_version": 1, "name": name, "version": "1.0.0", "namespace": "acme",
        "description": "d", "authors": ["tests"], "kind": kind,
        "assets": std::iter::once("README.md".to_owned())
            .chain(plugins.iter().map(|plugin| format!("plugins/{plugin}.afb")))
            .collect::<Vec<_>>(),
        "plugins": plugins.iter().map(|plugin| json!({
            "name": plugin, "description": "d", "artifact": format!("plugins/{plugin}.afb"),
            "language": "rust", "input_schema": {"type": "object"},
        })).collect::<Vec<_>>(),
    }))
    .unwrap();
    if kind == "documents" {
        manifest.config = Some("config.json".to_owned());
        manifest.metadata.assets.push("config.json".to_owned());
    }
    manifest
}

fn config_naming_plugin(plugin: &str) -> PackConfig {
    serde_json::from_value(json!({
        "node": {"node_did": OWNER},
        "tools": [{"tools_id": "web_persona:tools", "node_did": OWNER,
            "integrations": {"plugins": [{"plugin": plugin}]}}],
    }))
    .unwrap()
}

async fn prepare_naming_plugin(
    manifest: &crate::pack::PackManifest,
    plugin_home: Option<&Path>,
) -> Result<PreparedDocumentPackInstall> {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    crate::document_config::ensure_node(&node, OWNER)
        .await
        .unwrap();
    prepare_document_pack_install(
        &ConfigAccess::local(node),
        OWNER,
        manifest,
        &config_naming_plugin("acme/web_search"),
        &crate::pack::PackInferenceBindings::new(),
        &[],
        crate::pack::InstalledVia::Operator,
        plugin_home,
        &|_| None,
    )
    .await
}

#[tokio::test]
async fn an_install_naming_an_unresolved_plugin_is_refused_before_writing() {
    let home = tempfile::tempdir().unwrap();
    let manifest = plugin_pack_manifest("workflow", "documents", &[]);
    let error = match prepare_naming_plugin(&manifest, Some(home.path())).await {
        Ok(_) => panic!("an unresolved plugin reference must be refused"),
        Err(error) => format!("{error:#}"),
    };
    assert!(
        error.contains("acme/web_search")
            && error.contains("web_persona:tools")
            && error.contains("gents pack install"),
        "{error}"
    );
}

#[tokio::test]
async fn a_plugin_the_pack_ships_or_the_node_has_installed_resolves() {
    let home = tempfile::tempdir().unwrap();
    let shipping = plugin_pack_manifest("workflow", "documents", &["web_search"]);
    prepare_naming_plugin(&shipping, Some(home.path()))
        .await
        .unwrap();

    let manifest = plugin_pack_manifest("workflow", "documents", &[]);
    let record: crate::plugin::store::InstalledPlugin = serde_json::from_value(json!({
        "namespace": "acme", "name": "web_search", "version": "1.0.0",
        "digest": format!("sha256:{}", "a".repeat(64)), "language": "rust",
        "declaration": plugin_pack_manifest("web", "plugins", &["web_search"]).metadata.plugins[0],
    }))
    .unwrap();
    crate::plugin::store::write_record(home.path(), &record).unwrap();
    prepare_naming_plugin(&manifest, Some(home.path()))
        .await
        .unwrap();
}

#[test]
fn a_plugin_a_declared_dependency_ships_resolves() {
    let home = tempfile::tempdir().unwrap();
    let manifest = plugin_pack_manifest("workflow", "documents", &[]);
    let config = config_naming_plugin("acme/web_search");
    let dependency = plugin_pack_manifest("web", "documents", &["web_search"]);
    let dependency_config = config_naming_plugin("acme/web_search");
    super::documents::ensure_plugin_references_resolve(
        &[(&manifest, &config), (&dependency, &dependency_config)],
        Some(home.path()),
    )
    .unwrap();
    assert!(super::documents::ensure_plugin_references_resolve(
        &[(&manifest, &config)],
        Some(home.path()),
    )
    .is_err());
}

#[test]
fn a_documents_dependency_naming_an_unresolved_plugin_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let manifest = plugin_pack_manifest("workflow", "documents", &["web_search"]);
    let config = config_naming_plugin("acme/web_search");
    let dependency = plugin_pack_manifest("base", "documents", &[]);
    let dependency_config = config_naming_plugin("acme/missing");
    let error = super::documents::ensure_plugin_references_resolve(
        &[(&manifest, &config), (&dependency, &dependency_config)],
        Some(home.path()),
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("pack base") && error.contains("acme/missing"),
        "{error}"
    );
}

#[test]
fn generated_plugin_reference_cases_validate_prepared_pack_resources() {
    fn names(value: &Value) -> Vec<&str> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().strip_prefix("acme/").unwrap())
            .collect()
    }
    fn config(references: &Value) -> PackConfig {
        serde_json::from_value(json!({
            "node": {"node_did": OWNER},
            "tools": [{"tools_id": "reference-tools", "node_did": OWNER,
                "integrations": {"plugins": references.as_array().unwrap().iter()
                    .map(|plugin| json!({"plugin": plugin})).collect::<Vec<_>>()}}],
        }))
        .unwrap()
    }
    let cases = crate::lean_vocab_test::lean_plugin_reference_cases();
    assert_eq!(cases.len(), 9);
    for case in cases {
        let home = tempfile::tempdir().unwrap();
        let root = plugin_pack_manifest("workflow", "documents", &names(&case["root_ships"]));
        let dependency =
            plugin_pack_manifest("dependency", "documents", &names(&case["dependency_ships"]));
        let references = case["references"].as_array().unwrap();
        assert!((1..=2).contains(&references.len()));
        let root_config = config(&references[0]);
        let dependency_config = config(references.get(1).unwrap_or(&json!([])));
        for plugin in names(&case["installed"]) {
            let record: crate::plugin::store::InstalledPlugin = serde_json::from_value(json!({
                "namespace": "acme", "name": plugin, "version": "1.0.0",
                "digest": format!("sha256:{}", "a".repeat(64)), "language": "rust",
                "declaration": plugin_pack_manifest("installed", "plugins", &[plugin]).metadata.plugins[0],
            })).unwrap();
            crate::plugin::store::write_record(home.path(), &record).unwrap();
        }
        let result = super::documents::ensure_plugin_references_resolve(
            &[(&root, &root_config), (&dependency, &dependency_config)],
            Some(home.path()),
        );
        assert_eq!(
            result.is_ok(),
            case["accepted"].as_bool().unwrap(),
            "{case}: {result:?}"
        );
    }
}
