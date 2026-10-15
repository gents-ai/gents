use std::collections::{BTreeMap, BTreeSet};

use gents::agent::directory_projection::{derive_directory_entries, AgentInfo};

fn node(did: &str, name: &str) -> (String, String, String) {
    (did.to_string(), name.to_string(), String::new())
}

fn node_with_default(did: &str, name: &str, default_agent_id: &str) -> (String, String, String) {
    (
        did.to_string(),
        name.to_string(),
        default_agent_id.to_string(),
    )
}

#[test]
fn derivation_projects_exactly_the_nodes() {
    let coder = AgentInfo {
        agent_id: "did:key:a:coder".to_string(),
        display_name: "Coder".to_string(),
    };
    let artist = AgentInfo {
        agent_id: "did:key:a:artist".to_string(),
        display_name: "Artist".to_string(),
    };

    let derived = derive_directory_entries(
        "did:key:home",
        &[
            node_with_default("did:key:a", "Amy", "did:key:a:coder"),
            node("did:key:b", "Bob"),
        ],
        &BTreeMap::from([("did:key:a".to_string(), vec![coder, artist])]),
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
    assert_eq!(a.agents, vec!["Artist".to_string(), "Coder".to_string()]);
    assert_eq!(
        a.agent_ids,
        vec![
            "did:key:a:artist".to_string(),
            "did:key:a:coder".to_string()
        ],
        "ids must stay index-aligned with display names"
    );
    assert_eq!(
        a.default_agent_id, "did:key:a:coder",
        "default_agent_id must copy through from the node"
    );
    assert_eq!(a.runtime_state, "running");

    let b = &derived["did:key:b"];
    assert!(b.agents.is_empty() && b.agent_ids.is_empty());
    assert_eq!(
        b.default_agent_id, "",
        "a node with no default agent must derive an empty default_agent_id"
    );
    assert_eq!(b.runtime_state, "");
}
