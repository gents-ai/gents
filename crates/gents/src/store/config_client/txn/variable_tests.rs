use std::sync::Arc;

use defra_node::EmbeddedNode;
use serde_json::json;

use super::*;

#[tokio::test]
async fn whole_input_variables_update_upsert_and_rollback_losslessly() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    node.add_collection("type MutationVariableProbe { key: String label: String enabled: Boolean count: Int tags: [String] payload: JSON }").await.unwrap();
    let payload = json!({"a-b": [], "nested": [{"": {}, "雪": [null, []]}]});
    for (query, input) in [
        (
            "mutation($input: MutationVariableProbeMutationInputArg!) { create_MutationVariableProbe(input:$input) {_docID} }",
            json!({"key":"one","label":"before","payload":null}),
        ),
        (
            "mutation($input: MutationVariableProbeMutationInputArg!) { update_MutationVariableProbe(filter:{key:{_eq:\"one\"}},input:$input) {_docID} }",
            json!({"label":"updated","enabled":true,"count":7,"tags":["tag"],"payload":payload}),
        ),
        (
            "mutation($input: MutationVariableProbeMutationInputArg!) { upsert_MutationVariableProbe(filter:{key:{_eq:\"one\"}},add:$input,update:$input) {_docID} }",
            json!({"label":"upserted","tags":null,"payload":payload}),
        ),
    ] {
        let vars = json!({"input": input});
        ConfigAccess::transact_local_write(&node, None, "variable_probe", |txn| {
            let vars = vars.clone();
            Box::pin(async move { txn.execute_with_variables(query, &vars).await.map(|_| ()) })
        })
        .await
        .unwrap();
    }
    let read = node
        .execute("{MutationVariableProbe(filter:{key:{_eq:\"one\"}}) {key label enabled count tags payload}}")
        .await;
    assert!(!read.has_errors(), "{:?}", read.errors);
    let before = read.data.unwrap()["MutationVariableProbe"][0].clone();
    assert_eq!(before["label"], "upserted");
    assert_eq!(before["enabled"], true);
    assert_eq!(before["count"], 7);
    assert!(before["tags"].is_null());
    assert_eq!(before["payload"], payload);
    ConfigAccess::transact_local_write(&node, None, "variable_upsert_insert", |txn| Box::pin(async move {
        txn.execute_with_variables("mutation($input: MutationVariableProbeMutationInputArg!) { upsert_MutationVariableProbe(filter:{key:{_eq:\"two\"}},add:$input,update:$input) {_docID} }", &json!({"input":{"key":"two","label":"inserted","payload":{"empty":[]}}})).await.map(|_| ())
    })).await.unwrap();
    let inserted = node
        .execute("{MutationVariableProbe(filter:{key:{_eq:\"two\"}}) {label payload}}")
        .await;
    assert!(!inserted.has_errors());
    assert_eq!(
        inserted.data.unwrap()["MutationVariableProbe"],
        json!([{"label":"inserted","payload":{"empty":[]}}])
    );
    let aborted: Result<(), ConfigClientError> = ConfigAccess::transact_local_write(&node, None, "variable_probe_rollback", |txn| Box::pin(async move {
        txn.execute_with_variables("mutation($input: MutationVariableProbeMutationInputArg!) { update_MutationVariableProbe(filter:{key:{_eq:\"one\"}},input:$input) {_docID} }", &json!({"input":{"label":"must rollback","payload":{"lost":[]}}})).await?;
        Err(ConfigClientError::InjectedRollbackFailure)
    })).await;
    assert!(aborted.is_err());
    let read = node
        .execute("{MutationVariableProbe(filter:{key:{_eq:\"one\"}}) {key label enabled count tags payload}}")
        .await;
    assert!(!read.has_errors());
    assert_eq!(read.data.unwrap()["MutationVariableProbe"][0], before);
    node.shutdown().await;
}

#[tokio::test]
async fn embedded_transaction_preserves_json_variables_and_rollback() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    node.add_collection("type TransactionJsonProbe { key: String payload: JSON }")
        .await
        .unwrap();
    let variables = json!({"payload": {"a-b": [], "nested": [{"": {}, "雪": [null, []]}]}});
    let txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
    txn.execute_with_variables("mutation($payload: JSON) { create_TransactionJsonProbe(input: {key: \"rolled-back\", payload: $payload}) {_docID} }", &variables).await.unwrap();
    let read = txn
        .execute("{TransactionJsonProbe {payload}}")
        .await
        .unwrap();
    assert_eq!(
        read["data"]["TransactionJsonProbe"][0]["payload"],
        variables["payload"]
    );
    txn.discard().await.unwrap();
    let read = node.execute("{TransactionJsonProbe {payload}}").await;
    assert!(!read.has_errors());
    assert!(read.data.unwrap()["TransactionJsonProbe"]
        .as_array()
        .unwrap()
        .is_empty());
    node.shutdown().await;
}
