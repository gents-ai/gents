use std::collections::BTreeMap;
use std::sync::Arc;

use defra_node::EmbeddedNode;
use serde_json::json;

use crate::config_client::ConfigAccess;
use crate::graph_package::GraphPackageInstallBindings;
use crate::graph_pipeline::activate_graph_revision;
use crate::graphql::escape_graphql_string;
use crate::pack::{remove_pack, DriftPolicy};
use crate::test_support::{
    install_test_graph_package, install_test_graph_package_explicit, load_test_graph_package,
};

const OWNER: &str = "did:key:zGraphRemoveOwner";

async fn fixture() -> (Arc<EmbeddedNode>, ConfigAccess, GraphPackageInstallBindings) {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    crate::document_config::ensure_agent_principal(&node, OWNER)
        .await
        .unwrap();
    for profile in ["claude", "glm", "grok"] {
        crate::test_support::install_test_behavior(&node, OWNER, profile).await;
    }
    let options = GraphPackageInstallBindings {
        agent_did: OWNER.into(),
        inference_slots: BTreeMap::from([
            ("coordinator".into(), "claude:inference".into()),
            ("worker".into(), "glm:inference".into()),
            ("verifier".into(), "grok:inference".into()),
        ]),
    };
    let access = ConfigAccess::Local(node.clone());
    (node, access, options)
}

async fn count(access: &ConfigAccess, collection: &str, field: &str, owner: &str) -> usize {
    let response = access
        .execute(&format!(
            r#"{{ {collection}(filter: {{ {field}: {{ _eq: "{owner}" }} }}) {{ _docID }} }}"#
        ))
        .await
        .unwrap();
    response["data"][collection].as_array().unwrap().len()
}

#[tokio::test]
async fn a_graph_install_is_recorded_and_remove_leaves_no_graph_documents() {
    let (node, access, options) = fixture().await;
    let receipt = install_test_graph_package(&access, OWNER, "review_graph", &options)
        .await
        .unwrap();
    activate_graph_revision(
        &node,
        None,
        OWNER,
        &receipt.graph_id,
        &receipt.revision_digest,
        None,
    )
    .await
    .unwrap();

    let response = access
        .execute(&format!(
            r#"{{ PackInstallation(filter: {{ agent_did: {{ _eq: "{OWNER}" }} }}) {{ coordinate documents }} }}"#
        ))
        .await
        .unwrap();
    let rows = response["data"]["PackInstallation"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["coordinate"], "fixture/review_graph");
    let documents = rows[0]["documents"].as_array().unwrap();
    assert!(documents
        .iter()
        .any(|document| document["collection"] == "GraphDefinition"));
    assert!(documents
        .iter()
        .any(|document| document["collection"] == "GraphRevision"));
    assert!(documents
        .iter()
        .any(|document| document["id"] == "review-recon-task"));

    assert_eq!(
        count(&access, "GraphDefinition", "agent_did", OWNER).await,
        1
    );
    assert_eq!(count(&access, "GraphRevision", "owner_did", OWNER).await, 1);
    assert!(count(&access, "Trigger", "agent_did", OWNER).await > 0);
    assert!(count(&access, "EventSource", "agent_did", OWNER).await > 0);
    assert!(count(&access, "Task", "agent_did", OWNER).await > 0);

    let report = remove_pack(&access, OWNER, "fixture/review_graph", DriftPolicy::Refuse)
        .await
        .unwrap();
    assert!(
        !report.retained.is_empty(),
        "the package's SDL schemas cannot be dropped and must be reported retained"
    );

    assert_eq!(
        count(&access, "GraphDefinition", "agent_did", OWNER).await,
        0
    );
    assert_eq!(count(&access, "GraphRevision", "owner_did", OWNER).await, 0);
    assert_eq!(count(&access, "Trigger", "agent_did", OWNER).await, 0);
    assert_eq!(count(&access, "EventSource", "agent_did", OWNER).await, 0);
    assert_eq!(
        count(&access, "Task", "agent_did", OWNER).await,
        0,
        "the package's own Task documents are removed with it"
    );

    let remaining = access
        .execute("{ PackInstallation { _docID } }")
        .await
        .unwrap();
    assert_eq!(remaining["data"]["PackInstallation"], json!([]));

    // A second remove finds no record.
    assert!(
        remove_pack(&access, OWNER, "fixture/review_graph", DriftPolicy::Refuse)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_non_terminal_graph_run_refuses_removal_and_names_it() {
    let (node, access, options) = fixture().await;
    let receipt = install_test_graph_package(&access, OWNER, "review_graph", &options)
        .await
        .unwrap();
    activate_graph_revision(
        &node,
        None,
        OWNER,
        &receipt.graph_id,
        &receipt.revision_digest,
        None,
    )
    .await
    .unwrap();
    let run = crate::graph_pipeline::start_graph_run(
        &node,
        None,
        OWNER,
        &receipt.graph_id,
        None,
        "review",
        json!({
            "repository_path": "/tmp/repo", "base_ref": "base-sha", "head_ref": "head-sha",
            "lens_count": "4", "lens_min": "4", "lens_max": "4", "focus": "durability"
        }),
        crate::graph_pipeline::EntryInputOrigin::Operator,
    )
    .await
    .unwrap();

    let error = remove_pack(&access, OWNER, "fixture/review_graph", DriftPolicy::Refuse)
        .await
        .unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains(&run.run_id), "{message}");
    assert!(message.contains("code-review"), "{message}");
    assert!(message.contains("gents graph cancel"), "{message}");

    // A run reaching a terminal status unblocks removal. Reaching "cancelled"
    // legally needs its active request to finish first, which needs a live
    // provider this test has none of; force the terminal status directly, the
    // same way this crate's other tests simulate a state no legal transition
    // reaches on its own.
    let query = access
        .execute(&format!(
            r#"{{ GraphRun(filter: {{ run_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            run.run_id
        ))
        .await
        .unwrap();
    let doc_id = query["data"]["GraphRun"][0]["_docID"].as_str().unwrap();
    access
        .write(
            "test.force_graph_run_terminal",
            &format!(
                r#"mutation {{ update_GraphRun(docID: "{doc_id}", input: {{ status: "cancelled" }}) {{ _docID }} }}"#
            ),
        )
        .await
        .unwrap();

    let report = remove_pack(&access, OWNER, "fixture/review_graph", DriftPolicy::Refuse)
        .await
        .expect("a terminal run no longer blocks removal");
    assert!(
        report
            .retained
            .iter()
            .any(|retained| retained.item == format!("run {}", run.run_id)
                && retained.reason == "result view needs the graph reinstalled"),
        "the finished run's lost result view must be reported: {:?}",
        report.retained
    );
}

#[tokio::test]
async fn remove_deletes_retired_revisions_too() {
    let (node, access, options) = fixture().await;
    let first = install_test_graph_package(&access, OWNER, "review_graph", &options)
        .await
        .unwrap();
    activate_graph_revision(
        &node,
        None,
        OWNER,
        &first.graph_id,
        &first.revision_digest,
        None,
    )
    .await
    .unwrap();

    // A metadata-only successor: a new revision, superseding the first.
    let mut successor = load_test_graph_package("review_graph", &options);
    successor.manifest.version.push_str("-successor");
    let second = crate::graph_package::install_loaded_graph_package(
        &access,
        OWNER,
        &successor,
        &options,
        None,
        &crate::graph_package::GraphInstallRecord {
            plugins: Vec::new(),
            explicit: true,
        },
    )
    .await
    .unwrap();
    assert_ne!(second.revision_digest, first.revision_digest);
    activate_graph_revision(
        &node,
        None,
        OWNER,
        &second.graph_id,
        &second.revision_digest,
        Some(&first.revision_digest),
    )
    .await
    .unwrap();

    assert_eq!(count(&access, "GraphRevision", "owner_did", OWNER).await, 2);

    remove_pack(&access, OWNER, "fixture/review_graph", DriftPolicy::Refuse)
        .await
        .unwrap();
    assert_eq!(
        count(&access, "GraphRevision", "owner_did", OWNER).await,
        0,
        "a retired revision is removed along with the active one"
    );
}

#[tokio::test]
async fn an_untracked_graph_is_adopted_on_reinstall_and_survives_removal() {
    // An operator's graph that already existed before a `PackInstallation`
    // record for it did (a graph installed before this change, or one
    // export/import carried over, which brings no record): a later install
    // of the same package must record its `GraphDefinition` and revision as
    // adopted, not created, so removing that install leaves them alone (#1
    // in the review: `required_by=false` recording every graph as created
    // let removing a dependent delete a graph the operator already had).
    let (node, access, options) = fixture().await;
    let first = install_test_graph_package(&access, OWNER, "review_graph", &options)
        .await
        .unwrap();
    activate_graph_revision(
        &node,
        None,
        OWNER,
        &first.graph_id,
        &first.revision_digest,
        None,
    )
    .await
    .unwrap();

    // Forget the record directly (bypassing `remove_pack`, which would also
    // take the graph documents with it): the graph and its revision stay
    // live, exactly as an untracked or import-carried graph would be found.
    let doc_id = access
        .execute(&format!(
            r#"{{ PackInstallation(filter: {{ agent_did: {{ _eq: "{OWNER}" }} }}) {{ _docID }} }}"#
        ))
        .await
        .unwrap()["data"]["PackInstallation"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    access
        .write(
            "test.forget_pack_installation_record",
            &format!(
                r#"mutation {{ delete_PackInstallation(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}) {{ _docID }} }}"#
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        count(&access, "GraphDefinition", "agent_did", OWNER).await,
        1
    );
    assert_eq!(count(&access, "GraphRevision", "owner_did", OWNER).await, 1);

    // A reinstall of the identical package (standing in for a dependency
    // install landing on top of the untracked graph) must find and adopt
    // what is already there rather than materializing a new revision.
    let second =
        install_test_graph_package_explicit(&access, OWNER, "review_graph", &options, false)
            .await
            .unwrap();
    assert_eq!(second.revision_digest, first.revision_digest);
    assert_eq!(
        count(&access, "GraphRevision", "owner_did", OWNER).await,
        1,
        "the untracked revision is adopted, not duplicated"
    );

    let response = access
        .execute(&format!(
            r#"{{ PackInstallation(filter: {{ agent_did: {{ _eq: "{OWNER}" }} }}) {{ documents }} }}"#
        ))
        .await
        .unwrap();
    let documents = response["data"]["PackInstallation"][0]["documents"]
        .as_array()
        .unwrap();
    let graph_definition = documents
        .iter()
        .find(|document| document["collection"] == "GraphDefinition")
        .unwrap();
    assert_eq!(
        graph_definition["created"], false,
        "an untracked GraphDefinition this install did not create must be adopted: {documents:?}"
    );
    let revision = documents
        .iter()
        .find(|document| document["collection"] == "GraphRevision")
        .unwrap();
    assert_eq!(
        revision["created"], false,
        "an untracked GraphRevision this install did not create must be adopted: {documents:?}"
    );

    remove_pack(&access, OWNER, "fixture/review_graph", DriftPolicy::Refuse)
        .await
        .unwrap();
    assert_eq!(
        count(&access, "GraphDefinition", "agent_did", OWNER).await,
        1,
        "the operator's pre-existing graph must survive removal of the install that only adopted it"
    );
    assert_eq!(
        count(&access, "GraphRevision", "owner_did", OWNER).await,
        1,
        "its pre-existing revision must survive too"
    );
}

#[tokio::test]
async fn removal_reports_an_exact_truncated_count_past_the_retained_run_limit() {
    let (node, access, options) = fixture().await;
    let receipt = install_test_graph_package(&access, OWNER, "review_graph", &options)
        .await
        .unwrap();
    activate_graph_revision(
        &node,
        None,
        OWNER,
        &receipt.graph_id,
        &receipt.revision_digest,
        None,
    )
    .await
    .unwrap();

    // More terminal runs pinned to the removed revision than the retained-run
    // report's bound: the report must still name the exact total, not just
    // "more than N".
    let run_count = super::RETAINED_RUN_REPORT_LIMIT + 2;
    for index in 0..run_count {
        access
            .write(
                "test.seed_retained_run",
                &format!(
                    r#"mutation {{ create_GraphRun(input: {{run_id: "retained-{index}",
                        graph_id: "{graph_id}", owner_did: "{owner}",
                        revision_digest: "{digest}", status: "succeeded",
                        correlation: "retained-{index}", created_at: "2026-08-25T00:00:00Z"}}) {{_docID}} }}"#,
                    graph_id = escape_graphql_string(&receipt.graph_id),
                    owner = escape_graphql_string(OWNER),
                    digest = escape_graphql_string(&receipt.revision_digest),
                ),
            )
            .await
            .unwrap();
    }

    let report = remove_pack(&access, OWNER, "fixture/review_graph", DriftPolicy::Refuse)
        .await
        .unwrap();

    let named_runs = report
        .retained
        .iter()
        .filter(|retained| retained.item.starts_with("run retained-"))
        .count();
    assert_eq!(
        named_runs,
        super::RETAINED_RUN_REPORT_LIMIT,
        "{:?}",
        report.retained
    );
    let summary = report
        .retained
        .iter()
        .find(|retained| retained.reason.contains("truncated"))
        .unwrap_or_else(|| panic!("expected a truncated summary entry: {:?}", report.retained));
    assert!(
        summary.item.contains('2'),
        "the summary must name the exact remaining total: {summary:?}"
    );
}
