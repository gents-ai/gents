use anyhow::{anyhow, Result};
use defra_node::EmbeddedNode;

use crate::backend_registry::list_backend_records;
use crate::document_config::{
    ensure_agent_principal, list_agent_behavior_records, list_all_tool_selection_records,
    list_inference_profile_records, list_tool_selection_records, load_tool_selection_record,
    ToolSelectionDocument,
};

use super::{DocumentRecord, DocumentRuntimeView};

pub(crate) async fn load_document_runtime_view(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<DocumentRuntimeView> {
    use crate::document_config::load_agent_principal_record;
    use std::collections::HashMap;

    ensure_agent_principal(node, agent_did).await?;
    let principal = load_agent_principal_record(node, agent_did)
        .await?
        .ok_or_else(|| anyhow!("AgentPrincipal {agent_did} was not persisted"))?;

    let mut view = DocumentRuntimeView {
        principal: DocumentRecord {
            doc_id: principal.0,
            value: principal.1,
        },
        behaviors: HashMap::new(),
        tool_selections: HashMap::new(),
        inference_profiles: HashMap::new(),
        backends: HashMap::new(),
    };

    for (doc_id, selection) in list_tool_selection_records(node, agent_did).await? {
        view.tool_selections.insert(
            selection.selection_id.clone(),
            DocumentRecord {
                doc_id,
                value: selection,
            },
        );
    }

    for (doc_id, profile) in list_inference_profile_records(node).await? {
        view.inference_profiles.insert(
            profile.profile_id.clone(),
            DocumentRecord {
                doc_id,
                value: profile,
            },
        );
    }

    for (doc_id, backend) in list_backend_records(node).await? {
        view.backends.insert(
            backend.backend_id.clone(),
            DocumentRecord {
                doc_id,
                value: backend,
            },
        );
    }

    for (doc_id, behavior) in list_agent_behavior_records(node, agent_did).await? {
        let behavior_id = behavior.behavior_id.clone();
        view.behaviors.insert(
            behavior_id,
            DocumentRecord {
                doc_id,
                value: behavior,
            },
        );
    }

    hydrate_referenced_tool_selections(node, agent_did, &mut view).await?;

    Ok(view)
}

async fn hydrate_referenced_tool_selections(
    node: &EmbeddedNode,
    agent_did: &str,
    view: &mut DocumentRuntimeView,
) -> Result<()> {
    let missing_selection_ids = view
        .behaviors
        .values()
        .filter_map(|record| {
            record
                .value
                .tool_selection_id
                .as_deref()
                .and_then(super::non_empty)
        })
        .filter(|selection_id| !view.tool_selections.contains_key(*selection_id))
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    for selection_id in missing_selection_ids {
        let selection = match load_tool_selection_record(node, &selection_id).await? {
            Some(selection) => selection,
            None => match find_tool_selection_by_scan(node, &selection_id).await? {
                Some(selection) => {
                    tracing::warn!(
                        agent_did = %agent_did,
                        selection_id = %selection_id,
                        "runtime document view recovered referenced tool selection through unfiltered scan"
                    );
                    selection
                }
                None => continue,
            },
        };
        let (doc_id, selection) = selection;
        if selection.agent_did != agent_did {
            tracing::warn!(
                agent_did = %agent_did,
                selection_id = %selection_id,
                selection_agent_did = %selection.agent_did,
                "runtime document view ignored referenced tool selection owned by another agent"
            );
            continue;
        }
        tracing::warn!(
            agent_did = %agent_did,
            selection_id = %selection_id,
            doc_id = %doc_id,
            "runtime document view recovered referenced tool selection missing from agent filter query"
        );
        view.tool_selections.insert(
            selection.selection_id.clone(),
            DocumentRecord {
                doc_id,
                value: selection,
            },
        );
    }

    Ok(())
}

async fn find_tool_selection_by_scan(
    node: &EmbeddedNode,
    selection_id: &str,
) -> Result<Option<(String, ToolSelectionDocument)>> {
    let rows = list_all_tool_selection_records(node).await?;
    let available = rows
        .iter()
        .take(8)
        .map(|(_, selection)| format!("{}@{}", selection.selection_id, selection.agent_did))
        .collect::<Vec<_>>()
        .join(", ");
    let available_count = rows.len();
    let found = rows
        .into_iter()
        .find(|(_, selection)| selection.selection_id == selection_id);
    if found.is_none() {
        tracing::warn!(
            selection_id = %selection_id,
            available_count = available_count,
            available = %available,
            "runtime document view scan did not find referenced tool selection"
        );
    }
    Ok(found)
}
