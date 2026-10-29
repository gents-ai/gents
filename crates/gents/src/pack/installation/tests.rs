use std::sync::Arc;

use super::*;
use crate::pack::InstalledVia;

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

#[test]
fn document_ux_install_admission_matches_lean() {
    let raw = gents_lean_contract::run_prepared_generator(
        &gents_lean_contract::proofs_dir().unwrap(),
        "Proofs.Conformance.DocumentPackUx:olean",
        "Proofs/Conformance/DocumentPackUx.lean",
    )
    .unwrap();
    let cases: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
    assert_eq!(cases.len(), 8);
    for case in cases {
        let via = if case["operator"].as_bool().unwrap() {
            InstalledVia::Operator
        } else {
            InstalledVia::SelfConfig
        };
        assert_eq!(
            documents::document_ux_install_allowed(
                case["hasUx"].as_bool().unwrap(),
                case["home"].as_bool().unwrap(),
                via
            ),
            case["allowed"].as_bool().unwrap(),
            "{case}"
        );
    }
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
async fn plugin_store_notification_lost_commit_receipt_is_unconfirmed() {
    let home = tempfile::tempdir().unwrap();
    let (node, access) = dependency_fixture().await;
    let pack = identity("1");
    let (result, fired) =
        crate::config_client::ConfigApplyTxn::with_post_commit_receipt_loss_for_operation(
            Some("pack.record_plugin_store"),
            record_plugin_store_change(&access, OWNER, home.path(), &pack.coordinate, Some(&pack)),
        )
        .await;
    assert!(fired);
    assert!(
        matches!(&result, Err(PluginStoreChangeError::Unconfirmed(_))),
        "{result:?}"
    );
    let rows = access
        .execute("{PackInstallation {coordinate plugin_store}}")
        .await
        .unwrap();
    assert_eq!(
        rows["data"]["PackInstallation"][0]["coordinate"],
        pack.coordinate
    );
    assert_eq!(rows["data"]["PackInstallation"][0]["plugin_store"], true);
    node.shutdown().await;
}

#[tokio::test]
async fn pin_rejection_after_unknown_commit_preserves_unconfirmed_outcome() {
    let cases = tokio::task::spawn_blocking(|| {
        crate::lean_vocab_test::lean_contract_snapshot()
            .pack_finality_cases
            .clone()
    })
    .await
    .unwrap();
    for events in [&["no_commit"][..], &["unknown_commit", "no_commit"][..]] {
        let modeled = cases
            .iter()
            .find(|case| case["attempts"] == json!(events))
            .unwrap()["unconfirmed"]
            .as_bool()
            .unwrap();
        let result = classify_plugin_store_failure(
            PluginStoreChangeError::PinConflict("pin conflict".into()),
            modeled,
        );
        assert_eq!(
            matches!(result, PluginStoreChangeError::Unconfirmed(_)),
            modeled
        );
    }
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

#[tokio::test]
async fn operator_installs_document_pack_ux_with_its_data_plugin() {
    let (_guard, root) = crate::test_support::fixture_pack_copy("job_plugin_fixture", &json!({}));
    let path = root.join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    std::fs::create_dir_all(root.join("ux/board")).unwrap();
    std::fs::write(root.join("ux/board/plugin.js"), "export default {};").unwrap();
    manifest["assets"]
        .as_array_mut()
        .unwrap()
        .push(json!("ux/board/plugin.js"));
    manifest["ux"] = json!([{
        "name": "board", "description": "A board page", "entry": "ux/board/plugin.js",
        "contributes": { "areas": ["nav"], "directives": [] }
    }]);
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let (bytes, _) = crate::pack_archive::pack_dir(&root).unwrap();
    let archive = crate::pack_archive::PackArchive::from_bytes(&bytes).unwrap();
    let home = tempfile::tempdir().unwrap();
    install_prepared_document_pack(
        &access().await,
        OWNER,
        &archive,
        &[],
        &prepared(),
        Some(home.path()),
        true,
        InstalledVia::Operator,
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();

    let record = recorded(home.path(), &archive);
    assert_eq!(record.kind, crate::pack::PackKind::Documents);
    assert!(record
        .plugins
        .iter()
        .any(|plugin| plugin.name == "job_fixture"));
    let listing = crate::ux_plugin::find(home.path(), "fixture/job_plugin_fixture/board").unwrap();
    assert_eq!(listing.pack.as_deref(), Some("fixture/job_plugin_fixture"));
    assert!(std::path::Path::new(&listing.file).is_file());
    let plugin = crate::plugin::store::read_record(home.path(), "fixture", "job_fixture").unwrap();
    assert!(plugin.data_collection_fields.contains_key("FixtureNote"));
    assert!(
        documents::require_installed_dependency_ux(archive.manifest(), archive.digest(), None)
            .is_err()
    );
    assert!(documents::require_installed_dependency_ux(
        archive.manifest(),
        "other-digest",
        Some(home.path())
    )
    .is_err());
    documents::require_installed_dependency_ux(
        archive.manifest(),
        archive.digest(),
        Some(home.path()),
    )
    .unwrap();
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
    let home = tempfile::tempdir().unwrap();
    let declaration = plugin_pack_manifest("engine", "plugins", &["runner"])
        .metadata
        .plugins
        .remove(0);
    let record: crate::plugin::store::InstalledPlugin = serde_json::from_value(json!({
        "namespace": "acme", "name": "runner", "version": "1.0.0",
        "digest": format!("sha256:{}", "a".repeat(64)), "language": "rust", "declaration": declaration,
    })).unwrap();
    crate::plugin::store::write_record(home.path(), &record).unwrap();
    let prepare = |via| {
        prepare_document_pack_install(
            &access,
            OWNER,
            &manifest,
            &config,
            &requested,
            &[],
            via,
            Some(home.path()),
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
/// Packed as an install reads them; a broken dependent plugin rejects the graph.
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
        None,
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
async fn a_failing_dependent_does_not_publish_its_documents_dependency() {
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
    assert_eq!(rows["data"]["PackInstallation"], json!([]));
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
async fn a_failing_dependent_does_not_publish_dependency_plugin() {
    let (_node, access) = dependency_fixture().await;
    let (_guards, dependee, dependent) = documents_dependency_pair(true, true);
    let home = tempfile::tempdir().unwrap();
    try_install_dependent_with_dependee(&access, &dependee, &dependent, Some(home.path()))
        .await
        .unwrap_err();

    assert!(crate::plugin::store::read_record(home.path(), "fixture", "echo").is_err());
    assert!(
        read_installed_pack(&access, OWNER, "fixture/dependee_fixture")
            .await
            .unwrap()
            .is_none()
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
        None,
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
async fn remote_install_refuses_unverifiable_plugin_before_writes_and_installs_plugin_free_pack() {
    use crate::identity::NodeIdentity;

    let root = tempfile::tempdir().unwrap();
    let key = root.path().join("remote-install.key");
    let identity =
        tokio::task::spawn_blocking(move || crate::KeyIdentity::load_or_create(key, None))
            .await
            .unwrap()
            .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
            .with_node_acp_enabled()
            .with_http(defra_node::HttpConfig::with_listener(listener.into_std().unwrap()).unwrap())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    crate::document_config::ensure_node(&node, identity.did())
        .await
        .unwrap();
    let access = ConfigAccess::graphql_as(endpoint, identity.did());
    let (_guard, directory) =
        crate::test_support::fixture_pack_copy("documents_fixture", &json!({}));
    let manifest_path = directory.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest.as_object_mut().unwrap().remove("schemas");
    manifest.as_object_mut().unwrap().remove("inference_slots");
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let (bytes, _) = crate::pack_archive::pack_dir(&directory).unwrap();
    let archive = crate::pack_archive::PackArchive::from_bytes(&bytes).unwrap();
    let config = |plugin: Option<&str>| -> PackConfig {
        serde_json::from_value(json!({
            "node": {"node_did": identity.did()},
            "tools": [{"tools_id": "remote-tools", "node_did": identity.did(),
                "integrations": {"plugins": plugin.map(|name| vec![json!({"plugin": name})])}}],
        }))
        .unwrap()
    };
    async fn prepare_remote(
        access: &ConfigAccess,
        owner: &str,
        archive: &crate::pack_archive::PackArchive,
        config: &PackConfig,
    ) -> Result<PreparedDocumentPackInstall> {
        prepare_document_pack_install(
            access,
            owner,
            archive.manifest(),
            config,
            &crate::pack::PackInferenceBindings::new(),
            &[],
            crate::pack::InstalledVia::Operator,
            None,
            Some(archive),
            &|_| None,
        )
        .await
    }
    let error = match prepare_remote(
        &access,
        identity.did(),
        &archive,
        &config(Some("acme/web_search")),
    )
    .await
    {
        Ok(_) => panic!("remote plugin reference must be refused"),
        Err(error) => format!("{error:#}"),
    };
    assert!(
        error.contains("acme/web_search") && error.contains("without --graphql"),
        "{error}"
    );
    assert!(tools_ids(&access).await.is_empty());
    assert!(list_installed_packs(&access, identity.did())
        .await
        .unwrap()
        .is_empty());

    let prepared = prepare_remote(&access, identity.did(), &archive, &config(None))
        .await
        .unwrap();
    install_prepared_document_pack(
        &access,
        identity.did(),
        &archive,
        &[],
        &prepared,
        None,
        false,
        crate::pack::InstalledVia::Operator,
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();
    assert_eq!(tools_ids(&access).await, vec!["remote-tools"]);
    assert_eq!(
        list_installed_packs(&access, identity.did())
            .await
            .unwrap()
            .len(),
        1
    );
    node.shutdown().await;
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
        &BTreeMap::new(),
    )
    .unwrap();
    assert!(super::documents::ensure_plugin_references_resolve(
        &[(&manifest, &config)],
        Some(home.path()),
        &BTreeMap::new(),
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
        &BTreeMap::new(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("pack base") && error.contains("acme/missing"),
        "{error}"
    );
}

#[tokio::test]
async fn generated_plugin_reference_cases_validate_prepared_pack_resources() {
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
    let cases = tokio::task::spawn_blocking(|| {
        crate::lean_vocab_test::lean_plugin_reference_cases().to_vec()
    })
    .await
    .unwrap();
    assert_eq!(cases.len(), 14);
    for case in &cases {
        let home = tempfile::tempdir().unwrap();
        let root = plugin_pack_manifest("workflow", "documents", &names(&case["root_ships"]));
        let dependency =
            plugin_pack_manifest("dependency", "documents", &names(&case["dependency_ships"]));
        let references = case["references"].as_array().unwrap();
        assert!((1..=2).contains(&references.len()));
        let root_config = config(&references[0]);
        let mut dependency_config = config(references.get(1).unwrap_or(&json!([])));
        dependency_config.tools[0].tools_id = "dependency-tools".into();
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
            case["local"].as_bool().unwrap().then_some(home.path()),
            &BTreeMap::new(),
        );
        assert_eq!(
            result.is_ok(),
            case["accepted"].as_bool().unwrap(),
            "{case}: {result:?}"
        );
        let (_node, access) = dependency_fixture().await;
        let references = references
            .iter()
            .flat_map(|group| group.as_array().unwrap())
            .collect::<Vec<_>>();
        let config: PackConfig = serde_json::from_value(json!({
            "node": {"node_did": OWNER},
            "agents": references.iter().enumerate().map(|(index, _)| json!({
                "agent_id": format!("reference-{index}"), "node_did": OWNER,
                "context_id": format!("context-{index}"), "inference_profile_id": "glm:inference"
            })).collect::<Vec<_>>(),
            "contexts": references.iter().enumerate().map(|(index, _)| json!({
                "context_id": format!("context-{index}"), "node_did": OWNER,
                "tools_id": format!("tools-{index}")
            })).collect::<Vec<_>>(),
            "tools": references.iter().enumerate().map(|(index, plugin)| json!({
                "tools_id": format!("tools-{index}"), "node_did": OWNER,
                "integrations": {"plugins": [{"plugin": plugin}]}
            })).collect::<Vec<_>>(),
        }))
        .unwrap();
        install(&access, "1", &config, DriftPolicy::Refuse)
            .await
            .unwrap();
        let removed = case["root_ships"]
            .as_array()
            .unwrap()
            .iter()
            .chain(case["dependency_ships"].as_array().unwrap())
            .map(|value| value.as_str().unwrap().to_owned())
            .collect();
        let removal = super::ensure_plugins_unreferenced(&access, OWNER, &removed).await;
        assert_eq!(
            removal.is_ok(),
            case["removal_accepted"].as_bool().unwrap(),
            "{case}: {removal:?}"
        );
    }
}

#[tokio::test]
async fn removing_a_plugin_pack_refuses_surviving_agents_and_rolls_back_documents() {
    let (_node, access) = dependency_fixture().await;
    install(
        &access,
        "1",
        &config(&[("owned", "Owned")]),
        DriftPolicy::Refuse,
    )
    .await
    .unwrap();
    access.write("test.surviving_plugin_user", r#"mutation {
        create_Tools(input: {tools_id: "surviving-tools", node_did: "did:key:zPackInstallOwner", integrations: {plugins: [{plugin: "acme/echo"}]}}) {_docID}
        create_AgentContext(input: {context_id: "surviving-context", node_did: "did:key:zPackInstallOwner", tools_id: "surviving-tools"}) {_docID}
        create_Agent(input: {agent_id: "surviving-agent", node_did: "did:key:zPackInstallOwner", context_id: "surviving-context", inference_profile_id: "glm:inference"}) {_docID}
    }"#).await.unwrap();
    let before = tools_ids(&access).await;
    let error = remove_pack(&access, OWNER, "acme/demo", DriftPolicy::Refuse)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("acme/echo"), "{error:#}");
    assert!(error.to_string().contains("surviving-agent"), "{error:#}");
    assert_eq!(tools_ids(&access).await, before);
    assert!(read_installed_pack(&access, OWNER, "acme/demo")
        .await
        .unwrap()
        .is_some());
    access
        .write(
            "test.remove_plugin_user",
            r#"mutation {
        delete_Agent(filter: {agent_id: {_eq: "surviving-agent"}}) {_docID}
    }"#,
        )
        .await
        .unwrap();
    remove_pack(&access, OWNER, "acme/demo", DriftPolicy::Refuse)
        .await
        .unwrap();
    let retained = before
        .into_iter()
        .filter(|id| id != "owned")
        .collect::<Vec<_>>();
    assert_eq!(tools_ids(&access).await, retained);
}

#[tokio::test]
async fn removing_agents_with_their_plugin_pack_does_not_block_removal() {
    let (_node, access) = dependency_fixture().await;
    let retained_tools = tools_ids(&access).await;
    let mut config = config_naming_plugin("acme/echo");
    config.contexts = vec![serde_json::from_value(json!({
        "context_id": "owned-context", "node_did": OWNER, "tools_id": "web_persona:tools"
    }))
    .unwrap()];
    config.agents = vec![serde_json::from_value(json!({
        "agent_id": "owned-agent", "node_did": OWNER, "context_id": "owned-context", "inference_profile_id": "glm:inference"
    })).unwrap()];
    install(&access, "1", &config, DriftPolicy::Refuse)
        .await
        .unwrap();
    remove_pack(&access, OWNER, "acme/demo", DriftPolicy::Refuse)
        .await
        .unwrap();
    assert_eq!(tools_ids(&access).await, retained_tools);
}

#[tokio::test]
async fn plugin_removal_does_not_treat_dangling_scope_as_no_references() {
    let access = access().await;
    let removed = BTreeSet::from(["acme/echo".to_owned()]);
    access.write("test.dangling_plugin_scope", r#"mutation {
        create_Agent(input: {agent_id: "dangling", node_did: "did:key:zPackInstallOwner", context_id: "missing", inference_profile_id: "profile"}) {_docID}
    }"#).await.unwrap();
    let missing_context = ensure_plugins_unreferenced(&access, OWNER, &removed)
        .await
        .unwrap_err();
    assert!(missing_context.to_string().contains("missing context"));
    access.write("test.dangling_plugin_tools", r#"mutation {
        create_AgentContext(input: {context_id: "missing", node_did: "did:key:zPackInstallOwner", tools_id: "absent"}) {_docID}
    }"#).await.unwrap();
    let missing_tools = ensure_plugins_unreferenced(&access, OWNER, &removed)
        .await
        .unwrap_err();
    assert!(missing_tools.to_string().contains("missing Tools"));
    access.write("test.no_plugin_scope", r#"mutation {
        update_AgentContext(filter: {context_id: {_eq: "missing"}}, input: {tools_id: null}) {_docID}
    }"#).await.unwrap();
    ensure_plugins_unreferenced(&access, OWNER, &removed)
        .await
        .unwrap();
}

#[tokio::test]
async fn pinned_plugin_references_match_executable_model_before_install_or_update() {
    let cases = tokio::task::spawn_blocking(|| {
        crate::lean_vocab_test::lean_contract_snapshot()
            .plugin_pin_cases
            .clone()
    })
    .await
    .unwrap();
    assert_eq!(cases.len(), 5);
    let bytes = crate::plugin::tests::build_plugin_afb(&crate::plugin::tests::constant_output_wat(
        br#"{"ok":true}"#,
    ));
    let digest = format!(
        "sha256:{}",
        crate::plugin::store::content_digest_hex(&bytes)
    );
    let other = format!("sha256:{}", "b".repeat(64));
    for case in cases {
        let home = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let shipped = case["source"] == "shipped";
        let manifest = plugin_pack_manifest(
            "pin_fixture",
            "documents",
            if shipped { &["web_search"] } else { &[] },
        );
        std::fs::write(
            dir.path().join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        std::fs::write(dir.path().join("README.md"), "fixture").unwrap();
        std::fs::write(dir.path().join("config.json"), "{}").unwrap();
        if shipped {
            std::fs::create_dir_all(dir.path().join("plugins")).unwrap();
            std::fs::write(dir.path().join("plugins/web_search.afb"), &bytes).unwrap();
        } else {
            let record: crate::plugin::store::InstalledPlugin = serde_json::from_value(json!({
                "namespace": "acme", "name": "web_search", "version": "1.0.0",
                "digest": digest, "language": "rust",
                "declaration": plugin_pack_manifest("web", "plugins", &["web_search"]).metadata.plugins[0],
            })).unwrap();
            crate::plugin::store::write_record(home.path(), &record).unwrap();
        }
        let (packed, _) = crate::pack_archive::pack_dir(dir.path()).unwrap();
        let archive = crate::pack_archive::PackArchive::from_bytes(&packed).unwrap();
        let mut config = config_naming_plugin("acme/web_search");
        let plugin = &mut config.tools[0]
            .integrations
            .as_mut()
            .unwrap()
            .plugins
            .as_mut()
            .unwrap()[0];
        plugin.digest = case["pin"].as_str().map(|pin| {
            if pin == "a" {
                digest.clone()
            } else {
                other.clone()
            }
        });
        let (node, access) = dependency_fixture().await;
        let previous: PackConfig = serde_json::from_value(json!({
            "node": {"node_did": OWNER}, "tools": [{"tools_id": "existing", "node_did": OWNER}]
        }))
        .unwrap();
        let identity = PackIdentity::new(archive.manifest(), archive.digest(), Vec::new());
        assert_eq!(case["actual"], "a");
        for update in [false, true] {
            if update {
                super::super::install_pack_documents(
                    &access,
                    OWNER,
                    &identity,
                    &previous,
                    DriftPolicy::Refuse,
                )
                .await
                .unwrap();
            }
            let before = access
                .execute(
                    "{Tools {tools_id integrations} PackInstallation {coordinate version digest}}",
                )
                .await
                .unwrap();
            let result = prepare_document_pack_install(
                &access,
                OWNER,
                archive.manifest(),
                &config,
                &crate::pack::PackInferenceBindings::new(),
                &[],
                crate::pack::InstalledVia::Operator,
                Some(home.path()),
                Some(&archive),
                &|_| None,
            )
            .await;
            assert_eq!(
                result.is_ok(),
                case["accepted"].as_bool().unwrap(),
                "{case}, update={update}: {}",
                result.err().map(|e| e.to_string()).unwrap_or_default()
            );
            if !case["accepted"].as_bool().unwrap() {
                let after = access.execute("{Tools {tools_id integrations} PackInstallation {coordinate version digest}}").await.unwrap();
                assert_eq!(
                    after, before,
                    "refused pin cannot change installed documents (update={update})"
                );
            }
        }
        node.shutdown().await;
    }
}

#[tokio::test]
async fn failed_transaction_begin_restores_the_previous_plugin_record() {
    let home = tempfile::tempdir().unwrap();
    let (node, access) = dependency_fixture().await;
    let manifest = plugin_pack_manifest("begin_failure", "documents", &["web_search"]);
    let old = crate::plugin::tests::build_plugin_afb(&crate::plugin::tests::constant_output_wat(
        br#"{"version":1}"#,
    ));
    let new = crate::plugin::tests::build_plugin_afb(&crate::plugin::tests::constant_output_wat(
        br#"{"version":2}"#,
    ));
    let config: PackConfig = serde_json::from_value(json!({"node": {"node_did": OWNER}})).unwrap();
    let bindings = crate::pack::PackInferenceBindings::new();
    let assets = |bytes| BTreeMap::from([("plugins/web_search.afb".to_owned(), bytes)]);
    super::documents::install_documents_with_plugins(
        &access,
        OWNER,
        &manifest,
        &format!("sha256:{}", "1".repeat(64)),
        &config,
        DriftPolicy::Refuse,
        Some(home.path()),
        assets(old),
        false,
        &bindings,
        Vec::new(),
        true,
    )
    .await
    .unwrap();
    let before = crate::plugin::store::read_record(home.path(), "acme", "web_search").unwrap();
    let cases = tokio::task::spawn_blocking(|| {
        crate::lean_vocab_test::lean_contract_snapshot()
            .pack_finality_cases
            .clone()
    })
    .await
    .unwrap();
    let expected = cases
        .iter()
        .find(|case| case["attempts"] == json!(["no_commit"]))
        .unwrap()["unconfirmed"]
        .as_bool()
        .unwrap();
    node.shutdown().await;
    let error = super::documents::install_documents_with_plugins(
        &access,
        OWNER,
        &manifest,
        &format!("sha256:{}", "2".repeat(64)),
        &config,
        DriftPolicy::Refuse,
        Some(home.path()),
        assets(new),
        false,
        &bindings,
        Vec::new(),
        true,
    )
    .await
    .unwrap_err();
    assert!(!expected);
    assert!(
        format!("{error:#}").contains("transaction was rejected"),
        "{error:#}"
    );
    assert_eq!(
        crate::plugin::store::read_record(home.path(), "acme", "web_search").unwrap(),
        before
    );
}

#[tokio::test]
async fn plugin_update_rolls_back_files_when_surviving_pin_rejects_and_accepts_updated_pin() {
    let home = tempfile::tempdir().unwrap();
    let (node, access) = dependency_fixture().await;
    let manifest = plugin_pack_manifest("pin_fixture", "documents", &["web_search"]);
    let old_bytes = crate::plugin::tests::build_plugin_afb(
        &crate::plugin::tests::constant_output_wat(br#"{"version":1}"#),
    );
    let new_bytes = crate::plugin::tests::build_plugin_afb(
        &crate::plugin::tests::constant_output_wat(br#"{"version":2}"#),
    );
    let old_digest = format!(
        "sha256:{}",
        crate::plugin::store::content_digest_hex(&old_bytes)
    );
    let new_digest = format!(
        "sha256:{}",
        crate::plugin::store::content_digest_hex(&new_bytes)
    );
    let empty: PackConfig = serde_json::from_value(json!({"node": {"node_did": OWNER}})).unwrap();
    let assets = |bytes: Vec<u8>| BTreeMap::from([("plugins/web_search.afb".to_owned(), bytes)]);
    let bindings = crate::pack::PackInferenceBindings::new();
    let initial = super::documents::install_documents_with_plugins(
        &access,
        OWNER,
        &manifest,
        &format!("sha256:{}", "1".repeat(64)),
        &empty,
        DriftPolicy::Refuse,
        Some(home.path()),
        assets(old_bytes),
        false,
        &bindings,
        Vec::new(),
        true,
    )
    .await
    .unwrap();
    assert_eq!(initial.1[0].digest, old_digest);
    let external: PackConfig = serde_json::from_value(json!({
        "node": {"node_did": OWNER},
        "agents": [{"agent_id": "survivor", "node_did": OWNER, "context_id": "survivor-context", "inference_profile_id": "glm:inference"}],
        "contexts": [{"context_id": "survivor-context", "node_did": OWNER, "tools_id": "survivor-tools"}],
        "tools": [{"tools_id": "survivor-tools", "node_did": OWNER,
            "integrations": {"plugins": [{"plugin": "acme/web_search", "digest": old_digest}]}}],
    })).unwrap();
    install(&access, "1", &external, DriftPolicy::Refuse)
        .await
        .unwrap();
    let before = crate::plugin::store::read_record(home.path(), "acme", "web_search").unwrap();
    let before_docs = access
        .execute(
            "{Agent {agent_id} Tools {tools_id integrations} PackInstallation {coordinate digest}}",
        )
        .await
        .unwrap();
    let cases = tokio::task::spawn_blocking(|| {
        crate::lean_vocab_test::lean_contract_snapshot()
            .plugin_pin_cases
            .clone()
    })
    .await
    .unwrap();
    let model = |name: &str| {
        cases.iter().find(|case| case["name"] == name).unwrap()["accepted"]
            .as_bool()
            .unwrap()
    };
    let rejected = super::documents::install_documents_with_plugins(
        &access,
        OWNER,
        &manifest,
        &format!("sha256:{}", "2".repeat(64)),
        &empty,
        DriftPolicy::Refuse,
        Some(home.path()),
        assets(new_bytes.clone()),
        false,
        &bindings,
        Vec::new(),
        true,
    )
    .await
    .unwrap_err();
    assert!(format!("{rejected:#}").contains("survivor"), "{rejected:#}");
    assert_eq!(
        crate::plugin::store::read_record(home.path(), "acme", "web_search").unwrap(),
        before
    );
    assert_eq!(access.execute("{Agent {agent_id} Tools {tools_id integrations} PackInstallation {coordinate digest}}").await.unwrap(), before_docs);
    let updated: PackConfig = serde_json::from_value(json!({
        "node": {"node_did": OWNER},
        "tools": [{"tools_id": "survivor-tools", "node_did": OWNER,
            "integrations": {"plugins": [{"plugin": "acme/web_search", "digest": new_digest}]}}],
    }))
    .unwrap();
    let updated = super::super::provenance::stamp_pack_origin(&manifest, &updated).unwrap();
    let accepted = super::documents::install_documents_with_plugins(
        &access,
        OWNER,
        &manifest,
        &format!("sha256:{}", "2".repeat(64)),
        &updated,
        DriftPolicy::Overwrite,
        Some(home.path()),
        assets(new_bytes),
        false,
        &bindings,
        Vec::new(),
        true,
    )
    .await;
    assert_eq!(accepted.is_ok(), model("installed_match"), "{accepted:?}");
    accepted.unwrap();
    assert_eq!(
        crate::plugin::store::read_record(home.path(), "acme", "web_search")
            .unwrap()
            .digest,
        new_digest
    );
    node.shutdown().await;
}

#[tokio::test]
async fn cancelling_pack_install_caller_does_not_cancel_owned_commit() {
    let home = tempfile::tempdir().unwrap();
    let (node, access) = dependency_fixture().await;
    let manifest = plugin_pack_manifest("cancelled_caller", "documents", &["web_search"]);
    let bytes = crate::plugin::tests::build_plugin_afb(&crate::plugin::tests::constant_output_wat(
        br#"{"ok":true}"#,
    ));
    let expected = format!(
        "sha256:{}",
        crate::plugin::store::content_digest_hex(&bytes)
    );
    let config: PackConfig = serde_json::from_value(json!({"node": {"node_did": OWNER}})).unwrap();
    let reached = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let completed = Arc::new(tokio::sync::Notify::new());
    *super::documents::INSTALLATION_PAUSE.lock().unwrap() =
        Some(super::documents::InstallationPause {
            coordinate: "acme/cancelled_caller".into(),
            reached: reached.clone(),
            release: release.clone(),
            completed: completed.clone(),
        });
    let home_path = home.path().to_owned();
    let child_access = access.clone();
    let caller = tokio::spawn(async move {
        super::documents::install_documents_with_plugins(
            &child_access,
            OWNER,
            &manifest,
            &format!("sha256:{}", "3".repeat(64)),
            &config,
            DriftPolicy::Refuse,
            Some(&home_path),
            BTreeMap::from([("plugins/web_search.afb".to_owned(), bytes)]),
            false,
            &crate::pack::PackInferenceBindings::new(),
            Vec::new(),
            true,
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), reached.notified())
        .await
        .expect("plugin publication did not reach the transaction gate");
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(10), completed.notified())
        .await
        .expect("owned installation did not complete");
    assert_eq!(
        crate::plugin::store::read_record(home.path(), "acme", "web_search")
            .unwrap()
            .digest,
        expected
    );
    assert!(
        super::read_installed_pack(&access, OWNER, "acme/cancelled_caller")
            .await
            .unwrap()
            .is_some()
    );
    node.shutdown().await;
}

#[tokio::test]
async fn dependency_and_root_pin_update_commit_as_one_graph() {
    use super::documents::{install_document_graph, DocumentPackStage};
    let home = tempfile::tempdir().unwrap();
    let (node, access) = dependency_fixture().await;
    let dependency = plugin_pack_manifest("pin_dependency", "documents", &["web_search"]);
    let root = plugin_pack_manifest("demo", "documents", &[]);
    let old_bytes = crate::plugin::tests::build_plugin_afb(
        &crate::plugin::tests::constant_output_wat(br#"{"version":1}"#),
    );
    let new_bytes = crate::plugin::tests::build_plugin_afb(
        &crate::plugin::tests::constant_output_wat(br#"{"version":2}"#),
    );
    let old_digest = format!(
        "sha256:{}",
        crate::plugin::store::content_digest_hex(&old_bytes)
    );
    let new_digest = format!(
        "sha256:{}",
        crate::plugin::store::content_digest_hex(&new_bytes)
    );
    let empty: PackConfig = serde_json::from_value(json!({"node": {"node_did": OWNER}})).unwrap();
    let root_config = |pin: &str| {
        let config = serde_json::from_value::<PackConfig>(json!({
        "node": {"node_did": OWNER},
        "agents": [{"agent_id":"graph-agent", "node_did":OWNER,"context_id":"graph-context","inference_profile_id":"glm:inference"}],
        "contexts": [{"context_id":"graph-context", "node_did":OWNER,"tools_id":"graph-tools"}],
        "tools": [{"tools_id":"graph-tools", "node_did":OWNER,"integrations":{"plugins":[{"plugin":"acme/web_search","digest":pin}]}}]
    })).unwrap();
        super::super::provenance::stamp_pack_origin(&root, &config).unwrap()
    };
    let stage = |manifest: &crate::pack::PackManifest,
                 digest: &str,
                 config: PackConfig,
                 bytes: Option<Vec<u8>>,
                 dependencies: Vec<String>| DocumentPackStage {
        manifest: manifest.clone(),
        digest: format!("sha256:{}", digest.repeat(64)),
        config,
        assets: bytes
            .map(|bytes| BTreeMap::from([("plugins/web_search.afb".to_owned(), bytes)]))
            .unwrap_or_default(),
        bindings: Default::default(),
        dependencies,
        explicit: true,
    };
    let initial = vec![
        stage(&dependency, "1", empty.clone(), Some(old_bytes), vec![]),
        stage(
            &root,
            "1",
            root_config(&old_digest),
            None,
            vec!["acme/pin_dependency".into()],
        ),
    ];
    install_document_graph(
        &access,
        OWNER,
        initial,
        DriftPolicy::Refuse,
        Some(home.path()),
        false,
    )
    .await
    .unwrap();
    let replacement = vec![
        stage(&dependency, "2", empty, Some(new_bytes), vec![]),
        stage(
            &root,
            "2",
            root_config(&new_digest),
            None,
            vec!["acme/pin_dependency".into()],
        ),
    ];
    install_document_graph(
        &access,
        OWNER,
        replacement,
        DriftPolicy::Refuse,
        Some(home.path()),
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        crate::plugin::store::read_record(home.path(), "acme", "web_search")
            .unwrap()
            .digest,
        new_digest
    );
    let rows = access
        .execute("{Tools(filter: {tools_id: {_eq: \"graph-tools\"}}) {integrations}}")
        .await
        .unwrap();
    assert_eq!(
        rows["data"]["Tools"][0]["integrations"]["plugins"][0]["digest"],
        new_digest
    );
    node.shutdown().await;
}

#[tokio::test]
async fn installed_plugin_pin_is_rechecked_at_graph_commit() {
    let home = tempfile::tempdir().unwrap();
    let (node, access) = dependency_fixture().await;
    let (_source, _, old_archive) = crate::pack::plugin_lifecycle::tests::fixture("1.0.0", false);
    crate::pack::plugin_lifecycle::install(
        home.path(),
        &old_archive,
        false,
        None,
        &BTreeMap::new(),
        |_| async { Ok(()) },
    )
    .await
    .unwrap();
    let old = crate::plugin::store::read_record(home.path(), "fixture", "native_echo").unwrap();
    let (updated_dir, _, _) = crate::pack::plugin_lifecycle::tests::fixture("2.0.0", false);
    std::fs::write(
        updated_dir.path().join("source/plugins/native_echo.afb"),
        crate::plugin::tests::build_plugin_afb(&crate::plugin::tests::constant_output_wat(
            br#"{"changed":true}"#,
        )),
    )
    .unwrap();
    let (packed, _) = crate::pack_archive::pack_dir(&updated_dir.path().join("source")).unwrap();
    let updated = crate::pack_archive::PackArchive::from_bytes(&packed).unwrap();
    crate::pack::plugin_lifecycle::install(
        home.path(),
        &updated,
        false,
        None,
        &BTreeMap::new(),
        |_| async { Ok(()) },
    )
    .await
    .unwrap();
    let current = crate::plugin::store::read_record(home.path(), "fixture", "native_echo").unwrap();
    assert_ne!(old.digest, current.digest);
    let config: PackConfig = serde_json::from_value(json!({
        "node": {"node_did":OWNER},
        "tools": [{"tools_id":"stale-pin", "node_did":OWNER,"integrations":{"plugins":[{"plugin":"fixture/native_echo","digest":old.digest}]}}]
    })).unwrap();
    let manifest = plugin_pack_manifest("stale_pin", "documents", &[]);
    let cases = tokio::task::spawn_blocking(|| {
        crate::lean_vocab_test::lean_contract_snapshot()
            .plugin_pin_cases
            .clone()
    })
    .await
    .unwrap();
    let expected = cases
        .iter()
        .find(|case| case["name"] == "installed_mismatch")
        .unwrap()["accepted"]
        .as_bool()
        .unwrap();
    let result = super::documents::install_documents_with_plugins(
        &access,
        OWNER,
        &manifest,
        &format!("sha256:{}", "4".repeat(64)),
        &config,
        DriftPolicy::Refuse,
        Some(home.path()),
        BTreeMap::new(),
        false,
        &BTreeMap::new(),
        Vec::new(),
        true,
    )
    .await;
    assert_eq!(result.is_ok(), expected, "{result:?}");
    assert_eq!(
        crate::plugin::store::read_record(home.path(), "fixture", "native_echo").unwrap(),
        current
    );
    let rows = access
        .execute("{Tools(filter:{tools_id:{_eq:\"stale-pin\"}}){tools_id}}")
        .await
        .unwrap();
    assert!(rows["data"]["Tools"].as_array().unwrap().is_empty());
    node.shutdown().await;
}

#[tokio::test]
async fn installed_plugin_pin_stays_locked_through_document_commit() {
    let home = tempfile::tempdir().unwrap();
    let (node, access) = dependency_fixture().await;
    let (_source, _, old_archive) = crate::pack::plugin_lifecycle::tests::fixture("1.0.0", false);
    crate::pack::plugin_lifecycle::install(
        home.path(),
        &old_archive,
        false,
        None,
        &BTreeMap::new(),
        |_| async { Ok(()) },
    )
    .await
    .unwrap();
    let old = crate::plugin::store::read_record(home.path(), "fixture", "native_echo").unwrap();
    let manifest = plugin_pack_manifest("installed_pin", "documents", &[]);
    let config: PackConfig = serde_json::from_value(json!({
        "node": {"node_did": OWNER},
        "agents": [{"agent_id":"pin-agent", "node_did":OWNER,"context_id":"pin-context","inference_profile_id":"glm:inference"}],
        "contexts": [{"context_id":"pin-context", "node_did":OWNER,"tools_id":"pin-tools"}],
        "tools": [{"tools_id":"pin-tools", "node_did":OWNER,"integrations":{"plugins":[{"plugin":"fixture/native_echo","digest":old.digest}]}}]
    })).unwrap();
    let reached = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let completed = Arc::new(tokio::sync::Notify::new());
    *super::documents::INSTALLATION_PAUSE.lock().unwrap() =
        Some(super::documents::InstallationPause {
            coordinate: "acme/installed_pin".into(),
            reached: reached.clone(),
            release: release.clone(),
            completed,
        });
    let access_for_install = access.clone();
    let home_for_install = home.path().to_owned();
    let installer = tokio::spawn(async move {
        super::documents::install_documents_with_plugins(
            &access_for_install,
            OWNER,
            &manifest,
            &format!("sha256:{}", "3".repeat(64)),
            &config,
            DriftPolicy::Refuse,
            Some(&home_for_install),
            BTreeMap::new(),
            false,
            &BTreeMap::new(),
            Vec::new(),
            true,
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(20), reached.notified())
        .await
        .unwrap();
    let (updated_dir, _, _) = crate::pack::plugin_lifecycle::tests::fixture("2.0.0", false);
    std::fs::write(
        updated_dir.path().join("source/plugins/native_echo.afb"),
        crate::plugin::tests::build_plugin_afb(&crate::plugin::tests::constant_output_wat(
            br#"{"changed":true}"#,
        )),
    )
    .unwrap();
    let (packed, _) = crate::pack_archive::pack_dir(&updated_dir.path().join("source")).unwrap();
    let updated = crate::pack_archive::PackArchive::from_bytes(&packed).unwrap();
    let notify_access = access.clone();
    let notify_home = home.path().to_owned();
    let update = crate::pack::plugin_lifecycle::install(
        home.path(),
        &updated,
        false,
        None,
        &BTreeMap::new(),
        move |identity| async move {
            super::record_plugin_store_change(
                &notify_access,
                OWNER,
                &notify_home,
                &identity.coordinate,
                Some(&identity),
            )
            .await
        },
    )
    .await;
    assert!(
        update.is_err(),
        "concurrent update must not change the locked store"
    );
    assert_eq!(
        crate::plugin::store::read_record(home.path(), "fixture", "native_echo").unwrap(),
        old
    );
    release.notify_one();
    installer.await.unwrap().unwrap();
    let notify_access = access.clone();
    let notify_home = home.path().to_owned();
    let after_commit = crate::pack::plugin_lifecycle::install(
        home.path(),
        &updated,
        false,
        None,
        &BTreeMap::new(),
        move |identity| async move {
            super::record_plugin_store_change(
                &notify_access,
                OWNER,
                &notify_home,
                &identity.coordinate,
                Some(&identity),
            )
            .await
        },
    )
    .await;
    assert!(
        after_commit.is_err(),
        "the committed pin must reject the later update"
    );
    assert_eq!(
        crate::plugin::store::read_record(home.path(), "fixture", "native_echo").unwrap(),
        old
    );
    node.shutdown().await;
}

#[tokio::test]
async fn standalone_plugin_update_restores_store_and_home_on_pin_conflict() {
    let home = tempfile::tempdir().unwrap();
    let (node, access) = dependency_fixture().await;
    let (_source, _, old_archive) = crate::pack::plugin_lifecycle::tests::fixture("1.0.0", false);
    let (old_home, _) = crate::pack::plugin_lifecycle::install(
        home.path(),
        &old_archive,
        false,
        None,
        &BTreeMap::new(),
        |_| async { Ok(()) },
    )
    .await
    .unwrap();
    let old = crate::plugin::store::read_record(home.path(), "fixture", "native_echo").unwrap();
    let external: PackConfig = serde_json::from_value(json!({
        "node": {"node_did": OWNER},
        "agents": [{"agent_id":"standalone-survivor", "node_did": OWNER, "context_id":"standalone-context", "inference_profile_id":"glm:inference"}],
        "contexts": [{"context_id":"standalone-context", "node_did":OWNER,"tools_id":"standalone-tools"}],
        "tools": [{"tools_id":"standalone-tools", "node_did":OWNER, "integrations":{"plugins":[{"plugin":"fixture/native_echo","digest":old.digest}]}}]
    })).unwrap();
    install(&access, "1", &external, DriftPolicy::Refuse)
        .await
        .unwrap();
    let (updated_dir, _, _) = crate::pack::plugin_lifecycle::tests::fixture("2.0.0", false);
    let changed = crate::plugin::tests::build_plugin_afb(
        &crate::plugin::tests::constant_output_wat(br#"{"changed":true}"#),
    );
    std::fs::write(
        updated_dir.path().join("source/plugins/native_echo.afb"),
        changed,
    )
    .unwrap();
    let (packed, _) = crate::pack_archive::pack_dir(&updated_dir.path().join("source")).unwrap();
    let updated = crate::pack_archive::PackArchive::from_bytes(&packed).unwrap();
    let notify_access = access.clone();
    let notify_home = home.path().to_owned();
    let error = crate::pack::plugin_lifecycle::install(
        home.path(),
        &updated,
        false,
        None,
        &BTreeMap::new(),
        move |identity| async move {
            super::record_plugin_store_change(
                &notify_access,
                OWNER,
                &notify_home,
                &identity.coordinate,
                Some(&identity),
            )
            .await
        },
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("standalone-survivor"),
        "{error:#}"
    );
    assert_eq!(
        crate::plugin::store::read_record(home.path(), "fixture", "native_echo").unwrap(),
        old
    );
    assert_eq!(
        crate::pack::read_home_install(home.path(), "fixture/native_echo_pack")
            .unwrap()
            .unwrap(),
        old_home
    );
    node.shutdown().await;
}

#[tokio::test]
async fn dependency_schema_is_published_before_root_schema_rejection() {
    let (node, access) = dependency_fixture().await;
    let (guards, _, _) = documents_dependency_pair(false, false);
    let dependency_root = guards[0].path().join("dependee_fixture");
    let root = guards[1].path().join("dependent_fixture");
    for (dir, filename, schema) in [
        (
            &dependency_root,
            "dependency.graphql",
            "type PackSchemaDependency { name: String }",
        ),
        (
            &root,
            "root.graphql",
            "type PackSchemaRoot { dependency: PackSchemaDependency }",
        ),
    ] {
        std::fs::create_dir_all(dir.join("schemas")).unwrap();
        std::fs::write(dir.join("schemas").join(filename), schema).unwrap();
        let path = dir.join("manifest.json");
        let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        manifest["assets"]
            .as_array_mut()
            .unwrap()
            .push(json!(format!("schemas/{filename}")));
        std::fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    }
    let archive = |dir: &std::path::Path| {
        let (bytes, _) = crate::pack_archive::pack_dir(dir).unwrap();
        crate::pack_archive::PackArchive::from_bytes(&bytes).unwrap()
    };
    let dependency = archive(&dependency_root);
    let dependent = archive(&root);
    let error = try_install_dependent_with_dependee(&access, &dependency, &dependent, None)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("PackSchemaDependency"),
        "{error:#}"
    );
    let result = access
        .execute("{PackSchemaDependency {_docID}}")
        .await
        .unwrap();
    assert!(result["data"]["PackSchemaDependency"].is_array());
    std::fs::write(
        root.join("schemas/root.graphql"),
        "type PackSchemaRoot { name: String }",
    )
    .unwrap();
    let corrected = archive(&root);
    install_dependent_with_dependee(&access, &dependency, &corrected, None).await;
    let result = access
        .execute("{PackSchemaDependency {_docID} PackSchemaRoot {_docID}}")
        .await
        .unwrap();
    assert!(result["data"]["PackSchemaDependency"].is_array());
    assert!(result["data"]["PackSchemaRoot"].is_array());
    node.shutdown().await;
}

#[tokio::test]
async fn definitive_document_drift_rejection_restores_previously_pinned_plugin() {
    let home = tempfile::tempdir().unwrap();
    let (node, access) = dependency_fixture().await;
    let manifest = plugin_pack_manifest("drift_fixture", "documents", &["web_search"]);
    let old_bytes = crate::plugin::tests::build_plugin_afb(
        &crate::plugin::tests::constant_output_wat(br#"{"v":1}"#),
    );
    let new_bytes = crate::plugin::tests::build_plugin_afb(
        &crate::plugin::tests::constant_output_wat(br#"{"v":2}"#),
    );
    let pin = format!(
        "sha256:{}",
        crate::plugin::store::content_digest_hex(&old_bytes)
    );
    let config: PackConfig = serde_json::from_value(json!({
        "node": {"node_did": OWNER},
        "agents": [{"agent_id":"drift-agent", "node_did":OWNER,"context_id":"drift-context","inference_profile_id":"glm:inference"}],
        "contexts": [{"context_id":"drift-context", "node_did":OWNER,"tools_id":"drift-tools"}],
        "tools": [{"tools_id":"drift-tools", "node_did":OWNER,"display_name":"original",
            "integrations":{"plugins":[{"plugin":"acme/web_search","digest":pin}]}}]
    })).unwrap();
    let assets = |bytes| BTreeMap::from([("plugins/web_search.afb".to_owned(), bytes)]);
    let bindings = crate::pack::PackInferenceBindings::new();
    super::documents::install_documents_with_plugins(
        &access,
        OWNER,
        &manifest,
        &format!("sha256:{}", "1".repeat(64)),
        &config,
        DriftPolicy::Refuse,
        Some(home.path()),
        assets(old_bytes),
        false,
        &bindings,
        Vec::new(),
        true,
    )
    .await
    .unwrap();
    edit_tools(&access, "drift-tools", "operator edit").await;
    let before = crate::plugin::store::read_record(home.path(), "acme", "web_search").unwrap();
    let docs = access.execute("{Tools(filter: {tools_id: {_eq: \"drift-tools\"}}) {display_name integrations} PackInstallation {coordinate digest}}").await.unwrap();
    let error = super::documents::install_documents_with_plugins(
        &access,
        OWNER,
        &manifest,
        &format!("sha256:{}", "2".repeat(64)),
        &config,
        DriftPolicy::Refuse,
        Some(home.path()),
        assets(new_bytes),
        false,
        &bindings,
        Vec::new(),
        true,
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("drift"), "{error:#}");
    assert_eq!(
        crate::plugin::store::read_record(home.path(), "acme", "web_search").unwrap(),
        before
    );
    assert_eq!(access.execute("{Tools(filter: {tools_id: {_eq: \"drift-tools\"}}) {display_name integrations} PackInstallation {coordinate digest}}").await.unwrap(), docs);
    node.shutdown().await;
}

#[test]
fn staging_ignores_schema_patches_but_collects_sdl_and_plugin_assets() {
    let mut manifest = plugin_pack_manifest("staging_patch", "documents", &["web_search"]);
    manifest.metadata.assets.extend([
        "schemas/base.graphql".to_owned(),
        "schemas/add-field.patch.json".to_owned(),
        "schemas/other.json-patch".to_owned(),
    ]);
    let paths = crate::plugin::install::stage_asset_paths(&manifest).unwrap();
    assert!(paths.contains("schemas/base.graphql"));
    assert!(paths.contains("plugins/web_search.afb"));
    assert!(!paths.contains("schemas/add-field.patch.json"));
    assert!(!paths.contains("schemas/other.json-patch"));
    assert!(crate::pack::document_pack_schema_paths(&manifest).is_err());
}

#[tokio::test]
async fn committed_ack_timeout_retains_staged_plugin_records() {
    let home = tempfile::tempdir().unwrap();
    let manifest = plugin_pack_manifest("ack_timeout", "documents", &["web_search"]);
    let bytes = tokio::task::spawn_blocking(|| {
        crate::plugin::tests::build_plugin_afb(&crate::plugin::tests::constant_output_wat(
            br#"{"version":2}"#,
        ))
    })
    .await
    .unwrap();
    let expected_digest = format!(
        "sha256:{}",
        crate::plugin::store::content_digest_hex(&bytes)
    );
    let stage = crate::plugin::install::stage_pack_graph(
        home.path(),
        vec![crate::plugin::install::PluginStageInput {
            manifest,
            digest: format!("sha256:{}", "2".repeat(64)),
            assets: BTreeMap::from([("plugins/web_search.afb".to_owned(), bytes)]),
            bindings: BTreeMap::new(),
        }],
        false,
        OWNER,
    )
    .await
    .unwrap();
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    let access = ConfigAccess::local(node.clone());
    access
        .add_collection("type AckProbe { value: String }")
        .await
        .unwrap();
    let reached = Arc::new(tokio::sync::Notify::new());
    let mut write = Box::pin(
        crate::config_client::ConfigApplyTxn::with_embedded_commit_ack_pause(
            reached.clone(),
            access.transact_write_with_finality("test.pack.ack_timeout", |txn| {
                Box::pin(async move {
                    txn.execute(
                        "mutation { create_AckProbe(input: {value: \"committed\"}) { _docID } }",
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            }),
        ),
    );
    tokio::select! {
        () = reached.notified() => {},
        result = &mut write => panic!("commit did not reach acknowledgement pause: {result:?}"),
    }
    tokio::time::pause();
    tokio::time::advance(crate::config_client::EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT).await;
    tokio::time::resume();
    let error = write.await.unwrap_err();
    let cases = tokio::task::spawn_blocking(|| {
        crate::lean_vocab_test::lean_contract_snapshot()
            .pack_finality_cases
            .clone()
    })
    .await
    .unwrap();
    let expected = cases
        .iter()
        .find(|case| case["attempts"] == json!(["unknown_commit"]))
        .unwrap()["unconfirmed"]
        .as_bool()
        .unwrap();
    assert_eq!(error.unconfirmed, expected);
    stage.finish(error.unconfirmed).await.unwrap();
    assert_eq!(
        crate::plugin::store::read_record(home.path(), "acme", "web_search")
            .unwrap()
            .digest,
        expected_digest
    );
    let rows = access.execute("{ AckProbe { value } }").await.unwrap();
    assert_eq!(rows["data"]["AckProbe"], json!([{"value":"committed"}]));
    node.shutdown().await;
}
