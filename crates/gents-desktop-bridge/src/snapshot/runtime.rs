use std::collections::HashMap;

use chrono::Utc;
use gents::{BashMode, FileToolMode};
use gents_desktop_core::client::{ClientCore, ClientPeerStatus};
use gents_protocol::row::{
    AgentBehaviorReadinessRow, BehaviorReadinessUnknownReason, ProjectedBehaviorReadiness,
};

use super::super::types::{
    normalize_optional, turn_state_label, AgentContext, AgentPrincipalView,
    BehaviorEnvironmentView, BehaviorReadinessSourceView, BehaviorReadinessStatusView,
    BehaviorReadinessUnknownReasonView, BehaviorReadinessView, BehaviorUnavailableReasonView,
    BehaviorView, ClientRouteStatusView, DeploymentView, DesktopRuntimeSnapshot,
    InferenceBackendView, InferenceProfile, MailboxItemView, RuntimeView, SessionSummary,
    SkillView, TaskView, Tools, TriggerView,
};
use super::runtime_tasks::{
    recent_runs_for_task_views, session_summaries, source_matches_agent, task_run_history,
};
use super::to_health_view;

pub async fn build_runtime_snapshot(core: &ClientCore) -> DesktopRuntimeSnapshot {
    // Directory rows and transport status come from one watched revision.
    // Reading `core.peer_records()` here would reintroduce a split sample where
    // a readiness write wakes the bridge before the rebuilt view can see it.
    let sync_state = core.sync_state();
    let store = core.store().snapshot();
    let enrollment_requests = core
        .active_status_enrollment_requests()
        .await
        .map(|requests| requests.into_iter().map(Into::into).collect())
        .ok();
    let peer_statuses_by_id: HashMap<String, ClientPeerStatus> = sync_state
        .peers
        .iter()
        .cloned()
        .map(|status| (status.peer_id.clone(), status))
        .collect();

    let mut deployments = sync_state
        .directory
        .clone()
        .into_iter()
        .map(|peer| {
            let status = peer_statuses_by_id.get(&peer.peer_id);
            let require_source_scope = peer.is_enrollment()
                || peer
                    .graphql
                    .as_deref()
                    .is_some_and(|graphql| !graphql.trim().is_empty());
            let principal = store
                .agent_principals
                .iter()
                .find(|row| row.agent_did == peer.agent_did);
            let mailbox_items = store
                .mailbox_items
                .iter()
                .filter(|row| {
                    row.agent_did == peer.agent_did
                        && row.requester_did == core.principal().did()
                        && row.status == "open"
                })
                .map(MailboxItemView::from)
                .collect::<Vec<_>>();
            let mut agent_principal = principal
                .map(|row| AgentPrincipalView {
                    agent_did: row.agent_did.clone(),
                    display_name: normalize_optional(row.display_name.as_deref()),
                    default_behavior_id: normalize_optional(row.default_behavior_id.as_deref()),
                    enabled: Some(row.enabled),
                    created_at: normalize_optional(row.created_at.as_deref()),
                    created_by: normalize_optional(row.created_by.as_deref()),
                })
                .unwrap_or_else(|| AgentPrincipalView {
                    agent_did: peer.agent_did.clone(),
                    display_name: None,
                    default_behavior_id: None,
                    enabled: None,
                    created_at: None,
                    created_by: None,
                });
            let mut principal_config = principal.cloned();
            let mut behavior_configs = store
                .behaviors
                .iter()
                .filter(|row| row.agent_did == peer.agent_did)
                .cloned()
                .collect::<Vec<_>>();
            behavior_configs.sort_by(|left, right| left.behavior_id.cmp(&right.behavior_id));
            let mut default_behavior_id = store
                .default_behavior_id_for_agent(&peer.agent_did)
                .map(str::to_owned);
            let mut runtime = store
                .latest_runtime(&peer.agent_did)
                .map(|row| RuntimeView {
                    reconcile_phase: normalize_optional(row.reconcile_phase.as_deref()),
                    last_reconcile_result: normalize_optional(row.last_reconcile_result.as_deref()),
                    last_reconcile_error: normalize_optional(row.last_reconcile_error.as_deref()),
                    updated_at: normalize_optional(row.updated_at.as_deref()),
                    behavior_executor_capacity: row.behavior_executor_capacity,
                    behavior_executor_queue_depth: row.behavior_executor_queue_depth,
                });

            let mut behaviors = store
                .behavior_rows(&peer.agent_did)
                .into_iter()
                .map(|row| BehaviorView {
                    behavior_id: row.behavior_id.clone(),
                    display_name: normalize_optional(row.display_name.as_deref())
                        .unwrap_or_else(|| row.behavior_id.clone()),
                    agent_did: row.agent_did.clone(),
                    description: row.description.clone(),
                    context_id: row.context_id.clone(),
                    inference_profile_id: Some(row.inference_profile_id.clone()),
                    enabled: row.enabled,
                    is_default: default_behavior_id.as_deref() == Some(row.behavior_id.as_str()),
                    tags: row.tags.clone(),
                    created_at: row.created_at.clone(),
                })
                .collect::<Vec<_>>();
            behaviors.sort_by(|left, right| {
                right
                    .is_default
                    .cmp(&left.is_default)
                    .then_with(|| left.display_name.cmp(&right.display_name))
            });
            let behavior_ids = behaviors
                .iter()
                .map(|behavior| behavior.behavior_id.as_str())
                .collect::<Vec<_>>();
            let mut inference_backends = store
                .inference_backends
                .iter()
                .filter(|row| row.agent_did == peer.agent_did)
                .map(|row| {
                    let observation = store
                        .backend_observations
                        .iter()
                        .enumerate()
                        .find(|(index, observation)| {
                            observation.backend_id == row.backend_id
                                && source_matches_agent(
                                    &store.backend_observation_source_agent_dids,
                                    *index,
                                    &row.agent_did,
                                    true,
                                )
                        })
                        .map(|(_, observation)| observation);
                    backend_config_view(row, observation)
                })
                .collect::<Vec<_>>();
            inference_backends.sort_by(|left, right| left.backend_id.cmp(&right.backend_id));

            let mut inference_profiles = store
                .inference_profiles
                .iter()
                .enumerate()
                .filter(|(index, row)| {
                    row.agent_did == peer.agent_did
                        && source_matches_agent(
                            &store.inference_profile_source_agent_dids,
                            *index,
                            &peer.agent_did,
                            false,
                        )
                })
                .map(|(_, row)| row.clone())
                .collect::<Vec<_>>();
            inference_profiles.sort_by(|left, right| left.profile_id.cmp(&right.profile_id));
            let mut inference_sampling = store
                .inference_sampling
                .iter()
                .filter(|row| row.agent_did == peer.agent_did)
                .cloned()
                .collect::<Vec<_>>();
            inference_sampling.sort_by(|left, right| left.sampling_id.cmp(&right.sampling_id));
            let mut inference_execution = store
                .inference_execution
                .iter()
                .filter(|row| row.agent_did == peer.agent_did)
                .cloned()
                .collect::<Vec<_>>();
            inference_execution.sort_by(|left, right| left.execution_id.cmp(&right.execution_id));
            let mut contexts = store
                .contexts
                .iter()
                .filter(|row| row.agent_did == peer.agent_did)
                .cloned()
                .collect::<Vec<_>>();
            contexts.sort_by(|left, right| left.context_id.cmp(&right.context_id));
            let mut compactions = store
                .compactions
                .iter()
                .filter(|row| row.agent_did == peer.agent_did)
                .cloned()
                .collect::<Vec<_>>();
            compactions.sort_by(|left, right| left.compaction_id.cmp(&right.compaction_id));
            let mut tools = store
                .tools
                .iter()
                .filter(|row| row.agent_did == peer.agent_did)
                .cloned()
                .collect::<Vec<_>>();
            tools.sort_by(|left, right| left.tools_id.cmp(&right.tools_id));

            let mut tool_service_registries = store
                .tool_service_registries
                .iter()
                .filter(|row| row.agent_did == peer.agent_did)
                .cloned()
                .collect::<Vec<_>>();
            tool_service_registries.sort_by(|left, right| left.service_id.cmp(&right.service_id));
            let mut subagent_targets = store
                .subagent_targets
                .iter()
                .filter(|row| row.agent_did == peer.agent_did)
                .cloned()
                .collect::<Vec<_>>();
            subagent_targets.sort_by(|left, right| left.target_id.cmp(&right.target_id));
            let mut datastore_tool_surfaces = store
                .datastore_tool_surfaces
                .iter()
                .filter(|row| row.agent_did == peer.agent_did)
                .cloned()
                .collect::<Vec<_>>();
            datastore_tool_surfaces.sort_by(|left, right| left.surface_id.cmp(&right.surface_id));
            let mut chain_key_bindings = store
                .chain_key_bindings
                .iter()
                .filter(|row| row.agent_did == peer.agent_did)
                .cloned()
                .collect::<Vec<_>>();
            chain_key_bindings.sort_by(|left, right| left.binding_id.cmp(&right.binding_id));

            let mut skills = store
                .skills
                .iter()
                .enumerate()
                .filter(|(index, row)| {
                    source_matches_agent(
                        &store.skill_source_agent_dids,
                        *index,
                        &peer.agent_did,
                        false,
                    ) && row.agent_did == peer.agent_did
                })
                .map(|(_index, row)| SkillView {
                    skill_id: row.skill_id.clone(),
                    agent_did: Some(row.agent_did.clone()),
                    name: normalize_optional(row.name.as_deref()),
                    description: normalize_optional(row.description.as_deref()),
                    instructions: normalize_optional(row.instructions.as_deref()),
                    tool_refs: row.tool_refs.clone(),
                    display_name: normalize_optional(row.display_name.as_deref()),
                    enabled: Some(row.enabled),
                    created_at: normalize_optional(row.created_at.as_deref()),
                })
                .collect::<Vec<_>>();
            skills.sort_by(|left, right| left.skill_id.cmp(&right.skill_id));

            let scoped_task_rows = store
                .tasks
                .iter()
                .enumerate()
                .filter(|(index, row)| {
                    source_matches_agent(
                        &store.task_source_agent_dids,
                        *index,
                        &peer.agent_did,
                        false,
                    ) && row.agent_did == peer.agent_did
                        && behavior_ids.contains(&row.behavior_id.as_str())
                })
                .collect::<Vec<_>>();
            let mut schedules = store
                .schedules
                .iter()
                .filter(|row| row.agent_did == peer.agent_did)
                .cloned()
                .collect::<Vec<_>>();
            schedules.sort_by(|left, right| left.schedule_id.cmp(&right.schedule_id));
            let mut event_sources = store
                .event_sources
                .iter()
                .filter(|row| row.agent_did == peer.agent_did)
                .cloned()
                .collect::<Vec<_>>();
            event_sources.sort_by(|left, right| left.event_source_id.cmp(&right.event_source_id));
            let mut triggers = store
                .triggers
                .iter()
                .filter(|row| row.agent_did == peer.agent_did)
                .map(|row| {
                    let observation = store
                        .trigger_observations
                        .iter()
                        .enumerate()
                        .find(|(index, observation)| {
                            observation.trigger_id == row.trigger_id
                                && source_matches_agent(
                                    &store.trigger_observation_source_agent_dids,
                                    *index,
                                    &row.agent_did,
                                    true,
                                )
                        })
                        .map(|(_, observation)| observation);
                    let schedule_observation = store
                        .schedule_observations
                        .iter()
                        .enumerate()
                        .find(|(index, observation)| {
                            observation.trigger_id == row.trigger_id
                                && source_matches_agent(
                                    &store.schedule_observation_source_agent_dids,
                                    *index,
                                    &row.agent_did,
                                    true,
                                )
                        })
                        .map(|(_, observation)| observation);
                    TriggerView {
                        config: row.clone(),
                        next_run_at: schedule_observation.and_then(|item| item.next_run_at.clone()),
                        last_attempt_at: observation.and_then(|item| item.last_attempt_at.clone()),
                        last_fired_source_doc_id: observation
                            .and_then(|item| item.last_fired_source_doc_id.clone()),
                        last_status: observation.and_then(|item| item.last_status.clone()),
                        last_error: observation.and_then(|item| item.last_error.clone()),
                        fire_count: observation.and_then(|item| item.fire_count),
                    }
                })
                .collect::<Vec<_>>();
            triggers.sort_by(|left, right| left.config.trigger_id.cmp(&right.config.trigger_id));

            let mut tasks = scoped_task_rows
                .into_iter()
                .map(|(_index, row)| TaskView {
                    task_id: row.task_id.clone(),
                    name: normalize_optional(row.display_name.as_deref()),
                    description: normalize_optional(row.description.as_deref()),
                    behavior_id: Some(row.behavior_id.clone()),
                    prompt_template: Some(row.prompt_template.clone()),
                    goal_objective_template: normalize_optional(
                        row.goal_objective_template.as_deref(),
                    ),
                    goal_token_budget: row.goal_token_budget,
                    enabled: Some(row.enabled),
                    recent_runs: recent_runs_for_task_views(
                        &triggers,
                        &peer.agent_did,
                        &row.task_id,
                    ),
                    run_history: task_run_history(
                        store.as_ref(),
                        &peer.agent_did,
                        &row.task_id,
                        &triggers,
                    ),
                })
                .collect::<Vec<_>>();
            tasks.sort_by(|left, right| left.task_id.cmp(&right.task_id));

            let mut sessions = session_summaries(
                &store.sessions,
                &store.requests,
                &store.responses,
                &peer.agent_did,
                &tasks,
                &triggers,
            );

            let mut behavior_environments = resolve_behavior_environments(
                &behaviors,
                &inference_profiles,
                &contexts,
                &tools,
                &skills,
                &sessions,
            );

            let chat_safe = peer.is_chat_ready_at(Utc::now());
            let behavior_readiness = redact_unpaired_behavior_readiness(
                project_behavior_readiness(
                    store.behavior_readiness(&peer.agent_did),
                    &peer.agent_did,
                    behaviors
                        .iter()
                        .map(|behavior| behavior.behavior_id.as_str()),
                    default_behavior_id.as_deref(),
                ),
                chat_safe,
            );
            if !chat_safe {
                default_behavior_id = None;
                agent_principal.default_behavior_id = None;
                runtime = None;
                behaviors.clear();
                behavior_environments.clear();
                inference_backends.clear();
                inference_profiles.clear();
                tools.clear();
                contexts.clear();
                compactions.clear();
                inference_sampling.clear();
                inference_execution.clear();
                tool_service_registries.clear();
                skills.clear();
                tasks.clear();
                schedules.clear();
                event_sources.clear();
                triggers.clear();
                principal_config = None;
                behavior_configs.clear();
                subagent_targets.clear();
                datastore_tool_surfaces.clear();
                chain_key_bindings.clear();
                sessions.clear();
            }

            DeploymentView {
                peer_id: peer.peer_id,
                label: peer.label,
                agent_did: peer.agent_did,
                addr: peer.addr,
                source: peer.source,
                graphql: peer.graphql,
                dial_succeeded: status.is_some_and(|status| status.dial_succeeded),
                chat_safe,
                routes: status
                    .map(|status| {
                        status
                            .routes
                            .iter()
                            .map(|route| ClientRouteStatusView {
                                route_id: route.route_id.clone(),
                                direction: route.direction.clone(),
                                directory_id: route.directory_id.clone(),
                                transport_peer_id: route.transport_peer_id.clone(),
                                address: route.address.clone(),
                                template: route.template.clone(),
                                desired: route.desired,
                                applied: route.applied,
                                live_match: route.live_match,
                                filter_summary: route.filter_summary.clone(),
                                last_error: route.last_error.clone(),
                                retry_count: route.retry_count,
                                last_retry_at: super::system_time_rfc3339(route.last_retry_at),
                                last_retry_error_class: route
                                    .last_retry_error_class
                                    .map(|class| format!("{class:?}")),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                pairing: status
                    .map(|status| {
                        status
                            .pairing
                            .iter()
                            .map(super::to_pairing_collection_view)
                            .collect()
                    })
                    .unwrap_or_default(),
                last_error: status.and_then(|status| status.last_error.clone()),
                agent_principal,
                principal_config,
                behavior_configs,
                runtime,
                behavior_readiness,
                behaviors,
                behavior_environments,
                inference_backends,
                inference_profiles,
                tools,
                contexts,
                compactions,
                inference_sampling,
                inference_execution,
                tool_service_registries,
                subagent_targets,
                datastore_tool_surfaces,
                chain_key_bindings,
                skills,
                tasks,
                schedules,
                event_sources,
                triggers,
                sessions,
                mailbox_items,
            }
        })
        .collect::<Vec<_>>();

    deployments.sort_by(|left, right| left.label.cmp(&right.label));

    DesktopRuntimeSnapshot {
        local_peer_id: core.local_peer_id().to_string(),
        listen_addresses: core.listen_addresses().to_vec(),
        p2p_health: to_health_view(&sync_state.transport),
        sync_health: super::project_client_sync_health(&sync_state),
        enrollment_requests,
        bootstrap_errors: core.bootstrap_errors().to_vec(),
        last_mutation_error: core.last_mutation_error(),
        focused_request_id: core.store().focused_request_id(),
        configured_peer_count: sync_state.peers.len(),
        dialed_peer_count: sync_state
            .peers
            .iter()
            .filter(|status| status.dial_succeeded)
            .count(),
        peer_issue_count: sync_state
            .peers
            .iter()
            .filter(|status| status.last_error.is_some())
            .count(),
        row_count: store.row_count(),
        approx_serialized_bytes: store.approx_serialized_bytes(),
        deployments,
    }
}

pub(crate) fn project_behavior_readiness<'a>(
    row: Option<&AgentBehaviorReadinessRow>,
    expected_agent_did: &str,
    configured_behavior_ids: impl IntoIterator<Item = &'a str>,
    configured_default_behavior_id: Option<&str>,
) -> BehaviorReadinessView {
    let projection = gents_protocol::row::project_behavior_readiness(
        row,
        expected_agent_did,
        configured_behavior_ids,
        configured_default_behavior_id,
        Utc::now(),
    );
    BehaviorReadinessView {
        source: match projection.unknown_reason {
            Some(reason) => BehaviorReadinessSourceView::Unknown {
                reason: reason.into(),
            },
            None => BehaviorReadinessSourceView::Current,
        },
        active_generation: projection.active_generation,
        router_generation: projection.router_generation,
        updated_at: normalize_optional(projection.updated_at.as_deref()),
        behaviors: projection
            .behaviors
            .into_iter()
            .map(|(behavior_id, state)| match state {
                ProjectedBehaviorReadiness::Ready => {
                    BehaviorReadinessStatusView::Ready { behavior_id }
                }
                ProjectedBehaviorReadiness::Unavailable(reason) => {
                    BehaviorReadinessStatusView::Unavailable {
                        behavior_id,
                        reason: reason.into(),
                    }
                }
                ProjectedBehaviorReadiness::Unknown(reason) => {
                    BehaviorReadinessStatusView::Unknown {
                        behavior_id,
                        reason: reason.into(),
                    }
                }
            })
            .collect(),
    }
}

fn redact_unpaired_behavior_readiness(
    readiness: BehaviorReadinessView,
    pairing_ready: bool,
) -> BehaviorReadinessView {
    pairing_ready.then_some(readiness).unwrap_or_default()
}

impl From<gents_protocol::row::BehaviorReadinessUnavailableReason>
    for BehaviorUnavailableReasonView
{
    fn from(reason: gents_protocol::row::BehaviorReadinessUnavailableReason) -> Self {
        match reason {
            gents_protocol::row::BehaviorReadinessUnavailableReason::BehaviorDisabled => {
                Self::BehaviorDisabled
            }
            gents_protocol::row::BehaviorReadinessUnavailableReason::RuntimeConfigurationInvalid => {
                Self::RuntimeConfigurationInvalid
            }
            gents_protocol::row::BehaviorReadinessUnavailableReason::BackendNotConfigured => {
                Self::BackendNotConfigured
            }
            gents_protocol::row::BehaviorReadinessUnavailableReason::BackendDisabled => {
                Self::BackendDisabled
            }
            gents_protocol::row::BehaviorReadinessUnavailableReason::BackendTemporarilyUnavailable => {
                Self::BackendTemporarilyUnavailable
            }
            gents_protocol::row::BehaviorReadinessUnavailableReason::CredentialsRequired => {
                Self::CredentialsRequired
            }
            gents_protocol::row::BehaviorReadinessUnavailableReason::InferenceProfileInvalid => {
                Self::InferenceProfileInvalid
            }
            gents_protocol::row::BehaviorReadinessUnavailableReason::ToolConfigurationInvalid => {
                Self::ToolConfigurationInvalid
            }
            gents_protocol::row::BehaviorReadinessUnavailableReason::ToolSurfaceUnavailable => {
                Self::ToolSurfaceUnavailable
            }
            gents_protocol::row::BehaviorReadinessUnavailableReason::ExecutorStartFailed => {
                Self::ExecutorStartFailed
            }
        }
    }
}

impl From<BehaviorReadinessUnknownReason> for BehaviorReadinessUnknownReasonView {
    fn from(reason: BehaviorReadinessUnknownReason) -> Self {
        match reason {
            BehaviorReadinessUnknownReason::ReadinessMissing => Self::ReadinessMissing,
            BehaviorReadinessUnknownReason::ReadinessMalformed => Self::ReadinessMalformed,
            BehaviorReadinessUnknownReason::ReadinessVersionUnsupported => {
                Self::ReadinessVersionUnsupported
            }
            BehaviorReadinessUnknownReason::ReadinessStale => Self::ReadinessStale,
            BehaviorReadinessUnknownReason::ProcessNotReady => Self::ProcessNotReady,
            BehaviorReadinessUnknownReason::RouterGenerationStale => Self::RouterGenerationStale,
            BehaviorReadinessUnknownReason::BehaviorNotAssigned => Self::BehaviorNotAssigned,
        }
    }
}

fn backend_config_view(
    row: &gents::document_config::InferenceBackend,
    observation: Option<&gents::document_config::InferenceBackendObservation>,
) -> InferenceBackendView {
    use gents::document_config::BackendAuth;
    let (auth_kind, api_key_configured, api_key_env_var, catalog_scope) = match &row.auth {
        BackendAuth::Unauthenticated => ("unauthenticated", false, None, None),
        BackendAuth::ApiKey { .. } => ("api_key", true, None, None),
        BackendAuth::Environment { variable } => {
            ("environment", false, Some(variable.clone()), None)
        }
        BackendAuth::PrincipalOAuth => (
            "principal_o_auth",
            false,
            None,
            Some(row.agent_did.as_str()),
        ),
    };
    let models = observation
        .and_then(|observation| match observation.catalog_for(catalog_scope) {
            Ok(catalog) => catalog,
            Err(error) => {
                tracing::warn!(backend_id = %row.backend_id, agent_did = %row.agent_did,
                %error, "cannot project ambiguous backend catalog");
                None
            }
        })
        .map(|catalog| {
            catalog
                .models
                .iter()
                .map(|model| model.model_name.clone())
                .collect()
        })
        .unwrap_or_default();
    InferenceBackendView {
        backend_id: row.backend_id.clone(),
        name: Some(row.name.clone()),
        provider_kind: Some(row.provider_kind.as_str().to_owned()),
        openai_wire_api: row.openai_wire_api.map(|api| api.as_str().to_owned()),
        endpoint: Some(row.endpoint.clone()),
        auth_kind: Some(auth_kind.to_owned()),
        connect_timeout_secs: row.connect_timeout_secs,
        discovery_timeout_secs: row.discovery_timeout_secs,
        api_key_configured,
        api_key_env_var,
        max_concurrent: row.max_concurrent,
        max_queue_depth: row.max_queue_depth,
        enabled: Some(row.enabled),
        models,
        probe_status: observation.and_then(|observation| observation.probe_status.clone()),
    }
}

fn resolve_behavior_environments(
    behaviors: &[BehaviorView],
    profiles: &[InferenceProfile],
    contexts: &[AgentContext],
    tools: &[Tools],
    skills: &[SkillView],
    sessions: &[SessionSummary],
) -> Vec<BehaviorEnvironmentView> {
    behaviors
        .iter()
        .map(|behavior| {
            let context = behavior.context_id.as_deref().and_then(|id| {
                contexts.iter().find(|context| {
                    context.agent_did == behavior.agent_did && context.context_id == id
                })
            });
            let selected_tools = context
                .and_then(|context| context.tools_id.as_deref())
                .and_then(|id| {
                    tools
                        .iter()
                        .find(|tools| tools.agent_did == behavior.agent_did && tools.tools_id == id)
                });
            let unresolved_tools = (behavior.context_id.is_some() && context.is_none())
                || (context.is_some_and(|context| context.tools_id.is_some())
                    && selected_tools.is_none());
            let profile = behavior.inference_profile_id.as_deref().and_then(|id| {
                profiles.iter().find(|profile| {
                    profile.agent_did == behavior.agent_did && profile.profile_id == id
                })
            });
            let matching_sessions = sessions
                .iter()
                .filter(|session| {
                    session.behavior_id.as_deref() == Some(behavior.behavior_id.as_str())
                })
                .collect::<Vec<_>>();
            let skill_names = context
                .into_iter()
                .flat_map(|context| &context.skill_ids)
                .map(|id| {
                    skills
                        .iter()
                        .find(|skill| {
                            skill.agent_did.as_deref() == Some(behavior.agent_did.as_str())
                                && skill.skill_id == *id
                        })
                        .and_then(|skill| skill.display_name.clone().or_else(|| skill.name.clone()))
                        .unwrap_or_else(|| id.clone())
                })
                .collect();
            let host = selected_tools.and_then(|tools| tools.host.as_ref());
            BehaviorEnvironmentView {
                behavior_id: behavior.behavior_id.clone(),
                display_name: behavior.display_name.clone(),
                enabled: behavior.enabled,
                is_default: behavior.is_default,
                model_name: profile.map(|profile| profile.model_name.clone()),
                inference_profile_name: profile
                    .and_then(|profile| profile.display_name.clone())
                    .or_else(|| behavior.inference_profile_id.clone()),
                workspace_root: host.and_then(|host| host.root.clone()),
                file_access: if unresolved_tools {
                    "unknown"
                } else {
                    file_access_label(selected_tools)
                }
                .into(),
                bash_access: if unresolved_tools {
                    "unknown"
                } else {
                    bash_access_label(selected_tools)
                }
                .into(),
                network_access: host
                    .and_then(|host| host.bash.as_ref())
                    .and_then(|bash| bash.network_mode)
                    .map(|mode| mode.as_str().into()),
                skill_names,
                session_count: matching_sessions.len(),
                active_session_count: matching_sessions
                    .iter()
                    .filter(|row| session_is_active(row))
                    .count(),
            }
        })
        .collect()
}

fn file_access_label(tools: Option<&Tools>) -> &'static str {
    match tools
        .and_then(|tools| tools.host.as_ref())
        .and_then(|host| host.files.as_ref())
        .map(|files| files.mode)
    {
        None | Some(FileToolMode::Off) => "off",
        Some(FileToolMode::ReadOnly) => "read-only",
        Some(FileToolMode::ReadWrite) => "read / write",
    }
}

fn bash_access_label(tools: Option<&Tools>) -> &'static str {
    match tools
        .and_then(|tools| tools.host.as_ref())
        .and_then(|host| host.bash.as_ref())
        .map(|bash| bash.mode)
    {
        None | Some(BashMode::Off) => "off",
        Some(BashMode::ReadOnly) => "read-only",
        Some(BashMode::Unrestricted) => "unrestricted",
    }
}

fn session_is_active(session: &SessionSummary) -> bool {
    let Some(state) = session.turn_state.as_deref() else {
        return false;
    };
    !matches!(
        state.to_ascii_lowercase().as_str(),
        "completed"
            | "failed"
            | "error"
            | "dead"
            | "superseded"
            | "interrupted"
            | "cancelled"
            | "idle"
    )
}

#[cfg(test)]
mod behavior_readiness_conformance_tests {
    use super::*;

    #[test]
    fn malformed_and_unpaired_observations_fail_closed_with_typed_reasons() {
        let row = AgentBehaviorReadinessRow {
            agent_did: "did:test:bad-readiness".to_string(),
            snapshot_json: "not-json".to_string(),
            updated_at: "2026-08-28T00:00:00Z".to_string(),
        };
        let malformed =
            project_behavior_readiness(Some(&row), "did:test:agent", ["default"], Some("default"));
        assert!(matches!(
            malformed.behaviors.as_slice(),
            [BehaviorReadinessStatusView::Unknown {
                reason: BehaviorReadinessUnknownReasonView::ReadinessMalformed,
                ..
            }]
        ));
        assert!(matches!(
            malformed.source,
            BehaviorReadinessSourceView::Unknown {
                reason: BehaviorReadinessUnknownReasonView::ReadinessMalformed
            }
        ));
    }

    #[test]
    fn unpaired_deployment_redacts_a_retained_current_readiness_snapshot() {
        let retained = BehaviorReadinessView {
            source: BehaviorReadinessSourceView::Current,
            active_generation: Some(7),
            router_generation: Some(7),
            updated_at: Some("2026-08-29T00:00:00Z".to_string()),
            behaviors: vec![BehaviorReadinessStatusView::Ready {
                behavior_id: "private-default".to_string(),
            }],
        };

        let redacted = redact_unpaired_behavior_readiness(retained, false);

        assert_eq!(redacted, BehaviorReadinessView::default());
    }
}

#[cfg(test)]
mod behavior_environment_tests {
    use super::*;

    fn behavior() -> BehaviorView {
        BehaviorView {
            behavior_id: "default".into(),
            agent_did: "did:test:owner".into(),
            display_name: "Amy".into(),
            description: None,
            context_id: Some("context".into()),
            inference_profile_id: Some("profile".into()),
            enabled: true,
            is_default: true,
            tags: Vec::new(),
            created_at: None,
        }
    }
    fn context() -> AgentContext {
        serde_json::from_value(serde_json::json!({"context_id":"context", "agent_did":"did:test:owner", "tools_id":"tools", "skill_ids":["diagnostics"]})).unwrap()
    }
    fn profile() -> InferenceProfile {
        serde_json::from_value(serde_json::json!({"profile_id":"profile", "agent_did":"did:test:owner", "backend_id":"backend", "model_name":"gpt-test", "display_name":"Long context"})).unwrap()
    }
    fn tools() -> Tools {
        serde_json::from_value(serde_json::json!({"tools_id":"tools", "agent_did":"did:test:owner", "host":{"root":"/work/amygdala", "files":{"mode":"ReadWrite"}, "bash":{"mode":"ReadOnly"}}})).unwrap()
    }

    fn skill() -> SkillView {
        SkillView {
            skill_id: "diagnostics".to_string(),
            agent_did: Some("did:test:owner".into()),
            name: Some("diagnostics".to_string()),
            description: None,
            instructions: None,
            tool_refs: vec![],
            display_name: Some("Host diagnostics".to_string()),
            enabled: Some(true),
            created_at: None,
        }
    }

    fn session(
        session_id: &str,
        behavior_id: Option<&str>,
        turn_state: Option<&str>,
    ) -> SessionSummary {
        SessionSummary {
            agent_did: "did:test:owner".into(),
            requester_did: None,
            latest_request_doc_id: None,
            closed_at: None,
            tags: vec![],
            provenance: None,
            session_id: session_id.to_string(),
            title: None,
            preview_text: None,
            status: None,
            behavior_id: behavior_id.map(str::to_string),
            latest_request_id: None,
            task_id: None,
            task_name: None,
            trigger_id: None,
            trigger_kind: None,
            created_at: None,
            updated_at: None,
            turn_state: turn_state.map(str::to_string),
            message_count: None,
            tool_call_count: None,
        }
    }

    #[test]
    fn resolves_runnable_environment_once_for_clients() {
        let mut status_only_active = session("status-active", Some("default"), None);
        status_only_active.status = Some("active".to_string());
        let unassigned = session("unassigned", None, Some("processing"));
        let environments = resolve_behavior_environments(
            &[behavior()],
            &[profile()],
            &[context()],
            &[tools()],
            &[skill()],
            &[
                session("active", Some("default"), Some("processing")),
                session("complete", Some("default"), Some("completed")),
                status_only_active,
                unassigned,
            ],
        );

        let environment = &environments[0];
        assert_eq!(environment.display_name, "Amy");
        assert_eq!(environment.model_name.as_deref(), Some("gpt-test"));
        assert_eq!(
            environment.inference_profile_name.as_deref(),
            Some("Long context")
        );
        assert_eq!(
            environment.workspace_root.as_deref(),
            Some("/work/amygdala")
        );
        assert_eq!(environment.file_access, "read / write");
        assert_eq!(environment.bash_access, "read-only");
        assert_eq!(environment.skill_names, vec!["Host diagnostics"]);
        assert_eq!(environment.session_count, 3);
        assert_eq!(environment.active_session_count, 1);
    }

    #[test]
    fn invalid_tool_modes_are_rejected_instead_of_misreported() {
        for field in ["files", "bash"] {
            let mut value = serde_json::to_value(tools()).unwrap();
            value["host"][field]["mode"] = serde_json::json!("FutureMode");
            assert!(serde_json::from_value::<Tools>(value).is_err());
        }
    }

    #[test]
    fn missing_or_foreign_references_do_not_invent_model_or_tool_permissions() {
        let mut foreign_profile = profile();
        foreign_profile.agent_did = "did:test:foreign".into();
        let mut foreign_tools = tools();
        foreign_tools.agent_did = "did:test:foreign".into();
        let environments = resolve_behavior_environments(
            &[behavior()],
            &[foreign_profile],
            &[context()],
            &[foreign_tools],
            &[],
            &[],
        );
        assert_eq!(environments[0].model_name, None);
        assert_eq!(environments[0].file_access, "unknown");
        assert_eq!(environments[0].bash_access, "unknown");
        assert_eq!(environments[0].workspace_root, None);
    }
}

#[cfg(test)]
mod backend_config_view_tests {
    use super::*;

    #[test]
    fn backend_view_never_serializes_key_and_uses_exact_oauth_catalog() {
        let mut backend: gents::document_config::InferenceBackend =
            serde_json::from_value(serde_json::json!({
                "agent_did":"owner","backend_id":"backend","name":"Backend",
                "provider_kind":"OpenAiCompatible","endpoint":"http://localhost/v1",
                "auth":{"kind":"api_key","key":"NEVER-EXPORT-KEY"}
            }))
            .unwrap();
        let view = backend_config_view(&backend, None);
        assert!(view.api_key_configured);
        assert!(!serde_json::to_string(&view)
            .unwrap()
            .contains("NEVER-EXPORT-KEY"));
        backend.auth = gents::document_config::BackendAuth::PrincipalOAuth;
        let observation = serde_json::from_value(serde_json::json!({
            "backend_id":"backend","catalogs":[
                {"agent_did":"foreign","observed_at":"now","models":[{"model_name":"foreign-model"}]},
                {"agent_did":"owner","observed_at":"now","models":[{"model_name":"own-model"}]}
            ]
        })).unwrap();
        assert_eq!(
            backend_config_view(&backend, Some(&observation)).models,
            ["own-model"]
        );
    }
}
