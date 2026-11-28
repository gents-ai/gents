use gents_protocol::schemas;

use super::common::fresh_node;
use crate::schema::catalog::{check_lineages, ensure_runtime, SchemaError};

/// A collection an earlier build registered before its current fields existed
/// is on an older lineage: preflight and startup refuse it alike, and neither
/// its schema nor its rows change.
#[tokio::test]
async fn a_collection_from_before_its_current_fields_is_refused_without_conversion() {
    let node = fresh_node().await;
    let ids = schemas::HomePolicyIds::node_store();
    let sdl = ids.render(schemas::TASK);
    let older = sdl
        .lines()
        .filter(|line| !line.trim_start().starts_with("outcome_readers:"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_ne!(older, sdl);
    node.add_collection(&older).await.unwrap();
    let created = node
        .execute(
            r#"mutation {create_Task(input: {task_id: "retained", node_did: "owner"}) {_docID}}"#,
        )
        .await;
    assert!(!created.has_errors(), "{created:?}");
    let query = "{Task {_docID}}";
    let retained = node.execute(query).await.data;
    let before = node.get_all_collection_versions().await.unwrap();
    assert!(matches!(
        check_lineages(&before, ids),
        Err(SchemaError::UnknownLineage { ref collection, .. }) if collection == schemas::TASK_NAME
    ));
    assert!(matches!(
        ensure_runtime(node.as_ref()).await,
        Err(SchemaError::UnknownLineage { ref collection, .. }) if collection == schemas::TASK_NAME
    ));
    assert_eq!(
        serde_json::to_value(&before).unwrap(),
        serde_json::to_value(node.get_all_collection_versions().await.unwrap()).unwrap(),
    );
    assert_eq!(node.execute(query).await.data, retained);
    assert!(node.get_collection(schemas::NODE_NAME).unwrap().is_none());
    node.shutdown().await.expect("shut down test node");
}
