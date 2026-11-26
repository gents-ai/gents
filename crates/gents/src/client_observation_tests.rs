use anyhow::Result;
use gents_documents::InferenceBackend;
use gents_observe::client_projection::observe::{
    spawn_observer, ObservedStores, ObserverHandle, RuntimeConnectionStatus,
};
use gents_observe::client_projection::query::{
    fetch_doc_patch, load_inference_configuration, load_node_snapshot_on,
};
use gents_observe::test_support::client_observer::{wait_for_connection, wait_for_name};
use gents_protocol::schemas::{
    INFERENCE_BACKEND_METADATA_NAME as INFERENCE_BACKEND_NAME, INFERENCE_BACKEND_OBSERVATION_NAME,
};
use gents_store::client_journal::runtime_directory::RuntimeDirectory;
use std::{sync::Arc, time::Duration};

/// The bearer lifetime of the observers whose tests reach a renewal: its
/// subscriptions renew every half of it, in seconds rather than the default
/// half minute.
const SHORT_BEARER_LIFETIME: Duration = Duration::from_secs(6);

/// An observer of `home`'s runtime whose bearers last `bearer_lifetime`.
async fn served_observer(
    home: &crate::client_test_support::ServedRuntimeHome,
    bearer_lifetime: Duration,
) -> (tempfile::TempDir, Arc<ObservedStores>, ObserverHandle) {
    let directory = tempfile::tempdir().unwrap();
    let mut peer = gents_protocol::runtime_connection::RuntimeConnection {
        label: ("runtime").to_string(),
        node_did: (home.node_did()).to_string(),
        graphql: String::new(),
        local_node_home: None,
        access_request: None,
        access_state: None,
    };
    peer.graphql = home.graphql().to_owned();
    let peers = RuntimeDirectory::open(directory.path().join("connections.json"))
        .await
        .unwrap();
    peers.upsert(peer).await.unwrap();
    let configured = peers;
    let store = ObservedStores::new();
    let endpoints = gents_store::config_client::runtime_endpoints::RuntimeEndpoints::new(
        home.user().client_signing_capability().unwrap(),
    )
    .with_bearer_lifetime(bearer_lifetime);
    let handle = spawn_observer(store.clone(), configured, Arc::new(endpoints));
    (directory, store, handle)
}

async fn node_doc_id(home: &crate::client_test_support::ServedRuntimeHome) -> String {
    let rows = home
        .user_graphql_access()
        .execute(&format!(
            "{{ Node(filter: {{node_did: {{_eq: \"{}\"}}}}) {{ _docID }} }}",
            gents_store::graphql::escape_graphql_string(home.node_did())
        ))
        .await
        .unwrap();
    rows["data"]["Node"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// A subscription is renewed at half its bearer's lifetime, and that renewal
/// is not a transport failure: the node stays connected and changes keep
/// arriving. The observer's bearers last [`SHORT_BEARER_LIFETIME`], so this
/// waits for a real renewal within seconds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn planned_renewal_keeps_the_runtime_connected() {
    let home = crate::client_test_support::ServedRuntimeHome::start().await;
    let (_directory, store, handle) = served_observer(&home, SHORT_BEARER_LIFETIME).await;
    wait_for_connection(&store, home.node_did(), RuntimeConnectionStatus::Connected).await;
    let before = handle.metrics_snapshot();
    let mut shown = vec![RuntimeConnectionStatus::Connected];
    let mut changes = store.subscribe();
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let metrics_notified = handle.metrics_notified();
            tokio::pin!(metrics_notified);
            metrics_notified.as_mut().enable();
            let metrics = handle.metrics_snapshot();
            if metrics.subscription_renewals > before.subscription_renewals
                || metrics.transport_failures > before.transport_failures
            {
                break;
            }
            tokio::select! {
                changed = changes.changed() => changed.unwrap(),
                () = &mut metrics_notified => {}
            }
            if let Some(status) = store.runtime_connection(home.node_did()) {
                if shown.last() != Some(&status) {
                    shown.push(status);
                }
            }
        }
    })
    .await
    .expect("the subscriptions reach their renewal point");
    let after = handle.metrics_snapshot();
    assert_eq!(
        after.transport_failures, before.transport_failures,
        "a planned renewal is not a transport failure; shown {shown:?}"
    );
    assert!(after.subscription_renewals > before.subscription_renewals);
    let physical = node_doc_id(&home).await;
    home.user_graphql_access()
        .write(
            "test.observer.after_renewal",
            &format!(
                "mutation {{ update_Node(docID: \"{}\", input: {{display_name: \"after renewal\"}}) {{ _docID }} }}",
                gents_store::graphql::escape_graphql_string(&physical)
            ),
        )
        .await
        .unwrap();
    wait_for_name(&store, Some("after renewal")).await;
    assert_eq!(shown, vec![RuntimeConnectionStatus::Connected], "{shown:?}");
    assert_eq!(
        store.runtime_connection(home.node_did()),
        Some(RuntimeConnectionStatus::Connected)
    );
    handle.shutdown().await;
    home.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoked_user_is_refused_by_the_served_runtime() {
    let home = crate::client_test_support::ServedRuntimeHome::start().await;
    let access = home.user_graphql_access();
    let before =
        gents_observe::client_projection::query::load_node_snapshot_on(&access, home.node_did())
            .await
            .unwrap();
    assert!(!before.nodes.is_empty());

    let subscription = "subscription { Node(showDeleted: true) { _docID _deleted } }";
    let mut open = access.subscribe(subscription).await.unwrap();
    gents_store::identity::GrantSet::HouseholdOwner
        .revoke(
            &home.runtime_access(),
            &::identity::Did::new(home.user().did()).unwrap(),
        )
        .await
        .unwrap();
    let read = access.execute("{ Node { _docID } }").await.unwrap_err();
    assert!(read.is_permission_denied(), "{read}");
    assert!(access.subscribe(subscription).await.is_err());
    assert!(
        tokio::time::timeout(Duration::from_secs(2), open.recv())
            .await
            .is_err(),
        "the existing stream stays silent after revocation"
    );
    let hidden =
        gents_observe::client_projection::query::load_node_snapshot_on(&access, home.node_did())
            .await
            .unwrap_err();
    assert!(matches!(
        hidden,
        gents_observe::client_projection::query::SnapshotReadError::Refused(_)
    ));
    home.shutdown().await;
}

/// A revoked User reads as refused within one renewal period: the runtime
/// keeps a revoked subscriber's silent stream, so the renewal's handshake is
/// what refuses it (`tla/SubscriptionStreamEnd.tla`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoked_user_sees_access_unavailable_and_recovers_after_approval() {
    let home = crate::client_test_support::ServedRuntimeHome::start().await;
    let (_directory, store, handle) = served_observer(&home, SHORT_BEARER_LIFETIME).await;
    wait_for_connection(&store, home.node_did(), RuntimeConnectionStatus::Connected).await;
    assert!(!store.snapshot(home.node_did()).nodes.is_empty());
    let user = ::identity::Did::new(home.user().did()).unwrap();
    gents_store::identity::GrantSet::HouseholdOwner
        .revoke(&home.runtime_access(), &user)
        .await
        .unwrap();
    let mut changes = store.subscribe();
    tokio::time::timeout(SHORT_BEARER_LIFETIME / 2 + Duration::from_secs(15), async {
        while store.runtime_connection(home.node_did()) != Some(RuntimeConnectionStatus::Refused) {
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("renewal reports unavailable access to the selected Node");
    gents_store::identity::GrantSet::HouseholdOwner
        .grant(&home.runtime_access(), &user)
        .await
        .unwrap();
    wait_for_connection(&store, home.node_did(), RuntimeConnectionStatus::Connected).await;
    assert!(!store.snapshot(home.node_did()).nodes.is_empty());
    handle.shutdown().await;
    home.shutdown().await;
}

/// Every snapshot load sees runtime changes while it is in flight: one to its
/// own node's row and one adding another node's row on the same runtime. Both
/// arrive on the runtime's subscription as its node's events, and the reload
/// publishes only its node's rows. Each load still publishes, so the node
/// connects after its first load and the rows catch up to the last change.
#[tokio::test]
async fn request_events_during_every_snapshot_load_still_publish() {
    use crate::client_test_support::{GatedRuntimeProxy, ServedRuntimeHome};

    let runtime = ServedRuntimeHome::start().await;
    runtime
        .publish_node(serde_json::json!({"display_name": "before"}))
        .await;
    let mut proxy =
        GatedRuntimeProxy::start(runtime.graphql(), GatedRuntimeProxy::node_snapshot).await;
    let directory = tempfile::tempdir().unwrap();
    let peers = RuntimeDirectory::open(directory.path().join("connections.json"))
        .await
        .unwrap();
    let mut peer = gents_protocol::runtime_connection::RuntimeConnection {
        label: ("runtime").to_string(),
        node_did: (runtime.node_did()).to_string(),
        graphql: String::new(),
        local_node_home: None,
        access_request: None,
        access_state: None,
    };
    peer.graphql = proxy.url().to_owned();
    peers.upsert(peer).await.unwrap();
    let configured = peers;
    let store = ObservedStores::new();
    let mut resyncs = gents_observe::test_support::client_observer::resync_notices(&store);
    let handle = spawn_observer(
        store.clone(),
        configured,
        Arc::new(
            gents_store::config_client::runtime_endpoints::RuntimeEndpoints::new(
                runtime.user().client_signing_capability().unwrap(),
            ),
        ),
    );
    let node = runtime.node_did().to_owned();
    for load in 1..=3_u64 {
        let held = proxy.next().await;
        assert!(
            held.body.contains(&node),
            "the held read is this node's snapshot"
        );
        runtime
            .publish_node(serde_json::json!({"display_name": format!("during load {load}")}))
            .await;
        resyncs.changed().await.unwrap();
        runtime.publish_foreign_node().await;
        resyncs.changed().await.unwrap();
        held.release();
        let mut notices = store.subscribe();
        tokio::time::timeout(Duration::from_secs(15), async {
            while handle.metrics_snapshot().scope_reloads < load {
                notices.changed().await.unwrap();
            }
        })
        .await
        .expect("a load overlapping request events publishes");
        assert_eq!(
            store.runtime_connection(&node),
            Some(RuntimeConnectionStatus::Connected)
        );
    }
    let release = tokio::spawn(async move {
        loop {
            proxy.next().await.release();
        }
    });
    wait_for_name(&store, Some("during load 3")).await;
    handle.shutdown().await;
    release.abort();
    runtime.shutdown().await;
}

#[tokio::test]
async fn catalog_freshness_reaches_snapshot_operator_and_document_patch_readers() -> Result<()> {
    let runtime = crate::client_test_support::ServedRuntimeHome::start().await;
    let backend: InferenceBackend = serde_json::from_value(serde_json::json!({
        "node_did":runtime.node_did(), "backend_id":"catalog-backend", "name":"Catalog",
        "provider_kind":"OpenAiCompatible", "endpoint":"http://127.0.0.1:1/v1",
        "auth":{"kind":"unauthenticated"}
    }))?;
    let access = runtime.user_access();
    gents_documents::write_inference_backend_document(
        &access,
        &backend,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await?;
    let observer = runtime.runtime_access();
    for observed_at in ["2026-10-28T01:00:00Z", "2026-10-28T02:00:00Z"] {
        let catalog: gents_documents::BackendModelCatalog =
            serde_json::from_value(serde_json::json!({
                "node_did":null, "observed_at":observed_at,
                "models":[{"model_name":"retained-model"}]
            }))?;
        observer
            .transact_write("test.catalog.refresh", |txn| {
                let backend = &backend;
                let catalog = catalog.clone();
                Box::pin(async move {
                    gents_inference::backends::registry::record_model_catalog_in_txn(
                        txn, backend, catalog,
                    )
                    .await
                })
            })
            .await?;
    }
    let response = access
        .execute("{ InferenceBackendMetadata { _docID } }")
        .await?;
    let doc_id = response["data"]["InferenceBackendMetadata"][0]["_docID"]
        .as_str()
        .unwrap();
    let response = access
        .execute("{ InferenceBackendObservation { _docID } }")
        .await?;
    let observation_doc = response["data"]["InferenceBackendObservation"][0]["_docID"]
        .as_str()
        .unwrap();
    let snapshot = load_node_snapshot_on(&runtime.user_access(), runtime.node_did()).await?;
    let operator = load_inference_configuration(&access, &backend.node_did).await?;
    let patch = fetch_doc_patch(
        &access,
        runtime.node_did(),
        INFERENCE_BACKEND_NAME,
        &[doc_id],
    )
    .await?
    .store;
    let observation_patch = fetch_doc_patch(
        &access,
        runtime.node_did(),
        INFERENCE_BACKEND_OBSERVATION_NAME,
        &[observation_doc],
    )
    .await?
    .store;
    assert_eq!(patch.inference_backends.len(), 1);
    assert!(patch.backend_observations.is_empty());
    for store in [snapshot, operator, observation_patch] {
        assert_eq!(store.backend_observations.len(), 1);
        assert_eq!(
            store.backend_observations[0].catalogs[0].observed_at,
            "2026-10-28T02:00:00Z"
        );
        assert_eq!(
            store.backend_observations[0].catalogs[0].models[0].model_name,
            "retained-model"
        );
    }
    runtime.shutdown().await;
    Ok(())
}
