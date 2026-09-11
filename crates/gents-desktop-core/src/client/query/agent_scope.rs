use super::*;

/// Load the bounded observer projection for a specific `agent_did`.
/// Agent-keyed collections (including Goal) are filtered by `agent_did`;
/// session metadata is read directly from the canonical AgentSession owner.
/// Transcript content is intentionally excluded and remains in DefraDB.
/// Control-plane
/// collections (InferenceBackend, InferenceProfile, ToolServiceRegistry,
/// Task and Schedule) load in full — they're operator-authored
/// and small.
pub async fn load_agent_scoped_snapshot(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<ClientStore> {
    let did = escape_graphql_string(agent_did);
    let did_filter = format!("filter: {{ agent_did: {{ _eq: \"{did}\" }} }}");

    // Agent-keyed collections.
    let agent_principals: Vec<AgentPrincipal> = load_rows(
        node,
        AGENT_PRINCIPAL_NAME,
        &format!("query {{ {AGENT_PRINCIPAL_NAME}({did_filter}) {{ {AGENT_PRINCIPAL_FIELDS} }} }}"),
    )
    .await?;
    let behaviors: Vec<AgentBehavior> = load_rows(
        node,
        AGENT_BEHAVIOR_NAME,
        &format!("query {{ {AGENT_BEHAVIOR_NAME}({did_filter}) {{ {AGENT_BEHAVIOR_FIELDS} }} }}"),
    )
    .await?;
    let runtimes: Vec<AgentRuntimeRow> = load_rows(
        node,
        AGENT_RUNTIME_NAME,
        &format!("query {{ {AGENT_RUNTIME_NAME}({did_filter}) {{ {AGENT_RUNTIME_FIELDS} }} }}"),
    )
    .await?;
    let behavior_readiness: Vec<AgentBehaviorReadinessRow> = load_rows(
        node,
        AGENT_BEHAVIOR_READINESS_NAME,
        &format!(
            "query {{ {AGENT_BEHAVIOR_READINESS_NAME}({did_filter}) {{ {AGENT_BEHAVIOR_READINESS_FIELDS} }} }}"
        ),
    )
    .await?;
    let requests: Vec<AgentRequestRow> = load_rows(
        node,
        AGENT_REQUEST_NAME,
        &format!("query {{ {AGENT_REQUEST_NAME}({did_filter}) {{ {AGENT_REQUEST_FIELDS} }} }}"),
    )
    .await?;
    let mailbox_items: Vec<MailboxItemRow> = load_rows(
        node,
        MAILBOX_ITEM_NAME,
        &format!("query {{ {MAILBOX_ITEM_NAME}({did_filter}) {{ {MAILBOX_ITEM_FIELDS} }} }}"),
    )
    .await?;
    let responses: Vec<AgentResponseRow> = load_rows(
        node,
        AGENT_RESPONSE_NAME,
        &format!("query {{ {AGENT_RESPONSE_NAME}({did_filter}) {{ {AGENT_RESPONSE_FIELDS} }} }}"),
    )
    .await?;
    let goals: Vec<GoalRow> = load_rows(
        node,
        GOAL_NAME,
        &format!("query {{ {GOAL_NAME}({did_filter}) {{ {GOAL_FIELDS} }} }}"),
    )
    .await?;
    let sessions: Vec<AgentSession> = load_rows(
        node,
        AGENT_SESSION_NAME,
        &format!("query {{ {AGENT_SESSION_NAME}({did_filter}) {{ {AGENT_SESSION_FIELDS} }} }}"),
    )
    .await?;
    let tools: Vec<Tools> = load_rows(
        node,
        TOOLS_NAME,
        &format!("query {{ {TOOLS_NAME}({did_filter}) {{ {TOOLS_FIELDS} }} }}"),
    )
    .await?;
    let contexts: Vec<AgentContext> = load_rows(
        node,
        "AgentContext",
        &format!("query {{ AgentContext({did_filter}) {{ {AGENT_CONTEXT_FIELDS} }} }}"),
    )
    .await?;
    let compactions: Vec<CompactionConfig> = load_rows(
        node,
        "CompactionConfig",
        &format!("query {{ CompactionConfig({did_filter}) {{ {COMPACTION_CONFIG_FIELDS} }} }}"),
    )
    .await?;
    let triggers: Vec<Trigger> = load_rows(
        node,
        TRIGGER_NAME,
        &format!("query {{ {TRIGGER_NAME}({did_filter}) {{ {TRIGGER_FIELDS} }} }}"),
    )
    .await?;
    let trigger_observations: Vec<TriggerObservation> = load_rows(
        node,
        TRIGGER_NAME,
        &format!("query {{ {TRIGGER_NAME}({did_filter}) {{ {TRIGGER_OBSERVATION_FIELDS} }} }}"),
    )
    .await?;

    let tasks: Vec<Task> = load_rows(
        node,
        TASK_NAME,
        &format!("query {{ {TASK_NAME}({did_filter}) {{ {TASK_FIELDS} }} }}"),
    )
    .await?;
    let schedules: Vec<Schedule> = load_rows(
        node,
        SCHEDULE_NAME,
        &format!("query {{ {SCHEDULE_NAME}({did_filter}) {{ {SCHEDULE_FIELDS} }} }}"),
    )
    .await?;
    let schedule_observations: Vec<ScheduleObservation> = load_rows(
        node,
        TRIGGER_NAME,
        &format!("query {{ {TRIGGER_NAME}({did_filter}) {{ {SCHEDULE_OBSERVATION_FIELDS} }} }}"),
    )
    .await?;
    let skills: Vec<SkillDocument> = load_rows(
        node,
        SKILL_NAME,
        &format!("query {{ {SKILL_NAME}({did_filter}) {{ {SKILL_FIELDS} }} }}"),
    )
    .await?;
    let inference_backends: Vec<InferenceBackend> = load_rows(
        node,
        INFERENCE_BACKEND_NAME,
        &format!(
            "query {{ {INFERENCE_BACKEND_NAME}({did_filter}) {{ {INFERENCE_BACKEND_FIELDS} }} }}"
        ),
    )
    .await?;
    let backend_observations: Vec<InferenceBackendObservation> = load_rows(
        node,
        INFERENCE_BACKEND_NAME,
        &format!(
            "query {{ {INFERENCE_BACKEND_NAME}({did_filter}) {{ {INFERENCE_BACKEND_OBSERVATION_FIELDS} }} }}"
        ),
    )
    .await?;
    let inference_profiles: Vec<InferenceProfile> = load_rows(
        node,
        INFERENCE_PROFILE_NAME,
        &format!(
            "query {{ {INFERENCE_PROFILE_NAME}({did_filter}) {{ {INFERENCE_PROFILE_FIELDS} }} }}"
        ),
    )
    .await?;
    let inference_sampling: Vec<InferenceSampling> = load_rows(
        node,
        "InferenceSampling",
        &format!("query {{ InferenceSampling({did_filter}) {{ {INFERENCE_SAMPLING_FIELDS} }} }}"),
    )
    .await?;
    let inference_execution: Vec<InferenceExecution> = load_rows(
        node,
        "InferenceExecution",
        &format!("query {{ InferenceExecution({did_filter}) {{ {INFERENCE_EXECUTION_FIELDS} }} }}"),
    )
    .await?;
    let tool_service_registries: Vec<ToolServiceRegistry> = load_rows(
        node,
        TOOL_SERVICE_REGISTRY_NAME,
        &format!(
            "query {{ {TOOL_SERVICE_REGISTRY_NAME}({did_filter}) {{ {TOOL_SERVICE_REGISTRY_FIELDS} }} }}"
        ),
    )
    .await?;
    let event_sources: Vec<EventSource> = load_rows(
        node,
        "EventSource",
        &format!("query {{ EventSource({did_filter}) {{ {EVENT_SOURCE_FIELDS} }} }}"),
    )
    .await?;
    let subagent_targets: Vec<SubagentTargetDocument> = load_rows(
        node,
        "SubagentTarget",
        &format!("query {{ SubagentTarget({did_filter}) {{ {SUBAGENT_TARGET_FIELDS} }} }}"),
    )
    .await?;
    let datastore_tool_surfaces: Vec<DatastoreToolSurfaceDocument> = load_rows(
        node,
        "DatastoreToolSurface",
        &format!(
            "query {{ DatastoreToolSurface({did_filter}) {{ {DATASTORE_TOOL_SURFACE_FIELDS} }} }}"
        ),
    )
    .await?;
    let chain_key_bindings: Vec<ChainKeyBindingDocument> = load_rows(
        node,
        "ChainKeyBinding",
        &format!("query {{ ChainKeyBinding({did_filter}) {{ {CHAIN_KEY_BINDING_FIELDS} }} }}"),
    )
    .await?;
    let session_source_agent_dids = vec![Some(agent_did.to_string()); sessions.len()];
    let trigger_source_agent_dids = vec![Some(agent_did.to_string()); triggers.len()];
    let trigger_observation_source_agent_dids =
        vec![Some(agent_did.to_string()); trigger_observations.len()];
    let source_scope = |len| vec![Some(agent_did.to_string()); len];
    let task_source_agent_dids = source_scope(tasks.len());
    let schedule_source_agent_dids = source_scope(schedules.len());
    let schedule_observation_source_agent_dids = source_scope(schedule_observations.len());
    let skill_source_agent_dids = source_scope(skills.len());
    let tools_source_agent_dids = source_scope(tools.len());
    let context_source_agent_dids = source_scope(contexts.len());
    let compaction_source_agent_dids = source_scope(compactions.len());
    let inference_backend_source_agent_dids = source_scope(inference_backends.len());
    let backend_observation_source_agent_dids = source_scope(backend_observations.len());
    let inference_profile_source_agent_dids = source_scope(inference_profiles.len());
    let inference_sampling_source_agent_dids = source_scope(inference_sampling.len());
    let inference_execution_source_agent_dids = source_scope(inference_execution.len());
    let tool_service_registry_source_agent_dids = source_scope(tool_service_registries.len());
    let event_source_source_agent_dids = source_scope(event_sources.len());
    let subagent_target_source_agent_dids = source_scope(subagent_targets.len());
    let datastore_tool_surface_source_agent_dids = source_scope(datastore_tool_surfaces.len());
    let chain_key_binding_source_agent_dids = source_scope(chain_key_bindings.len());

    Ok(ClientStore::from_rows(ClientStoreRows {
        agent_principals,
        behaviors,
        runtimes,
        behavior_readiness,
        requests,
        mailbox_items,
        responses,
        sessions,
        session_source_agent_dids,
        goals,
        tasks,
        task_source_agent_dids,
        schedules,
        schedule_source_agent_dids,
        schedule_observation_source_agent_dids,
        schedule_observations,
        triggers,
        trigger_observations,
        trigger_source_agent_dids,
        trigger_observation_source_agent_dids,
        skills,
        skill_source_agent_dids,
        tools,
        tools_source_agent_dids,
        contexts,
        context_source_agent_dids,
        compactions,
        compaction_source_agent_dids,
        inference_backends,
        inference_backend_source_agent_dids,
        backend_observations,
        backend_observation_source_agent_dids,
        inference_profiles,
        inference_profile_source_agent_dids,
        inference_sampling,
        inference_sampling_source_agent_dids,
        inference_execution,
        inference_execution_source_agent_dids,
        tool_service_registries,
        tool_service_registry_source_agent_dids,
        event_sources,
        event_source_source_agent_dids,
        subagent_targets,
        subagent_target_source_agent_dids,
        datastore_tool_surfaces,
        datastore_tool_surface_source_agent_dids,
        chain_key_bindings,
        chain_key_binding_source_agent_dids,
        ..ClientStoreRows::default()
    }))
}
