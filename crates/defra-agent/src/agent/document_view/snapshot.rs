use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use defra_node::EmbeddedNode;

use crate::admission::backend_admission_configs_from_backends;
use crate::config::BehaviorConfig;
use crate::document_config::{default_behavior_id_for_agent, AgentBehavior};
use crate::runtime_snapshot::ResolvedRuntimeSnapshot;
use crate::tool_surface::ToolSelection;

use super::DocumentRuntimeView;

use crate::agent::{
    behavior_config_from_documents, tool_selection_from_document, DocumentResolveContext,
};

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

pub(super) fn collect_unresolved_behavior_references(
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

pub(super) fn behavior_references_ready(
    view: &DocumentRuntimeView,
    behavior: &AgentBehavior,
) -> bool {
    let mut details = Vec::new();
    collect_unresolved_behavior_references(view, behavior, &mut details);
    details.is_empty()
}

pub(super) fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}
