//! Typed discriminator for the set of operator-controlled collections.
//!
//! Mirrors the Lean inductive `ApplyReconcile.Collection` in
//! `crates/defra-agent/proofs/Proofs/ApplyReconcile.lean`. Any change
//! to the set of variants, their GraphQL names, or their apply-order
//! ranks must be reflected in the Lean module.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Collection {
    AgentPrincipal,
    AgentBehavior,
    ToolSelection,
    InferenceBackend,
    InferenceProfile,
    ToolServiceRegistry,
    ScheduledTask,
}

impl Collection {
    /// All variants in declaration order. Not sorted by `apply_order()` —
    /// callers that need apply-ordered iteration must sort explicitly.
    pub(crate) const ALL: [Collection; 7] = [
        Collection::AgentPrincipal,
        Collection::AgentBehavior,
        Collection::ToolSelection,
        Collection::InferenceBackend,
        Collection::InferenceProfile,
        Collection::ToolServiceRegistry,
        Collection::ScheduledTask,
    ];

    /// Manifest file name on disk for the single-file form.
    pub(crate) fn file_name(self) -> &'static str {
        match self {
            Collection::AgentPrincipal => "agent-principal.json",
            Collection::AgentBehavior => "agent-behaviors.json",
            Collection::ToolSelection => "tool-selections.json",
            Collection::InferenceBackend => "inference-backends.json",
            Collection::InferenceProfile => "inference-profiles.json",
            Collection::ToolServiceRegistry => "tool-services.json",
            Collection::ScheduledTask => "scheduled-tasks.json",
        }
    }

    /// Manifest directory name (for collections that support a per-doc dir form).
    pub(crate) fn dir_name(self) -> Option<&'static str> {
        match self {
            Collection::ToolServiceRegistry => Some("tool-services"),
            Collection::ScheduledTask => Some("scheduled-tasks"),
            _ => None,
        }
    }

    /// DefraDB GraphQL type name for this collection.
    pub(crate) fn graphql_type(self) -> &'static str {
        match self {
            Collection::AgentPrincipal => "AgentPrincipal",
            Collection::AgentBehavior => "AgentBehavior",
            Collection::ToolSelection => "ToolSelection",
            Collection::InferenceBackend => "InferenceBackend",
            Collection::InferenceProfile => "InferenceProfile",
            Collection::ToolServiceRegistry => "ToolServiceRegistry",
            Collection::ScheduledTask => "ScheduledTask",
        }
    }

    /// Unique-id field name used in `filter: { <field>: { _eq: ... } }`.
    pub(crate) fn unique_field(self) -> &'static str {
        match self {
            Collection::AgentPrincipal => "agent_did",
            Collection::AgentBehavior => "behavior_id",
            Collection::ToolSelection => "selection_id",
            Collection::InferenceBackend => "backend_id",
            Collection::InferenceProfile => "profile_id",
            Collection::ToolServiceRegistry => "service_id",
            Collection::ScheduledTask => "task_id",
        }
    }

    /// Apply ordering rank: lower ranks are written first so referenced
    /// documents exist before referrers. Mirrors
    /// `ApplyReconcile.Collection.applyOrder` in Lean.
    pub(crate) fn apply_order(self) -> u8 {
        match self {
            Collection::InferenceBackend
            | Collection::ToolSelection
            | Collection::InferenceProfile
            | Collection::ToolServiceRegistry => 0,
            Collection::AgentPrincipal => 1,
            Collection::AgentBehavior => 2,
            Collection::ScheduledTask => 3,
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
            Collection::ScheduledTask => "scheduled_tasks",
        };
        f.write_str(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn all_collections_have_distinct_file_names() {
        let names: BTreeSet<&str> =
            Collection::ALL.iter().map(|c| c.file_name()).collect();
        assert_eq!(names.len(), Collection::ALL.len());
    }

    #[test]
    fn all_collections_have_distinct_graphql_types() {
        let names: BTreeSet<&str> =
            Collection::ALL.iter().map(|c| c.graphql_type()).collect();
        assert_eq!(names.len(), Collection::ALL.len());
    }

    #[test]
    fn all_collections_have_distinct_display_strings() {
        let names: BTreeSet<String> =
            Collection::ALL.iter().map(|c| c.to_string()).collect();
        assert_eq!(names.len(), Collection::ALL.len());
    }

    #[test]
    fn apply_order_puts_referees_before_referrers() {
        assert!(
            Collection::InferenceBackend.apply_order()
                < Collection::AgentBehavior.apply_order()
        );
        assert!(
            Collection::ToolSelection.apply_order()
                < Collection::AgentBehavior.apply_order()
        );
        assert!(
            Collection::InferenceProfile.apply_order()
                < Collection::AgentBehavior.apply_order()
        );
        assert!(
            Collection::AgentBehavior.apply_order()
                < Collection::ScheduledTask.apply_order()
        );
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
