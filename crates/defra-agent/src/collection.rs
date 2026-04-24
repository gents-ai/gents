//! Typed discriminator for the set of operator-controlled collections.
//!
//! Mirrors the Lean inductive `ApplyReconcile.Collection` in
//! `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`. Any change
//! to the set of variants, their GraphQL names, or their apply-order
//! ranks must be reflected in the Lean module.

use std::fmt;

// PartialOrd/Ord derived for BTreeMap<DocRef, _> use in apply_model; ordering
// is declaration order, NOT apply_order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Collection {
    AgentPrincipal,
    AgentBehavior,
    ToolSelection,
    InferenceBackend,
    InferenceProfile,
    ToolServiceRegistry,
    Task,
    Schedule,
    EventTrigger,
}

impl Collection {
    /// All variants in declaration order. Not sorted by `apply_order()` —
    /// callers that need apply-ordered iteration must sort explicitly.
    pub const ALL: [Collection; 9] = [
        Collection::AgentPrincipal,
        Collection::AgentBehavior,
        Collection::ToolSelection,
        Collection::InferenceBackend,
        Collection::InferenceProfile,
        Collection::ToolServiceRegistry,
        Collection::Task,
        Collection::Schedule,
        Collection::EventTrigger,
    ];

    /// Top-level file name, only for collections that don't use a directory form.
    pub fn file_name(self) -> Option<&'static str> {
        match self {
            Collection::AgentPrincipal => Some("agent-principal.json"),
            _ => None,
        }
    }

    /// Directory name for the per-doc subdirectory form.
    pub fn dir_name(self) -> Option<&'static str> {
        match self {
            Collection::AgentPrincipal => None,
            Collection::AgentBehavior => Some("agent-behaviors"),
            Collection::ToolSelection => Some("tool-selections"),
            Collection::InferenceBackend => Some("inference-backends"),
            Collection::InferenceProfile => Some("inference-profiles"),
            Collection::ToolServiceRegistry => Some("tool-services"),
            Collection::Task => Some("tasks"),
            Collection::Schedule => Some("schedules"),
            // EventTrigger uses underscore (not hyphen) to match the schema
            // field name that originated in PR #68; the inconsistency with
            // other hyphenated dir names is intentional and documented.
            Collection::EventTrigger => Some("event_triggers"),
        }
    }

    /// DefraDB GraphQL type name for this collection.
    pub fn graphql_type(self) -> &'static str {
        match self {
            Collection::AgentPrincipal => "AgentPrincipal",
            Collection::AgentBehavior => "AgentBehavior",
            Collection::ToolSelection => "ToolSelection",
            Collection::InferenceBackend => "InferenceBackend",
            Collection::InferenceProfile => "InferenceProfile",
            Collection::ToolServiceRegistry => "ToolServiceRegistry",
            Collection::Task => "Task",
            Collection::Schedule => "Schedule",
            Collection::EventTrigger => "EventTrigger",
        }
    }

    /// Unique-id field name used in `filter: { <field>: { _eq: ... } }`.
    pub fn unique_field(self) -> &'static str {
        match self {
            Collection::AgentPrincipal => "agent_did",
            Collection::AgentBehavior => "behavior_id",
            Collection::ToolSelection => "selection_id",
            Collection::InferenceBackend => "backend_id",
            Collection::InferenceProfile => "profile_id",
            Collection::ToolServiceRegistry => "service_id",
            Collection::Task => "task_id",
            Collection::Schedule => "schedule_id",
            Collection::EventTrigger => "trigger_id",
        }
    }

    /// Apply ordering rank: lower ranks are written first so referenced
    /// documents exist before referrers. Mirrors
    /// `ApplyReconcile.Collection.applyOrder` in Lean.
    pub fn apply_order(self) -> u8 {
        match self {
            Collection::InferenceBackend
            | Collection::ToolSelection
            | Collection::InferenceProfile
            | Collection::ToolServiceRegistry => 0,
            Collection::AgentBehavior => 1,
            Collection::Task => 2,
            Collection::Schedule => 2,
            Collection::AgentPrincipal => 3,
            Collection::EventTrigger => 3,
        }
    }
}

/// Snake-case plural identifier used as the `ConfigExportBundle` /
/// `DesiredStateManifest` field name for this collection. Note the
/// irregular plural `tool_service_registries` — preserve it when
/// renaming variants.
impl fmt::Display for Collection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Collection::AgentPrincipal => "agent_principal",
            Collection::AgentBehavior => "agent_behaviors",
            Collection::ToolSelection => "tool_selections",
            Collection::InferenceBackend => "inference_backends",
            Collection::InferenceProfile => "inference_profiles",
            Collection::ToolServiceRegistry => "tool_service_registries",
            Collection::Task => "tasks",
            Collection::Schedule => "schedules",
            Collection::EventTrigger => "event_triggers",
        };
        f.write_str(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn all_collections_have_distinct_file_or_dir_names() {
        let names: BTreeSet<&str> = Collection::ALL
            .iter()
            .map(|c| {
                c.file_name()
                    .or(c.dir_name())
                    .expect("every variant has one")
            })
            .collect();
        assert_eq!(names.len(), Collection::ALL.len());
    }

    #[test]
    fn all_collections_have_distinct_graphql_types() {
        let names: BTreeSet<&str> = Collection::ALL.iter().map(|c| c.graphql_type()).collect();
        assert_eq!(names.len(), Collection::ALL.len());
    }

    #[test]
    fn all_collections_have_distinct_display_strings() {
        let names: BTreeSet<String> = Collection::ALL.iter().map(|c| c.to_string()).collect();
        assert_eq!(names.len(), Collection::ALL.len());
    }

    #[test]
    fn canonical_variants_and_ranks() {
        // This list is the Rust side of the parity contract. The Lean
        // inductive `ApplyReconcile.Collection` and the
        // `ApplyReconcile.Collection.applyOrder` function in
        // crates/defra-agent/proofs/Proofs/ApplyReconcile.lean must
        // match this sequence exactly. When you add a variant here, you
        // MUST also:
        //
        // 1. Add the variant to the Lean inductive.
        // 2. Add the variant's rank to Collection.applyOrder in Lean.
        // 3. Update the exhaustive pattern-match example at the bottom
        //    of ApplyReconcile.lean (added in Task A4 alongside this test).
        //
        // Both the Lean build and this test must stay green.
        let canonical: &[(Collection, u8, &str)] = &[
            (Collection::AgentPrincipal, 3, "AgentPrincipal"),
            (Collection::AgentBehavior, 1, "AgentBehavior"),
            (Collection::ToolSelection, 0, "ToolSelection"),
            (Collection::InferenceBackend, 0, "InferenceBackend"),
            (Collection::InferenceProfile, 0, "InferenceProfile"),
            (Collection::ToolServiceRegistry, 0, "ToolServiceRegistry"),
            (Collection::Task, 2, "Task"),
            (Collection::Schedule, 2, "Schedule"),
            (Collection::EventTrigger, 3, "EventTrigger"),
        ];

        // ALL must list every canonical variant exactly once.
        assert_eq!(Collection::ALL.len(), canonical.len());
        for (variant, _, _) in canonical.iter() {
            assert!(
                Collection::ALL.contains(variant),
                "Collection::ALL missing variant {variant:?}; \
                 see ApplyReconcile.lean parity contract"
            );
        }

        // apply_order and graphql_type must match the canonical values.
        for (variant, expected_rank, expected_type) in canonical.iter() {
            assert_eq!(
                variant.apply_order(),
                *expected_rank,
                "Collection::{variant:?}.apply_order() drifted from Lean parity contract"
            );
            assert_eq!(
                variant.graphql_type(),
                *expected_type,
                "Collection::{variant:?}.graphql_type() drifted from Lean parity contract"
            );
        }
    }

    #[test]
    fn exactly_one_of_file_or_dir_name() {
        for variant in Collection::ALL {
            let has_file = variant.file_name().is_some();
            let has_dir = variant.dir_name().is_some();
            assert!(
                has_file ^ has_dir,
                "Collection::{variant:?} must return Some from exactly one of file_name()/dir_name()"
            );
        }
    }

    #[test]
    fn apply_order_puts_referees_before_referrers() {
        assert!(
            Collection::InferenceBackend.apply_order() < Collection::AgentBehavior.apply_order()
        );
        assert!(Collection::ToolSelection.apply_order() < Collection::AgentBehavior.apply_order());
        assert!(
            Collection::InferenceProfile.apply_order() < Collection::AgentBehavior.apply_order()
        );
        assert!(Collection::AgentBehavior.apply_order() < Collection::Task.apply_order());
        assert!(Collection::AgentBehavior.apply_order() < Collection::Schedule.apply_order());
        // Rank-0 members must all agree on rank 0.
        assert_eq!(
            Collection::InferenceBackend.apply_order(),
            Collection::ToolSelection.apply_order(),
        );
        assert_eq!(
            Collection::InferenceBackend.apply_order(),
            Collection::InferenceProfile.apply_order(),
        );
        assert_eq!(
            Collection::InferenceBackend.apply_order(),
            Collection::ToolServiceRegistry.apply_order(),
        );
    }
}
