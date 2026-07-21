//! Production bridge from the CLI desired-state manifest to the proven
//! `apply_model` so that config-apply `--prune` deletes live-only documents
//! through the **same** delete-safety logic the Lean `ApplyReconcile` model
//! pins (`t_delete_safety` / `delete_safe`), rather than an ad-hoc path.
//!
//! We build an `apply_model::LiveState` whose per-document `refs` capture the
//! structural cross-document references declared by each config doc, then call
//! `apply_model::diff_prune`. Its `.delete` is exactly the live-only set that
//! no live document references — the proven-safe deletes.
//!
//! ## Reference map (REVIEW THIS — it is the safety boundary)
//! A doc is protected from pruning while any live doc references it. We take a
//! deliberately **conservative superset**: any FK that names another config
//! doc counts, regardless of apply-order rank. `delete_safe` is monotone in
//! refs, so over-inclusion can only ever *refuse* an otherwise-safe delete (the
//! safe direction) — it can never permit deleting a referenced doc.
//!
//! | referrer | field(s) | referee |
//! |---|---|---|
//! | AgentPrincipal | `default_behavior_id` | AgentBehavior |
//! | AgentBehavior | `backend_id` | InferenceBackend |
//! | AgentBehavior | `inference_profile_id` | InferenceProfile |
//! | AgentBehavior | `tool_selection_id` | ToolSelection |
//! | AgentBehavior | `skill_refs[]` | Skill |
//! | Task | `behavior_id` | AgentBehavior |
//! | Schedule | `task_id` | Task |
//! | EventTrigger | `task_id` | Task |
//! | ToolSelection | `allowed_mcp_service_ids[]` | ToolServiceRegistry |
//! | ProjectionAcpBinding | `behavior_id` | AgentBehavior |
//!
//! ### Open questions for review
//! - **Schedule → Task is same-rank** (both apply_order 2), so it is NOT a
//!   `WellFormed` strictly-lower-rank reference in the Lean model. We include it
//!   anyway for runtime safety (don't delete a task a schedule still fires).
//!   `delete_safe` doesn't depend on rank, so this is sound at runtime; it just
//!   means a same-rank protection lives outside the proven WellFormed invariant.
//! - **`Skill.tool_refs[]` is EXCLUDED**: its entries are tool identifiers whose
//!   correspondence to a config doc id is not established here. If they can name
//!   a ToolServiceRegistry, add that edge.
//! - `agent_did` ownership fields are excluded (they name the principal, which
//!   is the apply root, not a prunable lower-rank dependency).

use std::collections::BTreeMap;

use gents::apply_model::{self, DesiredFields, DocRef, LiveState, Manifest};
use gents::Collection;

use super::DesiredStateManifest;

fn doc(collection: Collection, id: &str) -> DocRef {
    DocRef {
        collection,
        id: id.to_string(),
    }
}

/// Build the live `apply_model::LiveState`, projecting structural references
/// (see module docs) into each doc's `refs` so `delete_safe` is faithful.
fn live_state_from_manifest(m: &DesiredStateManifest) -> LiveState {
    let mut desired: BTreeMap<DocRef, DesiredFields> = BTreeMap::new();

    let principal_refs = m
        .agent_principal
        .default_behavior_id
        .as_deref()
        .map(|b| vec![doc(Collection::AgentBehavior, b)])
        .unwrap_or_default();
    desired.insert(
        doc(Collection::AgentPrincipal, &m.agent_principal.agent_did),
        DesiredFields::with_refs("", principal_refs),
    );

    for b in &m.agent_behaviors {
        let mut refs = Vec::new();
        if let Some(x) = &b.backend_id {
            refs.push(doc(Collection::InferenceBackend, x));
        }
        if let Some(x) = &b.inference_profile_id {
            refs.push(doc(Collection::InferenceProfile, x));
        }
        if let Some(x) = &b.tool_selection_id {
            refs.push(doc(Collection::ToolSelection, x));
        }
        for s in &b.skill_refs {
            refs.push(doc(Collection::Skill, s));
        }
        desired.insert(
            doc(Collection::AgentBehavior, &b.behavior_id),
            DesiredFields::with_refs("", refs),
        );
    }

    for t in &m.tasks {
        desired.insert(
            doc(Collection::Task, &t.task_id),
            DesiredFields::with_refs("", vec![doc(Collection::AgentBehavior, &t.behavior_id)]),
        );
    }

    for s in &m.schedules {
        desired.insert(
            doc(Collection::Schedule, &s.schedule_id),
            DesiredFields::with_refs("", vec![doc(Collection::Task, &s.task_id)]),
        );
    }

    for e in &m.event_triggers {
        desired.insert(
            doc(Collection::EventTrigger, &e.trigger_id),
            DesiredFields::with_refs("", vec![doc(Collection::Task, &e.task_id)]),
        );
    }

    for ts in &m.tool_selections {
        let mut refs = Vec::new();
        for sid in &ts.allowed_mcp_service_ids {
            refs.push(doc(Collection::ToolServiceRegistry, sid));
        }
        desired.insert(
            doc(Collection::ToolSelection, &ts.selection_id),
            DesiredFields::with_refs("", refs),
        );
    }

    for binding in &m.projection_acp_bindings {
        let refs = binding
            .behavior_id
            .as_deref()
            .map(|behavior_id| vec![doc(Collection::AgentBehavior, behavior_id)])
            .unwrap_or_default();
        desired.insert(
            doc(Collection::ProjectionAcpBinding, &binding.binding_id),
            DesiredFields::with_refs("", refs),
        );
    }

    // Leaf dependencies (no outgoing structural references for prune safety).
    for s in &m.skills {
        desired.insert(
            doc(Collection::Skill, &s.skill_id),
            DesiredFields::opaque(""),
        );
    }
    for b in &m.inference_backends {
        desired.insert(
            doc(Collection::InferenceBackend, &b.backend_id),
            DesiredFields::opaque(""),
        );
    }
    for p in &m.inference_profiles {
        desired.insert(
            doc(Collection::InferenceProfile, &p.profile_id),
            DesiredFields::opaque(""),
        );
    }
    for r in &m.tool_service_registries {
        desired.insert(
            doc(Collection::ToolServiceRegistry, &r.service_id),
            DesiredFields::opaque(""),
        );
    }

    LiveState {
        desired,
        live: BTreeMap::new(),
    }
}

/// The desired manifest as an `apply_model::Manifest`. Only DocRef identity
/// matters for delete selection (a live doc is prunable iff absent here), so
/// payloads are opaque.
fn manifest_from_desired(m: &DesiredStateManifest) -> Manifest {
    let mut docs: BTreeMap<DocRef, DesiredFields> = BTreeMap::new();
    docs.insert(
        doc(Collection::AgentPrincipal, &m.agent_principal.agent_did),
        DesiredFields::opaque(""),
    );
    for b in &m.agent_behaviors {
        docs.insert(
            doc(Collection::AgentBehavior, &b.behavior_id),
            DesiredFields::opaque(""),
        );
    }
    for s in &m.skills {
        docs.insert(
            doc(Collection::Skill, &s.skill_id),
            DesiredFields::opaque(""),
        );
    }
    for ts in &m.tool_selections {
        docs.insert(
            doc(Collection::ToolSelection, &ts.selection_id),
            DesiredFields::opaque(""),
        );
    }
    for b in &m.inference_backends {
        docs.insert(
            doc(Collection::InferenceBackend, &b.backend_id),
            DesiredFields::opaque(""),
        );
    }
    for p in &m.inference_profiles {
        docs.insert(
            doc(Collection::InferenceProfile, &p.profile_id),
            DesiredFields::opaque(""),
        );
    }
    for r in &m.tool_service_registries {
        docs.insert(
            doc(Collection::ToolServiceRegistry, &r.service_id),
            DesiredFields::opaque(""),
        );
    }
    for t in &m.tasks {
        docs.insert(doc(Collection::Task, &t.task_id), DesiredFields::opaque(""));
    }
    for s in &m.schedules {
        docs.insert(
            doc(Collection::Schedule, &s.schedule_id),
            DesiredFields::opaque(""),
        );
    }
    for e in &m.event_triggers {
        docs.insert(
            doc(Collection::EventTrigger, &e.trigger_id),
            DesiredFields::opaque(""),
        );
    }
    for binding in &m.projection_acp_bindings {
        docs.insert(
            doc(Collection::ProjectionAcpBinding, &binding.binding_id),
            DesiredFields::opaque(""),
        );
    }
    Manifest { docs }
}

/// Proven-safe live-only deletes for pruning, in `apply_model` delete order
/// (reverse apply-order). Routes through `apply_model::diff_prune`, so the
/// selection is identical to what the Lean model and the apply conformance
/// fence prove safe.
pub(crate) fn prune_safe_deletes(
    desired: &DesiredStateManifest,
    live: &DesiredStateManifest,
) -> Vec<DocRef> {
    let m = manifest_from_desired(desired);
    let l = live_state_from_manifest(live);
    apply_model::diff_prune(&m, &l).delete
}
