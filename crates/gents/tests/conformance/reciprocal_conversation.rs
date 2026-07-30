use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use gents::agent::p2p_reconcile::{
    derive_reciprocal_desired, reconcile_reciprocal_tick, NetworkEndpointEntry, ReciprocalRowState,
    ReciprocalStore, ReciprocalTickOutcome,
};

fn endpoint(did: &str, peer: &str, address: &str) -> NetworkEndpointEntry {
    NetworkEndpointEntry {
        peer_id: peer.to_string(),
        agent_did: did.to_string(),
        address: address.to_string(),
    }
}

#[test]
fn reciprocal_derivation_matches_intent_endpoint_join() {
    let intents = BTreeSet::from(["did:key:phone".to_string()]);
    let endpoints = vec![
        endpoint("did:key:phone", "peer-phone", "/ticket/phone"),
        endpoint("did:key:other", "peer-other", "/ticket/other"),
        endpoint("did:key:phone", "", "/ticket/blank-peer"),
        endpoint("did:key:phone", "peer-blank-address", ""),
    ];

    let derived = derive_reciprocal_desired(&intents, &BTreeSet::new(), &endpoints);

    assert_eq!(derived.len(), 1);
    assert_eq!(derived[0].peer_id, "peer-phone");
    assert_eq!(derived[0].agent_did, "did:key:phone");
    assert_eq!(derived[0].address, "/ticket/phone");
}

#[test]
fn reciprocal_derivation_is_idempotent_and_convergent() {
    let intents = BTreeSet::from(["did:key:phone".to_string()]);
    let endpoints = vec![endpoint("did:key:phone", "peer-phone", "/ticket/phone")];

    let first = derive_reciprocal_desired(&intents, &BTreeSet::new(), &endpoints)
        .into_iter()
        .map(|entry| entry.peer_id.clone())
        .collect::<BTreeSet<_>>();
    let second = derive_reciprocal_desired(&intents, &BTreeSet::new(), &endpoints)
        .into_iter()
        .map(|entry| entry.peer_id.clone())
        .collect::<BTreeSet<_>>();

    assert_eq!(first, second);
    assert_eq!(first, BTreeSet::from(["peer-phone".to_string()]));
}

struct ReciprocalPartitionStore {
    intents: BTreeMap<String, String>,
    revoked_members: BTreeSet<String>,
    endpoints: BTreeMap<String, NetworkEndpointEntry>,
    reciprocal_owned: Mutex<BTreeMap<String, ReciprocalRowState>>,
    operator_owned: BTreeSet<String>,
    upserts: Mutex<Vec<(String, String, String)>>,
    deletes: Mutex<Vec<String>>,
}

impl ReciprocalPartitionStore {
    fn new(
        intents: &[(&str, &str)],
        endpoints: Vec<NetworkEndpointEntry>,
        reciprocal_owned: &[(&str, &str, &str)],
        operator_owned: &[&str],
    ) -> Self {
        Self {
            intents: intents
                .iter()
                .map(|&(did, template)| (did.to_string(), template.to_string()))
                .collect(),
            revoked_members: BTreeSet::new(),
            endpoints: endpoints
                .into_iter()
                .map(|entry| (entry.agent_did.clone(), entry))
                .collect(),
            reciprocal_owned: Mutex::new(
                reciprocal_owned
                    .iter()
                    .map(|&(peer, address, template)| {
                        (
                            peer.to_string(),
                            ReciprocalRowState {
                                address: address.to_string(),
                                template: template.to_string(),
                            },
                        )
                    })
                    .collect(),
            ),
            operator_owned: operator_owned
                .iter()
                .map(|value| value.to_string())
                .collect(),
            upserts: Mutex::new(Vec::new()),
            deletes: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ReciprocalStore for ReciprocalPartitionStore {
    async fn load_intents(&self) -> Result<BTreeMap<String, String>> {
        Ok(self.intents.clone())
    }

    async fn load_revoked_member_dids(&self) -> Result<BTreeSet<String>> {
        Ok(self.revoked_members.clone())
    }

    async fn load_endpoint_for_did(&self, did: &str) -> Result<Option<NetworkEndpointEntry>> {
        Ok(self.endpoints.get(did).cloned())
    }

    async fn upsert_reciprocal_data_plane(
        &self,
        peer_id: &str,
        agent_did: &str,
        address: &str,
        template: &str,
    ) -> Result<()> {
        self.reciprocal_owned.lock().unwrap().insert(
            peer_id.to_string(),
            ReciprocalRowState {
                address: address.to_string(),
                template: template.to_string(),
            },
        );
        self.upserts.lock().unwrap().push((
            peer_id.to_string(),
            agent_did.to_string(),
            address.to_string(),
        ));
        Ok(())
    }

    async fn delete_reciprocal_data_plane(&self, peer_id: &str) -> Result<()> {
        self.reciprocal_owned.lock().unwrap().remove(peer_id);
        self.deletes.lock().unwrap().push(peer_id.to_string());
        Ok(())
    }

    async fn list_reciprocal_data_plane_rows(
        &self,
    ) -> Result<BTreeMap<String, ReciprocalRowState>> {
        Ok(self.reciprocal_owned.lock().unwrap().clone())
    }

    async fn list_non_reciprocal_data_plane_peers(&self) -> Result<BTreeSet<String>> {
        Ok(self.operator_owned.clone())
    }
}

#[tokio::test]
async fn reciprocal_reconcile_is_ownership_safe() {
    let store = ReciprocalPartitionStore::new(
        &[
            ("did:key:phone", "conversation"),
            ("did:key:taken", "conversation"),
        ],
        vec![
            endpoint("did:key:phone", "peer-phone", "/ticket/phone"),
            endpoint("did:key:taken", "peer-operator", "/ticket/taken"),
        ],
        &[],
        &["peer-operator"],
    );

    let outcome = reconcile_reciprocal_tick(&store, "did:key:server")
        .await
        .expect("tick");

    assert_eq!(outcome.upserted, BTreeSet::from(["peer-phone".to_string()]));
    assert!(outcome.refreshed.is_empty());
    assert!(outcome.retracted.is_empty());
    let touched = {
        let upserts = store.upserts.lock().unwrap();
        let deletes = store.deletes.lock().unwrap();
        upserts
            .iter()
            .map(|(peer, _, _)| peer.clone())
            .chain(deletes.iter().cloned())
            .collect::<Vec<_>>()
    };
    for touched in touched {
        assert!(
            !store.operator_owned.contains(&touched),
            "reciprocal reconciler touched operator-owned peer {touched}"
        );
    }
}

#[tokio::test]
async fn reciprocal_reconcile_converges_row_contents_then_quiesces() {
    let store = ReciprocalPartitionStore::new(
        &[("did:key:phone", "conversation")],
        vec![endpoint("did:key:phone", "peer-phone", "/ticket/phone-new")],
        &[("peer-phone", "/ticket/phone-old", "conversation")],
        &[],
    );

    let first = reconcile_reciprocal_tick(&store, "did:key:server")
        .await
        .expect("first tick");
    assert_eq!(
        first.refreshed,
        BTreeSet::from(["peer-phone".to_string()]),
        "drifted address must converge to the derived row"
    );
    assert_eq!(
        store
            .reciprocal_owned
            .lock()
            .unwrap()
            .get("peer-phone")
            .expect("row present")
            .address,
        "/ticket/phone-new"
    );

    let second = reconcile_reciprocal_tick(&store, "did:key:server")
        .await
        .expect("second tick");
    assert_eq!(
        second,
        ReciprocalTickOutcome::default(),
        "settled state must be a write-free fixpoint"
    );
}

#[tokio::test]
async fn reciprocal_retraction_removes_only_derived_rows() {
    let store = ReciprocalPartitionStore::new(
        &[("did:key:phone-b", "conversation")],
        vec![endpoint("did:key:phone-b", "peer-b", "/ticket/b")],
        &[
            ("peer-a", "/ticket/a", "conversation"),
            ("peer-b", "/ticket/b", "conversation"),
        ],
        &["peer-operator"],
    );

    let outcome = reconcile_reciprocal_tick(&store, "did:key:server")
        .await
        .expect("tick");

    assert_eq!(outcome.retracted, BTreeSet::from(["peer-a".to_string()]));
    assert_eq!(*store.deletes.lock().unwrap(), vec!["peer-a".to_string()]);
    assert!(!store
        .deletes
        .lock()
        .unwrap()
        .contains(&"peer-b".to_string()));
    assert!(!store
        .deletes
        .lock()
        .unwrap()
        .contains(&"peer-operator".to_string()));
}

#[tokio::test]
async fn reciprocal_revocation_retracts_only_the_revoked_member() {
    let mut store = ReciprocalPartitionStore::new(
        &[
            ("did:key:phone-a", "conversation"),
            ("did:key:phone-b", "conversation"),
        ],
        vec![
            endpoint("did:key:phone-a", "peer-a", "/ticket/a"),
            endpoint("did:key:phone-b", "peer-b", "/ticket/b"),
        ],
        &[
            ("peer-a", "/ticket/a", "conversation"),
            ("peer-b", "/ticket/b", "conversation"),
        ],
        &[],
    );
    store.revoked_members.insert("did:key:phone-a".to_string());

    let outcome = reconcile_reciprocal_tick(&store, "did:key:server")
        .await
        .expect("tick");

    assert_eq!(outcome.retracted, BTreeSet::from(["peer-a".to_string()]));
    assert_eq!(*store.deletes.lock().unwrap(), vec!["peer-a".to_string()]);
    assert!(store
        .reciprocal_owned
        .lock()
        .unwrap()
        .contains_key("peer-b"));
}

#[tokio::test]
async fn reciprocal_reconcile_converges_template_drift() {
    let store = ReciprocalPartitionStore::new(
        &[("did:key:phone", "machine")],
        vec![endpoint("did:key:phone", "peer-phone", "/ticket/phone")],
        &[("peer-phone", "/ticket/phone", "conversation")],
        &[],
    );

    let first = reconcile_reciprocal_tick(&store, "did:key:server")
        .await
        .expect("first tick");
    assert_eq!(first.refreshed, BTreeSet::from(["peer-phone".to_string()]));
    assert_eq!(
        store
            .reciprocal_owned
            .lock()
            .unwrap()
            .get("peer-phone")
            .expect("row present")
            .template,
        "machine"
    );

    let second = reconcile_reciprocal_tick(&store, "did:key:server")
        .await
        .expect("second tick");
    assert_eq!(
        second,
        ReciprocalTickOutcome::default(),
        "settled after upgrade"
    );
}
