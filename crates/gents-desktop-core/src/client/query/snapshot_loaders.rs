use super::*;

pub(crate) fn snapshot_query(filter: Option<&str>) -> Result<String> {
    let args = filter
        .map(|filter| format!("({filter})"))
        .unwrap_or_default();
    let trigger_fields = gents::config_client::config_projection(gents::Collection::Trigger, None)?
        .0
        .join(" ");
    let backend_fields =
        gents::config_client::config_projection(gents::Collection::InferenceBackend, None)?
            .0
            .join(" ");
    Ok(format!("query DesktopSnapshot {{
      nodes: {NODE_NAME}{args} {{ {NODE_FIELDS} }}
      agents: {AGENT_NAME}{args} {{ {AGENT_FIELDS} }}
      runtimes: {NODE_RUNTIME_NAME}{args} {{ {NODE_RUNTIME_FIELDS} }}
      node_readiness: {NODE_READINESS_NAME}{args} {{ {NODE_READINESS_FIELDS} }}
      requests: {AGENT_REQUEST_NAME}{args} {{ {AGENT_REQUEST_FIELDS} }}
      mailbox_items: {MAILBOX_ITEM_NAME}{args} {{ {MAILBOX_ITEM_FIELDS} }}
      goals: {GOAL_NAME}{args} {{ {GOAL_FIELDS} }}
      sessions: {AGENT_SESSION_NAME}{args} {{ {AGENT_SESSION_FIELDS} }}
      tools: {TOOLS_NAME}{args} {{ {TOOLS_FIELDS} }}
      contexts: AgentContext{args} {{ {AGENT_CONTEXT_FIELDS} }}
      compactions: CompactionConfig{args} {{ {COMPACTION_CONFIG_FIELDS} }}
      triggers: {TRIGGER_NAME}{args} {{ {trigger_fields} {TRIGGER_OBSERVATION_FIELDS} {SCHEDULE_OBSERVATION_FIELDS} }}
      tasks: {TASK_NAME}{args} {{ {TASK_FIELDS} }}
      schedules: {SCHEDULE_NAME}{args} {{ {SCHEDULE_FIELDS} }}
      skills: {SKILL_NAME}{args} {{ {SKILL_FIELDS} }}
      inference_backends: {INFERENCE_BACKEND_NAME}{args} {{ {backend_fields} {INFERENCE_BACKEND_OBSERVATION_FIELDS} }}
      inference_profiles: {INFERENCE_PROFILE_NAME}{args} {{ {INFERENCE_PROFILE_FIELDS} }}
      inference_sampling: InferenceSampling{args} {{ {INFERENCE_SAMPLING_FIELDS} }}
      inference_execution: InferenceExecution{args} {{ {INFERENCE_EXECUTION_FIELDS} }}
      tool_service_registries: {TOOL_SERVICE_REGISTRY_NAME}{args} {{ {TOOL_SERVICE_REGISTRY_FIELDS} }}
      event_sources: EventSource{args} {{ {EVENT_SOURCE_FIELDS} }}
      agent_targets: {AGENT_TARGET_NAME}{args} {{ {AGENT_TARGET_FIELDS} }}
      datastore_tool_surfaces: DatastoreToolSurface{args} {{ {DATASTORE_TOOL_SURFACE_FIELDS} }}
      chain_key_bindings: ChainKeyBinding{args} {{ {CHAIN_KEY_BINDING_FIELDS} }}
    }}"))
}

pub(crate) fn snapshot_rows(data: &Value, source_node_did: Option<&str>) -> Result<ClientStore> {
    let trigger_observations: Vec<TriggerObservation> =
        parse_observation_query_rows(data, "triggers", TRIGGER_OBSERVATION_FIELDS)?;
    let schedule_observations: Vec<ScheduleObservation> =
        parse_observation_query_rows(data, "triggers", SCHEDULE_OBSERVATION_FIELDS)?;
    let backend_observations: Vec<InferenceBackendObservation> = parse_observation_query_rows(
        data,
        "inference_backends",
        INFERENCE_BACKEND_OBSERVATION_FIELDS,
    )?;
    let triggers: Vec<Trigger> = parse_projected_query_rows(
        data,
        "triggers",
        TRIGGER_FIELDS,
        &format!("{TRIGGER_OBSERVATION_FIELDS} {SCHEDULE_OBSERVATION_FIELDS}"),
    )?;
    let inference_backends: Vec<InferenceBackend> = parse_projected_query_rows(
        data,
        "inference_backends",
        INFERENCE_BACKEND_FIELDS,
        INFERENCE_BACKEND_OBSERVATION_FIELDS,
    )?;
    let scope = |len: usize| {
        source_node_did
            .map(|did| vec![Some(did.to_owned()); len])
            .unwrap_or_default()
    };
    let trigger_source_node_dids = scope(triggers.len());
    let trigger_observation_source_node_dids = scope(trigger_observations.len());
    let schedule_observation_source_node_dids = scope(schedule_observations.len());
    let inference_backend_source_node_dids = scope(inference_backends.len());
    let backend_observation_source_node_dids = scope(backend_observations.len());
    macro_rules! rows {
        ($ty:ty, $key:literal) => {
            parse_query_rows::<$ty>(data, $key)?
        };
    }
    let sessions: Vec<AgentSession> = rows!(AgentSession, "sessions");
    let tasks: Vec<Task> = rows!(Task, "tasks");
    let schedules: Vec<Schedule> = rows!(Schedule, "schedules");
    let skills: Vec<SkillDocument> = rows!(SkillDocument, "skills");
    let tools: Vec<Tools> = rows!(Tools, "tools");
    let contexts: Vec<AgentContext> = rows!(AgentContext, "contexts");
    let compactions: Vec<CompactionConfig> = rows!(CompactionConfig, "compactions");
    let inference_profiles: Vec<InferenceProfile> = rows!(InferenceProfile, "inference_profiles");
    let inference_sampling: Vec<InferenceSampling> = rows!(InferenceSampling, "inference_sampling");
    let inference_execution: Vec<InferenceExecution> =
        rows!(InferenceExecution, "inference_execution");
    let tool_service_registries: Vec<ToolServiceRegistry> =
        rows!(ToolServiceRegistry, "tool_service_registries");
    let event_sources: Vec<EventSource> = rows!(EventSource, "event_sources");
    let agent_targets: Vec<AgentTargetDocument> = rows!(AgentTargetDocument, "agent_targets");
    let datastore_tool_surfaces: Vec<DatastoreToolSurfaceDocument> =
        rows!(DatastoreToolSurfaceDocument, "datastore_tool_surfaces");
    let chain_key_bindings: Vec<ChainKeyBindingDocument> =
        rows!(ChainKeyBindingDocument, "chain_key_bindings");
    let source = |len| scope(len);
    let session_source_node_dids = source(sessions.len());
    let task_source_node_dids = source(tasks.len());
    let schedule_source_node_dids = source(schedules.len());
    let skill_source_node_dids = source(skills.len());
    let tools_source_node_dids = source(tools.len());
    let context_source_node_dids = source(contexts.len());
    let compaction_source_node_dids = source(compactions.len());
    let inference_profile_source_node_dids = source(inference_profiles.len());
    let inference_sampling_source_node_dids = source(inference_sampling.len());
    let inference_execution_source_node_dids = source(inference_execution.len());
    let tool_service_registry_source_node_dids = source(tool_service_registries.len());
    let event_source_source_node_dids = source(event_sources.len());
    let agent_target_source_node_dids = source(agent_targets.len());
    let datastore_tool_surface_source_node_dids = source(datastore_tool_surfaces.len());
    let chain_key_binding_source_node_dids = source(chain_key_bindings.len());
    Ok(ClientStore::from_rows(ClientStoreRows {
        nodes: rows!(Node, "nodes"),
        agents: rows!(Agent, "agents"),
        runtimes: rows!(NodeRuntimeRow, "runtimes"),
        node_readiness: rows!(NodeReadinessRow, "node_readiness"),
        requests: rows!(AgentRequestRow, "requests"),
        mailbox_items: rows!(MailboxItemRow, "mailbox_items"),
        goals: rows!(GoalRow, "goals"),
        sessions,
        session_source_node_dids,
        tasks,
        task_source_node_dids,
        schedules,
        schedule_source_node_dids,
        schedule_observations,
        schedule_observation_source_node_dids,
        triggers,
        trigger_source_node_dids,
        trigger_observations,
        trigger_observation_source_node_dids,
        skills,
        skill_source_node_dids,
        tools,
        tools_source_node_dids,
        contexts,
        context_source_node_dids,
        compactions,
        compaction_source_node_dids,
        inference_backends,
        inference_backend_source_node_dids,
        backend_observations,
        backend_observation_source_node_dids,
        inference_profiles,
        inference_profile_source_node_dids,
        inference_sampling,
        inference_sampling_source_node_dids,
        inference_execution,
        inference_execution_source_node_dids,
        tool_service_registries,
        tool_service_registry_source_node_dids,
        event_sources,
        event_source_source_node_dids,
        agent_targets,
        agent_target_source_node_dids,
        datastore_tool_surfaces,
        datastore_tool_surface_source_node_dids,
        chain_key_bindings,
        chain_key_binding_source_node_dids,
        ..ClientStoreRows::default()
    }))
}

pub(crate) fn parse_projected_query_rows<T>(
    data: &Value,
    root: &str,
    fields: &str,
    sibling_fields: &str,
) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    parse_projected_query_rows_with_source(data, root, fields, sibling_fields, None)
        .map(|(rows, _)| rows)
}

pub(crate) fn parse_observation_query_rows<T>(
    data: &Value,
    root: &str,
    fields: &str,
) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    parse_rows_with_source(data, root, fields, "", None, true).map(|(rows, _)| rows)
}

pub(crate) fn parse_projected_query_rows_with_source<T>(
    data: &Value,
    root: &str,
    fields: &str,
    sibling_fields: &str,
    source_field: Option<&str>,
) -> Result<(Vec<T>, Vec<Option<String>>)>
where
    T: DeserializeOwned,
{
    parse_rows_with_source(data, root, fields, sibling_fields, source_field, false)
}

pub(crate) fn parse_observation_query_rows_with_source<T>(
    data: &Value,
    root: &str,
    fields: &str,
    source_field: Option<&str>,
) -> Result<(Vec<T>, Vec<Option<String>>)>
where
    T: DeserializeOwned,
{
    parse_rows_with_source(data, root, fields, "", source_field, true)
}

fn parse_rows_with_source<T>(
    data: &Value,
    root: &str,
    fields: &str,
    sibling_fields: &str,
    source_field: Option<&str>,
    exact_projection: bool,
) -> Result<(Vec<T>, Vec<Option<String>>)>
where
    T: DeserializeOwned,
{
    let (parsed, sources, _) = parse_rows_with_source_completeness(
        data,
        root,
        fields,
        sibling_fields,
        source_field,
        exact_projection,
    )?;
    Ok((parsed, sources))
}

fn parse_query_rows_with_completeness<T>(data: &Value, root: &str) -> Result<(Vec<T>, bool)>
where
    T: DeserializeOwned,
{
    let (rows, _, complete) = parse_rows_with_source_completeness(data, root, "", "", None, false)?;
    Ok((rows, complete))
}

fn parse_projected_query_rows_with_completeness<T>(
    data: &Value,
    root: &str,
    fields: &str,
    sibling_fields: &str,
) -> Result<(Vec<T>, bool)>
where
    T: DeserializeOwned,
{
    let (rows, _, complete) =
        parse_rows_with_source_completeness(data, root, fields, sibling_fields, None, false)?;
    Ok((rows, complete))
}

fn parse_rows_with_source_completeness<T>(
    data: &Value,
    root: &str,
    fields: &str,
    sibling_fields: &str,
    source_field: Option<&str>,
    exact_projection: bool,
) -> Result<(Vec<T>, Vec<Option<String>>, bool)>
where
    T: DeserializeOwned,
{
    let rows = data
        .get(root)
        .ok_or_else(|| anyhow!("query result missing root field {root}"))?;
    match rows {
        Value::Null => Ok((Vec::new(), Vec::new(), true)),
        Value::Array(rows) => {
            let fields: Vec<&str> = fields.split_whitespace().collect();
            let sibling_fields: Vec<&str> = sibling_fields.split_whitespace().collect();
            let mut parsed = Vec::with_capacity(rows.len());
            let mut sources = Vec::with_capacity(rows.len());
            let mut complete = true;
            for row in rows {
                let source = source_field
                    .and_then(|field| row.get(field))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                let Some(object) = row.as_object() else {
                    complete = false;
                    tracing::warn!(target: "gents_desktop_core::query", root, "skipping malformed query row");
                    continue;
                };
                let projected = Value::Object(
                    object
                        .iter()
                        .filter(|(key, _)| {
                            if exact_projection {
                                fields.contains(&key.as_str())
                            } else {
                                !sibling_fields.contains(&key.as_str())
                                    || fields.contains(&key.as_str())
                            }
                        })
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect(),
                );
                match serde_json::from_value(projected) {
                    Ok(row) => {
                        parsed.push(row);
                        sources.push(source);
                    }
                    Err(error) => {
                        complete = false;
                        tracing::warn!(target: "gents_desktop_core::query", root, error = %error, "skipping malformed query row")
                    }
                }
            }
            Ok((parsed, sources, complete))
        }
        other => Err(anyhow!(
            "query result for {root} returned non-array payload: {other}"
        )),
    }
}

pub async fn load_full_snapshot(node: &EmbeddedNode) -> Result<ClientStore> {
    let query = snapshot_query(None)?;
    let data = execute_local_graphql_query(node, &query, "desktop snapshot").await?;
    snapshot_rows(&data, None)
}

pub async fn load_full_snapshot_with_peer_records(
    node: &EmbeddedNode,
    peers: &[PeerRecord],
    _requester_did: &str,
) -> Result<ClientStore> {
    let mut store = load_full_snapshot(node).await?;
    for peer in peers {
        let Some(graphql) = crate::local_runtime::operator_endpoint(peer) else {
            continue;
        };
        match load_operator_config(
            &gents::config_client::ConfigAccess::Graphql(graphql.clone()),
            &peer.node_did,
        )
        .await
        {
            Ok(remote) => {
                store = store.overlay_node_operator_config(&peer.node_did, &remote);
            }
            Err(error) => {
                tracing::warn!(
                    target: "gents_desktop_core::query",
                    node_did = %peer.node_did,
                    graphql = %graphql,
                    error = %error,
                    "operator GraphQL config overlay failed; keeping the desktop replica"
                );
            }
        }
    }
    Ok(store)
}

async fn load_operator_config(
    access: &gents::config_client::ConfigAccess,
    node_did: &str,
) -> Result<ClientStore> {
    let did = escape_graphql_string(node_did);
    let did_filter = format!("filter: {{ node_did: {{ _eq: \"{did}\" }} }}");
    let (trigger_fields, _) =
        gents::config_client::config_projection(gents::Collection::Trigger, None)?;
    let (backend_fields, _) =
        gents::config_client::config_projection(gents::Collection::InferenceBackend, None)?;
    let trigger_fields = trigger_fields.join(" ");
    let backend_fields = backend_fields.join(" ");
    let query = format!(
        "query DesktopOperatorConfig {{
            nodes: {NODE_NAME}({did_filter}) {{ {NODE_FIELDS} }}
            agents: {AGENT_NAME}({did_filter}) {{ {AGENT_FIELDS} }}
            runtimes: {NODE_RUNTIME_NAME}({did_filter}) {{ {NODE_RUNTIME_FIELDS} }}
            node_readiness: {NODE_READINESS_NAME}({did_filter}) {{ {NODE_READINESS_FIELDS} }}
            contexts: AgentContext({did_filter}) {{ {AGENT_CONTEXT_FIELDS} }}
            tools: {TOOLS_NAME}({did_filter}) {{ {TOOLS_FIELDS} }}
            inference_backends: InferenceBackend({did_filter}) {{ {backend_fields} {INFERENCE_BACKEND_OBSERVATION_FIELDS} }}
            inference_profiles: InferenceProfile({did_filter}) {{ {INFERENCE_PROFILE_FIELDS} }}
            inference_sampling: InferenceSampling({did_filter}) {{ {INFERENCE_SAMPLING_FIELDS} }}
            inference_execution: InferenceExecution({did_filter}) {{ {INFERENCE_EXECUTION_FIELDS} }}
            tasks: {TASK_NAME}({did_filter}) {{ {TASK_FIELDS} }}
            schedules: {SCHEDULE_NAME}({did_filter}) {{ {SCHEDULE_FIELDS} }}
            triggers: {TRIGGER_NAME}({did_filter}) {{ {trigger_fields} {TRIGGER_OBSERVATION_FIELDS} {SCHEDULE_OBSERVATION_FIELDS} }}
            skills: {SKILL_NAME}({did_filter}) {{ {SKILL_FIELDS} }}
            compactions: CompactionConfig({did_filter}) {{ {COMPACTION_CONFIG_FIELDS} }}
            tool_service_registries: {TOOL_SERVICE_REGISTRY_NAME}({did_filter}) {{ {TOOL_SERVICE_REGISTRY_FIELDS} }}
            event_sources: EventSource({did_filter}) {{ {EVENT_SOURCE_FIELDS} }}
            agent_targets: {AGENT_TARGET_NAME}({did_filter}) {{ {AGENT_TARGET_FIELDS} }}
            datastore_tool_surfaces: DatastoreToolSurface({did_filter}) {{ {DATASTORE_TOOL_SURFACE_FIELDS} }}
            chain_key_bindings: ChainKeyBinding({did_filter}) {{ {CHAIN_KEY_BINDING_FIELDS} }}
            sessions: {AGENT_SESSION_NAME}({did_filter}) {{ {AGENT_SESSION_FIELDS} }}
            requests: {AGENT_REQUEST_NAME}({did_filter}) {{ {AGENT_REQUEST_FIELDS} }}
        }}"
    );
    let data = execute_access_graphql_query(access, &query, "operator config").await?;
    operator_config_store_from_data(&data, node_did)
}

pub(crate) fn operator_config_store_from_data(data: &Value, node_did: &str) -> Result<ClientStore> {
    let (nodes, nodes_complete) = parse_query_rows_with_completeness(data, "nodes")?;
    let (agents, agents_complete) = parse_query_rows_with_completeness(data, "agents")?;
    let (inference_profiles, profiles_complete) =
        parse_query_rows_with_completeness(data, "inference_profiles")?;
    let (inference_backends, backends_complete) = parse_projected_query_rows_with_completeness(
        data,
        "inference_backends",
        INFERENCE_BACKEND_FIELDS,
        INFERENCE_BACKEND_OBSERVATION_FIELDS,
    )?;
    let mut rows = ClientStoreRows {
        nodes,
        agents,
        runtimes: parse_query_rows(data, "runtimes")?,
        node_readiness: parse_query_rows(data, "node_readiness")?,
        contexts: parse_query_rows(data, "contexts")?,
        tools: parse_query_rows(data, "tools")?,
        inference_backends,
        backend_observations: parse_observation_query_rows(
            data,
            "inference_backends",
            INFERENCE_BACKEND_OBSERVATION_FIELDS,
        )?,
        inference_profiles,
        inference_sampling: parse_query_rows(data, "inference_sampling")?,
        inference_execution: parse_query_rows(data, "inference_execution")?,
        tasks: parse_query_rows(data, "tasks")?,
        schedules: parse_query_rows(data, "schedules")?,
        schedule_observations: parse_observation_query_rows(
            data,
            "triggers",
            SCHEDULE_OBSERVATION_FIELDS,
        )?,
        triggers: parse_projected_query_rows(
            data,
            "triggers",
            TRIGGER_FIELDS,
            &format!("{TRIGGER_OBSERVATION_FIELDS} {SCHEDULE_OBSERVATION_FIELDS}"),
        )?,
        trigger_observations: parse_observation_query_rows(
            data,
            "triggers",
            TRIGGER_OBSERVATION_FIELDS,
        )?,
        skills: parse_query_rows(data, "skills")?,
        compactions: parse_query_rows(data, "compactions")?,
        tool_service_registries: parse_query_rows(data, "tool_service_registries")?,
        event_sources: parse_query_rows(data, "event_sources")?,
        agent_targets: parse_query_rows(data, "agent_targets")?,
        datastore_tool_surfaces: parse_query_rows(data, "datastore_tool_surfaces")?,
        chain_key_bindings: parse_query_rows(data, "chain_key_bindings")?,
        sessions: parse_query_rows(data, "sessions")?,
        requests: parse_query_rows(data, "requests")?,
        ..ClientStoreRows::default()
    };
    if nodes_complete && agents_complete && profiles_complete && backends_complete {
        rows.operator_config_authoritative_node_dids
            .insert(node_did.to_owned());
    }
    Ok(ClientStore::from_rows(rows))
}

pub async fn load_node_scoped_snapshot_with_peer_records(
    node: &EmbeddedNode,
    node_did: &str,
    peers: &[PeerRecord],
    _requester_did: &str,
) -> Result<ClientStore> {
    let mut store = load_node_scoped_snapshot(node, node_did).await?;
    if let Some(graphql) = peers
        .iter()
        .find(|peer| peer.node_did == node_did)
        .and_then(crate::local_runtime::operator_endpoint)
    {
        match load_operator_config(
            &gents::config_client::ConfigAccess::Graphql(graphql.clone()),
            node_did,
        )
        .await
        {
            Ok(remote) => {
                store = store.overlay_node_operator_config(node_did, &remote);
            }
            Err(error) => {
                tracing::warn!(
                    target: "gents_desktop_core::query",
                    node_did,
                    graphql = %graphql,
                    error = %error,
                    "operator GraphQL config overlay failed; keeping the desktop replica"
                );
            }
        }
    }
    Ok(store)
}

#[cfg(test)]
pub async fn load_node_runtimes(node: &EmbeddedNode) -> Result<Vec<NodeRuntimeRow>> {
    load_rows(
        node,
        NODE_RUNTIME_NAME,
        &format!("query {{ {NODE_RUNTIME_NAME} {{ {NODE_RUNTIME_FIELDS} }} }}"),
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
