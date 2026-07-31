use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use gents::agent::directory_projection::{
    derive_directory_entries, directory_entry_key, reconcile_directory_tick, DirectoryEntry,
    DirectoryStore, DirectoryTickOutcome,
};

#[derive(Default)]
struct DirectoryFixtureStore {
    principals: Vec<(String, String)>,
    behaviors: BTreeMap<String, Vec<(String, String)>>,
    runtimes: BTreeMap<String, (String, String)>,
    entries: Mutex<BTreeMap<(String, String), DirectoryEntry>>,
    upserts: Mutex<Vec<String>>,
    deletes: Mutex<Vec<String>>,
}

#[async_trait]
impl DirectoryStore for DirectoryFixtureStore {
    async fn load_principals(&self) -> Result<Vec<(String, String)>> {
        Ok(self.principals.clone())
    }
    async fn load_behaviors(&self) -> Result<BTreeMap<String, Vec<(String, String)>>> {
        Ok(self.behaviors.clone())
    }
    async fn load_runtime_states(&self) -> Result<BTreeMap<String, (String, String)>> {
        Ok(self.runtimes.clone())
    }
    async fn list_directory_entries(
        &self,
        source_did: &str,
    ) -> Result<BTreeMap<String, DirectoryEntry>> {
        Ok(self
            .entries
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, entry)| entry.source_did == source_did)
            .map(|((_, did), entry)| (did.clone(), entry.clone()))
            .collect())
    }
    async fn upsert_directory_entry(&self, entry: &DirectoryEntry) -> Result<()> {
        self.upserts.lock().unwrap().push(entry.agent_did.clone());
        self.entries.lock().unwrap().insert(
            (entry.source_did.clone(), entry.agent_did.clone()),
            entry.clone(),
        );
        Ok(())
    }
    async fn delete_directory_entry(&self, source_did: &str, agent_did: &str) -> Result<()> {
        self.deletes.lock().unwrap().push(agent_did.to_string());
        let mut entries = self.entries.lock().unwrap();
        if entries.contains_key(&(source_did.to_string(), agent_did.to_string())) {
            entries.remove(&(source_did.to_string(), agent_did.to_string()));
        }
        Ok(())
    }
}

fn principal(did: &str, name: &str) -> (String, String) {
    (did.to_string(), name.to_string())
}

#[test]
fn derivation_projects_exactly_the_principals() {
    let derived = derive_directory_entries(
        "did:key:home",
        &[principal("did:key:a", "Amy"), principal("did:key:b", "Bob")],
        &BTreeMap::from([(
            "did:key:a".to_string(),
            vec![
                ("did:key:a:coder".to_string(), "Coder".to_string()),
                ("did:key:a:artist".to_string(), "Artist".to_string()),
            ],
        )]),
        &BTreeMap::from([(
            "did:key:a".to_string(),
            ("running".to_string(), "2026-07-23T00:00:00Z".to_string()),
        )]),
    );
    assert_eq!(
        derived.keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from(["did:key:a".to_string(), "did:key:b".to_string()])
    );
    let a = &derived["did:key:a"];
    assert_eq!(a.source_did, "did:key:home");
    assert_eq!(a.display_name, "Amy");
    assert_eq!(a.behaviors, vec!["Artist".to_string(), "Coder".to_string()]);
    assert_eq!(
        a.behavior_ids,
        vec![
            "did:key:a:artist".to_string(),
            "did:key:a:coder".to_string()
        ],
        "ids must stay index-aligned with display names"
    );
    assert_eq!(a.runtime_state, "running");
    let b = &derived["did:key:b"];
    assert!(b.behaviors.is_empty() && b.behavior_ids.is_empty());
    assert_eq!(b.runtime_state, "");
}

#[tokio::test]
async fn tick_converges_then_quiesces() {
    let store = DirectoryFixtureStore {
        principals: vec![principal("did:key:a", "Amy")],
        runtimes: BTreeMap::from([(
            "did:key:a".to_string(),
            ("running".to_string(), "2026-07-23T00:00:00Z".to_string()),
        )]),
        entries: Mutex::new(BTreeMap::from([(
            ("did:key:home".to_string(), "did:key:a".to_string()),
            DirectoryEntry {
                directory_key: directory_entry_key("did:key:home", "did:key:a"),
                agent_did: "did:key:a".to_string(),
                source_did: "did:key:home".to_string(),
                display_name: "Amy".to_string(),
                behaviors: Vec::new(),
                behavior_ids: Vec::new(),
                runtime_state: "starting".to_string(),
                last_seen: String::new(),
            },
        )])),
        ..Default::default()
    };

    let first = reconcile_directory_tick(&store, "did:key:home")
        .await
        .expect("first tick");
    assert_eq!(first.refreshed, BTreeSet::from(["did:key:a".to_string()]));

    let second = reconcile_directory_tick(&store, "did:key:home")
        .await
        .expect("second tick");
    assert_eq!(
        second,
        DirectoryTickOutcome::default(),
        "settled state must be a write-free fixpoint"
    );
    assert_eq!(
        store.upserts.lock().unwrap().len(),
        1,
        "exactly one write total"
    );
}

#[tokio::test]
async fn tick_retracts_only_removed_principals() {
    let store = DirectoryFixtureStore {
        principals: vec![principal("did:key:b", "Bob")],
        entries: Mutex::new(BTreeMap::from([
            (
                ("did:key:home".to_string(), "did:key:a".to_string()),
                DirectoryEntry {
                    directory_key: directory_entry_key("did:key:home", "did:key:a"),
                    agent_did: "did:key:a".to_string(),
                    source_did: "did:key:home".to_string(),
                    display_name: "Amy".to_string(),
                    behaviors: Vec::new(),
                    behavior_ids: Vec::new(),
                    runtime_state: String::new(),
                    last_seen: String::new(),
                },
            ),
            (
                ("did:key:home".to_string(), "did:key:b".to_string()),
                DirectoryEntry {
                    directory_key: directory_entry_key("did:key:home", "did:key:b"),
                    agent_did: "did:key:b".to_string(),
                    source_did: "did:key:home".to_string(),
                    display_name: "Bob".to_string(),
                    behaviors: Vec::new(),
                    behavior_ids: Vec::new(),
                    runtime_state: String::new(),
                    last_seen: String::new(),
                },
            ),
        ])),
        ..Default::default()
    };

    let outcome = reconcile_directory_tick(&store, "did:key:home")
        .await
        .expect("tick");
    assert_eq!(outcome.retracted, BTreeSet::from(["did:key:a".to_string()]));
    assert_eq!(
        *store.deletes.lock().unwrap(),
        vec!["did:key:a".to_string()]
    );
    assert!(outcome.upserted.is_empty() && outcome.refreshed.is_empty());
}

#[tokio::test]
async fn tick_preserves_foreign_same_agent_did_and_converges_local_row() {
    let foreign = DirectoryEntry {
        directory_key: directory_entry_key("did:key:foreign-home", "did:key:shared-agent"),
        agent_did: "did:key:shared-agent".to_string(),
        source_did: "did:key:foreign-home".to_string(),
        display_name: "Foreign".to_string(),
        behaviors: Vec::new(),
        behavior_ids: Vec::new(),
        runtime_state: String::new(),
        last_seen: String::new(),
    };
    let store = DirectoryFixtureStore {
        principals: vec![principal("did:key:shared-agent", "Local")],
        entries: Mutex::new(BTreeMap::from([(
            (foreign.source_did.clone(), foreign.agent_did.clone()),
            foreign.clone(),
        )])),
        ..Default::default()
    };

    let first = reconcile_directory_tick(&store, "did:key:local-home")
        .await
        .expect("first tick");
    assert_eq!(
        first.upserted,
        BTreeSet::from(["did:key:shared-agent".to_string()])
    );
    {
        let entries = store.entries.lock().unwrap();
        assert_eq!(
            entries.get(&(foreign.source_did.clone(), foreign.agent_did.clone())),
            Some(&foreign),
            "local projection must not overwrite the foreign same-DID row"
        );
        assert_eq!(
            entries
                .get(&(
                    "did:key:local-home".to_string(),
                    "did:key:shared-agent".to_string()
                ))
                .map(|entry| entry.display_name.as_str()),
            Some("Local")
        );
    }

    let second = reconcile_directory_tick(&store, "did:key:local-home")
        .await
        .expect("second tick");
    assert_eq!(second, DirectoryTickOutcome::default());
    assert!(store.deletes.lock().unwrap().is_empty());
}
