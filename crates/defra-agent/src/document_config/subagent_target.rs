//! Structured delegation target for subagent spawning.
//!
//! A delegation target is a named `(agent_did, behavior_id)` pair with an
//! optional description. The calling model only ever sees/uses the friendly
//! `name`; the runtime maps `name -> (agent_did, behavior_id)` and writes the
//! child `AgentRequest` locally with that `agent_did` + `behavior_id`. If the
//! `agent_did` is remote, out-of-band P2P replication carries the request to the
//! owning node.
//!
//! Each `ToolSelection.subagent_targets` `[String]` entry is the JSON
//! serialization of one [`SubagentTarget`]. We keep using the existing
//! `[String]` field so there is no schema change and no Lean change.

use serde::{Deserialize, Serialize};

/// A named delegation target for subagent spawning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentTarget {
    /// Friendly, model-facing name. The only identifier the model uses.
    pub name: String,
    /// DID of the agent that owns the target behavior. May be local or remote.
    pub agent_did: String,
    /// Behavior id on the owning agent.
    pub behavior_id: String,
    /// Optional human-readable description surfaced to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl SubagentTarget {
    /// Parse a single `subagent_targets` `[String]` entry (JSON) into a
    /// [`SubagentTarget`].
    pub fn parse(entry: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(entry)
    }

    /// Serialize this target into the JSON form stored in a `subagent_targets`
    /// `[String]` entry.
    pub fn to_entry(&self) -> String {
        serde_json::to_string(self).expect("SubagentTarget serializes to JSON")
    }

    /// Returns true when every structural field is non-empty (after trimming).
    pub fn is_structurally_valid(&self) -> bool {
        !self.name.trim().is_empty()
            && !self.agent_did.trim().is_empty()
            && !self.behavior_id.trim().is_empty()
    }

    /// The description text, falling back to an empty string.
    pub fn description_text(&self) -> &str {
        self.description.as_deref().unwrap_or_default()
    }
}

/// Test/apply helper: build a JSON `subagent_targets` entry from parts. Useful
/// for fixtures that want to stay readable.
pub fn subagent_target_entry(
    name: impl Into<String>,
    agent_did: impl Into<String>,
    behavior_id: impl Into<String>,
    description: Option<String>,
) -> String {
    SubagentTarget {
        name: name.into(),
        agent_did: agent_did.into(),
        behavior_id: behavior_id.into(),
        description,
    }
    .to_entry()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json_entry() {
        let target = SubagentTarget {
            name: "researcher".to_string(),
            agent_did: "did:key:zParent".to_string(),
            behavior_id: "amy-research".to_string(),
            description: Some("Does deep research".to_string()),
        };
        let entry = target.to_entry();
        let parsed = SubagentTarget::parse(&entry).unwrap();
        assert_eq!(parsed, target);
    }

    #[test]
    fn description_optional_in_json() {
        let entry = r#"{"name":"a","agent_did":"did:key:z","behavior_id":"b"}"#;
        let parsed = SubagentTarget::parse(entry).unwrap();
        assert_eq!(parsed.description, None);
        assert!(parsed.is_structurally_valid());
    }

    #[test]
    fn rejects_non_json_entry() {
        assert!(SubagentTarget::parse("just-a-behavior-id").is_err());
    }

    #[test]
    fn structural_validity_requires_all_fields() {
        let mut target = SubagentTarget {
            name: "n".to_string(),
            agent_did: "d".to_string(),
            behavior_id: "b".to_string(),
            description: None,
        };
        assert!(target.is_structurally_valid());
        target.name = "  ".to_string();
        assert!(!target.is_structurally_valid());
    }
}
