use super::automation::resolve_automation;
use rapidhash::{HashMapExt, RapidHashMap};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use defra_node::Node as EmbeddedNode;
use gents_protocol::node_readiness::AgentReadinessUnavailableReason;

use gents_automation::triggers::runtime_snapshot::{ResolvedRuntimeSnapshot, UnavailableAgent};
use gents_documents::Agent as AgentDocument;
use gents_inference::admission::BackendAvailability;
use gents_tools::surface::AgentWithTools;

use super::DocumentRuntimeView;

use crate::runtime::agent::{
    agent_config_from_documents, assemble_node_and_agents, AgentBuildError, AgentDocumentInputs,
    AgentFactory, DocumentResolveContext,
};
use gents_store::identity::RuntimeNode;

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
                record.value.node_did == $owner,
                "foreign configuration reference {id:?}"
            );
            Ok(&record.value)
        })()
    }};
}

/// An agent_config on a backend this build cannot run is unavailable, not
/// pending: no arriving document repairs it.
#[derive(Debug, thiserror::Error)]
#[error("backend {backend_id} has provider kind {kind} this build does not know")]
struct UnknownKindBackend {
    backend_id: String,
    kind: String,
}

#[derive(Debug, thiserror::Error)]
enum InferenceSelectionError {
    #[error(transparent)]
    UnknownKind(#[from] UnknownKindBackend),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// First-run setup must remain reachable without a model. Only an explicitly
/// disabled backend permits that unfinished selection; it supplies no runnable
/// agent and becomes invalid if enabled before model selection.
#[derive(Debug, thiserror::Error)]
#[error("disabled backend {backend_id} awaits model selection")]
struct UnselectedDisabledBackend {
    backend_id: String,
}

struct AgentResolutionError {
    code: AgentReadinessUnavailableReason,
    detail: anyhow::Error,
}

impl AgentResolutionError {
    fn new(code: AgentReadinessUnavailableReason, detail: anyhow::Error) -> Self {
        Self { code, detail }
    }
}

mod resolution;

pub(crate) async fn resolve_document_runtime_snapshot_from_view(
    node: &Arc<EmbeddedNode>,
    context: &DocumentResolveContext,
    view: &DocumentRuntimeView,
) -> Result<ResolvedRuntimeSnapshot> {
    if !view.node.value.enabled {
        anyhow::bail!("node {} is disabled", view.node.value.node_did);
    }

    let default_agent_did = view
        .default_agent_did()
        .map(ToOwned::to_owned)
        .unwrap_or_default();

    let node_data = RuntimeNode {
        node_did: view.node.value.node_did.clone(),
        identity: context.identity.clone(),
        default_agent_did: default_agent_did.clone(),
        display_name: view.node.value.display_name.clone(),
        enabled: view.node.value.enabled,
    };

    let (backend_admission_configs, measured_health) =
        resolution::backend_admission_configs(context, view).await?;
    let (mut unavailable_agents, agent_factories) = resolve_agent_factories(
        node,
        context,
        view,
        &backend_admission_configs,
        &measured_health,
    )?;

    let (principal, agent_results) = assemble_node_and_agents(node_data, agent_factories);

    let mut agents = Vec::<AgentWithTools>::new();
    for result in agent_results {
        match result {
            Ok(agent) => agents.push(agent),
            Err(AgentBuildError { agent_did, error }) => {
                unavailable_agents.insert(
                    agent_did,
                    UnavailableAgent::new(
                        AgentReadinessUnavailableReason::RuntimeConfigurationInvalid,
                        error.to_string(),
                    ),
                );
            }
        }
    }

    let own_node_did = context.identity.did().to_string();
    let behavior_surfaces = resolution::resolve_behavior_surfaces(
        node,
        context,
        &own_node_did,
        agents,
        &mut unavailable_agents,
    )
    .await;
    let (agents, tool_surfaces) =
        resolution::retain_active_agent_targets(&own_node_did, behavior_surfaces);
    resolution::attach_watcher_identities(context, view, &mut unavailable_agents);

    let automation = resolve_automation(view, &unavailable_agents);
    Ok(ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
        default_agent_did,
        agents,
        tool_surfaces,
        backend_admission_configs,
        unavailable_agents,
    )
    .with_node(principal)
    .with_local_did(context.identity.did().to_string())
    .with_automation(automation))
}

fn resolve_agent_factories(
    node: &Arc<EmbeddedNode>,
    context: &DocumentResolveContext,
    view: &DocumentRuntimeView,
    backend_admission_configs: &RapidHashMap<
        String,
        gents_inference::admission::BackendAdmissionConfig,
    >,
    measured_health: &RapidHashMap<
        String,
        gents_inference::backends::health::BackendHealthSnapshot,
    >,
) -> Result<(RapidHashMap<String, UnavailableAgent>, Vec<AgentFactory>)> {
    let mut unavailable_agents = RapidHashMap::new();
    let mut agent_factories: Vec<AgentFactory> = Vec::new();

    let all_skills = sorted_skills(view);

    for behavior_record in view.agents.values() {
        let agent_config = &behavior_record.value;
        if !agent_config.enabled {
            unavailable_agents.insert(
                agent_config.agent_did.clone(),
                UnavailableAgent::new(
                    AgentReadinessUnavailableReason::AgentDisabled,
                    format!("agent {} is disabled", agent_config.agent_did),
                ),
            );
            continue;
        }

        let resolved_result: std::result::Result<_, AgentResolutionError> = (|| {
            let inference =
                resolve_inference(view, &agent_config.inference_profile_id).map_err(|error| {
                    AgentResolutionError::new(
                        if error.is::<UnselectedDisabledBackend>() {
                            AgentReadinessUnavailableReason::BackendDisabled
                        } else {
                            AgentReadinessUnavailableReason::InferenceProfileInvalid
                        },
                        error,
                    )
                })?;
            ensure_inference_available(
                view,
                &inference,
                backend_admission_configs,
                measured_health,
            )?;
            let (context, compaction, summary, tools, agents) =
                resolution::agent_context_documents(
                    view,
                    backend_admission_configs,
                    measured_health,
                    agent_config,
                )
                .map_err(|error| {
                    AgentResolutionError::new(
                        AgentReadinessUnavailableReason::ToolConfigurationInvalid,
                        error,
                    )
                })?;
            Ok((inference, context, compaction, summary, tools, agents))
        })();

        match resolved_result {
            Ok((inference, resolved_context, compaction, summary, tool_selection, agent_tools)) => {
                let agent_did = agent_config.agent_did.clone();
                let behavior_value = agent_config.clone();
                let configuration_documents = resolution::agent_configuration_documents(
                    view,
                    behavior_record,
                    &inference,
                    summary.as_ref(),
                    resolved_context.as_ref(),
                )?;
                let configuration_node = Arc::clone(node);
                let tool_ceiling = context.tool_ceiling.clone();
                let skill_ids = resolved_context
                    .as_ref()
                    .map(|context| context.skill_ids.as_slice())
                    .unwrap_or(&[]);
                let oauth_pool = view.oauth_pool_ids(&inference.backend);
                let behavior_skills = gents_documents::skills::effective_skills(
                    &all_skills,
                    &agent_config.node_did,
                    skill_ids,
                )
                .into_iter()
                .cloned()
                .collect::<Vec<_>>();
                let factory: AgentFactory = Box::new(move |principal| {
                    agent_config_from_documents(
                        principal,
                        AgentDocumentInputs {
                            agent_config: &behavior_value,
                            node: configuration_node,
                            configuration_documents,
                            context: resolved_context.as_ref(),
                            compaction,
                            compaction_inference: summary,
                            inference: &inference,
                            tool_selection,
                            agent_tools,
                            tool_ceiling: &tool_ceiling,
                            skills: behavior_skills,
                            oauth_pool,
                        },
                    )
                    .map_err(|error| AgentBuildError {
                        agent_did: agent_did.clone(),
                        error,
                    })
                });
                agent_factories.push(factory);
            }
            Err(error) => {
                unavailable_agents.insert(
                    agent_config.agent_did.clone(),
                    UnavailableAgent::new(error.code, error.detail.to_string()),
                );
            }
        }
    }
    Ok((unavailable_agents, agent_factories))
}

fn ensure_inference_available(
    view: &DocumentRuntimeView,
    inference: &gents_inference::resolved::ResolvedInference,
    admission_configs: &RapidHashMap<String, gents_inference::admission::BackendAdmissionConfig>,
    measured_health: &RapidHashMap<
        String,
        gents_inference::backends::health::BackendHealthSnapshot,
    >,
) -> std::result::Result<(), AgentResolutionError> {
    let scope = view.node.value.node_did.as_str();
    let backend = &inference.backend;
    let admission = admission_configs.get(&backend.backend_id).ok_or_else(|| {
        AgentResolutionError::new(
            AgentReadinessUnavailableReason::BackendNotConfigured,
            anyhow!("backend {} has no matching observation", backend.backend_id),
        )
    })?;
    let unavailable = match admission.availability() {
        BackendAvailability::Available => None,
        BackendAvailability::Disabled => Some(AgentReadinessUnavailableReason::BackendDisabled),
        BackendAvailability::ProbeNotHealthy | BackendAvailability::MeasuredUnhealthy => Some(
            measured_health
                .get(&backend.backend_id)
                .and_then(|health| health.state_for(backend))
                .and_then(|state| state.readiness_reason())
                .unwrap_or(AgentReadinessUnavailableReason::BackendTemporarilyUnavailable),
        ),
    };
    if admission.availability() != BackendAvailability::Disabled && backend.auth.uses_node_oauth() {
        let provider = match backend.provider_kind {
            gents_inference::backends::provider::BackendProviderKind::ChatGptCodex => {
                gents_inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER
            }
            gents_inference::backends::provider::BackendProviderKind::XaiGrokOAuth => {
                gents_inference::clients::xai_grok::XAI_OAUTH_PROVIDER
            }
            gents_inference::backends::provider::BackendProviderKind::ClaudeCliSubscription => {
                gents_inference::auth::claude::CLAUDE_OAUTH_PROVIDER
            }
            _ => {
                return Err(AgentResolutionError::new(
                    AgentReadinessUnavailableReason::CredentialsRequired,
                    anyhow!("provider has no node OAuth adapter"),
                ));
            }
        };
        let pick = gents_inference::auth::credential::account_pick(&backend.auth).unwrap_or(
            gents_inference::auth::credential::AccountPick::Reference(None),
        );
        if !view.resolves_oauth_credential(provider, pick) {
            return Err(AgentResolutionError::new(
                AgentReadinessUnavailableReason::CredentialsRequired,
                anyhow!(
                    "backend {} requires enabled OAuthCredential for {scope}",
                    backend.backend_id
                ),
            ));
        }
    }
    if let Some(code) = unavailable {
        return Err(AgentResolutionError::new(
            code,
            anyhow!(
                "backend {} is unavailable: {:?}",
                backend.backend_id,
                admission.availability()
            ),
        ));
    }
    Ok(())
}

pub(super) fn collect_unresolved_behavior_references(
    view: &DocumentRuntimeView,
    agent_config: &AgentDocument,
    details: &mut Vec<String>,
) {
    let scope = view.node.value.node_did.as_str();
    let result: std::result::Result<(), InferenceSelectionError> = (|| {
        if agent_config.node_did != scope {
            return Err(anyhow!("agent owner mismatch").into());
        }
        select_inference_documents(view, &agent_config.inference_profile_id)?;
        if let Some(id) = &agent_config.context_id {
            let context = owned_doc!(&view.contexts, id.as_str(), scope)?;
            if let Some(id) = &context.compaction_id {
                let compaction = owned_doc!(&view.compactions, id.as_str(), scope)?;
                if let Some(id) = &compaction.inference_profile_id {
                    select_inference_documents(view, id)?;
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
    if let Err(InferenceSelectionError::Other(error)) = result {
        details.push(format!("agent {}: {error:#}", agent_config.agent_did));
    }
}

fn append_inference_selection(
    view: &DocumentRuntimeView,
    inference: &gents_inference::resolved::ResolvedInference,
    documents: &mut Vec<gents_inference::resolved::SelectedConfigurationDocument>,
) -> Result<()> {
    use gents_inference::resolved::SelectedConfigurationDocument;
    macro_rules! append {
        ($map:expr, $id:expr, $collection:expr) => {{
            let record = $map
                .get($id)
                .ok_or_else(|| anyhow!("resolved inference document disappeared"))?;
            documents.push(SelectedConfigurationDocument {
                collection: $collection,
                doc_id: record.doc_id.clone(),
                logical_id: None,
            });
        }};
    }
    append!(
        view.inference_profiles,
        &inference.profile.profile_id,
        gents_store::collection::Collection::InferenceProfile
    );
    append!(
        view.backends,
        &inference.backend.backend_id,
        gents_store::collection::Collection::InferenceBackend
    );
    if let Some(sampling) = &inference.sampling {
        append!(
            view.inference_sampling,
            &sampling.sampling_id,
            gents_store::collection::Collection::InferenceSampling
        );
    }
    if let Some(execution) = &inference.execution {
        append!(
            view.inference_execution,
            &execution.execution_id,
            gents_store::collection::Collection::InferenceExecution
        );
    }
    if let Some(retry) = &inference.retry_policy {
        append!(
            view.inference_retry_policies,
            &retry.retry_policy_id,
            gents_store::collection::Collection::InferenceRetryPolicy
        );
    }
    Ok(())
}

struct SelectedInferenceDocuments<'a> {
    profile: &'a gents_documents::InferenceProfile,
    backend: &'a gents_documents::InferenceBackend,
    sampling: Option<&'a gents_documents::InferenceSampling>,
    execution: Option<&'a gents_documents::InferenceExecution>,
    retry_policy: Option<&'a gents_documents::InferenceRetryPolicy>,
}

/// Resolves the documents one inference selection names, without judging
/// them: only a failure here can be repaired by a document arriving, so this
/// is what the control watcher's visibility gate may wait on; the deeper
/// validation in `resolve_inference` is reported per agent as an
/// `UnavailableAgent` instead of blocking.
fn select_inference_documents<'a>(
    view: &'a DocumentRuntimeView,
    id: &str,
) -> std::result::Result<SelectedInferenceDocuments<'a>, InferenceSelectionError> {
    let scope = view.node.value.node_did.as_str();
    let profile = owned_doc!(&view.inference_profiles, id, scope)?;
    if let Some(kind) = view.unknown_kind_backends.get(&profile.backend_id) {
        return Err(UnknownKindBackend {
            backend_id: profile.backend_id.clone(),
            kind: kind.clone(),
        }
        .into());
    }
    let backend = owned_doc!(&view.backends, profile.backend_id.as_str(), scope)?;
    let sampling = profile
        .sampling_id
        .as_deref()
        .map(|id| owned_doc!(&view.inference_sampling, id, scope))
        .transpose()?;
    let execution = profile
        .execution_id
        .as_deref()
        .map(|id| owned_doc!(&view.inference_execution, id, scope))
        .transpose()?;
    let retry_policy = execution
        .and_then(|execution| execution.retry_policy_id.as_deref())
        .map(|id| owned_doc!(&view.inference_retry_policies, id, scope))
        .transpose()?;
    Ok(SelectedInferenceDocuments {
        profile,
        backend,
        sampling,
        execution,
        retry_policy,
    })
}

fn resolve_inference(
    view: &DocumentRuntimeView,
    id: &str,
) -> Result<gents_inference::resolved::ResolvedInference> {
    let selected = select_inference_documents(view, id)?;
    let profile = selected.profile.clone();
    let backend = selected.backend.clone();
    if profile.model_name.trim().is_empty() && !backend.enabled {
        return Err(UnselectedDisabledBackend {
            backend_id: backend.backend_id,
        }
        .into());
    }
    anyhow::ensure!(
        !profile.model_name.trim().is_empty(),
        "profile {} has no model selection",
        profile.profile_id
    );
    let sampling = selected.sampling.cloned();
    let execution = selected.execution.cloned();
    let retry_policy = selected.retry_policy.cloned();
    let advertised_model = gents_inference::resolved::advertised_model_for_profile(
        &backend,
        &profile,
        view.backend_observations.get(&backend.backend_id),
    )?;
    let resolved = gents_inference::resolved::ResolvedInference {
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

// Skills are the one value vector embedded in ResolvedAgent's Debug value;
// canonicalize before both prompt construction and fingerprinting.
pub(super) fn sorted_skills(view: &DocumentRuntimeView) -> Vec<gents_documents::skills::Skill> {
    let mut skills = view
        .skills
        .values()
        .map(|record| skill_from_document(&record.value))
        .collect::<Vec<_>>();
    skills.sort_by(|left, right| left.skill_id.cmp(&right.skill_id));
    skills
}

fn skill_from_document(doc: &gents_documents::SkillDocument) -> gents_documents::skills::Skill {
    gents_documents::skills::Skill {
        skill_id: doc.skill_id.clone(),
        node_did: doc.node_did.clone(),
        name: doc.name.clone().unwrap_or_default(),
        description: doc.description.clone().unwrap_or_default(),
        instructions: doc.instructions.clone().unwrap_or_default(),
        source_directory: doc.source_directory.clone(),
        tool_refs: doc.tool_refs.clone(),
        display_name: doc.display_name.clone(),
        enabled: doc.enabled,
    }
}

#[cfg(test)]
mod advertised_context_override_tests {
    use gents_inference::resolved::validate_advertised_context_override;

    #[test]
    fn context_override_admission_matches_lean() {
        use gents_lean_vocab::families::configuration_scope_context_bounds as family;

        let limit = |value: &Option<family::ContextLimit>| {
            value.as_ref().map(|limit| {
                let magnitude = limit.magnitude as i64;
                if limit.negative {
                    -magnitude
                } else {
                    magnitude
                }
            })
        };
        family::replay(|i| {
            let mut selected = profile(1);
            selected.context_window = limit(&i.selected);
            let model = advertised(limit(&i.model_default), limit(&i.maximum));
            validate_advertised_context_override(&selected, &model).is_ok()
        });
    }

    fn profile(context_window: i64) -> gents_documents::InferenceProfile {
        serde_json::from_value(serde_json::json!({
            "node_did": "did:test:owner",
            "profile_id": "profile",
            "backend_id": "backend",
            "model_name": "gpt-5.6-sol",
            "context_window": context_window
        }))
        .unwrap()
    }

    fn advertised(default: Option<i64>, maximum: Option<i64>) -> gents_documents::AdvertisedModel {
        gents_documents::AdvertisedModel {
            model_name: "gpt-5.6-sol".into(),
            display_name: None,
            context_window: default,
            max_context_window: maximum,
            max_output_tokens: None,
            reasoning_efforts: None,
        }
    }
}
