use anyhow::Result;
use defra_node::EmbeddedNode;

use crate::backend_registry::lookup_backend_by_doc_id;
use crate::document_config::{
    load_agent_behavior_by_doc_id, load_agent_principal_by_doc_id,
    load_inference_profile_by_doc_id, load_tool_selection_by_doc_id,
};

use super::snapshot::behavior_references_ready;
use super::{ControlUpdateOutcome, DocumentRecord, DocumentRuntimeView};

pub(crate) async fn apply_control_update(
    node: &EmbeddedNode,
    agent_did: &str,
    _collection_id: &str,
    doc_id: &str,
    view: &mut DocumentRuntimeView,
) -> Result<ControlUpdateOutcome> {
    if let Some((loaded_doc_id, principal)) = load_agent_principal_by_doc_id(node, doc_id).await? {
        if principal.agent_did != agent_did {
            return Ok(ControlUpdateOutcome::Irrelevant);
        }
        let default_behavior_visible = principal
            .default_behavior_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .is_none_or(|behavior_id| view.behaviors.contains_key(behavior_id));
        view.principal = DocumentRecord {
            doc_id: loaded_doc_id,
            value: principal,
        };
        return Ok(if default_behavior_visible {
            ControlUpdateOutcome::Applied
        } else {
            ControlUpdateOutcome::PendingVisibility
        });
    }
    if view.principal.doc_id == doc_id {
        return Ok(ControlUpdateOutcome::PendingVisibility);
    }

    if let Some((loaded_doc_id, behavior)) = load_agent_behavior_by_doc_id(node, doc_id).await? {
        if behavior.agent_did != agent_did {
            return Ok(ControlUpdateOutcome::Irrelevant);
        }
        if !behavior_references_ready(view, &behavior) {
            return Ok(ControlUpdateOutcome::PendingVisibility);
        }
        view.remove_behavior_by_doc_id(doc_id);
        view.behaviors.insert(
            behavior.behavior_id.clone(),
            DocumentRecord {
                doc_id: loaded_doc_id,
                value: behavior,
            },
        );
        return Ok(ControlUpdateOutcome::Applied);
    }
    if view.has_behavior_doc_id(doc_id) {
        return Ok(ControlUpdateOutcome::PendingVisibility);
    }

    if let Some((loaded_doc_id, selection)) = load_tool_selection_by_doc_id(node, doc_id).await? {
        if selection.agent_did != agent_did {
            return Ok(ControlUpdateOutcome::Irrelevant);
        }
        view.remove_tool_selection_by_doc_id(doc_id);
        view.tool_selections.insert(
            selection.selection_id.clone(),
            DocumentRecord {
                doc_id: loaded_doc_id,
                value: selection,
            },
        );
        return Ok(ControlUpdateOutcome::Applied);
    }
    if view.has_tool_selection_doc_id(doc_id) {
        return Ok(ControlUpdateOutcome::PendingVisibility);
    }

    if let Some((loaded_doc_id, profile)) = load_inference_profile_by_doc_id(node, doc_id).await? {
        if !view.references_profile(&profile.profile_id)
            && !view.has_inference_profile_doc_id(doc_id)
        {
            return Ok(ControlUpdateOutcome::Irrelevant);
        }
        view.remove_inference_profile_by_doc_id(doc_id);
        view.inference_profiles.insert(
            profile.profile_id.clone(),
            DocumentRecord {
                doc_id: loaded_doc_id,
                value: profile,
            },
        );
        return Ok(ControlUpdateOutcome::Applied);
    }
    if view.has_inference_profile_doc_id(doc_id) {
        return Ok(ControlUpdateOutcome::PendingVisibility);
    }

    if let Some((loaded_doc_id, backend)) = lookup_backend_by_doc_id(node, doc_id).await? {
        if !view.references_backend(&backend.backend_id) && !view.has_backend_doc_id(doc_id) {
            return Ok(ControlUpdateOutcome::Irrelevant);
        }
        view.remove_backend_by_doc_id(doc_id);
        view.backends.insert(
            backend.backend_id.clone(),
            DocumentRecord {
                doc_id: loaded_doc_id,
                value: backend,
            },
        );
        return Ok(ControlUpdateOutcome::Applied);
    }
    if view.has_backend_doc_id(doc_id) {
        return Ok(ControlUpdateOutcome::PendingVisibility);
    }

    Ok(ControlUpdateOutcome::Irrelevant)
}
