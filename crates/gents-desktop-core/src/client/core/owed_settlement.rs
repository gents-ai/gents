use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::sync_state::ClientSyncStateOwner;
use crate::client::PrincipalIdentity;

/// A change a home did not keep, as the person is told it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnkeptChange {
    pub node_did: String,
    pub label: String,
    pub at: String,
    pub message: String,
}

/// Settles what this desktop's User owes each saved Vera home while the home
/// answers, one pass at a time per home
/// (`Identity.OwedRegistration.continues`).
#[derive(Clone, Default)]
pub(super) struct OwedSettlements {
    running: Arc<Mutex<BTreeMap<String, tokio::task::JoinHandle<()>>>>,
}

impl OwedSettlements {
    /// Start settling for every saved home whose record names an audience
    /// and has no settlement running; a home whose last one ended starts
    /// again here.
    pub(super) fn settle_saved(&self, sync_state: &ClientSyncStateOwner, user: &PrincipalIdentity) {
        let Ok(capability) = gents_store::config_client::ClientSigningCapability::from_ed25519_key(
            user.private_key_bytes(),
        ) else {
            return;
        };
        let mut running = self
            .running
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        running.retain(|_, task| !task.is_finished());
        for record in sync_state.records() {
            if record.delegation_audience.is_none() || running.contains_key(&record.node_did) {
                continue;
            }
            let Some(url) = record.operator_graphql() else {
                continue;
            };
            let Ok(endpoint) =
                crate::client::home_endpoint::user_saved(url, &capability, &record, sync_state)
            else {
                continue;
            };
            let sync_state = sync_state.clone();
            let node_did = record.node_did.clone();
            let Some(task) = gents_store::config_client::owed::spawn_settlement(
                endpoint,
                record.node_did.clone(),
                move |failed| {
                    sync_state.report_unkept_change(UnkeptChange {
                        node_did: record.node_did,
                        label: record.label,
                        at: chrono::Utc::now().to_rfc3339(),
                        message: failed.to_string(),
                    });
                },
            ) else {
                continue;
            };
            running.insert(node_did, task);
        }
    }

    /// Stop every settlement and wait until each has let go of what it
    /// holds, the peer directory among them.
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

    use super::OwedSettlements;
    use crate::client::core::sync_state::ClientSyncStateOwner;
    use crate::client::{DesktopPaths, PeerRecord, PrincipalIdentity};

    fn home(server: &MockServer, node_did: &str, vera: bool) -> PeerRecord {
        let mut record = PeerRecord::new(node_did, "iroh://peer", node_did);
        record.graphql = Some(format!("{}/api/v0/graphql", server.uri()));
        record.delegation_audience = vera.then(|| DelegationAudience {
            worker: "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".into(),
            deployment: 7,
            genesis_id: None,
        });
        record
    }

    #[tokio::test]
    async fn a_vera_home_is_settled_and_a_change_it_did_not_keep_is_told() {
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
        let temp = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(temp.path());
        paths.ensure_root_dirs().await.unwrap();
        let user = PrincipalIdentity::load_or_create(&paths).await.unwrap();
        let (_dir, sync_state) = ClientSyncStateOwner::for_test(
            vec![
                home(&vera, "did:key:vera", true),
                home(&local, "did:key:local", false),
            ],
            vec![],
        )
        .await;
        let mut updates = sync_state.subscribe();
        OwedSettlements::default().settle_saved(&sync_state, &user);
        let told = loop {
            let unkept = updates.borrow_and_update().unkept_changes.clone();
            if !unkept.is_empty() {
                break unkept;
            }
            updates.changed().await.unwrap();
        };
        assert_eq!(told.len(), 1);
        assert_eq!(told[0].node_did, "did:key:vera");
        assert_eq!(
            told[0].message,
            gents_store::config_client::owed::OWED_FAILED_FOR_A_PERSON
        );
        assert!(local.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_stopped_settlement_lets_go_of_the_peer_directory() {
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
        let temp = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(temp.path());
        paths.ensure_root_dirs().await.unwrap();
        let user = PrincipalIdentity::load_or_create(&paths).await.unwrap();
        let (dir, sync_state) =
            ClientSyncStateOwner::for_test(vec![home(&vera, "did:key:vera", true)], vec![]).await;
        let owed = OwedSettlements::default();
        owed.settle_saved(&sync_state, &user);
        asked.notified().await;
        owed.stop().await;
        drop(sync_state);
        crate::client::peer_directory::PeerDirectory::open_writer(dir.path().join("peers.json"))
            .await
            .expect("the peer directory is free once its settlements stopped");
    }
}
