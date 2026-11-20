use crate::types::UnkeptChangeView;
use gents_observe::client_projection::observe::ObservedStores;
use gents_protocol::runtime_connection::RuntimeConnection;
use gents_store::config_client::GraphqlEndpoint;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub(crate) struct OwedSettlements {
    running: Mutex<BTreeMap<String, tokio::task::JoinHandle<()>>>,
    changes: Arc<Mutex<BTreeMap<String, UnkeptChangeView>>>,
}

impl OwedSettlements {
    pub(crate) fn unkept(&self) -> Vec<UnkeptChangeView> {
        self.changes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }

    pub(super) fn settle(
        &self,
        record: &RuntimeConnection,
        endpoint: GraphqlEndpoint,
        stores: Arc<ObservedStores>,
    ) {
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        running.retain(|_, task| !task.is_finished());
        if running.contains_key(&record.node_did) {
            return;
        }
        let label = record.label.clone();
        let node_did = record.node_did.clone();
        let changes = self.changes.clone();
        if let Some(task) = gents_store::config_client::owed::spawn_settlement(
            endpoint,
            node_did.clone(),
            move |failed| {
                changes.lock().unwrap_or_else(|e| e.into_inner()).insert(
                    node_did.clone(),
                    UnkeptChangeView {
                        node_did,
                        label,
                        at: chrono::Utc::now().to_rfc3339(),
                        message: failed.to_string(),
                    },
                );
                stores.notify_client_change();
            },
        ) {
            running.insert(record.node_did.clone(), task);
        }
    }

    pub(super) async fn stop(&self) {
        let tasks = std::mem::take(
            &mut *self
                .running
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
        );
        for task in tasks.values() {
            task.abort();
        }
        for task in tasks.into_values() {
            drop(task.await);
        }
    }
}

#[cfg(test)]
mod tests {
    use gents_protocol::delegation_audience::DelegationAudience;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    fn home(server: &MockServer, node: &str, vera: bool) -> (RuntimeConnection, GraphqlEndpoint) {
        let graphql = format!("{}/api/v0/graphql", server.uri());
        let capability = gents_store::test_support::test_identity("owed-settlement-user")
            .client_signing_capability()
            .unwrap();
        let mut endpoint = GraphqlEndpoint::as_principal(&graphql, capability.did())
            .with_client_signing_capability(capability)
            .unwrap();
        if vera {
            endpoint = endpoint
                .with_reported_delegation_audience(
                    gents_protocol::delegation_audience::ReportedDelegationAudience::Named(
                        DelegationAudience {
                            worker: "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK"
                                .into(),
                            deployment: 7,
                            genesis_id: None,
                        },
                    ),
                )
                .unwrap();
        }
        (
            RuntimeConnection {
                node_did: node.into(),
                label: node.into(),
                graphql,
                local_node_home: None,
                access_request: None,
                access_state: None,
            },
            endpoint,
        )
    }

    #[tokio::test]
    async fn a_local_and_a_vera_home_are_settled_and_a_change_they_did_not_keep_is_told() {
        let vera = MockServer::start().await;
        let local = MockServer::start().await;
        for server in [&vera, &local] {
            Mock::given(method("POST"))
                .and(path(gents_protocol::owed_settlement::OWED_SETTLEMENT_PATH))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({ "standings": ["failed"] })),
                )
                .mount(server)
                .await;
        }
        let stores = ObservedStores::new();
        let mut updates = stores.subscribe();
        let owed = OwedSettlements::default();
        for (record, endpoint) in [
            home(&vera, "did:key:vera", true),
            home(&local, "did:key:local", false),
        ] {
            owed.settle(&record, endpoint, stores.clone());
        }
        while owed.unkept().len() < 2 {
            updates.changed().await.unwrap();
        }
        let told = owed.unkept();
        assert_eq!(
            told.iter()
                .map(|change| change.node_did.as_str())
                .collect::<Vec<_>>(),
            ["did:key:local", "did:key:vera"]
        );
        assert!(told
            .iter()
            .all(|change| change.message
                == gents_store::config_client::owed::OWED_FAILED_FOR_A_PERSON));
    }

    #[tokio::test]
    async fn a_stopped_settlement_releases_its_observation_store() {
        let vera = MockServer::start().await;
        let asked = std::sync::Arc::new(tokio::sync::Notify::new());
        let notify = std::sync::Arc::clone(&asked);
        Mock::given(method("POST"))
            .and(path(gents_protocol::owed_settlement::OWED_SETTLEMENT_PATH))
            .respond_with(move |_: &wiremock::Request| {
                notify.notify_one();
                ResponseTemplate::new(200).set_body_json(json!({ "standings": ["sending"] }))
            })
            .mount(&vera)
            .await;
        let stores = ObservedStores::new();
        let held = Arc::downgrade(&stores);
        let (record, endpoint) = home(&vera, "did:key:vera", true);
        let owed = OwedSettlements::default();
        owed.settle(&record, endpoint, stores);
        asked.notified().await;
        owed.stop().await;
        assert!(held.upgrade().is_none());
    }
}
