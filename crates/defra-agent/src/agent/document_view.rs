use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use defra_node::EmbeddedNode;

use crate::admission::backend_admission_configs_from_backends;
use crate::backend_registry::{list_backend_records, lookup_backend_by_doc_id, InferenceBackend};
use crate::config::BehaviorConfig;
use crate::document_config::{
    default_behavior_id_for_agent, ensure_agent_principal, list_agent_behavior_records,
    list_all_tool_selection_records, list_inference_profile_records, list_tool_selection_records,
    load_agent_behavior_by_doc_id, load_agent_principal_by_doc_id, load_agent_principal_record,
    load_inference_profile_by_doc_id, load_tool_selection_by_doc_id, load_tool_selection_record,
    AgentBehavior, AgentPrincipal, InferenceProfile, ToolSelectionDocument,
};
use crate::runtime_snapshot::ResolvedRuntimeSnapshot;
use crate::tool_surface::ToolSelection;

use super::{behavior_config_from_documents, tool_selection_from_document, DocumentResolveContext};

#[derive(Debug, Clone)]
pub(crate) struct DocumentRecord<T> {
    pub(crate) doc_id: String,
    pub(crate) value: T,
}

#[derive(Debug, Clone)]
pub(crate) struct DocumentRuntimeView {
    pub(crate) principal: DocumentRecord<AgentPrincipal>,
    pub(crate) behaviors: HashMap<String, DocumentRecord<AgentBehavior>>,
    pub(crate) tool_selections: HashMap<String, DocumentRecord<ToolSelectionDocument>>,
    pub(crate) inference_profiles: HashMap<String, DocumentRecord<InferenceProfile>>,
    pub(crate) backends: HashMap<String, DocumentRecord<InferenceBackend>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlUpdateOutcome {
    Irrelevant,
    Applied,
    PendingVisibility,
}

impl DocumentRuntimeView {
    fn has_behavior_doc_id(&self, doc_id: &str) -> bool {
        self.behaviors
            .values()
            .any(|record| record.doc_id == doc_id)
    }

    fn has_tool_selection_doc_id(&self, doc_id: &str) -> bool {
        self.tool_selections
            .values()
            .any(|record| record.doc_id == doc_id)
    }

    fn has_inference_profile_doc_id(&self, doc_id: &str) -> bool {
        self.inference_profiles
            .values()
            .any(|record| record.doc_id == doc_id)
    }

    fn has_backend_doc_id(&self, doc_id: &str) -> bool {
        self.backends.values().any(|record| record.doc_id == doc_id)
    }

    fn remove_behavior_by_doc_id(&mut self, doc_id: &str) -> bool {
        let key = self.behaviors.iter().find_map(|(behavior_id, record)| {
            (record.doc_id == doc_id).then_some(behavior_id.clone())
        });
        key.is_some_and(|behavior_id| self.behaviors.remove(&behavior_id).is_some())
    }

    fn remove_tool_selection_by_doc_id(&mut self, doc_id: &str) -> bool {
        let key = self
            .tool_selections
            .iter()
            .find_map(|(selection_id, record)| {
                (record.doc_id == doc_id).then_some(selection_id.clone())
            });
        key.is_some_and(|selection_id| self.tool_selections.remove(&selection_id).is_some())
    }

    fn remove_inference_profile_by_doc_id(&mut self, doc_id: &str) -> bool {
        let key = self
            .inference_profiles
            .iter()
            .find_map(|(profile_id, record)| {
                (record.doc_id == doc_id).then_some(profile_id.clone())
            });
        key.is_some_and(|profile_id| self.inference_profiles.remove(&profile_id).is_some())
    }

    fn remove_backend_by_doc_id(&mut self, doc_id: &str) -> bool {
        let key = self.backends.iter().find_map(|(backend_id, record)| {
            (record.doc_id == doc_id).then_some(backend_id.clone())
        });
        key.is_some_and(|backend_id| self.backends.remove(&backend_id).is_some())
    }

    fn references_profile(&self, profile_id: &str) -> bool {
        self.behaviors.values().any(|record| {
            record
                .value
                .inference_profile_id
                .as_deref()
                .is_some_and(|id| id == profile_id)
        })
    }

    fn references_backend(&self, backend_id: &str) -> bool {
        self.behaviors.values().any(|record| {
            record
                .value
                .backend_id
                .as_deref()
                .is_some_and(|id| id == backend_id)
        })
    }

    pub(crate) fn has_unresolved_behavior_references(&self) -> bool {
        !self.pending_visibility_details().is_empty()
    }

    pub(crate) fn pending_visibility_details(&self) -> Vec<String> {
        let mut details = Vec::new();

        if let Some(default_behavior_id) = self
            .principal
            .value
            .default_behavior_id
            .as_deref()
            .and_then(non_empty)
        {
            if !self.behaviors.contains_key(default_behavior_id) {
                details.push(format!(
                    "principal {} references missing default behavior {}",
                    self.principal.value.agent_did, default_behavior_id
                ));
            }
        }

        for record in self.behaviors.values() {
            collect_unresolved_behavior_references(self, &record.value, &mut details);
        }

        details.sort();
        details
    }
}

pub(crate) async fn load_document_runtime_view(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<DocumentRuntimeView> {
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
                .and_then(non_empty)
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

pub(crate) async fn resolve_document_runtime_snapshot_from_view(
    node: &EmbeddedNode,
    context: &DocumentResolveContext,
    view: &DocumentRuntimeView,
) -> Result<ResolvedRuntimeSnapshot> {
    if !view.principal.value.enabled {
        anyhow::bail!(
            "agent principal {} is disabled",
            view.principal.value.agent_did
        );
    }

    let default_behavior_id = view
        .principal
        .value
        .default_behavior_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_behavior_id_for_agent(context.identity.did()));

    let mut behaviors = Vec::<Arc<BehaviorConfig>>::new();
    let mut tool_surfaces = HashMap::new();
    let mut unavailable_behaviors = HashMap::new();

    for behavior_record in view.behaviors.values() {
        let behavior = &behavior_record.value;
        if !behavior.enabled {
            unavailable_behaviors.insert(
                behavior.behavior_id.clone(),
                format!("behavior {} is disabled", behavior.behavior_id),
            );
            continue;
        }

        let backend = match behavior
            .backend_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            Some(backend_id) => view
                .backends
                .get(backend_id)
                .map(|record| &record.value)
                .ok_or_else(|| {
                    anyhow!(
                        "behavior {} references missing backend {}",
                        behavior.behavior_id,
                        backend_id
                    )
                }),
            None => Err(anyhow!(
                "behavior {} has no backend binding",
                behavior.behavior_id
            )),
        };

        let resolved = async {
            let backend = backend?;
            if !backend.is_available() {
                anyhow::bail!(
                    "behavior {} backend {} is unavailable (enabled={} probe_status={})",
                    behavior.behavior_id,
                    backend.backend_id,
                    backend.enabled,
                    backend.probe_status
                );
            }
            let inference_profile = behavior
                .inference_profile_id
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(|profile_id| {
                    view.inference_profiles
                        .get(profile_id)
                        .map(|record| &record.value)
                        .ok_or_else(|| {
                            anyhow!(
                                "behavior {} references missing inference profile {}",
                                behavior.behavior_id,
                                profile_id
                            )
                        })
                })
                .transpose()?;
            let tool_selection = match behavior
                .tool_selection_id
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                Some(selection_id) => match view.tool_selections.get(selection_id) {
                    Some(record) => tool_selection_from_document(&record.value)?,
                    None => anyhow::bail!(
                        "behavior {} references missing tool selection {}",
                        behavior.behavior_id,
                        selection_id
                    ),
                },
                None => ToolSelection::default(),
            };

            let behavior_config = behavior_config_from_documents(
                context.identity.clone(),
                behavior,
                backend,
                inference_profile,
                tool_selection,
                &context.tool_ceiling,
            )?;
            let behavior = Arc::new(behavior_config);
            let tool_surface = Arc::new(behavior.tools.resolve(node).await?);
            Ok::<_, anyhow::Error>((behavior, tool_surface))
        }
        .await;

        match resolved {
            Ok((behavior_config, tool_surface)) => {
                tool_surfaces.insert(behavior_config.name.clone(), tool_surface);
                behaviors.push(behavior_config);
            }
            Err(error) => {
                unavailable_behaviors.insert(behavior.behavior_id.clone(), error.to_string());
            }
        }
    }

    let backend_admission_configs = backend_admission_configs_from_backends(
        view.backends.values().map(|record| &record.value),
    )?;

    Ok(ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
        default_behavior_id,
        behaviors,
        tool_surfaces,
        backend_admission_configs,
        unavailable_behaviors,
    ))
}

fn collect_unresolved_behavior_references(
    view: &DocumentRuntimeView,
    behavior: &AgentBehavior,
    details: &mut Vec<String>,
) {
    if let Some(selection_id) = behavior.tool_selection_id.as_deref().and_then(non_empty) {
        if !view.tool_selections.contains_key(selection_id) {
            details.push(format!(
                "behavior {} references missing tool selection {}",
                behavior.behavior_id, selection_id
            ));
        }
    }

    if let Some(profile_id) = behavior.inference_profile_id.as_deref().and_then(non_empty) {
        if !view.inference_profiles.contains_key(profile_id) {
            details.push(format!(
                "behavior {} references missing inference profile {}",
                behavior.behavior_id, profile_id
            ));
        }
    }

    if let Some(backend_id) = behavior.backend_id.as_deref().and_then(non_empty) {
        if !view.backends.contains_key(backend_id) {
            details.push(format!(
                "behavior {} references missing backend {}",
                behavior.behavior_id, backend_id
            ));
        }
    }
}

fn behavior_references_ready(view: &DocumentRuntimeView, behavior: &AgentBehavior) -> bool {
    let mut details = Vec::new();
    collect_unresolved_behavior_references(view, behavior, &mut details);
    details.is_empty()
}

fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

#[cfg(test)]
mod tests;
