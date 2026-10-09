use std::collections::HashMap;

use chrono::Utc;
use gents::{BashMode, FileToolMode};
use gents_desktop_core::client::{ClientCore, ClientPeerStatus};
use gents_protocol::row::{
    AgentReadinessUnavailableReason, AgentReadinessUnknownReason, NodeReadinessRow,
    ProjectedAgentReadiness,
};

use super::super::types::{
    normalize_optional, AgentContext, AgentEnvironmentView, AgentReadinessStatusView,
    AgentReadinessUnknownReasonView, AgentUnavailableReasonView, AgentView, ClientRouteStatusView,
    DeploymentView, DesktopRuntimeSnapshot, InferenceBackendView, InferenceProfile,
    MailboxItemView, NodeReadinessSourceView, NodeReadinessView, NodeView, RuntimeView,
    SessionSummary, SkillView, TaskRecentRunsView, TaskRunSummaryView, TaskView, Tools,
    TriggerView,
};
use super::runtime_tasks::{
    recent_runs_for_task_views, session_summaries, source_matches_node, task_run_history,
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
    let requester_did = core.node_identity().did().to_string();

    let mut deployments = sync_state
        .directory
        .clone()
        .into_iter()
        .map(|peer| {
            let status = peer_statuses_by_id.get(&peer.peer_id);
            let node_row = store.nodes.iter().find(|row| row.node_did == peer.node_did);
            let mailbox_items = store
                .mailbox_items
                .iter()
                .filter(|row| {
                    row.node_did == peer.node_did
                        && row.requester_did == requester_did
                        && row.status == "open"
                })
                .map(MailboxItemView::from)
                .collect::<Vec<_>>();
            let node = node_row
                .map(|row| NodeView {
                    node_did: row.node_did.clone(),
                    display_name: normalize_optional(row.display_name.as_deref()),
                    default_agent_id: normalize_optional(row.default_agent_id.as_deref()),
                    enabled: Some(row.enabled),
                    created_at: normalize_optional(row.created_at.as_deref()),
                    created_by: normalize_optional(row.created_by.as_deref()),
                })
                .unwrap_or_else(|| NodeView {
                    node_did: peer.node_did.clone(),
                    display_name: None,
                    default_agent_id: None,
                    enabled: None,
                    created_at: None,
                    created_by: None,
                });
            let node_config = node_row.cloned();
            let mut agent_configs = store
                .agents
                .iter()
                .filter(|row| row.node_did == peer.node_did)
                .cloned()
                .collect::<Vec<_>>();
            agent_configs.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
            let default_agent_id = store
                .default_agent_id_for_node(&peer.node_did)
                .map(str::to_owned);
            let runtime = store.latest_runtime(&peer.node_did).map(|row| RuntimeView {
                reconcile_phase: normalize_optional(row.reconcile_phase.as_deref()),
                last_reconcile_result: normalize_optional(row.last_reconcile_result.as_deref()),
                last_reconcile_error: normalize_optional(row.last_reconcile_error.as_deref()),
                updated_at: normalize_optional(row.updated_at.as_deref()),
                agent_executor_capacity: row.agent_executor_capacity,
                agent_executor_queue_depth: row.agent_executor_queue_depth,
            });

            let mut agents = store
                .agent_rows(&peer.node_did)
                .into_iter()
                .map(|row| AgentView {
                    agent_id: row.agent_id.clone(),
                    node_did: row.node_did.clone(),
                    display_name: normalize_optional(row.display_name.as_deref())
                        .unwrap_or_else(|| row.agent_id.clone()),
                    description: row.description.clone(),
                    context_id: row.context_id.clone(),
                    inference_profile_id: Some(row.inference_profile_id.clone()),
                    enabled: row.enabled,
                    is_default: default_agent_id.as_deref() == Some(row.agent_id.as_str()),
                    tags: row.tags.clone(),
                    created_at: row.created_at.clone(),
                })
                .collect::<Vec<_>>();
            agents.sort_by(|left, right| {
                right
                    .is_default
                    .cmp(&left.is_default)
                    .then_with(|| left.display_name.cmp(&right.display_name))
            });
            let agent_ids = agents
                .iter()
                .map(|agent| agent.agent_id.as_str())
                .collect::<Vec<_>>();
            let mut inference_backends = store
                .inference_backends
                .iter()
                .filter(|row| row.node_did == peer.node_did)
                .map(|row| {
                    let observation = store
                        .backend_observations
                        .iter()
                        .enumerate()
                        .find(|(index, observation)| {
                            observation.backend_id == row.backend_id
                                && source_matches_node(
                                    &store.backend_observation_source_node_dids,
                                    *index,
                                    &row.node_did,
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
                    row.node_did == peer.node_did
                        && source_matches_node(
                            &store.inference_profile_source_node_dids,
                            *index,
                            &peer.node_did,
                            false,
                        )
                })
                .map(|(_, row)| row.clone())
                .collect::<Vec<_>>();
            inference_profiles.sort_by(|left, right| left.profile_id.cmp(&right.profile_id));
            let mut inference_sampling = store
                .inference_sampling
                .iter()
                .filter(|row| row.node_did == peer.node_did)
                .cloned()
                .collect::<Vec<_>>();
            inference_sampling.sort_by(|left, right| left.sampling_id.cmp(&right.sampling_id));
            let mut inference_execution = store
                .inference_execution
                .iter()
                .filter(|row| row.node_did == peer.node_did)
                .cloned()
                .collect::<Vec<_>>();
            inference_execution.sort_by(|left, right| left.execution_id.cmp(&right.execution_id));
            let mut contexts = store
                .contexts
                .iter()
                .filter(|row| row.node_did == peer.node_did)
                .cloned()
                .collect::<Vec<_>>();
            contexts.sort_by(|left, right| left.context_id.cmp(&right.context_id));
            let mut compactions = store
                .compactions
                .iter()
                .filter(|row| row.node_did == peer.node_did)
                .cloned()
                .collect::<Vec<_>>();
            compactions.sort_by(|left, right| left.compaction_id.cmp(&right.compaction_id));
            let mut tools = store
                .tools
                .iter()
                .filter(|row| row.node_did == peer.node_did)
                .cloned()
                .collect::<Vec<_>>();
            tools.sort_by(|left, right| left.tools_id.cmp(&right.tools_id));

            let mut tool_service_registries = store
                .tool_service_registries
                .iter()
                .filter(|row| row.node_did == peer.node_did)
                .cloned()
                .collect::<Vec<_>>();
            tool_service_registries.sort_by(|left, right| left.service_id.cmp(&right.service_id));
            let mut agent_targets = store
                .agent_targets
                .iter()
                .filter(|row| row.node_did == peer.node_did)
                .cloned()
                .collect::<Vec<_>>();
            agent_targets.sort_by(|left, right| left.target_id.cmp(&right.target_id));
            let mut datastore_tool_surfaces = store
                .datastore_tool_surfaces
                .iter()
                .filter(|row| row.node_did == peer.node_did)
                .cloned()
                .collect::<Vec<_>>();
            datastore_tool_surfaces.sort_by(|left, right| left.surface_id.cmp(&right.surface_id));
            let mut chain_key_bindings = store
                .chain_key_bindings
                .iter()
                .filter(|row| row.node_did == peer.node_did)
                .cloned()
                .collect::<Vec<_>>();
            chain_key_bindings.sort_by(|left, right| left.binding_id.cmp(&right.binding_id));

            let mut skills = store
                .skills
                .iter()
                .enumerate()
                .filter(|(index, row)| {
                    source_matches_node(
                        &store.skill_source_node_dids,
                        *index,
                        &peer.node_did,
                        false,
                    ) && row.node_did == peer.node_did
                })
                .map(|(_index, row)| SkillView {
                    skill_id: row.skill_id.clone(),
                    node_did: Some(row.node_did.clone()),
                    name: normalize_optional(row.name.as_deref()),
                    description: normalize_optional(row.description.as_deref()),
                    instructions: normalize_optional(row.instructions.as_deref()),
                    source_directory: row.source_directory.clone(),
                    tool_refs: row.tool_refs.clone(),
                    display_name: normalize_optional(row.display_name.as_deref()),
                    interface_json: normalize_optional(row.interface_json.as_deref()),
                    enabled: Some(row.enabled),
                    created_at: normalize_optional(row.created_at.as_deref()),
                    tags: row.tags.clone(),
                })
                .collect::<Vec<_>>();
            skills.sort_by(|left, right| left.skill_id.cmp(&right.skill_id));

            let scoped_task_rows = store
                .tasks
                .iter()
                .enumerate()
                .filter(|(index, row)| {
                    source_matches_node(&store.task_source_node_dids, *index, &peer.node_did, false)
                        && row.node_did == peer.node_did
                        && agent_ids.contains(&row.agent_id.as_str())
                })
                .collect::<Vec<_>>();
            let mut schedules = store
                .schedules
                .iter()
                .filter(|row| row.node_did == peer.node_did)
                .cloned()
                .collect::<Vec<_>>();
            schedules.sort_by(|left, right| left.schedule_id.cmp(&right.schedule_id));
            let mut event_sources = store
                .event_sources
                .iter()
                .filter(|row| row.node_did == peer.node_did)
                .cloned()
                .collect::<Vec<_>>();
            event_sources.sort_by(|left, right| left.event_source_id.cmp(&right.event_source_id));
            let mut triggers = store
                .triggers
                .iter()
                .filter(|row| row.node_did == peer.node_did)
                .map(|row| {
                    let observation = store
                        .trigger_observations
                        .iter()
                        .enumerate()
                        .find(|(index, observation)| {
                            observation.trigger_id == row.trigger_id
                                && source_matches_node(
                                    &store.trigger_observation_source_node_dids,
                                    *index,
                                    &row.node_did,
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
                                && source_matches_node(
                                    &store.schedule_observation_source_node_dids,
                                    *index,
                                    &row.node_did,
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
                .map(|(_index, row)| {
                    project_task_view(
                        row,
                        recent_runs_for_task_views(&triggers, &peer.node_did, &row.task_id),
                        task_run_history(store.as_ref(), &peer.node_did, &row.task_id, &triggers),
                    )
                })
                .collect::<Vec<_>>();
            tasks.sort_by(|left, right| left.task_id.cmp(&right.task_id));

            let mut sessions = session_summaries(
                &store.sessions,
                &store.requests,
                &peer.node_did,
                &tasks,
                &triggers,
            );
            let node_scope = core.transcript_node_scope(&peer.node_did);
            let operator = core.operator_graphql(&peer.node_did).is_some();
            for summary in &mut sessions {
                summary.unreadable_reason = store
                    .sessions
                    .iter()
                    .find(|row| {
                        row.session_id == summary.session_id && row.node_did == summary.node_did
                    })
                    .and_then(|row| {
                        gents_desktop_core::client::session_unreadable_reason(
                            row,
                            node_scope.as_deref(),
                            operator,
                        )
                    })
                    .map(str::to_owned);
            }

            let agent_environments = resolve_agent_environments(
                &agents,
                &inference_profiles,
                &contexts,
                &tools,
                &skills,
                &sessions,
            );

            let chat_safe = peer.is_chat_ready_at(Utc::now());
            // Pairing readiness controls admission of new remote work. It does
            // not control visibility of rows already authorized and present in
            // the local database. Keeping this projection stable is what lets
            // the mobile UI remain useful while the transport reconnects.
            let node_readiness = project_node_readiness(
                store.node_readiness(&peer.node_did),
                &peer.node_did,
                agents.iter().map(|agent| agent.agent_id.as_str()),
                default_agent_id.as_deref(),
            );

            DeploymentView {
                peer_id: peer.peer_id,
                label: peer.label,
                node_did: peer.node_did,
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
                node,
                node_config,
                agent_configs,
                runtime,
                node_readiness,
                agents,
                agent_environments,
                inference_backends,
                inference_profiles,
                tools,
                contexts,
                compactions,
                inference_sampling,
                inference_execution,
                tool_service_registries,
                agent_targets,
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

    let access = gents::config_client::ConfigAccess::Local(core.node_arc());
    super::runtime_tasks::resolve_summary_starters(
        &access,
        deployments
            .iter_mut()
            .flat_map(|deployment| deployment.sessions.iter_mut()),
    )
    .await;
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

fn project_task_view(
    task: &gents::document_config::Task,
    recent_runs: TaskRecentRunsView,
    run_history: Vec<TaskRunSummaryView>,
) -> TaskView {
    TaskView {
        emit_outcome: task.emit_outcome,
        task_id: task.task_id.clone(),
        name: normalize_optional(task.display_name.as_deref()),
        description: normalize_optional(task.description.as_deref()),
        agent_id: Some(task.agent_id.clone()),
        prompt_template: Some(task.prompt_template.clone()),
        goal_objective_template: normalize_optional(task.goal_objective_template.as_deref()),
        goal_token_budget: task.goal_token_budget,
        hooks: task.hooks.clone(),
        enabled: Some(task.enabled),
        output_schema_ref: task.output_schema_ref.clone(),
        tags: task.tags.clone(),
        recent_runs,
        run_history,
    }
}

#[cfg(test)]
mod task_view_tests {
    use super::*;

    #[test]
    fn canonical_task_hooks_and_tags_survive_projection() {
        let task: gents::document_config::Task = serde_json::from_value(serde_json::json!({
            "node_did": "did:test:owner",
            "task_id": "release",
            "display_name": "Release",
            "agent_id": "operator",
            "prompt_template": "Ship it",
            "emit_outcome": true,
            "output_schema_ref": "schemas/release-result.json",
            "hooks": [
                {
                    "hook_id": "prepare",
                    "phase": "before",
                    "command": ["sh", "-c", "./prepare.sh"],
                    "timeout_secs": 45
                },
                {
                    "hook_id": "cleanup",
                    "phase": "finally",
                    "command": ["./cleanup"]
                }
            ],
            "tags": ["release", "operator"]
        }))
        .expect("canonical task");
        let view = project_task_view(
            &task,
            TaskRecentRunsView {
                total_fires: 0,
                last_attempt_at: None,
                last_status: None,
                last_error: None,
                schedule_count: 0,
                event_count: 0,
            },
            Vec::new(),
        );

        assert_eq!(view.hooks, task.hooks);
        assert!(view.emit_outcome);
        assert_eq!(view.output_schema_ref, task.output_schema_ref);
        assert_eq!(view.tags, ["release", "operator"]);
        let wire = serde_json::to_value(view).expect("TaskView wire value");
        assert_eq!(wire["hooks"][0]["hook_id"], "prepare");
        assert_eq!(wire["hooks"][0]["phase"], "before");
        assert_eq!(wire["hooks"][0]["timeout_secs"], 45);
        assert_eq!(wire["hooks"][1]["phase"], "finally");
        assert_eq!(wire["tags"], serde_json::json!(["release", "operator"]));
    }
}

pub(crate) fn project_node_readiness<'a>(
    row: Option<&NodeReadinessRow>,
    expected_node_did: &str,
    configured_agent_ids: impl IntoIterator<Item = &'a str>,
    configured_default_agent_id: Option<&str>,
) -> NodeReadinessView {
    let projection = gents_protocol::row::project_node_readiness(
        row,
        expected_node_did,
        configured_agent_ids,
        configured_default_agent_id,
    );
    NodeReadinessView {
        source: match projection.unknown_reason {
            Some(reason) => NodeReadinessSourceView::Unknown {
                reason: reason.into(),
            },
            None => NodeReadinessSourceView::Current,
        },
        active_generation: projection.active_generation,
        router_generation: projection.router_generation,
        updated_at: normalize_optional(projection.updated_at.as_deref()),
        agents: projection
            .agents
            .into_iter()
            .map(|(agent_id, state)| match state {
                ProjectedAgentReadiness::Ready => AgentReadinessStatusView::Ready { agent_id },
                ProjectedAgentReadiness::Unavailable(reason) => {
                    AgentReadinessStatusView::Unavailable {
                        agent_id,
                        reason: reason.into(),
                    }
                }
                ProjectedAgentReadiness::Unknown(reason) => AgentReadinessStatusView::Unknown {
                    agent_id,
                    reason: reason.into(),
                },
            })
            .collect(),
    }
}

impl From<AgentReadinessUnavailableReason> for AgentUnavailableReasonView {
    fn from(reason: AgentReadinessUnavailableReason) -> Self {
        match reason {
            AgentReadinessUnavailableReason::AgentDisabled => Self::AgentDisabled,
            AgentReadinessUnavailableReason::RuntimeConfigurationInvalid => {
                Self::RuntimeConfigurationInvalid
            }
            AgentReadinessUnavailableReason::BackendNotConfigured => Self::BackendNotConfigured,
            AgentReadinessUnavailableReason::BackendDisabled => Self::BackendDisabled,
            AgentReadinessUnavailableReason::BackendTemporarilyUnavailable => {
                Self::BackendTemporarilyUnavailable
            }
            AgentReadinessUnavailableReason::CredentialsRequired => Self::CredentialsRequired,
            AgentReadinessUnavailableReason::InferenceProfileInvalid => {
                Self::InferenceProfileInvalid
            }
            AgentReadinessUnavailableReason::ToolConfigurationInvalid => {
                Self::ToolConfigurationInvalid
            }
            AgentReadinessUnavailableReason::ToolSurfaceUnavailable => Self::ToolSurfaceUnavailable,
            AgentReadinessUnavailableReason::ExecutorStartFailed => Self::ExecutorStartFailed,
        }
    }
}

impl From<AgentReadinessUnknownReason> for AgentReadinessUnknownReasonView {
    fn from(reason: AgentReadinessUnknownReason) -> Self {
        match reason {
            AgentReadinessUnknownReason::ReadinessMissing => Self::ReadinessMissing,
            AgentReadinessUnknownReason::ReadinessMalformed => Self::ReadinessMalformed,
            AgentReadinessUnknownReason::ReadinessVersionUnsupported => {
                Self::ReadinessVersionUnsupported
            }
            AgentReadinessUnknownReason::ProcessNotReady => Self::ProcessNotReady,
            AgentReadinessUnknownReason::RouterGenerationStale => Self::RouterGenerationStale,
            AgentReadinessUnknownReason::AgentNotAssigned => Self::AgentNotAssigned,
        }
    }
}

fn backend_config_view(
    row: &gents::document_config::InferenceBackend,
    observation: Option<&gents::document_config::InferenceBackendObservation>,
) -> InferenceBackendView {
    use gents::document_config::BackendAuth;
    let (auth_kind, api_key_configured, api_key_env_var) = match &row.auth {
        BackendAuth::Unauthenticated => ("unauthenticated", false, None),
        BackendAuth::ApiKey { .. } => ("api_key", true, None),
        BackendAuth::Environment { variable } => ("environment", false, Some(variable.clone())),
        BackendAuth::NodeOAuth { .. } => ("node_oauth", false, None),
    };
    let advertised_models = match gents::config::backend_catalog(row, observation) {
        Ok(catalog) => catalog,
        Err(error) => {
            tracing::warn!(backend_id = %row.backend_id, node_did = %row.node_did,
            %error, "cannot project ambiguous backend catalog");
            None
        }
    }
    .map(|catalog| catalog.models.clone())
    .unwrap_or_default();
    let models = advertised_models
        .iter()
        .map(|model| model.model_name.clone())
        .collect();
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
        tags: row.tags.clone(),
        models,
        advertised_models,
        probe_status: observation.and_then(|observation| observation.probe_status.clone()),
        account_ref: row.auth.oauth_account_ref().map(str::to_owned),
    }
}

fn resolve_agent_environments(
    agents: &[AgentView],
    profiles: &[InferenceProfile],
    contexts: &[AgentContext],
    tools: &[Tools],
    skills: &[SkillView],
    sessions: &[SessionSummary],
) -> Vec<AgentEnvironmentView> {
    agents
        .iter()
        .map(|agent| {
            let context = agent.context_id.as_deref().and_then(|id| {
                contexts
                    .iter()
                    .find(|context| context.node_did == agent.node_did && context.context_id == id)
            });
            let selected_tools = context
                .and_then(|context| context.tools_id.as_deref())
                .and_then(|id| {
                    tools
                        .iter()
                        .find(|tools| tools.node_did == agent.node_did && tools.tools_id == id)
                });
            let unresolved_tools = (agent.context_id.is_some() && context.is_none())
                || (context.is_some_and(|context| context.tools_id.is_some())
                    && selected_tools.is_none());
            let profile = agent.inference_profile_id.as_deref().and_then(|id| {
                profiles
                    .iter()
                    .find(|profile| profile.node_did == agent.node_did && profile.profile_id == id)
            });
            let matching_sessions = sessions
                .iter()
                .filter(|session| session.agent_id.as_deref() == Some(agent.agent_id.as_str()))
                .collect::<Vec<_>>();
            let skill_names = context
                .into_iter()
                .flat_map(|context| &context.skill_ids)
                .map(|id| {
                    skills
                        .iter()
                        .find(|skill| {
                            skill.node_did.as_deref() == Some(agent.node_did.as_str())
                                && skill.skill_id == *id
                        })
                        .and_then(|skill| skill.display_name.clone().or_else(|| skill.name.clone()))
                        .unwrap_or_else(|| id.clone())
                })
                .collect();
            let host = selected_tools.and_then(|tools| tools.host.as_ref());
            AgentEnvironmentView {
                agent_id: agent.agent_id.clone(),
                display_name: agent.display_name.clone(),
                enabled: agent.enabled,
                is_default: agent.is_default,
                model_name: profile.map(|profile| profile.model_name.clone()),
                inference_profile_name: profile
                    .and_then(|profile| profile.display_name.clone())
                    .or_else(|| agent.inference_profile_id.clone()),
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
mod node_readiness_conformance_tests {
    use super::*;

    #[test]
    fn malformed_and_unpaired_observations_fail_closed_with_typed_reasons() {
        let row = NodeReadinessRow {
            node_did: "did:test:bad-readiness".to_string(),
            snapshot_json: "not-json".to_string(),
            updated_at: "2026-08-28T00:00:00Z".to_string(),
        };
        let malformed =
            project_node_readiness(Some(&row), "did:test:node", ["default"], Some("default"));
        assert!(matches!(
            malformed.agents.as_slice(),
            [AgentReadinessStatusView::Unknown {
                reason: AgentReadinessUnknownReasonView::ReadinessMalformed,
                ..
            }]
        ));
        assert!(matches!(
            malformed.source,
            NodeReadinessSourceView::Unknown {
                reason: AgentReadinessUnknownReasonView::ReadinessMalformed
            }
        ));
    }
}

#[cfg(test)]
mod agent_environment_tests {
    use super::*;

    fn agent() -> AgentView {
        AgentView {
            agent_id: "default".into(),
            node_did: "did:test:owner".into(),
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
        serde_json::from_value(serde_json::json!({"context_id":"context", "node_did":"did:test:owner", "tools_id":"tools", "skill_ids":["diagnostics"]})).unwrap()
    }
    fn profile() -> InferenceProfile {
        serde_json::from_value(serde_json::json!({"profile_id":"profile", "node_did":"did:test:owner", "backend_id":"backend", "model_name":"gpt-test", "display_name":"Long context"})).unwrap()
    }
    fn tools() -> Tools {
        serde_json::from_value(serde_json::json!({"tools_id":"tools", "node_did":"did:test:owner", "host":{"root":"/work/amygdala", "files":{"mode":"ReadWrite"}, "bash":{"mode":"ReadOnly"}}})).unwrap()
    }

    fn skill() -> SkillView {
        SkillView {
            skill_id: "diagnostics".to_string(),
            node_did: Some("did:test:owner".into()),
            name: Some("diagnostics".to_string()),
            description: None,
            instructions: None,
            tool_refs: vec![],
            display_name: Some("Host diagnostics".to_string()),
            interface_json: None,
            source_directory: None,
            enabled: Some(true),
            created_at: None,
            tags: vec![],
        }
    }

    fn session(
        session_id: &str,
        agent_id: Option<&str>,
        turn_state: Option<&str>,
    ) -> SessionSummary {
        SessionSummary {
            started_by: None,
            node_did: "did:test:owner".into(),
            requester_did: None,
            latest_request_doc_id: None,
            closed_at: None,
            tags: vec![],
            provenance: None,
            session_id: session_id.to_string(),
            title: None,
            preview_text: None,
            status: None,
            agent_id: agent_id.map(str::to_string),
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
            unreadable_reason: None,
        }
    }

    #[test]
    fn resolves_runnable_environment_once_for_clients() {
        let mut status_only_active = session("status-active", Some("default"), None);
        status_only_active.status = Some("active".to_string());
        let unassigned = session("unassigned", None, Some("processing"));
        let environments = resolve_agent_environments(
            &[agent()],
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
        foreign_profile.node_did = "did:test:foreign".into();
        let mut foreign_tools = tools();
        foreign_tools.node_did = "did:test:foreign".into();
        let environments = resolve_agent_environments(
            &[agent()],
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
                "node_did":"owner","backend_id":"backend","name":"Backend",
                "provider_kind":"OpenAiCompatible","endpoint":"http://localhost/v1",
                "auth":{"kind":"api_key","key":"NEVER-EXPORT-KEY"}
            }))
            .unwrap();
        let view = backend_config_view(&backend, None);
        assert!(view.api_key_configured);
        assert!(!serde_json::to_string(&view)
            .unwrap()
            .contains("NEVER-EXPORT-KEY"));
        backend.auth = gents::document_config::BackendAuth::NodeOAuth { account_ref: None };
        let observation = serde_json::from_value(serde_json::json!({
            "backend_id":"backend","catalogs":[
                {"node_did":"foreign","observed_at":"now","models":[{"model_name":"foreign-model"}]},
                {"node_did":"owner","observed_at":"now","models":[{
                    "model_name":"own-model",
                    "context_window":272000,
                    "max_context_window":872000
                }]}
            ]
        })).unwrap();
        let view = backend_config_view(&backend, Some(&observation));
        assert_eq!(view.account_ref, None);
        assert_eq!(view.models, ["own-model"]);
        assert_eq!(view.advertised_models[0].context_window, Some(272_000));
        assert_eq!(view.advertised_models[0].max_context_window, Some(872_000));
        backend.auth = gents::document_config::BackendAuth::NodeOAuth {
            account_ref: Some("acct-2".into()),
        };
        let view = backend_config_view(&backend, Some(&observation));
        assert_eq!(view.account_ref.as_deref(), Some("acct-2"));
    }
}
