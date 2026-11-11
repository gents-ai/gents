use std::sync::Arc;

use defra_node::Node as EmbeddedNode;
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
    node.shutdown().await.expect("node shutdown");
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
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn filter_doc_id_and_limit_variables_bind_in_reads_and_writes() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    node.add_collection(
        "type FilterVariableProbe { key: String label: String enabled: Boolean count: Int }",
    )
    .await
    .unwrap();
    let hostile = "o\"ne\\ } ) { _docID } # \n雪";
    let mut doc_ids = Vec::new();
    for (key, count) in [(hostile, 1), ("two", 2), ("three", 3)] {
        let created = ConfigAccess::transact_local_write(&node, None, "filter_probe_create", |txn| {
            Box::pin(async move {
                txn.execute_with_variables(
                    "mutation($input: FilterVariableProbeMutationInputArg!) { create_FilterVariableProbe(input:$input) {_docID} }",
                    &json!({"input": {"key": key, "label": "before", "enabled": true, "count": count}}),
                )
                .await
            })
        })
        .await
        .unwrap();
        doc_ids
            .push(crate::store::graphql::created_doc_id(&created, "FilterVariableProbe").unwrap());
    }
    let read = |query: &'static str, variables: serde_json::Value| {
        let node = Arc::clone(&node);
        async move {
            ConfigAccess::transact_local_readonly(&node, None, "filter_probe_read", |txn| {
                let variables = variables.clone();
                Box::pin(async move { txn.execute_with_variables(query, &variables).await })
            })
            .await
            .unwrap_or_else(|error| panic!("{query} failed: {error}"))["data"]
                ["FilterVariableProbe"]
                .clone()
        }
    };

    let eq = read(
        "query($key: String) { FilterVariableProbe(filter:{key:{_eq:$key}}) {key count} }",
        json!({"key": hostile}),
    )
    .await;
    assert_eq!(eq, json!([{"key": hostile, "count": 1}]));

    let conjunction = read(
        "query($key: String, $enabled: Boolean, $count: Int) { FilterVariableProbe(filter:{_and:[{key:{_eq:$key}},{enabled:{_eq:$enabled}},{count:{_ge:$count}}]}) {key} }",
        json!({"key": "two", "enabled": true, "count": 2}),
    )
    .await;
    assert_eq!(conjunction, json!([{"key": "two"}]));

    let membership = read(
        "query($keys: [String!]) { FilterVariableProbe(filter:{key:{_in:$keys}}, order:{count:ASC}) {count} }",
        json!({"keys": [hostile, "three"]}),
    )
    .await;
    assert_eq!(membership, json!([{"count": 1}, {"count": 3}]));

    let by_doc_id = read(
        "query($id: ID!) { FilterVariableProbe(docID:$id) {key} }",
        json!({"id": doc_ids[1]}),
    )
    .await;
    assert_eq!(by_doc_id, json!([{"key": "two"}]));

    let doc_id_filter = read(
        "query($id: String) { FilterVariableProbe(filter:{_docID:{_eq:$id}}) {key} }",
        json!({"id": doc_ids[2]}),
    )
    .await;
    assert_eq!(doc_id_filter, json!([{"key": "three"}]));

    let limited = read(
        "query($limit: Int) { FilterVariableProbe(order:{count:DESC}, limit:$limit) {count} }",
        json!({"limit": 2}),
    )
    .await;
    assert_eq!(limited, json!([{"count": 3}, {"count": 2}]));

    let whole_filter = read(
        "query($filter: FilterVariableProbeFilterArg) { FilterVariableProbe(filter:$filter) {key} }",
        json!({"filter": {"count": {"_eq": 3}}}),
    )
    .await;
    assert_eq!(whole_filter, json!([{"key": "three"}]));

    let guarded = ConfigAccess::transact_local_write(&node, None, "filter_probe_update", |txn| {
        let id = doc_ids[0].clone();
        Box::pin(async move {
            let miss = txn
                .execute_with_variables(
                    "mutation($id: ID!, $label: String, $input: FilterVariableProbeMutationInputArg!) { update_FilterVariableProbe(docID:$id, filter:{label:{_eq:$label}}, input:$input) {_docID label} }",
                    &json!({"id": id, "label": "stale", "input": {"label": "wrong"}}),
                )
                .await?;
            let hit = txn
                .execute_with_variables(
                    "mutation($id: ID!, $label: String, $input: FilterVariableProbeMutationInputArg!) { update_FilterVariableProbe(docID:$id, filter:{label:{_eq:$label}}, input:$input) {_docID label} }",
                    &json!({"id": id, "label": "before", "input": {"label": "after"}}),
                )
                .await?;
            Ok::<_, ConfigClientError>((miss, hit))
        })
    })
    .await
    .unwrap();
    assert_eq!(guarded.0["data"]["update_FilterVariableProbe"], json!([]));
    assert_eq!(
        guarded.1["data"]["update_FilterVariableProbe"],
        json!([{"_docID": doc_ids[0], "label": "after"}])
    );

    use gents_protocol::document_query::{
        kind, Collection, Direction, Field, Filter, Input, Query, Selection, Update,
    };
    enum Probe {}
    impl Collection for Probe {
        const NAME: &'static str = "FilterVariableProbe";
    }
    const KEY: Field<Probe, kind::Str> = Field::new("key");
    const LABEL: Field<Probe, kind::Str> = Field::new("label");
    const COUNT: Field<Probe, kind::Int> = Field::new("count");

    let newest = Query::select(Selection::new().field(KEY).field(LABEL))
        .filter(Filter::any_of(KEY, [hostile, "three"]))
        .filter(Filter::ge(COUNT, 1))
        .filter(Filter::not_null(LABEL))
        .order_by(COUNT, Direction::Desc)
        .limit(2)
        .render();
    let autocommit = crate::store::graphql::read_rendered(&node, &newest, "typed_probe_read")
        .await
        .unwrap();
    assert_eq!(
        autocommit.data.unwrap()["FilterVariableProbe"],
        json!([{"key": "three", "label": "before"}, {"key": hostile, "label": "after"}])
    );

    let update = Update::matching(
        Filter::eq(KEY, hostile).and(Filter::eq(LABEL, "after")),
        Input::new().set(LABEL, "typed").set(COUNT, 9),
    )
    .returning(Selection::new().field(LABEL).field(COUNT))
    .render();
    let updated = ConfigAccess::transact_local_write(&node, None, "typed_probe_update", |txn| {
        let update = update.clone();
        Box::pin(async move { txn.run(&update).await })
    })
    .await
    .unwrap();
    assert_eq!(
        updated["data"]["update_FilterVariableProbe"],
        json!([{"label": "typed", "count": 9}])
    );
    node.shutdown().await.expect("node shutdown");
}
