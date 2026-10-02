use std::sync::Arc;

use super::*;

const OWNER: &str = "did:key:zPackInstallOwner";

async fn access() -> ConfigAccess {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
    ConfigAccess::Local(node)
}

fn config(tools: &[(&str, &str)]) -> PackConfig {
    serde_json::from_value(json!({
        "agent_principal": {"agent_did": OWNER},
        "tools": tools
            .iter()
            .map(|(id, name)| json!({"tools_id": id, "display_name": name, "agent_did": OWNER, "tags": ["gents:pack:demo"]}))
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
        add: json!({"tools_id": id, "display_name": name, "agent_did": OWNER}),
        update: json!({"tools_id": id, "display_name": name, "agent_did": OWNER}),
    }])
    .unwrap();
    access
        .transact("test.edit", |txn| {
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
    // Reinstalling what is installed replaces nothing it should not.
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
        .transact("test.corrupt_installation_field", |txn| {
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

/// A `plugins`/`documents`/`history` field that decodes (present, not null)
/// but does not match its expected shape must fail loudly, naming the
/// record, rather than silently reading as an empty list.
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

/// `list_installed_packs` reports every record's coordinate, version and
/// digest; a record missing one must fail loudly naming the record, not
/// silently report an empty string a caller could mistake for real data.
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

/// Same as above, for `read_installed_pack`'s single-coordinate lookup.
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

/// A DefraDB unique index is a single-node guarantee, not a distributed one
/// (see [`crate::goal::delete_goals_for_session`]'s doc comment): a
/// replicated home can still end up with two `PackInstallation` records for
/// the same owner and coordinate. `read_installed_pack`'s `limit: 2` plus
/// `ensure!` exists for exactly that state, so this pins it by registering
/// the schema with its unique index dropped and creating the duplicate
/// directly, the same way `register_config_schemas_dropping_unique_indexes`
/// does for desired-state collections.
#[tokio::test]
async fn read_installed_pack_fails_loudly_on_more_than_one_record() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    let schema = gents_protocol::schemas::PACK_INSTALLATION.replace(
        r#"@index(fields: ["agent_did", "coordinate"], unique: true)"#,
        "",
    );
    node.add_schema(&schema).await.unwrap();
    let access = ConfigAccess::Local(node);

    for digest_seed in ["1", "2"] {
        let input = json!({
            "agent_did": OWNER,
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

// --- Dependency bookkeeping (design C) ---------------------------------

async fn dependency_fixture() -> (
    Arc<defra_node::EmbeddedNode>,
    ConfigAccess,
    crate::graph_package::GraphPackageInstallBindings,
) {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    crate::document_config::ensure_agent_principal(&node, OWNER)
        .await
        .unwrap();
    for profile in ["claude", "glm", "grok"] {
        crate::test_support::install_test_behavior(&node, OWNER, profile).await;
    }
    let options = crate::graph_package::GraphPackageInstallBindings {
        agent_did: OWNER.into(),
        inference_slots: std::collections::BTreeMap::from([
            ("coordinator".into(), "claude:inference".into()),
            ("worker".into(), "glm:inference".into()),
            ("verifier".into(), "grok:inference".into()),
        ]),
    };
    let access = ConfigAccess::Local(node.clone());
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

    // Removing the dependency directly is refused, naming the dependent.
    let error = remove_pack(&access, OWNER, "fixture/review_graph", DriftPolicy::Refuse)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("acme/demo"), "{error:#}");

    // Removing the dependent releases its last non-explicit claim too.
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
    assert_eq!(rows[0]["required_by"], Value::Null);

    // Still removable directly now that nothing requires it.
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

    // An upgrade of acme/demo that no longer depends on review_graph.
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
        "the dropped, non-explicit dependency is released outright (its own graph \
         documents removed), the same as `gents pack remove` releases a dependency \
         whose last dependent is removed; a dangling required_by is not enough: {rows:?}"
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

    // Simulate a record written before `explicit` existed.
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

    // required_by is now empty, but a legacy `explicit: null` reads as
    // `true`, so the dependency is never auto-released.
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
