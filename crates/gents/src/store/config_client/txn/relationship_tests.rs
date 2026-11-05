use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};

use axum::{
    extract::State,
    http::{HeaderMap, Method},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};

use super::{ConfigAccess, ConfigApplyTxn, GraphqlEndpoint, TxnBackend};

#[tokio::test]
async fn http_relationships_and_ownership_use_the_transaction_actor() {
    type Requests = Arc<Mutex<Vec<(Method, HeaderMap, Value)>>>;
    async fn relationship(
        State(requests): State<Requests>,
        method: Method,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        requests.lock().unwrap().push((method, headers, body));
        Json(json!({}))
    }
    async fn ownership(
        State(requests): State<Requests>,
        method: Method,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let malformed = body["doc_ids"] == json!(["malformed"]);
        requests.lock().unwrap().push((method, headers, body));
        if malformed {
            return Json(json!({"documents": [
                {"doc_id": "malformed", "visible": false, "is_owner": true}
            ]}));
        }
        Json(json!({"documents": [
            {"doc_id": "owned", "visible": true, "is_owner": true},
            {"doc_id": "other", "visible": true, "is_owner": false},
            {"doc_id": "hidden", "visible": false, "is_owner": false}
        ]}))
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
    let requests: Requests = Arc::default();
    let app = Router::new()
        .route(
            "/api/v0/acp/document/relationship",
            post(relationship).delete(relationship),
        )
        .route("/api/v0/acp/document/is-owner", post(ownership))
        .with_state(requests.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let actor = identity::Did::new("did:key:actor").unwrap();
    let other = identity::Did::new("did:key:other").unwrap();
    let endpoint =
        GraphqlEndpoint::with_authenticated_delegation(url, "Bearer signed-token", actor.clone())
            .unwrap();
    let txn = ConfigApplyTxn {
        backend: TxnBackend::Http {
            endpoint: &endpoint,
            id: "42".into(),
            client: reqwest::Client::new(),
        },
        rollback_on_drop: None,
        affected_documents: AtomicU64::new(0),
        operation: None,
        contends_with_request_siblings: AtomicBool::new(false),
        relationship_grants: Mutex::new(Vec::new()),
        publication_observer: None,
    };
    txn.add_resource_relationship("Agent", "owned", "parent", "nodes", "node")
        .await
        .unwrap();
    txn.add_actor_relationship("Agent", "owned", "reader", &other)
        .await
        .unwrap();
    txn.delete_actor_relationship("Agent", "owned", "reader", &other)
        .await
        .unwrap();
    let ids = vec!["owned".into(), "other".into(), "hidden".into()];
    let owners = txn.document_owners("Agent", &ids).await.unwrap();
    assert_eq!(owners[0].owner.as_ref(), Some(&actor));
    assert!(owners[0].visible);
    assert!(owners[1].visible && owners[1].owner.is_none());
    assert!(!owners[2].visible && owners[2].owner.is_none());
    assert!(txn.document_owners_as(&other, "Agent", &ids).await.is_err());
    assert!(matches!(
        txn.document_owners("Agent", &["malformed".into()]).await,
        Err(super::ConfigClientError::InvalidOwnerObservation)
    ));
    let entries = requests.lock().unwrap();
    assert_eq!(entries.len(), 5);
    assert_eq!(entries[0].2["target_actor"], "nodes:\"node\"");
    assert_eq!(entries[1].2["target_actor"], other.as_str());
    assert_eq!(entries[2].0, Method::DELETE);
    assert_eq!(entries[3].2, json!({"collection":"Agent", "doc_ids": ids}));
    for (_, headers, _) in entries.iter() {
        assert_eq!(headers["authorization"], "Bearer signed-token");
        assert_eq!(headers["x-defradb-tx"], "42");
    }
    server.abort();
}

#[tokio::test]
async fn embedded_relationship_grants_and_revocations_follow_transaction_finality() {
    const OWNER: &str = "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK";
    const READER: &str = "did:key:z6MkfXG2FkNy3u7Eg3jm8e2YQpGz7Z1JqWgHDAP1hLk9r2bR";
    let node = Arc::new(defra_node::Node::builder().build().await.unwrap());
    let policy = node.add_dac_policy_as(
        &defra_core::RequestContext::default().with_identity(Some(OWNER.to_owned())),
        "name: TxnRelationships\nresources:\n  - name: rows\n    relations:\n      - name: reader\n    permissions:\n      - name: read\n        expr: reader\n      - name: update\n      - name: delete\n",
    ).await.unwrap();
    node.add_collection(&format!(
        "type TxnRelationshipRow @policy(id: \"{policy}\", resource: \"rows\") {{ value: String }}"
    ))
    .await
    .unwrap();
    let owner = identity::Did::new(OWNER).unwrap();
    let reader = identity::Did::new(READER).unwrap();
    let created = ConfigAccess::local_as(node.clone(), owner.clone())
        .write(
            "test.txn_relationship_create",
            "mutation { add_TxnRelationshipRow(input: {value: \"secret\"}) { _docID } }",
        )
        .await
        .unwrap();
    let doc = created["data"]["add_TxnRelationshipRow"][0]["_docID"]
        .as_str()
        .unwrap();
    let reader_access = ConfigAccess::local_as(node.clone(), reader.clone());
    for commit in [false, true] {
        let txn = ConfigApplyTxn::begin_local(&node, Some(owner.clone()))
            .await
            .unwrap();
        txn.add_actor_relationship("TxnRelationshipRow", doc, "reader", &reader)
            .await
            .unwrap();
        let visible = reader_access
            .execute("{ TxnRelationshipRow { _docID } }")
            .await
            .unwrap();
        assert!(visible["data"]["TxnRelationshipRow"]
            .as_array()
            .unwrap()
            .is_empty());
        if commit {
            txn.commit().await.unwrap();
        } else {
            txn.discard().await.unwrap();
        }
    }
    let visible = reader_access
        .execute("{ TxnRelationshipRow { _docID } }")
        .await
        .unwrap();
    assert_eq!(
        visible["data"]["TxnRelationshipRow"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let txn = ConfigApplyTxn::begin_local(&node, Some(owner))
        .await
        .unwrap();
    txn.delete_actor_relationship("TxnRelationshipRow", doc, "reader", &reader)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let visible = reader_access
        .execute("{ TxnRelationshipRow { _docID } }")
        .await
        .unwrap();
    assert!(visible["data"]["TxnRelationshipRow"]
        .as_array()
        .unwrap()
        .is_empty());
    node.shutdown().await.unwrap();
}
