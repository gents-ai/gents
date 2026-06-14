//! Scope-template catalog for P2P filtered replication.
//!
//! A `ScopeTemplate` is a named pairing intent: a fixed collection set, a
//! `Scope` (how per-peer document filtering is derived), and a `Delivery`
//! (push vs. replicate).  The catalog is static and hardcoded here; later
//! tasks will wire it into the pairing reconciler and defradb.rs #1033.
//!
//! `PairingFilters` is our own seam type that decouples this crate from the
//! unmerged defradb.rs #1033 filter API.  It holds per-collection single-field
//! equality predicates and can be translated by later tasks into whatever
//! upstream shape emerges.

use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Delivery mode for a template pairing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// Caller pushes documents to the peer.
    Push,
    /// Bidirectional replication.
    Replicate,
}

/// Scoping policy for per-peer document filtering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Filter each collection on a single field that must equal the peer's
    /// `agent_did`.
    PeerDid {
        /// The field name on each collection document.
        field: &'static str,
    },
    /// No per-peer filtering — replicate all documents in the collection set.
    Unscoped,
}

/// A named pairing intent in the static catalog.
#[derive(Debug, Clone)]
pub struct ScopeTemplate {
    /// Stable identifier used to look up the template from CLI args or config.
    pub id: &'static str,
    /// The exact collection names included in this template.
    pub collections: &'static [&'static str],
    /// How to derive per-peer document filters.
    pub scope: Scope,
    /// Delivery mode for this pairing.
    pub delivery: Delivery,
}

// ---------------------------------------------------------------------------
// PairingFilters seam type (#1033-independent)
// ---------------------------------------------------------------------------

/// A single-field equality predicate for one collection.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FilterPredicate {
    /// The field name to filter on.
    pub field: String,
    /// The value the field must equal.
    pub value: String,
}

/// Per-collection filter predicates for a concrete pairing.
///
/// `key` = collection name, `value` = equality predicate to apply when
/// subscribing / pushing documents for that collection.  An empty map means
/// no filtering (Unscoped).
///
/// This type is our own seam that later tasks will translate into whatever
/// shape defradb.rs #1033 exposes.
pub type PairingFilters = BTreeMap<String, FilterPredicate>;

// ---------------------------------------------------------------------------
// Built-in template catalog
// ---------------------------------------------------------------------------

/// Conversation collections: all request/response/turn artifacts, scoped by
/// the peer's agent DID.  CodexThreadProjection is deliberately excluded
/// because it is a denormalised projection and should not be replicated raw.
const CONVERSATION_COLLECTIONS: &[&str] = &[
    "AgentRequest",
    "AgentResponse",
    "AgentMessage",
    "AgentToolCall",
    "AgentToolResult",
    "AgentSession",
    "AgentConversation",
    "CompactionEntry",
];

/// Agent-config collections: behavior + tool configuration.  Unscoped because
/// the operator wants the full config set replicated, not per-peer slices.
const AGENT_CONFIG_COLLECTIONS: &[&str] = &[
    "AgentBehavior",
    "ToolSelection",
    "InferenceBackend",
    "InferenceProfile",
    "ToolServiceRegistry",
    "Skill",
];

static BUILTIN_TEMPLATES: &[ScopeTemplate] = &[
    ScopeTemplate {
        id: "conversation",
        collections: CONVERSATION_COLLECTIONS,
        scope: Scope::PeerDid { field: "agent_did" },
        delivery: Delivery::Push,
    },
    ScopeTemplate {
        id: "agent-config",
        collections: AGENT_CONFIG_COLLECTIONS,
        scope: Scope::Unscoped,
        delivery: Delivery::Replicate,
    },
    ScopeTemplate {
        id: "backup",
        collections: CONVERSATION_COLLECTIONS,
        scope: Scope::Unscoped,
        delivery: Delivery::Replicate,
    },
];

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Return all built-in templates in catalog order.
pub fn builtin_templates() -> &'static [ScopeTemplate] {
    BUILTIN_TEMPLATES
}

/// Look up a template by id.  Returns `None` for unknown ids.
pub fn resolve_template(id: &str) -> Option<&'static ScopeTemplate> {
    BUILTIN_TEMPLATES.iter().find(|t| t.id == id)
}

/// Build per-collection `PairingFilters` for a template scope against a
/// concrete peer DID.
///
/// - `Scope::PeerDid { field }` → for each collection, insert a predicate
///   `{ field, value: peer_did }`.
/// - `Scope::Unscoped` → empty map (no filtering).
pub fn scope_filter(scope: &Scope, collections: &[&str], peer_did: &str) -> PairingFilters {
    match scope {
        Scope::PeerDid { field } => collections
            .iter()
            .map(|&col| {
                (
                    col.to_string(),
                    FilterPredicate {
                        field: (*field).to_string(),
                        value: peer_did.to_string(),
                    },
                )
            })
            .collect(),
        Scope::Unscoped => BTreeMap::new(),
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_is_scoped_push_with_eight_collections() {
        let t = resolve_template("conversation").unwrap();
        assert_eq!(t.delivery, Delivery::Push);
        assert!(matches!(t.scope, Scope::PeerDid { field } if field == "agent_did"));
        assert_eq!(t.collections.len(), 8);
        assert!(t.collections.contains(&"AgentRequest"));
        assert!(!t.collections.contains(&"CodexThreadProjection"));
    }

    #[test]
    fn agent_config_includes_behavior_excludes_principal() {
        let t = resolve_template("agent-config").unwrap();
        assert_eq!(t.delivery, Delivery::Replicate);
        assert!(matches!(t.scope, Scope::Unscoped));
        assert!(t.collections.contains(&"AgentBehavior"));
        assert!(!t.collections.contains(&"AgentPrincipal"));
    }

    #[test]
    fn backup_is_unscoped_replicate() {
        let t = resolve_template("backup").unwrap();
        assert!(matches!(t.scope, Scope::Unscoped));
        assert_eq!(t.delivery, Delivery::Replicate);
    }

    #[test]
    fn scope_filter_builds_per_collection_agent_did_equality() {
        let t = resolve_template("conversation").unwrap();
        let f = scope_filter(&t.scope, t.collections, "did:key:bob");
        assert_eq!(f.len(), 8);
        let p = f.get("AgentRequest").unwrap();
        assert_eq!(p.field, "agent_did");
        assert_eq!(p.value, "did:key:bob");
    }

    #[test]
    fn unscoped_scope_filter_is_empty() {
        let t = resolve_template("backup").unwrap();
        assert!(scope_filter(&t.scope, t.collections, "did:key:bob").is_empty());
    }

    #[test]
    fn unknown_template_is_none() {
        assert!(resolve_template("nope").is_none());
    }

    // Additional coverage
    #[test]
    fn all_builtin_templates_have_nonempty_collections() {
        for t in builtin_templates() {
            assert!(
                !t.collections.is_empty(),
                "template {} has no collections",
                t.id
            );
        }
    }

    #[test]
    fn builtin_template_count_is_three() {
        assert_eq!(builtin_templates().len(), 3);
    }

    #[test]
    fn scope_filter_covers_all_collections_in_template() {
        let t = resolve_template("conversation").unwrap();
        let f = scope_filter(&t.scope, t.collections, "did:key:alice");
        for col in t.collections {
            assert!(f.contains_key(*col), "missing filter for {col}");
        }
    }
}
