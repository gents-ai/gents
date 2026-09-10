use super::automation::resolve_automation;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use defra_node::EmbeddedNode;
use gents_protocol::row::BehaviorReadinessUnavailableReason;

use crate::admission::BackendAvailability;
use crate::config::AgentBehavior;
use crate::document_config::AgentBehavior as AgentBehaviorDocument;
use crate::runtime_snapshot::{ResolvedRuntimeSnapshot, UnavailableBehavior};
use crate::tool_surface::ToolSelection;

use super::DocumentRuntimeView;

use crate::agent::{
    assemble_principal_and_behaviors, behavior_config_from_documents, tool_selection_from_document,
    BehaviorBuildError, DocumentResolveContext,
};
use crate::identity::AgentPrincipal;
use crate::tool_surface::SubagentToolConfig;

// The view is already scoped; check again at reference resolution so a foreign
// record cannot satisfy a reference even in an independently constructed view.
macro_rules! owned_doc {
    ($map:expr, $id:expr, $owner:expr) => {{
        (|| -> anyhow::Result<_> {
            let id = $id;
            anyhow::ensure!(
                !id.trim().is_empty(),
                "explicit configuration reference is blank"
            );
            let record = $map
                .get(id)
                .ok_or_else(|| anyhow!("missing {} reference {id:?}", stringify!($map)))?;
            anyhow::ensure!(
                record.value.agent_did == $owner,
                "foreign configuration reference {id:?}"
            );
            Ok(&record.value)
        })()
    }};
}

struct BehaviorResolutionError {
    code: BehaviorReadinessUnavailableReason,
    detail: anyhow::Error,
}

impl BehaviorResolutionError {
    fn new(code: BehaviorReadinessUnavailableReason, detail: anyhow::Error) -> Self {
        Self { code, detail }
    }
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
        .unwrap_or_default();

    let principal_data = AgentPrincipal {
        agent_did: view.principal.value.agent_did.clone(),
        identity: context.identity.clone(),
        default_behavior_id: default_behavior_id.clone(),
        display_name: view.principal.value.display_name.clone(),
        enabled: view.principal.value.enabled,
    };

    let measured_vetoed = context.backend_health.vetoed_backend_ids().await;
    let mut backend_admission_configs = HashMap::new();
    for record in view.backends.values() {
        let backend = &record.value;
        if let Some(observation) = view.backend_observations.get(&backend.backend_id) {
            backend_admission_configs.insert(
                backend.backend_id.clone(),
                crate::admission::BackendAdmissionConfig::from_backend(backend, observation)?
                    .with_measured_unhealthy(measured_vetoed.contains(&backend.backend_id)),
            );
        }
    }

    let mut unavailable_behaviors = HashMap::new();
    let mut behavior_factories: Vec<
        Box<
            dyn FnOnce(
                    Arc<AgentPrincipal>,
                ) -> std::result::Result<AgentBehavior, BehaviorBuildError>
                + Send,
        >,
    > = Vec::new();

    let all_skills = sorted_skills(view);

    for behavior_record in view.behaviors.values() {
        let behavior = &behavior_record.value;
        if !behavior.enabled {
            unavailable_behaviors.insert(
                behavior.behavior_id.clone(),
                UnavailableBehavior::new(
                    BehaviorReadinessUnavailableReason::BehaviorDisabled,
                    format!("behavior {} is disabled", behavior.behavior_id),
                ),
            );
            continue;
        }

        let resolved_result: std::result::Result<_, BehaviorResolutionError> = (|| {
            let scope = view.principal.value.agent_did.as_str();
            let inference =
                resolve_inference(view, &behavior.inference_profile_id).map_err(|error| {
                    BehaviorResolutionError::new(
                        BehaviorReadinessUnavailableReason::InferenceProfileInvalid,
                        error,
                    )
                })?;
            ensure_inference_available(view, &inference, &backend_admission_configs)?;
            let context_result: Result<_> = (|| {
                anyhow::ensure!(behavior.agent_did == scope, "behavior owner mismatch");
                let context = behavior
                    .context_id
                    .as_deref()
                    .map(|id| owned_doc!(&view.contexts, id, scope))
                    .transpose()?
                    .cloned();
                if let Some(context) = &context {
                    for id in &context.skill_ids {
                        owned_doc!(&view.skills, id.as_str(), scope)?;
                    }
                }
                let compaction = context
                    .as_ref()
                    .and_then(|context| context.compaction_id.as_deref())
                    .map(|id| owned_doc!(&view.compactions, id, scope))
                    .transpose()?
                    .cloned();
                let summary = compaction
                    .as_ref()
                    .and_then(|config| config.inference_profile_id.as_deref())
                    .map(|id| resolve_inference(view, id))
                    .transpose()?;
                if let Some(summary) = &summary {
                    ensure_inference_available(view, summary, &backend_admission_configs)
                        .map_err(|error| error.detail)?;
                }
                let tools = context
                    .as_ref()
                    .and_then(|context| context.tools_id.as_deref())
                    .map(|id| owned_doc!(&view.tools, id, scope))
                    .transpose()?;
                let (tool_selection, subagents) = match tools {
                    None => (ToolSelection::default(), SubagentToolConfig::default()),
                    Some(tools) => {
                        tools.validate()?;
                        if let Some(remote) = &tools.remote {
                            for service in &remote.services {
                                owned_doc!(
                                    &view.tool_services,
                                    service.mcp_service_id.as_str(),
                                    scope
                                )?;
                            }
                        }

                        let mut selected = tool_selection_from_document(tools)?;
                        let merged = super::merge_surface_tools(tools, view)?;
                        selected.query_tools = merged.query_tools;
                        // Canonical surfaces own all datastore write declarations.
                        selected.write_tools = merged.write_tools;
                        let eth = super::expand_eth_tools(tools, view)?;
                        selected.eth_queries = eth.queries;
                        selected.eth_calls = eth.calls;
                        let subagents =
                            crate::tool_surface::SubagentToolConfig::from_document_with_targets(
                                tools,
                                view.subagent_targets.values().map(|record| &record.value),
                            )?;
                        for target in &subagents.targets {
                            if target.target_agent_did == scope {
                                owned_doc!(&view.behaviors, target.behavior_id.as_str(), scope)?;
                            }
                        }
                        (selected, subagents)
                    }
                };
                Ok((context, compaction, summary, tool_selection, subagents))
            })();
            let (context, compaction, summary, tools, subagents) =
                context_result.map_err(|error| {
                    BehaviorResolutionError::new(
                        BehaviorReadinessUnavailableReason::ToolConfigurationInvalid,
                        error,
                    )
                })?;
            Ok((inference, context, compaction, summary, tools, subagents))
        })();

        match resolved_result {
            Ok((
                inference,
                resolved_context,
                compaction,
                summary,
                tool_selection,
                subagent_tools,
            )) => {
                let behavior_id = behavior.behavior_id.clone();
                let behavior_value = behavior.clone();
                let tool_ceiling = context.tool_ceiling.clone();
                let skill_ids = resolved_context
                    .as_ref()
                    .map(|context| context.skill_ids.as_slice())
                    .unwrap_or(&[]);
                let behavior_skills =
                    crate::skills::effective_skills(&all_skills, &behavior.agent_did, skill_ids)
                        .into_iter()
                        .cloned()
                        .collect::<Vec<_>>();
                let factory: Box<
                    dyn FnOnce(
                            Arc<AgentPrincipal>,
                        )
                            -> std::result::Result<AgentBehavior, BehaviorBuildError>
                        + Send,
                > = Box::new(move |principal| {
                    behavior_config_from_documents(
                        principal,
                        &behavior_value,
                        resolved_context.as_ref(),
                        compaction,
                        summary,
                        &inference,
                        tool_selection,
                        subagent_tools,
                        &tool_ceiling,
                        behavior_skills,
                    )
                    .map_err(|error| BehaviorBuildError {
                        behavior_id: behavior_id.clone(),
                        error,
                    })
                });
                behavior_factories.push(factory);
            }
            Err(error) => {
                unavailable_behaviors.insert(
                    behavior.behavior_id.clone(),
                    UnavailableBehavior::new(error.code, error.detail.to_string()),
                );
            }
        }
    }

    let (principal, behavior_results) =
        assemble_principal_and_behaviors(principal_data, behavior_factories);

    let mut behaviors = Vec::<Arc<AgentBehavior>>::new();
    for result in behavior_results {
        match result {
            Ok(behavior_arc) => behaviors.push(behavior_arc),
            Err(BehaviorBuildError { behavior_id, error }) => {
                unavailable_behaviors.insert(
                    behavior_id,
                    UnavailableBehavior::new(
                        BehaviorReadinessUnavailableReason::RuntimeConfigurationInvalid,
                        error.to_string(),
                    ),
                );
            }
        }
    }

    let own_agent_did = context.identity.did().to_string();
    let candidate_behavior_ids = behaviors
        .iter()
        .map(|behavior| behavior.behavior_id.clone())
        .collect::<HashSet<_>>();
    let mut behavior_surfaces = Vec::with_capacity(behaviors.len());
    for behavior in behaviors {
        match behavior
            .tools
            .resolve_with_available_subagent_targets(node, &own_agent_did, &candidate_behavior_ids)
            .await
        {
            Ok(tool_surface) => behavior_surfaces.push((behavior, tool_surface)),
            Err(error) => {
                unavailable_behaviors.insert(
                    behavior.behavior_id.clone(),
                    UnavailableBehavior::new(
                        BehaviorReadinessUnavailableReason::ToolSurfaceUnavailable,
                        error.to_string(),
                    ),
                );
            }
        }
    }

    let active_behavior_ids = behavior_surfaces
        .iter()
        .map(|(behavior, _)| behavior.behavior_id.clone())
        .collect::<HashSet<_>>();
    let mut behaviors = Vec::with_capacity(behavior_surfaces.len());
    let mut tool_surfaces = HashMap::with_capacity(behavior_surfaces.len());
    for (behavior, mut tool_surface) in behavior_surfaces {
        for target in tool_surface.subagent_targets() {
            if target.target_agent_did == own_agent_did
                && !active_behavior_ids.contains(&target.behavior_id)
            {
                tracing::warn!(
                    behavior_id = %behavior.behavior_id,
                    target_name = %target.name,
                    target_behavior_id = %target.behavior_id,
                    "dropping LOCAL subagent target: target behavior is not active \
                     (behavior may be disabled or its backend/MCP resolution failed)"
                );
            }
        }
        tool_surface.retain_subagent_targets(&own_agent_did, &active_behavior_ids);
        tool_surfaces.insert(behavior.behavior_id.clone(), Arc::new(tool_surface));
        behaviors.push(behavior);
    }

    let automation = resolve_automation(view, &unavailable_behaviors);
    Ok(ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
        default_behavior_id,
        behaviors,
        tool_surfaces,
        backend_admission_configs,
        unavailable_behaviors,
    )
    .with_principal(principal)
    .with_local_did(context.identity.did().to_string())
    .with_automation(automation))
}

fn ensure_inference_available(
    view: &DocumentRuntimeView,
    inference: &crate::config::ResolvedInference,
    admission_configs: &HashMap<String, crate::admission::BackendAdmissionConfig>,
) -> std::result::Result<(), BehaviorResolutionError> {
    let scope = view.principal.value.agent_did.as_str();
    let backend = &inference.backend;
    let admission = admission_configs.get(&backend.backend_id).ok_or_else(|| {
        BehaviorResolutionError::new(
            BehaviorReadinessUnavailableReason::BackendNotConfigured,
            anyhow!("backend {} has no matching observation", backend.backend_id),
        )
    })?;
    let unavailable = match admission.availability() {
        BackendAvailability::Available => None,
        BackendAvailability::Disabled => Some(BehaviorReadinessUnavailableReason::BackendDisabled),
        BackendAvailability::ProbeNotHealthy | BackendAvailability::MeasuredUnhealthy => {
            Some(BehaviorReadinessUnavailableReason::BackendTemporarilyUnavailable)
        }
    };
    if let Some(code) = unavailable {
        return Err(BehaviorResolutionError::new(
            code,
            anyhow!(
                "backend {} is unavailable: {:?}",
                backend.backend_id,
                admission.availability()
            ),
        ));
    }
    if matches!(
        backend.auth,
        crate::document_config::BackendAuth::PrincipalOAuth
    ) {
        let provider = match backend.provider_kind {
            crate::backend_provider::BackendProviderKind::ChatGptCodex => {
                crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER
            }
            crate::backend_provider::BackendProviderKind::XaiGrokOAuth => {
                crate::xai_grok_oauth::XAI_OAUTH_PROVIDER
            }
            crate::backend_provider::BackendProviderKind::ClaudeCliSubscription => {
                crate::claude_oauth::CLAUDE_OAUTH_PROVIDER
            }
            _ => {
                return Err(BehaviorResolutionError::new(
                    BehaviorReadinessUnavailableReason::CredentialsRequired,
                    anyhow!("provider has no principal OAuth adapter"),
                ))
            }
        };
        if !view.has_enabled_oauth_credential(provider) {
            return Err(BehaviorResolutionError::new(
                BehaviorReadinessUnavailableReason::CredentialsRequired,
                anyhow!(
                    "backend {} requires enabled OAuthCredential for {scope}",
                    backend.backend_id
                ),
            ));
        }
    }
    Ok(())
}

pub(super) fn collect_unresolved_behavior_references(
    view: &DocumentRuntimeView,
    behavior: &AgentBehaviorDocument,
    details: &mut Vec<String>,
) {
    let scope = view.principal.value.agent_did.as_str();
    let result: Result<()> = (|| {
        anyhow::ensure!(behavior.agent_did == scope, "behavior owner mismatch");
        resolve_inference(view, &behavior.inference_profile_id)?;
        if let Some(id) = &behavior.context_id {
            let context = owned_doc!(&view.contexts, id.as_str(), scope)?;
            if let Some(id) = &context.compaction_id {
                let compaction = owned_doc!(&view.compactions, id.as_str(), scope)?;
                if let Some(id) = &compaction.inference_profile_id {
                    resolve_inference(view, id)?;
                }
            }
            if let Some(id) = &context.tools_id {
                owned_doc!(&view.tools, id.as_str(), scope)?;
            }
            for id in &context.skill_ids {
                owned_doc!(&view.skills, id.as_str(), scope)?;
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        details.push(format!("behavior {}: {error:#}", behavior.behavior_id));
    }
}

fn resolve_inference(
    view: &DocumentRuntimeView,
    id: &str,
) -> Result<crate::config::ResolvedInference> {
    let scope = view.principal.value.agent_did.as_str();
    let profile = owned_doc!(&view.inference_profiles, id, scope)?.clone();
    let backend = owned_doc!(&view.backends, profile.backend_id.as_str(), scope)?.clone();
    anyhow::ensure!(
        !profile.model_name.trim().is_empty(),
        "profile {} has no model selection",
        profile.profile_id
    );
    let sampling = profile
        .sampling_id
        .as_deref()
        .map(|id| owned_doc!(&view.inference_sampling, id, scope))
        .transpose()?
        .cloned();
    let execution = profile
        .execution_id
        .as_deref()
        .map(|id| owned_doc!(&view.inference_execution, id, scope))
        .transpose()?
        .cloned();
    let retry_policy = execution
        .as_ref()
        .and_then(|execution| execution.retry_policy_id.as_deref())
        .map(|id| owned_doc!(&view.inference_retry_policies, id, scope))
        .transpose()?
        .cloned();
    let credential_scope = matches!(
        backend.auth,
        crate::document_config::BackendAuth::PrincipalOAuth
    )
    .then_some(scope);
    let catalog = view
        .backend_observations
        .get(&backend.backend_id)
        .filter(|observation| observation.backend_id == backend.backend_id)
        .map(|observation| observation.catalog_for(credential_scope))
        .transpose()?
        .flatten();
    let advertised_model = if let Some(catalog) = catalog {
        let mut models = catalog
            .models
            .iter()
            .filter(|model| model.model_name == profile.model_name);
        let model = models.next().ok_or_else(|| {
            anyhow!(
                "model {} is not advertised by backend {} in the selected credential scope",
                profile.model_name,
                backend.backend_id
            )
        })?;
        anyhow::ensure!(
            models.next().is_none(),
            "ambiguous advertised model {}",
            profile.model_name
        );
        if let (Some(effort), Some(supported)) =
            (profile.reasoning_effort, model.reasoning_efforts.as_ref())
        {
            anyhow::ensure!(
                supported.contains(&effort),
                "model {} does not advertise selected reasoning effort {effort:?}",
                profile.model_name
            );
        }
        Some(model.clone())
    } else {
        None
    };
    let resolved = crate::config::ResolvedInference {
        backend,
        profile,
        sampling,
        execution,
        retry_policy,
        advertised_model,
    };
    resolved.backend.validate()?;
    resolved.profile.validate()?;
    if let Some(sampling) = &resolved.sampling {
        sampling.validate()?;
    }
    if let Some(execution) = &resolved.execution {
        execution.validate()?;
    }
    if let Some(retry) = &resolved.retry_policy {
        retry.validate()?;
    }
    resolved.context_window()?;
    resolved.max_turns()?;
    resolved.sampling_config()?;
    Ok(resolved)
}

pub(super) fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

// The runtime configuration fingerprint is compared across independently
// resolved views. Every collection map is keyed/sorted by the projector;
// skills are the one value vector embedded in AgentBehavior's Debug value, so
// canonicalize it before both prompt construction and fingerprinting.
pub(super) fn sorted_skills(view: &DocumentRuntimeView) -> Vec<crate::skills::Skill> {
    let mut skills = view
        .skills
        .values()
        .map(|record| skill_from_document(&record.value))
        .collect::<Vec<_>>();
    skills.sort_by(|left, right| left.skill_id.cmp(&right.skill_id));
    skills
}

fn skill_from_document(doc: &crate::document_config::SkillDocument) -> crate::skills::Skill {
    crate::skills::Skill {
        skill_id: doc.skill_id.clone(),
        agent_did: doc.agent_did.clone(),
        name: doc.name.clone().unwrap_or_default(),
        description: doc.description.clone().unwrap_or_default(),
        instructions: doc.instructions.clone().unwrap_or_default(),
        tool_refs: doc.tool_refs.clone(),
        display_name: doc.display_name.clone(),
        enabled: doc.enabled,
    }
}
