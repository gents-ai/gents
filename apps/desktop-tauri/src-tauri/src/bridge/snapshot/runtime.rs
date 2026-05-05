use std::collections::HashMap;

use defra_agent_desktop_core::client::{ClientCore, ClientPeerStatus};

use super::super::types::{
    normalize_optional, turn_state_label, AgentPrincipalView, BehaviorView, ConversationSummary,
    DeploymentView, DesktopRuntimeSnapshot, EventTriggerView, InferenceBackendView,
    InferenceProfileView, RuntimeView, ScheduleView, TaskView, ToolSelectionView,
    ToolServiceRegistryView,
};
use super::runtime_tasks::{
    conversation_task_tag, recent_runs_for_task_views, retain_latest_conversation_summaries,
    source_matches_agent, task_run_history,
};
use super::to_health_view;

pub(crate) async fn build_runtime_snapshot(core: &ClientCore) -> DesktopRuntimeSnapshot {
    let store = core.store().snapshot();
    let peer_records = core.peer_records().await;
    let peer_statuses: HashMap<String, ClientPeerStatus> = core
        .peer_statuses()
        .into_iter()
        .map(|status| (status.agent_did.clone(), status))
        .collect();

    let mut deployments = peer_records
        .into_iter()
        .map(|peer| {
            let status = peer_statuses.get(&peer.agent_did);
            let require_source_scope = peer
                .graphql
                .as_deref()
                .is_some_and(|graphql| !graphql.trim().is_empty());
            let principal = store
                .agent_principals
                .iter()
                .find(|row| row.agent_did == peer.agent_did);
            let agent_principal = principal
                .map(|row| AgentPrincipalView {
                    agent_did: row.agent_did.clone(),
                    display_name: normalize_optional(row.display_name.as_deref()),
                    default_behavior_id: normalize_optional(row.default_behavior_id.as_deref()),
                    enabled: row.enabled,
                    created_at: normalize_optional(row.created_at.as_deref()),
                    created_by: normalize_optional(row.created_by.as_deref()),
                })
                .unwrap_or_else(|| AgentPrincipalView {
                    agent_did: peer.agent_did.clone(),
                    display_name: Some(peer.label.clone()),
                    default_behavior_id: None,
                    enabled: Some(true),
                    created_at: None,
                    created_by: None,
                });
            let default_behavior_id = store
                .default_behavior_id_for_agent(&peer.agent_did)
                .map(str::to_owned);
            let runtime = store
                .latest_runtime(&peer.agent_did)
                .map(|row| RuntimeView {
                    process_state: normalize_optional(row.process_state.as_deref()),
                    reconcile_phase: normalize_optional(row.reconcile_phase.as_deref()),
                    last_reconcile_result: normalize_optional(row.last_reconcile_result.as_deref()),
                    last_reconcile_error: normalize_optional(row.last_reconcile_error.as_deref()),
                    updated_at: normalize_optional(row.updated_at.as_deref()),
                });

            let mut behaviors = store
                .behavior_rows(&peer.agent_did)
                .into_iter()
                .map(|row| BehaviorView {
                    behavior_id: row.behavior_id.clone(),
                    display_name: normalize_optional(row.display_name.as_deref())
                        .unwrap_or_else(|| row.behavior_id.clone()),
                    system_prompt: normalize_optional(row.system_prompt.as_deref()),
                    backend_id: normalize_optional(row.backend_id.as_deref()),
                    model_name: normalize_optional(row.model_name.as_deref()),
                    tool_selection_id: normalize_optional(row.tool_selection_id.as_deref()),
                    inference_profile_id: normalize_optional(row.inference_profile_id.as_deref()),
                    compaction_strategy: normalize_optional(row.compaction_strategy.as_deref()),
                    compaction_threshold: row.compaction_threshold,
                    enabled: row.enabled.unwrap_or(true),
                    is_default: default_behavior_id.as_deref() == Some(row.behavior_id.as_str()),
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
                .enumerate()
                .filter(|(index, _row)| {
                    source_matches_agent(
                        &store.inference_backend_source_agent_dids,
                        *index,
                        &peer.agent_did,
                        require_source_scope,
                    )
                })
                .map(|(_index, row)| InferenceBackendView {
                    backend_id: row.backend_id.clone(),
                    name: normalize_optional(row.name.as_deref()),
                    provider_kind: normalize_optional(row.provider_kind.as_deref()),
                    endpoint: normalize_optional(row.endpoint.as_deref()),
                    api_key_configured: normalize_optional(row.api_key.as_deref()).is_some(),
                    api_key_env_var: normalize_optional(row.api_key_env_var.as_deref()),
                    max_concurrent: row.max_concurrent,
                    max_queue_depth: row.max_queue_depth,
                    enabled: row.enabled,
                    models: row.models.clone(),
                    probe_status: normalize_optional(row.probe_status.as_deref()),
                })
                .collect::<Vec<_>>();
            inference_backends.sort_by(|left, right| left.backend_id.cmp(&right.backend_id));

            let mut inference_profiles = store
                .inference_profiles
                .iter()
                .enumerate()
                .filter(|(index, _row)| {
                    source_matches_agent(
                        &store.inference_profile_source_agent_dids,
                        *index,
                        &peer.agent_did,
                        require_source_scope,
                    )
                })
                .map(|(_index, row)| InferenceProfileView {
                    profile_id: row.profile_id.clone(),
                    display_name: normalize_optional(row.display_name.as_deref()),
                    context_window: row.context_window,
                    max_output_tokens: row.max_output_tokens,
                    max_turns: row.max_turns,
                    temperature: row.temperature,
                    stream_batch_ms: row.stream_batch_ms,
                    deadline_duration_secs: row.deadline_duration_secs,
                })
                .collect::<Vec<_>>();
            inference_profiles.sort_by(|left, right| left.profile_id.cmp(&right.profile_id));

            let mut tool_selections = store
                .tool_selections
                .iter()
                .filter(|row| row.agent_did.as_deref() == Some(peer.agent_did.as_str()))
                .map(|row| ToolSelectionView {
                    selection_id: row.selection_id.clone(),
                    agent_did: normalize_optional(row.agent_did.as_deref()),
                    display_name: normalize_optional(row.display_name.as_deref()),
                    enable_file_tools: row.enable_file_tools,
                    file_tools_mode: normalize_optional(row.file_tools_mode.as_deref()),
                    file_tool_root: normalize_optional(row.file_tool_root.as_deref()),
                    enable_bash: row.enable_bash,
                    bash_mode: normalize_optional(row.bash_mode.as_deref()),
                    command_execution_policy: normalize_optional(
                        row.command_execution_policy.as_deref(),
                    ),
                    command_allowed_argv_prefixes: row.command_allowed_argv_prefixes.clone(),
                    command_forbidden_argv_prefixes: row.command_forbidden_argv_prefixes.clone(),
                    command_network_mode: normalize_optional(row.command_network_mode.as_deref()),
                    cli_tool_names: row.cli_tool_names.clone(),
                    enable_meta_tools: row.enable_meta_tools,
                    delegate_to: row.delegate_to.clone(),
                })
                .collect::<Vec<_>>();
            tool_selections.sort_by(|left, right| left.selection_id.cmp(&right.selection_id));

            let mut tool_service_registries = store
                .tool_service_registries
                .iter()
                .enumerate()
                .filter(|(index, _row)| {
                    source_matches_agent(
                        &store.tool_service_registry_source_agent_dids,
                        *index,
                        &peer.agent_did,
                        require_source_scope,
                    )
                })
                .map(|(_index, row)| ToolServiceRegistryView {
                    service_id: row.service_id.clone(),
                    display_name: normalize_optional(row.display_name.as_deref()),
                    description: normalize_optional(row.description.as_deref()),
                    hostname: normalize_optional(row.hostname.as_deref()),
                    tailscale_ip: normalize_optional(row.tailscale_ip.as_deref()),
                    lan_ip: normalize_optional(row.lan_ip.as_deref()),
                    mcp_port: row.mcp_port,
                    mcp_path: normalize_optional(row.mcp_path.as_deref()),
                    status: normalize_optional(row.status.as_deref()),
                    version: normalize_optional(row.version.as_deref()),
                    updated_at: normalize_optional(row.updated_at.as_deref()),
                })
                .collect::<Vec<_>>();
            tool_service_registries.sort_by(|left, right| left.service_id.cmp(&right.service_id));

            let scoped_task_rows = store
                .tasks
                .iter()
                .enumerate()
                .filter(|(index, row)| {
                    source_matches_agent(
                        &store.task_source_agent_dids,
                        *index,
                        &peer.agent_did,
                        require_source_scope,
                    ) && row
                        .behavior_id
                        .as_deref()
                        .is_some_and(|behavior_id| behavior_ids.contains(&behavior_id))
                })
                .collect::<Vec<_>>();
            let task_ids = scoped_task_rows
                .iter()
                .map(|(_index, task)| task.task_id.as_str())
                .collect::<Vec<_>>();

            let mut schedules = store
                .schedules
                .iter()
                .enumerate()
                .filter(|(index, row)| {
                    source_matches_agent(
                        &store.schedule_source_agent_dids,
                        *index,
                        &peer.agent_did,
                        require_source_scope,
                    ) && row
                        .task_id
                        .as_deref()
                        .is_some_and(|task_id| task_ids.contains(&task_id))
                })
                .map(|(_index, row)| ScheduleView {
                    schedule_id: row.schedule_id.clone(),
                    task_id: normalize_optional(row.task_id.as_deref()),
                    interval_secs: row.interval_secs,
                    enabled: row.enabled,
                    concurrency: normalize_optional(row.concurrency.as_deref()),
                    next_run_at: normalize_optional(row.next_run_at.as_deref()),
                    last_attempt_at: normalize_optional(row.last_attempt_at.as_deref()),
                    last_status: normalize_optional(row.last_status.as_deref()),
                    last_error: normalize_optional(row.last_error.as_deref()),
                    fire_count: row.fire_count,
                })
                .collect::<Vec<_>>();
            schedules.sort_by(|left, right| left.schedule_id.cmp(&right.schedule_id));

            let mut event_triggers = store
                .event_triggers
                .iter()
                .enumerate()
                .filter(|(index, row)| {
                    source_matches_agent(
                        &store.event_trigger_source_agent_dids,
                        *index,
                        &peer.agent_did,
                        require_source_scope,
                    ) && row
                        .task_id
                        .as_deref()
                        .is_some_and(|task_id| task_ids.contains(&task_id))
                })
                .map(|(_index, row)| EventTriggerView {
                    trigger_id: row.trigger_id.clone(),
                    task_id: normalize_optional(row.task_id.as_deref()),
                    source_collection: normalize_optional(row.source_collection.as_deref()),
                    event_kind: normalize_optional(row.event_kind.as_deref()),
                    filter: normalize_optional(row.filter.as_deref()),
                    enabled: row.enabled,
                    concurrency: normalize_optional(row.concurrency.as_deref()),
                    last_attempt_at: normalize_optional(row.last_attempt_at.as_deref()),
                    last_fired_source_doc_id: normalize_optional(
                        row.last_fired_source_doc_id.as_deref(),
                    ),
                    last_status: normalize_optional(row.last_status.as_deref()),
                    last_error: normalize_optional(row.last_error.as_deref()),
                    fire_count: row.fire_count,
                })
                .collect::<Vec<_>>();
            event_triggers.sort_by(|left, right| left.trigger_id.cmp(&right.trigger_id));

            let mut tasks = scoped_task_rows
                .into_iter()
                .map(|(_index, row)| TaskView {
                    task_id: row.task_id.clone(),
                    name: normalize_optional(row.name.as_deref()),
                    description: normalize_optional(row.description.as_deref()),
                    behavior_id: normalize_optional(row.behavior_id.as_deref()),
                    prompt_template: normalize_optional(row.prompt_template.as_deref()),
                    enabled: row.enabled,
                    output_schema_ref: normalize_optional(row.output_schema_ref.as_deref()),
                    recent_runs: recent_runs_for_task_views(
                        &schedules,
                        &event_triggers,
                        &row.task_id,
                    ),
                    run_history: task_run_history(
                        store.as_ref(),
                        &peer.agent_did,
                        require_source_scope,
                        &row.task_id,
                        &schedules,
                        &event_triggers,
                    ),
                })
                .collect::<Vec<_>>();
            tasks.sort_by(|left, right| left.task_id.cmp(&right.task_id));

            let mut conversations = store
                .conversation_rows(&peer.agent_did)
                .into_iter()
                .map(|row| {
                    let transcript = store.transcript_for_agent(&row.session_id, &peer.agent_did);
                    let task_tag = conversation_task_tag(
                        store.as_ref(),
                        &peer.agent_did,
                        require_source_scope,
                        &row.session_id,
                        &tasks,
                        &schedules,
                        &event_triggers,
                    );
                    ConversationSummary {
                        session_id: row.session_id.clone(),
                        title: normalize_optional(row.title.as_deref()),
                        preview_text: normalize_optional(row.preview_text.as_deref()),
                        status: normalize_optional(row.status.as_deref()),
                        behavior_id: normalize_optional(row.behavior_id.as_deref()),
                        latest_request_id: store.latest_request_id_for_session_for_agent(
                            &row.session_id,
                            &peer.agent_did,
                        ),
                        task_id: task_tag.as_ref().map(|tag| tag.task_id.clone()),
                        task_name: task_tag.as_ref().and_then(|tag| tag.task_name.clone()),
                        trigger_id: task_tag.as_ref().and_then(|tag| tag.trigger_id.clone()),
                        trigger_kind: task_tag.as_ref().and_then(|tag| tag.trigger_kind.clone()),
                        created_at: normalize_optional(row.created_at.as_deref()),
                        updated_at: normalize_optional(row.updated_at.as_deref()),
                        turn_state: store
                            .latest_request_id_for_session_for_agent(
                                &row.session_id,
                                &peer.agent_did,
                            )
                            .as_deref()
                            .and_then(|request_id| store.derive_turn_for_request(request_id))
                            .map(turn_state_label)
                            .map(str::to_owned),
                        message_count: transcript.messages.len(),
                        tool_call_count: transcript.tool_calls.len(),
                    }
                })
                .collect::<Vec<_>>();
            conversations.sort_by(|left, right| {
                right
                    .updated_at
                    .cmp(&left.updated_at)
                    .then_with(|| right.created_at.cmp(&left.created_at))
            });
            retain_latest_conversation_summaries(&mut conversations);

            DeploymentView {
                peer_id: peer.peer_id,
                label: peer.label,
                agent_did: peer.agent_did,
                addr: peer.addr,
                source: peer.source,
                graphql: peer.graphql,
                dial_succeeded: status.is_some_and(|status| status.dial_succeeded),
                last_error: status.and_then(|status| status.last_error.clone()),
                default_behavior_id,
                agent_principal,
                runtime,
                behaviors,
                inference_backends,
                inference_profiles,
                tool_selections,
                tool_service_registries,
                tasks,
                schedules,
                event_triggers,
                conversations,
            }
        })
        .collect::<Vec<_>>();

    deployments.sort_by(|left, right| left.label.cmp(&right.label));

    DesktopRuntimeSnapshot {
        local_peer_id: core.local_peer_id().to_string(),
        listen_addresses: core.listen_addresses().to_vec(),
        p2p_health: to_health_view(&core.p2p_health()),
        bootstrap_errors: core.bootstrap_errors().to_vec(),
        last_mutation_error: core.last_mutation_error(),
        focused_request_id: core.store().focused_request_id(),
        configured_peer_count: core.configured_peer_count(),
        dialed_peer_count: core.dialed_peer_count(),
        peer_issue_count: core.peer_issue_count(),
        row_count: store.row_count(),
        approx_serialized_bytes: store.approx_serialized_bytes(),
        deployments,
    }
}
