use super::*;

pub async fn load_full_snapshot(node: &EmbeddedNode) -> Result<ClientStore> {
    Ok(ClientStore::from_rows(ClientStoreRows {
        agent_principals: load_agent_principals(node).await?,
        behaviors: load_agent_behaviors(node).await?,
        runtimes: load_agent_runtimes(node).await?,
        behavior_readiness: load_agent_behavior_readiness(node).await?,
        requests: load_agent_requests(node).await?,
        mailbox_items: load_mailbox_items(node).await?,
        responses: load_agent_responses(node).await?,
        sessions: load_agent_sessions(node).await?,
        goals: load_goals(node).await?,
        tasks: load_tasks(node).await?,
        schedules: load_schedules(node).await?,
        schedule_observations: load_schedule_observations(node).await?,
        triggers: load_triggers(node).await?,
        trigger_observations: load_trigger_observations(node).await?,
        skills: load_skills(node).await?,
        tools: load_tools(node).await?,
        contexts: load_contexts(node).await?,
        compactions: load_compactions(node).await?,
        inference_backends: load_inference_backends(node).await?,
        backend_observations: load_backend_observations(node).await?,
        inference_profiles: load_inference_profiles(node).await?,
        inference_sampling: load_inference_sampling(node).await?,
        inference_execution: load_inference_execution(node).await?,
        tool_service_registries: load_tool_service_registries(node).await?,
        event_sources: load_event_sources(node).await?,
        subagent_targets: load_subagent_targets(node).await?,
        datastore_tool_surfaces: load_datastore_tool_surfaces(node).await?,
        chain_key_bindings: load_chain_key_bindings(node).await?,
        ..ClientStoreRows::default()
    }))
}

pub async fn load_full_snapshot_with_peer_records(
    node: &EmbeddedNode,
    peers: &[PeerRecord],
    _requester_did: &str,
) -> Result<ClientStore> {
    let mut store = load_full_snapshot(node).await?;
    for peer in peers {
        let Some(graphql) = peer.operator_graphql() else {
            continue;
        };
        match load_operator_config(&gents::config_client::ConfigAccess::Graphql(
            graphql.to_string(),
        ))
        .await
        {
            Ok(remote) => {
                store = store.overlay_agent_operator_config(&peer.agent_did, &remote);
            }
            Err(error) => {
                tracing::warn!(
                    target: "gents_desktop_core::query",
                    agent_did = %peer.agent_did,
                    graphql,
                    error = %error,
                    "operator GraphQL config overlay failed; keeping the desktop replica"
                );
            }
        }
    }
    Ok(store)
}

async fn load_operator_config(access: &gents::config_client::ConfigAccess) -> Result<ClientStore> {
    Ok(ClientStore::from_rows(ClientStoreRows {
        agent_principals: load_rows_from_access(
            access,
            "AgentPrincipal",
            &format!("query {{ AgentPrincipal {{ {AGENT_PRINCIPAL_FIELDS} }} }}"),
        )
        .await?,
        behaviors: load_rows_from_access(
            access,
            "AgentBehavior",
            &format!("query {{ AgentBehavior {{ {AGENT_BEHAVIOR_FIELDS} }} }}"),
        )
        .await?,
        runtimes: load_rows_from_access(
            access,
            AGENT_RUNTIME_NAME,
            &format!("query {{ {AGENT_RUNTIME_NAME} {{ {AGENT_RUNTIME_FIELDS} }} }}"),
        )
        .await?,
        behavior_readiness: load_rows_from_access(
            access,
            AGENT_BEHAVIOR_READINESS_NAME,
            &format!(
                "query {{ {AGENT_BEHAVIOR_READINESS_NAME} {{ {AGENT_BEHAVIOR_READINESS_FIELDS} }} }}"
            ),
        )
        .await?,
        contexts: load_rows_from_access(
            access,
            "AgentContext",
            &format!("query {{ AgentContext {{ {AGENT_CONTEXT_FIELDS} }} }}"),
        )
        .await?,
        tools: load_rows_from_access(
            access,
            TOOLS_NAME,
            &format!("query {{ {TOOLS_NAME} {{ {TOOLS_FIELDS} }} }}"),
        )
        .await?,
        inference_backends: load_rows_from_access(
            access,
            "InferenceBackend",
            &format!("query {{ InferenceBackend {{ {INFERENCE_BACKEND_FIELDS} }} }}"),
        )
        .await?,
        backend_observations: load_rows_from_access(
            access,
            "InferenceBackend",
            &format!(
                "query {{ InferenceBackend {{ {INFERENCE_BACKEND_OBSERVATION_FIELDS} }} }}"
            ),
        )
        .await?,
        inference_profiles: load_rows_from_access(
            access,
            "InferenceProfile",
            &format!("query {{ InferenceProfile {{ {INFERENCE_PROFILE_FIELDS} }} }}"),
        )
        .await?,
        inference_sampling: load_rows_from_access(
            access,
            "InferenceSampling",
            &format!("query {{ InferenceSampling {{ {INFERENCE_SAMPLING_FIELDS} }} }}"),
        )
        .await?,
        inference_execution: load_rows_from_access(
            access,
            "InferenceExecution",
            &format!("query {{ InferenceExecution {{ {INFERENCE_EXECUTION_FIELDS} }} }}"),
        )
        .await?,
        sessions: load_rows_from_access(
            access,
            AGENT_SESSION_NAME,
            &format!("query {{ {AGENT_SESSION_NAME} {{ {AGENT_SESSION_FIELDS} }} }}"),
        )
        .await?,
        requests: load_rows_from_access(
            access,
            AGENT_REQUEST_NAME,
            &format!("query {{ {AGENT_REQUEST_NAME} {{ {AGENT_REQUEST_FIELDS} }} }}"),
        )
        .await?,
        responses: load_rows_from_access(
            access,
            AGENT_RESPONSE_NAME,
            &format!("query {{ {AGENT_RESPONSE_NAME} {{ {AGENT_RESPONSE_FIELDS} }} }}"),
        )
        .await?,
        ..ClientStoreRows::default()
    }))
}

pub async fn load_agent_scoped_snapshot_with_peer_records(
    node: &EmbeddedNode,
    agent_did: &str,
    peers: &[PeerRecord],
    _requester_did: &str,
) -> Result<ClientStore> {
    let mut store = load_agent_scoped_snapshot(node, agent_did).await?;
    if let Some(graphql) = peers
        .iter()
        .find(|peer| peer.agent_did == agent_did)
        .and_then(PeerRecord::operator_graphql)
    {
        match load_operator_config(&gents::config_client::ConfigAccess::Graphql(
            graphql.to_string(),
        ))
        .await
        {
            Ok(remote) => {
                store = store.overlay_agent_operator_config(agent_did, &remote);
            }
            Err(error) => {
                tracing::warn!(
                    target: "gents_desktop_core::query",
                    agent_did,
                    graphql,
                    error = %error,
                    "operator GraphQL config overlay failed; keeping the desktop replica"
                );
            }
        }
    }
    Ok(store)
}

pub async fn load_agent_principals(node: &EmbeddedNode) -> Result<Vec<AgentPrincipal>> {
    load_rows(
        node,
        "AgentPrincipal",
        &format!("query {{ AgentPrincipal {{ {AGENT_PRINCIPAL_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_agent_behaviors(node: &EmbeddedNode) -> Result<Vec<AgentBehavior>> {
    load_rows(
        node,
        "AgentBehavior",
        &format!("query {{ AgentBehavior {{ {AGENT_BEHAVIOR_FIELDS} }} }}"),
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

pub async fn load_agent_sessions(node: &EmbeddedNode) -> Result<Vec<AgentSession>> {
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

pub async fn load_tasks(node: &EmbeddedNode) -> Result<Vec<Task>> {
    load_rows(
        node,
        "Task",
        &format!("query {{ Task {{ {TASK_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_skills(node: &EmbeddedNode) -> Result<Vec<SkillDocument>> {
    load_rows(
        node,
        SKILL_NAME,
        &format!("query {{ {SKILL_NAME} {{ {SKILL_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_schedules(node: &EmbeddedNode) -> Result<Vec<Schedule>> {
    load_rows(
        node,
        "Schedule",
        &format!("query {{ Schedule {{ {SCHEDULE_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_schedule_observations(node: &EmbeddedNode) -> Result<Vec<ScheduleObservation>> {
    load_rows(
        node,
        TRIGGER_NAME,
        &format!("query {{ {TRIGGER_NAME} {{ {SCHEDULE_OBSERVATION_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_triggers(node: &EmbeddedNode) -> Result<Vec<Trigger>> {
    load_rows(
        node,
        TRIGGER_NAME,
        &format!("query {{ {TRIGGER_NAME} {{ {TRIGGER_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_trigger_observations(node: &EmbeddedNode) -> Result<Vec<TriggerObservation>> {
    load_rows(
        node,
        TRIGGER_NAME,
        &format!("query {{ {TRIGGER_NAME} {{ {TRIGGER_OBSERVATION_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_tools(node: &EmbeddedNode) -> Result<Vec<Tools>> {
    load_rows(
        node,
        TOOLS_NAME,
        &format!("query {{ {TOOLS_NAME} {{ {TOOLS_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_contexts(node: &EmbeddedNode) -> Result<Vec<AgentContext>> {
    load_rows(
        node,
        "AgentContext",
        &format!("query {{ AgentContext {{ {AGENT_CONTEXT_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_compactions(node: &EmbeddedNode) -> Result<Vec<CompactionConfig>> {
    load_rows(
        node,
        "CompactionConfig",
        &format!("query {{ CompactionConfig {{ {COMPACTION_CONFIG_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_inference_backends(node: &EmbeddedNode) -> Result<Vec<InferenceBackend>> {
    load_rows(
        node,
        "InferenceBackend",
        &format!("query {{ InferenceBackend {{ {INFERENCE_BACKEND_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_backend_observations(
    node: &EmbeddedNode,
) -> Result<Vec<InferenceBackendObservation>> {
    load_rows(
        node,
        "InferenceBackend",
        &format!("query {{ InferenceBackend {{ {INFERENCE_BACKEND_OBSERVATION_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_inference_profiles(node: &EmbeddedNode) -> Result<Vec<InferenceProfile>> {
    load_rows(
        node,
        "InferenceProfile",
        &format!("query {{ InferenceProfile {{ {INFERENCE_PROFILE_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_inference_sampling(node: &EmbeddedNode) -> Result<Vec<InferenceSampling>> {
    load_rows(
        node,
        "InferenceSampling",
        &format!("query {{ InferenceSampling {{ {INFERENCE_SAMPLING_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_inference_execution(node: &EmbeddedNode) -> Result<Vec<InferenceExecution>> {
    load_rows(
        node,
        "InferenceExecution",
        &format!("query {{ InferenceExecution {{ {INFERENCE_EXECUTION_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_tool_service_registries(node: &EmbeddedNode) -> Result<Vec<ToolServiceRegistry>> {
    load_rows(
        node,
        "ToolServiceRegistry",
        &format!("query {{ ToolServiceRegistry {{ {TOOL_SERVICE_REGISTRY_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_event_sources(node: &EmbeddedNode) -> Result<Vec<EventSource>> {
    load_rows(
        node,
        "EventSource",
        &format!("query {{ EventSource {{ {EVENT_SOURCE_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_subagent_targets(node: &EmbeddedNode) -> Result<Vec<SubagentTargetDocument>> {
    load_rows(
        node,
        "SubagentTarget",
        &format!("query {{ SubagentTarget {{ {SUBAGENT_TARGET_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_datastore_tool_surfaces(
    node: &EmbeddedNode,
) -> Result<Vec<DatastoreToolSurfaceDocument>> {
    load_rows(
        node,
        "DatastoreToolSurface",
        &format!("query {{ DatastoreToolSurface {{ {DATASTORE_TOOL_SURFACE_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_chain_key_bindings(node: &EmbeddedNode) -> Result<Vec<ChainKeyBindingDocument>> {
    load_rows(
        node,
        "ChainKeyBinding",
        &format!("query {{ ChainKeyBinding {{ {CHAIN_KEY_BINDING_FIELDS} }} }}"),
    )
    .await
}
