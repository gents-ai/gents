//! Spike/regression coverage for rendered-request projection (#519).
//!
//! Rendered completion requests should be reconstructable as projections from
//! durable database state. This test confirms that the pinned DefraDB node
//! accepts CID time-travel GraphQL through `EmbeddedNode::execute`, which lets a
//! future projection read behavior/config documents as they existed when the
//! model call was made.

use std::sync::Arc;

use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
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
        .prefix("defra-agent-time-travel-probe-")
        .tempdir()
        .expect("tempdir");
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(tempdir.path())
            .build()
            .await
            .expect("embedded node"),
    );
    node.add_schema(PROBE_SCHEMA)
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
