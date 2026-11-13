use super::*;
use std::sync::Arc;

#[test]
fn canonical_counts_follow_the_count_carrier_model() {
    gents_lean_vocab::families::count_carrier_value::replay(|case| {
        let wire = match case.value.kind.as_str() {
            "text" => Value::String(case.value.spelling.clone()),
            _ => serde_json::from_str(&case.value.spelling).unwrap(),
        };
        canonical_positive_count(&wire, case.maximum as usize).map(|count| count as u64)
    });
}

#[test]
fn mutation_documents_preserves_missing_null_alias_and_error_handling() {
    let missing = serde_json::json!({"data": {}});
    assert!(mutation_documents(&missing, "create_AgentRequest")
        .unwrap()
        .is_none());
    let null = serde_json::json!({"data": {"create_AgentRequest": null}});
    assert!(mutation_documents(&null, "create_AgentRequest").is_err());
    let both = serde_json::json!({"data": {
        "create_AgentRequest": {"_docID": "a"},
        "add_AgentRequest": {"_docID": "b"}
    }});
    assert!(mutation_documents(&both, "create_AgentRequest").is_err());
    let errors = serde_json::json!({
        "errors": [{"message": "failed"}],
        "data": {"add_AgentRequest": {"_docID": "a"}}
    });
    assert!(mutation_documents(&errors, "create_AgentRequest").is_err());

    let multi = QueryResponse::success(serde_json::json!({
        "create_AgentRequest": [{"_docID": "a"}, {"_docID": "b"}]
    }));
    assert!(single_mutation_document(&multi, "create_AgentRequest").is_err());
}

#[test]
fn single_mutation_document_normalizes_create_to_add_response_key() {
    let response = QueryResponse::success(serde_json::json!({
        "add_AgentRequest": [{ "_docID": "doc-1" }]
    }));

    let document = single_mutation_document(&response, "create_AgentRequest")
        .unwrap()
        .expect("normalized mutation document");
    assert_eq!(document["_docID"], "doc-1");
}

#[test]
fn created_doc_id_uses_the_shared_single_document_contract() {
    for field in ["create_AgentOutputSegment", "add_AgentOutputSegment"] {
        for value in [
            serde_json::json!({"_docID": "physical"}),
            serde_json::json!([{"_docID": "physical"}]),
        ] {
            let response = serde_json::json!({"data": {field: value}});
            assert_eq!(
                created_doc_id(&response, "AgentOutputSegment").unwrap(),
                "physical"
            );
        }
    }
    for response in [
        serde_json::json!({"data": {}}),
        serde_json::json!({"data": {"add_AgentOutputSegment": []}}),
        serde_json::json!({"data": {"add_AgentOutputSegment": [{"_docID": "one"}, {"_docID": "two"}]}}),
        serde_json::json!({"data": {"add_AgentOutputSegment": {"_docID": " "}}}),
        serde_json::json!({"data": {"add_AgentOutputSegment": {"_docID": "one"}, "create_AgentOutputSegment": {"_docID": "one"}}}),
        serde_json::json!({"errors": ["failed"], "data": {"add_AgentOutputSegment": {"_docID": "one"}}}),
    ] {
        assert!(
            created_doc_id(&response, "AgentOutputSegment").is_err(),
            "{response}"
        );
    }
}

#[test]
fn mutation_write_ledger_counts_returned_documents_not_just_calls() {
    let response = QueryResponse::success(serde_json::json!({
        "upsert_One": { "_docID": "doc-1" },
        "delete_Many": [
            { "_docID": "doc-2" },
            { "_docID": "doc-3" }
        ],
        "delete_None": []
    }));
    assert_eq!(mutation_affected_documents(&response), 3);

    let no_match = QueryResponse::success(serde_json::json!({ "delete_None": [] }));
    assert_eq!(mutation_affected_documents(&no_match), 0);
}

#[test]
fn single_mutation_document_rejects_duplicate_normalized_keys() {
    let response = QueryResponse::success(serde_json::json!({
        "create_AgentRequest": { "_docID": "doc-create" },
        "add_AgentRequest": [{ "_docID": "doc-add" }]
    }));

    let error = single_mutation_document(&response, "create_AgentRequest").unwrap_err();
    assert!(error.to_string().contains("returned both"));
}

#[test]
fn mutation_composite_version_rejects_concurrent_newest_heads() {
    let response = QueryResponse::success(serde_json::json!({
        "update_AgentRequest": [{
            "_version": [
                { "cid": "bafy-head-a", "height": 7, "fieldName": "_C" },
                { "cid": "bafy-head-b", "height": 7, "fieldName": "_C" },
                { "cid": "bafy-parent", "height": 6, "fieldName": "_C" }
            ]
        }]
    }));

    let error = mutation_composite_version(&response, "update_AgentRequest").unwrap_err();
    assert!(error.to_string().contains("document version is ambiguous"));
}

#[test]
fn document_composite_version_sorts_and_never_falls_back_to_field_commits() {
    let document = serde_json::json!({
        "_version": [
            { "cid": "bafy-old", "height": 2, "fieldName": "_C" },
            { "cid": "bafy-field", "height": 9, "fieldName": "content" },
            { "cid": "bafy-new", "height": 4, "fieldName": "_C" }
        ]
    });
    let commit = document_composite_version(&document, "AgentMessage boundary")
        .unwrap()
        .expect("composite commit");
    assert_eq!(commit.cid, "bafy-new");

    let field_only = serde_json::json!({
        "_version": [{ "cid": "bafy-field", "height": 9, "fieldName": "content" }]
    });
    assert!(
        document_composite_version(&field_only, "AgentMessage boundary")
            .unwrap()
            .is_none()
    );
}

#[test]
fn mutation_composite_version_accepts_unique_newest_composite() {
    let response = QueryResponse::success(serde_json::json!({
        "add_AgentRequest": [{
            "_version": [
                { "cid": "bafy-field", "height": 8, "fieldName": "status" },
                { "cid": "bafy-new", "height": 8, "fieldName": "_C" },
                { "cid": "bafy-old", "height": 7, "fieldName": "_C" }
            ]
        }]
    }));

    let commit = mutation_composite_version(&response, "create_AgentRequest")
        .unwrap()
        .expect("composite commit");
    assert_eq!(commit.cid, "bafy-new");
}

#[tokio::test]
async fn concurrent_mutations_share_the_node_write_path() {
    const WRITES: usize = 32;

    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    node.add_collection("type GateWrite { write_id: String }")
        .await
        .unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(WRITES));
    let mut tasks = tokio::task::JoinSet::new();

    for index in 0..WRITES {
        let node = Arc::clone(&node);
        let barrier = Arc::clone(&barrier);
        tasks.spawn(async move {
            barrier.wait().await;
            let mutation = format!(
                r#"mutation {{ create_GateWrite(input: {{ write_id: "write-{index}" }}) {{ _docID }} }}"#
            );
            crate::config_client::ConfigAccess::write_local(
                &node,
                "test.concurrent_write",
                &mutation,
            )
            .await
        });
    }

    while let Some(result) = tasks.join_next().await {
        result
            .expect("concurrent mutation task panicked")
            .expect("concurrent mutation exhausted the shared write path");
    }

    let response = graphql_with_transaction_retry(&node, "{ GateWrite { write_id } }", "count")
        .await
        .unwrap();
    assert_eq!(rows::<Value>(&response, "GateWrite").unwrap().len(), WRITES);
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn public_graphql_read_helpers_reject_mutations_before_execution() {
    let node = EmbeddedNode::builder().build().await.unwrap();
    node.add_collection("type ReadBoundaryFact { value: String }")
        .await
        .unwrap();
    let mutation = r#"mutation { create_ReadBoundaryFact(input: {value: "bypass"}) { _docID } }"#;

    assert!(
        graphql_with_transaction_retry(&node, mutation, "read boundary")
            .await
            .is_err()
    );
    assert!(
        graphql_response_with_transaction_retry(&node, mutation, "raw read boundary")
            .await
            .is_err()
    );

    let response = node.execute("{ ReadBoundaryFact { value } }").await;
    assert_eq!(
        response.data.unwrap()["ReadBoundaryFact"],
        serde_json::json!([])
    );
    node.shutdown().await.expect("node shutdown");
}

use crate::graphql::escape_graphql_string;
use defra_node::Node as EmbeddedNode;
use serde_json::Value;
use tempfile::TempDir;

const PROBE_SCHEMA: &str = r#"
type RenderedTimeTravelProbe {
    probe_id: String @index(unique: true)
    label: String
}
"#;

struct ProbeDb {
    node: Arc<EmbeddedNode>,
    _tempdir: TempDir,
}

async fn probe_db() -> ProbeDb {
    let tempdir = tempfile::Builder::new()
        .prefix("gents-time-travel-probe-")
        .tempdir()
        .expect("tempdir");
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(tempdir.path())
            .build()
            .await
            .expect("embedded node"),
    );
    node.add_collection(PROBE_SCHEMA)
        .await
        .expect("add probe schema");
    ProbeDb {
        node,
        _tempdir: tempdir,
    }
}

#[tokio::test]
async fn node_execute_supports_cid_time_travel_projection_reads() {
    let db = probe_db().await;
    let node = db.node;

    let created = node
        .execute(
            r#"mutation {
                create_RenderedTimeTravelProbe(input: {
                    probe_id: "rendered-request-config",
                    label: "v1"
                }) {
                    _docID
                    probe_id
                    label
                    _version {
                        cid
                        height
                        fieldName
                    }
                }
            }"#,
        )
        .await;
    assert!(!created.has_errors(), "create failed: {:?}", created.errors);
    let created_data = created.data.expect("create data");
    let doc_id = first_str(&created_data, "add_RenderedTimeTravelProbe", "_docID");
    let doc_id = doc_id.to_string();
    let historical_cid =
        first_composite_version_cid(&created_data, "add_RenderedTimeTravelProbe").to_string();

    assert_eq!(
        composite_commit_cid(&node, &doc_id).await,
        historical_cid,
        "_version should expose the same composite commit CID as _commits"
    );

    let escaped_doc_id = escape_graphql_string(&doc_id);
    let update = format!(
        r#"mutation {{
            update_RenderedTimeTravelProbe(
                docID: "{escaped_doc_id}",
                input: {{ label: "v2" }}
            ) {{ _docID label }}
        }}"#
    );
    let updated = node.execute(&update).await;
    assert!(!updated.has_errors(), "update failed: {:?}", updated.errors);

    let latest = node
        .execute(
            r#"query {
                RenderedTimeTravelProbe(
                    filter: { probe_id: { _eq: "rendered-request-config" } }
                ) { probe_id label }
            }"#,
        )
        .await;
    assert!(
        !latest.has_errors(),
        "latest read failed: {:?}",
        latest.errors
    );
    assert_eq!(
        first_str(
            &latest.data.expect("latest data"),
            "RenderedTimeTravelProbe",
            "label"
        ),
        "v2",
        "latest read should see the updated document"
    );

    let escaped_cid = escape_graphql_string(&historical_cid);
    let historical_query = format!(
        r#"query {{
            RenderedTimeTravelProbe(cid: ["{escaped_cid}"]) {{
                probe_id
                label
            }}
        }}"#
    );
    let historical = node.execute(&historical_query).await;
    assert!(
        !historical.has_errors(),
        "historical read failed: {:?}",
        historical.errors
    );
    assert_eq!(
        first_str(
            &historical.data.expect("historical data"),
            "RenderedTimeTravelProbe",
            "label"
        ),
        "v1",
        "CID time-travel read should return the document as of the captured commit"
    );
}

async fn composite_commit_cid(node: &EmbeddedNode, doc_id: &str) -> String {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let commits_query = format!(
        r#"query {{
            _commits(
                docID: ["{escaped_doc_id}"],
                filter: {{ fieldName: {{ _eq: "_C" }} }}
            ) {{
                cid
                height
                docID
                fieldName
            }}
        }}"#
    );
    let commits = node.execute(&commits_query).await;
    assert!(
        !commits.has_errors(),
        "commit query failed: {:?}",
        commits.errors
    );
    let rows = commits
        .data
        .as_ref()
        .and_then(|data| data.get("_commits"))
        .and_then(Value::as_array)
        .expect("_commits rows");
    rows.iter()
        .find(|row| row["height"].as_u64() == Some(1))
        .or_else(|| rows.first())
        .and_then(|row| row["cid"].as_str())
        .expect("composite commit cid")
        .to_string()
}

fn first_str<'a>(data: &'a Value, collection: &str, field: &str) -> &'a str {
    data.get(collection)
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get(field))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing {collection}.{field}: {data}"))
}

fn first_composite_version_cid<'a>(data: &'a Value, collection: &str) -> &'a str {
    data.get(collection)
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_version"))
        .and_then(Value::as_array)
        .and_then(|versions| {
            versions
                .iter()
                .find(|version| version["fieldName"].as_str() == Some("_C"))
                .or_else(|| versions.first())
        })
        .and_then(|version| version["cid"].as_str())
        .unwrap_or_else(|| panic!("missing {collection}._version.cid: {data}"))
}

const TEXT_PROBE_SCHEMA: &str = r#"
type TextVersionProbe {
    probe_id: String @index(unique: true)
    text: String @crdt(type: text)
}
"#;

#[tokio::test]
async fn text_crdt_reads_each_revision_at_its_cid() {
    let db = probe_db().await;
    let node = db.node;
    node.add_collection(TEXT_PROBE_SCHEMA)
        .await
        .expect("add text probe schema");

    let revisions = [
        "fn main() {}\n",
        "fn main() {\n    println!(\"hi\");\n}\n",
        "// header\nfn main() {\n    println!(\"hi\");\n}\n",
        "// header\nfn main() {\n    tracing::info!(\"hi \u{e9}\u{1f600}\");\n}\n",
        "",
        "rewritten\n",
    ];
    let create = format!(
        r#"mutation {{
            row: create_TextVersionProbe(input: {{ probe_id: "file", text: "{}" }}) {{
                _docID
                _version {{ cid fieldName }}
            }}
        }}"#,
        escape_graphql_string(revisions[0])
    );
    let created = node.execute(&create).await;
    assert!(!created.has_errors(), "create failed: {:?}", created.errors);
    let created = created.data.expect("create data");
    let doc_id = first_str(&created, "row", "_docID").to_string();
    let mut cids = vec![first_composite_version_cid(&created, "row").to_string()];

    for revision in &revisions[1..] {
        let update = format!(
            r#"mutation {{
                row: update_TextVersionProbe(docID: "{}", input: {{ text: "{}" }}) {{
                    _version {{ cid fieldName }}
                }}
            }}"#,
            escape_graphql_string(&doc_id),
            escape_graphql_string(revision)
        );
        let updated = node.execute(&update).await;
        assert!(!updated.has_errors(), "update failed: {:?}", updated.errors);
        cids.push(
            first_composite_version_cid(&updated.data.expect("update data"), "row").to_string(),
        );
    }

    for (cid, expected) in cids.iter().zip(revisions) {
        let read = node
            .execute(&format!(
                r#"query {{ TextVersionProbe(cid: ["{}"]) {{ text }} }}"#,
                escape_graphql_string(cid)
            ))
            .await;
        assert!(
            !read.has_errors(),
            "historical read failed: {:?}",
            read.errors
        );
        assert_eq!(
            first_str(
                &read.data.expect("historical data"),
                "TextVersionProbe",
                "text"
            ),
            expected,
            "revision at {cid}"
        );
    }
}

#[tokio::test]
async fn text_crdt_versions_written_in_one_transaction_stay_readable() {
    use crate::config_client::{ConfigAccess, IdempotentTransactionRetry};

    let db = probe_db().await;
    let node = db.node;
    node.add_collection(TEXT_PROBE_SCHEMA)
        .await
        .expect("add text probe schema");
    let created = node
        .execute(r#"mutation { row: create_TextVersionProbe(input: { probe_id: "txn", text: "one\n" }) { _docID } }"#)
        .await;
    assert!(!created.has_errors(), "create failed: {:?}", created.errors);
    let doc_id = first_str(&created.data.expect("create data"), "row", "_docID").to_string();

    let revisions = ["one\ntwo\n", "zero\none\ntwo\n"];
    let cids = ConfigAccess::transact_local_idempotent(
        &node,
        None,
        IdempotentTransactionRetry::Standard,
        "test.text_versions_in_one_txn",
        |txn| {
            let doc_id = doc_id.clone();
            Box::pin(async move {
                let mut cids = Vec::new();
                for revision in revisions {
                    let updated = txn
                        .execute(&format!(
                            r#"mutation {{ row: update_TextVersionProbe(docID: "{}", input: {{ text: "{}" }}) {{ _version {{ cid fieldName }} }} }}"#,
                            escape_graphql_string(&doc_id),
                            escape_graphql_string(revision)
                        ))
                        .await?;
                    cids.push(first_composite_version_cid(&updated["data"], "row").to_string());
                }
                Ok::<_, crate::config_client::ConfigClientError>(cids)
            })
        },
    )
    .await
    .expect("transaction");
    assert_ne!(
        cids[0], cids[1],
        "each update in one transaction is its own version"
    );

    for (cid, expected) in cids.iter().zip(revisions) {
        let read = node
            .execute(&format!(
                r#"query {{ TextVersionProbe(cid: ["{}"]) {{ text }} }}"#,
                escape_graphql_string(cid)
            ))
            .await;
        assert!(
            !read.has_errors(),
            "historical read failed: {:?}",
            read.errors
        );
        assert_eq!(
            first_str(
                &read.data.expect("historical data"),
                "TextVersionProbe",
                "text"
            ),
            expected,
            "revision at {cid}"
        );
    }
}
