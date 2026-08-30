use super::*;

pub async fn load_full_snapshot(node: &EmbeddedNode) -> Result<ClientStore> {
    Ok(ClientStore::from_rows(ClientStoreRows {
        agent_principals: load_agent_principals(node).await?,
        behaviors: load_agent_behaviors(node).await?,
        runtimes: load_agent_runtimes(node).await?,
        behavior_readiness: load_agent_behavior_readiness(node).await?,
        conversations: load_agent_conversations(node).await?,
        requests: load_agent_requests(node).await?,
        mailbox_items: load_mailbox_items(node).await?,
        responses: load_agent_responses(node).await?,
        sessions: load_agent_sessions(node).await?,
        goals: load_goals(node).await?,
        tasks: load_tasks(node).await?,
        schedules: load_schedules(node).await?,
        event_triggers: load_event_triggers(node).await?,
        skills: load_skills(node).await?,
        tool_selections: load_tool_selections(node).await?,
        inference_backends: load_inference_backends(node).await?,
        inference_profiles: load_inference_profiles(node).await?,
        tool_service_registries: load_tool_service_registries(node).await?,
        ..ClientStoreRows::default()
    }))
}

pub async fn load_full_snapshot_with_peer_records(
    node: &EmbeddedNode,
    _peers: &[PeerRecord],
    _requester_did: &str,
) -> Result<ClientStore> {
    load_full_snapshot(node).await
}

pub async fn load_agent_scoped_snapshot_with_peer_records(
    node: &EmbeddedNode,
    agent_did: &str,
    _peers: &[PeerRecord],
    _requester_did: &str,
) -> Result<ClientStore> {
    load_agent_scoped_snapshot(node, agent_did).await
}

pub async fn load_agent_principals(node: &EmbeddedNode) -> Result<Vec<AgentPrincipalRow>> {
    load_rows(
        node,
        "AgentPrincipal",
        "query { AgentPrincipal { agent_did display_name default_behavior_id enabled created_at created_by } }",
    )
    .await
}

pub async fn load_agent_behaviors(node: &EmbeddedNode) -> Result<Vec<AgentBehaviorRow>> {
    load_rows(
        node,
        "AgentBehavior",
        "query { AgentBehavior { behavior_id agent_did display_name system_prompt backend_id model_name tool_selection_id inference_profile_id compaction_strategy compaction_threshold enabled skill_refs skill_excludes created_at } }",
    )
    .await
}

pub async fn load_agent_runtimes(node: &EmbeddedNode) -> Result<Vec<AgentRuntimeRow>> {
    load_rows(
        node,
        AGENT_RUNTIME_NAME,
        &format!("query {{ {AGENT_RUNTIME_NAME} {{ {AGENT_RUNTIME_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_agent_behavior_readiness(
    node: &EmbeddedNode,
) -> Result<Vec<AgentBehaviorReadinessRow>> {
    load_rows(
        node,
        AGENT_BEHAVIOR_READINESS_NAME,
        &format!(
            "query {{ {AGENT_BEHAVIOR_READINESS_NAME} {{ {AGENT_BEHAVIOR_READINESS_FIELDS} }} }}"
        ),
    )
    .await
}

pub async fn load_agent_conversations(node: &EmbeddedNode) -> Result<Vec<AgentConversationRow>> {
    load_rows(
        node,
        AGENT_CONVERSATION_NAME,
        &format!("query {{ {AGENT_CONVERSATION_NAME} {{ {AGENT_CONVERSATION_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_agent_requests(node: &EmbeddedNode) -> Result<Vec<AgentRequestRow>> {
    load_rows(
        node,
        AGENT_REQUEST_NAME,
        &format!("query {{ {AGENT_REQUEST_NAME} {{ {AGENT_REQUEST_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_mailbox_items(node: &EmbeddedNode) -> Result<Vec<MailboxItemRow>> {
    load_rows(
        node,
        MAILBOX_ITEM_NAME,
        &format!("query {{ {MAILBOX_ITEM_NAME} {{ {MAILBOX_ITEM_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_agent_responses(node: &EmbeddedNode) -> Result<Vec<AgentResponseRow>> {
    load_rows(
        node,
        AGENT_RESPONSE_NAME,
        &format!("query {{ {AGENT_RESPONSE_NAME} {{ {AGENT_RESPONSE_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_agent_sessions(node: &EmbeddedNode) -> Result<Vec<AgentSessionRow>> {
    load_rows(
        node,
        AGENT_SESSION_NAME,
        &format!("query {{ {AGENT_SESSION_NAME} {{ {AGENT_SESSION_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_goals(node: &EmbeddedNode) -> Result<Vec<GoalRow>> {
    load_rows(
        node,
        "Goal",
        &format!("query {{ Goal {{ {GOAL_FIELDS} }} }}"),
    )
    .await
}

#[cfg(test)]
pub(crate) async fn load_agent_tool_calls(node: &EmbeddedNode) -> Result<Vec<AgentToolCallRow>> {
    load_rows(
        node,
        AGENT_TOOL_CALL_NAME,
        &format!("query {{ {AGENT_TOOL_CALL_NAME} {{ {AGENT_TOOL_CALL_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_tasks(node: &EmbeddedNode) -> Result<Vec<TaskRow>> {
    load_rows(
        node,
        "Task",
        "query { Task { task_id name description behavior_id prompt_template enabled output_schema_ref created_at updated_at } }",
    )
    .await
}

pub async fn load_skills(node: &EmbeddedNode) -> Result<Vec<SkillRow>> {
    load_rows(
        node,
        SKILL_NAME,
        &format!("query {{ {SKILL_NAME} {{ {SKILL_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_schedules(node: &EmbeddedNode) -> Result<Vec<ScheduleRow>> {
    load_rows(
        node,
        "Schedule",
        "query { Schedule { schedule_id task_id interval_secs cron timezone missed_run_policy enabled concurrency next_run_at last_attempt_at last_status last_error fire_count created_at updated_at } }",
    )
    .await
}

pub async fn load_event_triggers(node: &EmbeddedNode) -> Result<Vec<EventTriggerRow>> {
    load_rows(
        node,
        "EventTrigger",
        "query { EventTrigger { trigger_id task_id source_collection event_kind filter correlation_field fire_mode expected_count expected_count_field group_timeout_secs group_min_count workspace_authority enabled concurrency created_at updated_at last_attempt_at last_fired_source_doc_id last_status last_error fire_count } }",
    )
    .await
}

pub async fn load_tool_selections(node: &EmbeddedNode) -> Result<Vec<ToolSelectionRow>> {
    load_rows(
        node,
        "ToolSelection",
        "query { ToolSelection { selection_id agent_did display_name enable_file_tools file_tools_mode file_tool_root enable_bash bash_mode command_execution_policy command_allowed_argv_prefixes command_forbidden_argv_prefixes command_network_mode cli_tool_names enable_meta_tools allowed_mcp_service_ids delegate_to backgroundable_tool_names enable_memory enable_session_history_tool enable_context_budget enable_defra_query defra_query_collections subagent_targets subagent_spawn_enabled subagent_steering_enabled subagent_background_enabled subagent_allow_cross_deployment cross_deployment_spawn_timeout_seconds tool_policy_version write_tools datastore_tool_surface_ids eth_tool_ids subagent_default_await_mode enable_self_config self_config_categories self_config_no_lockout self_config_dry_run enable_lsp lsp_config } }",
    )
    .await
}

pub async fn load_inference_backends(node: &EmbeddedNode) -> Result<Vec<InferenceBackendRow>> {
    load_rows(
        node,
        "InferenceBackend",
        "query { InferenceBackend { backend_id name provider_kind openai_wire_api endpoint api_key api_key_env_var max_concurrent max_queue_depth enabled models last_probe probe_status } }",
    )
    .await
}

pub async fn load_inference_profiles(node: &EmbeddedNode) -> Result<Vec<InferenceProfileRow>> {
    load_rows(
        node,
        "InferenceProfile",
        "query { InferenceProfile { profile_id display_name context_window max_output_tokens max_turns temperature top_p top_k seed min_p frequency_penalty presence_penalty repetition_penalty reasoning_effort stream_batch_ms stream_liveness_timeout_secs deadline_duration_secs retry_max_transport retry_backoff_ms retry_max_resample retry_allow_repair retry_interactive_max } }",
    )
    .await
}

pub async fn load_tool_service_registries(
    node: &EmbeddedNode,
) -> Result<Vec<ToolServiceRegistryRow>> {
    load_rows(
        node,
        "ToolServiceRegistry",
        "query { ToolServiceRegistry { service_id display_name description hostname tailscale_ip lan_ip mcp_port mcp_path status version updated_at } }",
    )
    .await
}
