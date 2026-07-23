//! Conformance fence for `Proofs/PeerRegistryDiscovery/DirectoryProjection.lean`.
//!
//! The Lean model projects agent principals into directory entries and pins
//! four properties: membership characterization (`mem_project`), idempotent
//! convergence (`projectStep_idempotent`), the settled write-free fixpoint
//! (`settled_fixpoint` — the sweep runs on Update events and must not
//! self-perpetuate), and retraction soundness (`mem_project_erase`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use gents::agent::directory_projection::{
    derive_directory_entries, reconcile_directory_tick, DirectoryEntry, DirectoryStore,
    DirectoryTickOutcome,
};

#[derive(Default)]
struct DirectoryFixtureStore {
    principals: Vec<(String, String)>,
    behaviors: BTreeMap<String, Vec<String>>,
    runtimes: BTreeMap<String, (String, String)>,
    entries: Mutex<BTreeMap<String, DirectoryEntry>>,
    upserts: Mutex<Vec<String>>,
    deletes: Mutex<Vec<String>>,
}

#[async_trait]
impl DirectoryStore for DirectoryFixtureStore {
    async fn load_principals(&self) -> Result<Vec<(String, String)>> {
        Ok(self.principals.clone())
    }
    async fn load_behavior_names(&self) -> Result<BTreeMap<String, Vec<String>>> {
        Ok(self.behaviors.clone())
    }
    async fn load_runtime_states(&self) -> Result<BTreeMap<String, (String, String)>> {
        Ok(self.runtimes.clone())
    }
    async fn list_directory_entries(&self) -> Result<BTreeMap<String, DirectoryEntry>> {
        Ok(self.entries.lock().unwrap().clone())
    }
    async fn upsert_directory_entry(&self, entry: &DirectoryEntry) -> Result<()> {
        self.upserts.lock().unwrap().push(entry.agent_did.clone());
        self.entries
            .lock()
            .unwrap()
            .insert(entry.agent_did.clone(), entry.clone());
        Ok(())
    }
    async fn delete_directory_entry(&self, agent_did: &str) -> Result<()> {
        self.deletes.lock().unwrap().push(agent_did.to_string());
        self.entries.lock().unwrap().remove(agent_did);
        Ok(())
    }
}

fn principal(did: &str, name: &str) -> (String, String) {
    (did.to_string(), name.to_string())
}

/// Mirrors `mem_project`: one entry per principal, contents a function of
/// (principal, behaviors, runtime).
#[test]
fn derivation_projects_exactly_the_principals() {
    let derived = derive_directory_entries(
        &[principal("did:key:a", "Amy"), principal("did:key:b", "Bob")],
        &BTreeMap::from([("did:key:a".to_string(), vec!["coder".to_string()])]),
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
    assert_eq!(a.display_name, "Amy");
    assert_eq!(a.behaviors, vec!["coder".to_string()]);
    assert_eq!(a.runtime_state, "running");
    let b = &derived["did:key:b"];
    assert!(b.behaviors.is_empty());
    assert_eq!(b.runtime_state, "");
}

/// Mirrors `projectStep_settles` + `settled_fixpoint`: the first sweep
/// converges (including drifted payloads) and the second is write-free.
#[tokio::test]
async fn tick_converges_then_quiesces() {
    let store = DirectoryFixtureStore {
        principals: vec![principal("did:key:a", "Amy")],
        runtimes: BTreeMap::from([(
            "did:key:a".to_string(),
            ("running".to_string(), "2026-07-23T00:00:00Z".to_string()),
        )]),
        entries: Mutex::new(BTreeMap::from([(
            "did:key:a".to_string(),
            DirectoryEntry {
                agent_did: "did:key:a".to_string(),
                display_name: "Amy".to_string(),
                behaviors: Vec::new(),
                runtime_state: "starting".to_string(), // drifted payload
                last_seen: String::new(),
            },
        )])),
        ..Default::default()
    };

    let first = reconcile_directory_tick(&store).await.expect("first tick");
    assert_eq!(first.refreshed, BTreeSet::from(["did:key:a".to_string()]));

    let second = reconcile_directory_tick(&store).await.expect("second tick");
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

/// Mirrors `mem_project_erase`: a removed principal retracts exactly its
/// entry and nothing else.
#[tokio::test]
async fn tick_retracts_only_removed_principals() {
    let store = DirectoryFixtureStore {
        principals: vec![principal("did:key:b", "Bob")],
        entries: Mutex::new(BTreeMap::from([
            (
                "did:key:a".to_string(),
                DirectoryEntry {
                    agent_did: "did:key:a".to_string(),
                    display_name: "Amy".to_string(),
                    behaviors: Vec::new(),
                    runtime_state: String::new(),
                    last_seen: String::new(),
                },
            ),
            (
                "did:key:b".to_string(),
                DirectoryEntry {
                    agent_did: "did:key:b".to_string(),
                    display_name: "Bob".to_string(),
                    behaviors: Vec::new(),
                    runtime_state: String::new(),
                    last_seen: String::new(),
                },
            ),
        ])),
        ..Default::default()
    };

    let outcome = reconcile_directory_tick(&store).await.expect("tick");
    assert_eq!(outcome.retracted, BTreeSet::from(["did:key:a".to_string()]));
    assert_eq!(
        *store.deletes.lock().unwrap(),
        vec!["did:key:a".to_string()]
    );
    assert!(outcome.upserted.is_empty() && outcome.refreshed.is_empty());
}
