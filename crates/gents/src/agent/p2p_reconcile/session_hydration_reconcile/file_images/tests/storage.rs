use super::*;
use crate::identity::KeyIdentity;
use serde_json::{json, Value};

async fn fixture() -> (
    tempfile::TempDir,
    GraphqlHydrationStore,
    ImageDemandRow,
    String,
) {
    let directory = tempfile::tempdir().unwrap();
    let identity: Arc<dyn NodeIdentity> =
        Arc::new(KeyIdentity::load_or_create(directory.path().join("source.key"), None).unwrap());
    let requester = KeyIdentity::load_or_create(directory.path().join("client.key"), None).unwrap();
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(directory.path().join("store"))
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let (spec, _) = super::fixture(FileImageSelection::Missing, vec![]);
    let mut demand = spec.demand.demand;
    demand.admission.node_did = identity.did().into();
    demand.admission.requester_did = requester.did().into();
    let fields = demand
        .reference_fields()
        .into_iter()
        .map(|(key, value)| format!("{key}: \"{}\"", escape_graphql_string(value)))
        .collect::<Vec<_>>()
        .join(", ");
    let response = ConfigAccess::write_local(
        &node,
        "test.file_image_storage.admit",
        &format!(
            r#"mutation {{ create_FileImageHydrationRequest(input: {{
            {fields}, created_at: "2026-10-20T00:00:00Z", status: "pending"
        }}) {{ _docID }} }}"#
        ),
    )
    .await
    .unwrap();
    let document_id =
        crate::graphql::created_doc_id(&response, "FileImageHydrationRequest").unwrap();
    let store = GraphqlHydrationStore {
        node,
        enrollment: crate::agent::p2p_reconcile::enrollment_authority_channel().1,
        identity,
    };
    (
        directory,
        store,
        ImageDemandRow {
            document_id,
            demand,
        },
        requester.did().into(),
    )
}

async fn journal(store: &GraphqlHydrationStore) -> Value {
    ConfigAccess::local(store.node.clone())
        .execute("{ FileImageHydrationReceipt { _docID request_key node_did receipt } }")
        .await
        .unwrap()
}

#[tokio::test]
async fn receipt_projection_failure_rolls_back_the_immutable_outcome() {
    for failure_at in [1, 2] {
        let (_directory, store, row, _) = fixture().await;
        let (result, writes) =
            crate::config_client::ConfigApplyTxn::with_successful_mutation_failure_at(
                Some(failure_at),
                store.commit(&row, &FileImageSelection::Missing, ""),
            )
            .await;
        assert!(result.is_err());
        assert_eq!(writes, failure_at);
        assert!(journal(&store).await["data"]["FileImageHydrationReceipt"]
            .as_array()
            .unwrap()
            .is_empty());
        let projected = ConfigAccess::local(store.node.clone())
            .execute("{ FileImageHydrationRequest { status outcome_signature } }")
            .await
            .unwrap();
        assert_eq!(
            projected["data"]["FileImageHydrationRequest"][0],
            json!({
                "status": "pending", "outcome_signature": null,
            })
        );
        store
            .commit(&row, &FileImageSelection::Missing, "")
            .await
            .unwrap();
        assert_eq!(
            store.prior(&row).await.unwrap().unwrap().selection,
            FileImageSelection::Missing
        );
        store.node.shutdown().await;
    }
}

#[tokio::test]
async fn the_requester_cannot_read_or_delete_the_source_receipt() {
    let (_directory, store, row, requester) = fixture().await;
    store
        .commit(&row, &FileImageSelection::Missing, "")
        .await
        .unwrap();
    let before = journal(&store).await;
    let id = before["data"]["FileImageHydrationReceipt"][0]["_docID"]
        .as_str()
        .unwrap();
    let mutation = format!(
        r#"mutation {{ delete_FileImageHydrationReceipt(docID: "{}") {{ _docID }} }}"#,
        escape_graphql_string(id),
    );
    let hidden = ConfigAccess::transact_local_write(
        &store.node,
        Some(requester.parse().unwrap()),
        "test.file_image_storage.foreign_read",
        |txn| {
            Box::pin(async move {
                txn.execute("{ FileImageHydrationReceipt { _docID } }")
                    .await
            })
        },
    )
    .await
    .unwrap();
    assert!(hidden["data"]["FileImageHydrationReceipt"]
        .as_array()
        .unwrap()
        .is_empty());
    let _result = ConfigAccess::transact_local_write(
        &store.node,
        Some(requester.parse().unwrap()),
        "test.file_image_storage.foreign_delete",
        |txn| {
            let mutation = mutation.clone();
            Box::pin(async move { txn.execute(&mutation).await })
        },
    )
    .await;
    assert_eq!(journal(&store).await, before);
    let change = format!(
        r#"mutation {{ update_FileImageHydrationReceipt(docID: "{}", input: {{ receipt: null }}) {{ _docID }} }}"#,
        escape_graphql_string(id),
    );
    assert!(
        ConfigAccess::write_local(&store.node, "test.file_image_storage.immutable", &change)
            .await
            .is_err()
    );
    assert_eq!(journal(&store).await, before);
    store.node.shutdown().await;
}
