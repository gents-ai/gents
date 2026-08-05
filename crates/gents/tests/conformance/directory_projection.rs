use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use gents::agent::directory_projection::{
    derive_directory_entries, directory_entry_key, reconcile_directory_tick, BehaviorInfo,
    CatalogOptions, DirectoryEntry, DirectoryStore, DirectoryTickOutcome, SelectionInfo,
    SourceSnapshot,
};
use gents::agent::persona_presets::preset_fields;

#[derive(Default)]
struct DirectoryFixtureStore {
    principals: Vec<(String, String, String)>,
    behaviors: BTreeMap<String, Vec<BehaviorInfo>>,
    runtimes: BTreeMap<String, (String, String)>,
    selections: BTreeMap<String, SelectionInfo>,
    options: CatalogOptions,
    entries: Mutex<BTreeMap<(String, String), DirectoryEntry>>,
    upserts: Mutex<Vec<String>>,
    deletes: Mutex<Vec<String>>,
}

#[async_trait]
impl DirectoryStore for DirectoryFixtureStore {
    async fn load_source_snapshot(&self) -> Result<SourceSnapshot> {
        Ok(SourceSnapshot {
            principals: self.principals.clone(),
            behaviors: self.behaviors.clone(),
            runtimes: self.runtimes.clone(),
            selections: self.selections.clone(),
            options: self.options.clone(),
        })
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

fn principal(did: &str, name: &str) -> (String, String, String) {
    (did.to_string(), name.to_string(), String::new())
}

fn principal_with_default(
    did: &str,
    name: &str,
    default_behavior_id: &str,
) -> (String, String, String) {
    (
        did.to_string(),
        name.to_string(),
        default_behavior_id.to_string(),
    )
}

fn empty_entry(agent_did: &str, source_did: &str, display_name: &str) -> DirectoryEntry {
    DirectoryEntry {
        directory_key: directory_entry_key(source_did, agent_did),
        agent_did: agent_did.to_string(),
        source_did: source_did.to_string(),
        display_name: display_name.to_string(),
        behaviors: Vec::new(),
        behavior_ids: Vec::new(),
        default_behavior_id: String::new(),
        behavior_models: Vec::new(),
        behavior_roots: Vec::new(),
        behavior_presets: Vec::new(),
        behavior_profiles: Vec::new(),
        options: CatalogOptions::default(),
        runtime_state: String::new(),
        last_seen: String::new(),
    }
}

#[test]
fn derivation_projects_exactly_the_principals() {
    let coder = BehaviorInfo {
        behavior_id: "did:key:a:coder".to_string(),
        display_name: "Coder".to_string(),
        backend_id: "openai".to_string(),
        model_name: "gpt-5".to_string(),
        tool_selection_id: "sel-coder".to_string(),
        inference_profile_id: "profile-fast".to_string(),
    };
    let artist = BehaviorInfo {
        behavior_id: "did:key:a:artist".to_string(),
        display_name: "Artist".to_string(),
        backend_id: "anthropic".to_string(),
        model_name: "claude".to_string(),
        tool_selection_id: "sel-artist".to_string(),
        inference_profile_id: "".to_string(),
    };
    let selections = BTreeMap::from([
        (
            "sel-coder".to_string(),
            SelectionInfo {
                file_tool_root: "/repo/a".to_string(),
                preset: preset_fields("readonly").expect("readonly preset should exist"),
            },
        ),
        (
            "sel-artist".to_string(),
            SelectionInfo {
                file_tool_root: String::new(),
                preset: {
                    let mut fields =
                        preset_fields("readonly").expect("readonly preset should exist");
                    fields
                        .command_allowed_argv_prefixes
                        .push("git status".to_string());
                    fields
                },
            },
        ),
    ]);
    let options = CatalogOptions {
        available_models: vec!["anthropic|claude".to_string(), "openai|gpt-5".to_string()],
        allowed_roots: vec!["/repo/a".to_string()],
        permission_presets: vec!["readonly".to_string(), "write".to_string()],
        available_profiles: vec!["profile-fast|Fast".to_string()],
    };

    let derived = derive_directory_entries(
        "did:key:home",
        &[
            principal_with_default("did:key:a", "Amy", "did:key:a:coder"),
            principal("did:key:b", "Bob"),
        ],
        &BTreeMap::from([("did:key:a".to_string(), vec![coder.clone(), artist.clone()])]),
        &BTreeMap::from([(
            "did:key:a".to_string(),
            ("running".to_string(), "2026-07-23T00:00:00Z".to_string()),
        )]),
        &selections,
        &options,
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
    assert_eq!(
        a.default_behavior_id, "did:key:a:coder",
        "default_behavior_id must copy through from the principal"
    );
    assert_eq!(a.runtime_state, "running");

    // The four `behavior_*` arrays are index-aligned with the sorted
    // `behavior_ids` (artist first, coder second).
    assert_eq!(
        a.behavior_models,
        vec!["anthropic|claude".to_string(), "openai|gpt-5".to_string()],
        "behavior_models must be backend_id|model_name, aligned with behavior_ids"
    );
    assert_eq!(
        a.behavior_roots,
        vec![String::new(), "/repo/a".to_string()],
        "behavior_roots must copy each selection's file_tool_root, aligned"
    );
    assert_eq!(
        a.behavior_presets,
        vec![String::new(), "readonly".to_string()],
        "custom selection (extra argv prefix) must classify as \"\", exact match as its preset name"
    );
    assert_eq!(
        a.behavior_profiles,
        vec![String::new(), "profile-fast".to_string()],
        "behavior_profiles must copy inference_profile_id, aligned"
    );

    assert_eq!(
        a.options, options,
        "the four option lists must pass through verbatim on every entry"
    );

    let b = &derived["did:key:b"];
    assert!(b.behaviors.is_empty() && b.behavior_ids.is_empty());
    assert_eq!(
        b.default_behavior_id, "",
        "a principal with no default behavior must derive an empty default_behavior_id"
    );
    assert_eq!(b.runtime_state, "");
    assert!(
        b.behavior_models.is_empty()
            && b.behavior_roots.is_empty()
            && b.behavior_presets.is_empty()
            && b.behavior_profiles.is_empty(),
        "a principal with no behaviors derives empty dimension arrays"
    );
    assert_eq!(
        b.options, options,
        "options are home-level, so every entry on the source carries them"
    );
}

#[test]
fn derivation_yields_empty_strings_for_a_behavior_with_no_matching_selection() {
    let coder = BehaviorInfo {
        behavior_id: "did:key:a:coder".to_string(),
        display_name: "Coder".to_string(),
        backend_id: String::new(),
        model_name: String::new(),
        tool_selection_id: "missing-selection".to_string(),
        inference_profile_id: String::new(),
    };
    let derived = derive_directory_entries(
        "did:key:home",
        &[principal("did:key:a", "Amy")],
        &BTreeMap::from([("did:key:a".to_string(), vec![coder])]),
        &BTreeMap::new(),
        &BTreeMap::new(),
        &CatalogOptions::default(),
    );
    let a = &derived["did:key:a"];
    assert_eq!(a.behavior_models, vec![String::new()]);
    assert_eq!(a.behavior_roots, vec![String::new()]);
    assert_eq!(a.behavior_presets, vec![String::new()]);
    assert_eq!(a.behavior_profiles, vec![String::new()]);
}

#[tokio::test]
async fn tick_converges_then_quiesces() {
    let mut settled = empty_entry("did:key:a", "did:key:home", "Amy");
    settled.runtime_state = "starting".to_string();
    let store = DirectoryFixtureStore {
        principals: vec![principal("did:key:a", "Amy")],
        runtimes: BTreeMap::from([(
            "did:key:a".to_string(),
            ("running".to_string(), "2026-07-23T00:00:00Z".to_string()),
        )]),
        entries: Mutex::new(BTreeMap::from([(
            ("did:key:home".to_string(), "did:key:a".to_string()),
            settled,
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
                empty_entry("did:key:a", "did:key:home", "Amy"),
            ),
            (
                ("did:key:home".to_string(), "did:key:b".to_string()),
                empty_entry("did:key:b", "did:key:home", "Bob"),
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
    let foreign = empty_entry("did:key:shared-agent", "did:key:foreign-home", "Foreign");
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
