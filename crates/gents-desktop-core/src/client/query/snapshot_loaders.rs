use super::*;
use rapidhash::{HashSetExt, RapidHashSet};

fn node_filter(node_did: Option<&str>) -> String {
    node_did
        .map(|did| format!("node_did: {{ _eq: \"{}\" }}", escape_graphql_string(did)))
        .unwrap_or_default()
}

fn snapshot_request_args(node_did: Option<&str>) -> String {
    let node = node_filter(node_did);
    let live = gents_protocol::request_lifecycle::RequestLifecycleState::nonterminal_graphql_list();
    format!("(filter: {{ {node} lifecycle_state: {{ _in: {live} }} }})")
}

pub(crate) fn snapshot_query(node_did: Option<&str>) -> Result<String> {
    let args = node_did
        .map(|_| format!("(filter: {{ {} }})", node_filter(node_did)))
        .unwrap_or_default();
    let request_args = snapshot_request_args(node_did);
    let mailbox_node = node_filter(node_did);
    let mailbox_args = format!("(filter: {{ {mailbox_node} status: {{ _eq: \"open\" }} }})");
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
      requests: {AGENT_REQUEST_NAME}{request_args} {{ {AGENT_REQUEST_FIELDS} }}
      mailbox_items: {MAILBOX_ITEM_NAME}{mailbox_args} {{ {MAILBOX_ITEM_FIELDS} }}
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

#[cfg(test)]
pub(crate) fn snapshot_rows(data: &Value, source_node_did: Option<&str>) -> Result<ClientStore> {
    snapshot_rows_with_requests(data, source_node_did, snapshot_request_rows(data)?)
}

pub(super) fn snapshot_rows_with_requests(
    data: &Value,
    source_node_did: Option<&str>,
    requests: Vec<AgentRequestRow>,
) -> Result<ClientStore> {
    let (trigger_observations, trigger_observation_source_node_dids): (
        Vec<TriggerObservation>,
        Vec<Option<String>>,
    ) = parse_observation_query_rows_with_source(
        data,
        "triggers",
        TRIGGER_OBSERVATION_FIELDS,
        Some("node_did"),
    )?;
    let (schedule_observations, schedule_observation_source_node_dids): (
        Vec<ScheduleObservation>,
        Vec<Option<String>>,
    ) = parse_observation_query_rows_with_source(
        data,
        "triggers",
        SCHEDULE_OBSERVATION_FIELDS,
        Some("node_did"),
    )?;
    let (backend_observations, backend_observation_source_node_dids): (
        Vec<InferenceBackendObservation>,
        Vec<Option<String>>,
    ) = parse_observation_query_rows_with_source(
        data,
        "inference_backends",
        INFERENCE_BACKEND_OBSERVATION_FIELDS,
        Some("node_did"),
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
    let inference_backend_source_node_dids = scope(inference_backends.len());
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
        requests,
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

const TASK_REQUEST_FIELDS: &str = AGENT_REQUEST_FIELDS;

/// A session head request's doc id, node DID, session id, requester DID and
/// request id; the field order is the sort order.
pub(super) type RequestHeadScope = (String, String, String, Option<String>, String);

fn request_head_scopes(data: &Value) -> Result<Vec<RequestHeadScope>> {
    let sessions: Vec<AgentSession> = parse_query_rows(data, "sessions")?;
    let mut scopes = Vec::new();
    for session in sessions {
        let Some(head) = session
            .observation
            .as_ref()
            .and_then(|observation| observation.latest_request.as_ref())
        else {
            continue;
        };
        scopes.push((
            head.request_doc_id.clone(),
            session.node_did,
            session.session_id,
            session.requester_did,
            head.request_id.clone(),
        ));
    }
    scopes.sort();
    scopes.dedup();
    Ok(scopes)
}

pub(super) fn session_head_queries(scopes: &[RequestHeadScope]) -> Vec<String> {
    let mut docs = scopes
        .iter()
        .map(|(doc_id, _, _, _, _)| doc_id.as_str())
        .collect::<Vec<_>>();
    docs.sort_unstable();
    docs.dedup();
    docs.chunks(128)
        .map(|chunk| {
            let docs = chunk
                .iter()
                .map(|doc| format!("\"{}\"", escape_graphql_string(doc)))
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "query DesktopSessionHeads {{ requests: AgentRequest(docIDs: [{docs}]) {{ {AGENT_REQUEST_FIELDS} }} }}"
            )
        })
        .collect()
}

fn missing_session_head_scopes(
    scopes: &[RequestHeadScope],
    seeds: &[AgentRequestRow],
) -> Vec<RequestHeadScope> {
    let seeded_docs = seeds
        .iter()
        .filter_map(|row| row.doc_id.as_deref())
        .collect::<RapidHashSet<_>>();
    scopes
        .iter()
        .filter(|(doc_id, _, _, _, _)| !seeded_docs.contains(doc_id.as_str()))
        .cloned()
        .collect()
}

pub(super) fn task_request_queries(data: &Value, node_did: Option<&str>) -> Vec<String> {
    let Some(triggers) = data.get("triggers").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut scopes = triggers
        .iter()
        .filter_map(|row| {
            let node = row.get("node_did")?.as_str()?;
            if node_did.is_some_and(|expected| expected != node) {
                return None;
            }
            let trigger = row.get("trigger_id")?.as_str()?;
            (!node.trim().is_empty() && !trigger.trim().is_empty())
                .then(|| (node.to_owned(), trigger.to_owned()))
        })
        .collect::<Vec<_>>();
    scopes.sort();
    scopes.dedup();
    scopes
        .chunks(32)
        .map(|chunk| {
            let fields = chunk
                .iter()
                .enumerate()
                .map(|(index, (node, trigger))| {
                    format!(
                        "trigger_{index}: AgentRequest(filter: {{ node_did: {{ _eq: \"{}\" }}, caused_by_trigger_id: {{ _eq: \"{}\" }}, purpose: {{ _eq: \"normal\" }} }}, order: [{{ node_did: ASC }}, {{ caused_by_trigger_id: ASC }}, {{ purpose: ASC }}, {{ created_at: DESC }}, {{ request_id: DESC }}, {{ session_id: ASC }}], limit: 8) {{ {TASK_REQUEST_FIELDS} }}",
                        escape_graphql_string(node),
                        escape_graphql_string(trigger),
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!("query DesktopTaskRequestHeads {{ {fields} }}")
        })
        .collect()
}

/// The pinned planner requires the equality prefix in index ordering and
/// cannot combine that ordering with the implicit physical-document suffix.
/// Refine only the cutoff tie so physical twins retain the full-store order.
fn task_request_boundary_queries(rows: &[AgentRequestRow]) -> Result<Vec<String>> {
    let mut groups = std::collections::BTreeMap::new();
    for row in rows {
        let node = row
            .node_did
            .as_deref()
            .context("task request lacks node scope")?;
        let trigger = row
            .caused_by_trigger_id
            .as_deref()
            .context("task request lacks trigger scope")?;
        groups
            .entry((node, trigger))
            .or_insert_with(Vec::new)
            .push(row);
    }
    let mut selections = Vec::new();
    for ((node, trigger), rows) in groups {
        if rows.len() < 8 {
            continue;
        }
        anyhow::ensure!(rows.len() == 8, "task candidate page exceeded its limit");
        let boundary = rows.last().expect("nonempty candidate page");
        let filter = gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
            "node_did": {"_eq": node}, "caused_by_trigger_id": {"_eq": trigger},
            "purpose": {"_eq": "normal"},
            "created_at": {"_eq": boundary.created_at},
            "request_id": {"_eq": boundary.request_id},
            "session_id": {"_eq": boundary.session_id},
        }))?;
        selections.push(format!("AgentRequest(filter: {filter}, order: {{ _docID: ASC }}, limit: 8) {{ {TASK_REQUEST_FIELDS} }}"));
    }
    Ok(selections
        .chunks(32)
        .map(|chunk| {
            let roots = chunk
                .iter()
                .enumerate()
                .map(|(index, selection)| format!("trigger_{index}: {selection}"))
                .collect::<Vec<_>>()
                .join(" ");
            format!("query DesktopTaskRequestBoundary {{ {roots} }}")
        })
        .collect())
}

fn dedupe_request_seeds(rows: &mut Vec<AgentRequestRow>) {
    let mut seen = RapidHashSet::new();
    rows.retain(|row| {
        row.doc_id
            .as_ref()
            .is_none_or(|doc| seen.insert(doc.clone()))
    });
}

pub(super) fn scoped_session_heads(
    rows: Vec<AgentRequestRow>,
    scopes: &[RequestHeadScope],
) -> Vec<AgentRequestRow> {
    let expected = scopes
        .iter()
        .map(|(doc, node, session, requester, request_id)| {
            (
                doc.as_str(),
                node.as_str(),
                session.as_str(),
                requester.as_deref(),
                request_id.as_str(),
            )
        })
        .collect::<RapidHashSet<_>>();
    rows.into_iter()
        .filter(|row| {
            row.purpose == Some(gents_protocol::request_admission::RequestPurpose::Normal)
                && row.doc_id.as_ref().is_some_and(|doc| {
                    row.node_did
                        .as_deref()
                        .zip(row.session_id.as_deref())
                        .is_some_and(|(node, session)| {
                            expected.contains(&(
                                doc.as_str(),
                                node,
                                session,
                                row.requester_did.as_deref(),
                                row.request_id.as_str(),
                            ))
                        })
                })
        })
        .collect()
}

pub(super) fn complete_request_rows(data: &Value, root: &str) -> Result<Vec<AgentRequestRow>> {
    let (rows, complete) = parse_query_rows_with_completeness::<AgentRequestRow>(data, root)?;
    anyhow::ensure!(complete, "request query {root} returned malformed rows");
    Ok(rows)
}

fn snapshot_request_rows(data: &Value) -> Result<Vec<AgentRequestRow>> {
    if data.get("requests").is_none() {
        return Ok(Vec::new());
    }
    complete_request_rows(data, "requests")
}

fn parse_task_request_rows(data: &Value) -> Result<Vec<AgentRequestRow>> {
    let Some(fields) = data.as_object() else {
        bail!("task request query returned a non-object payload");
    };
    let mut requests = Vec::new();
    for root in fields.keys().filter(|root| root.starts_with("trigger_")) {
        let rows = complete_request_rows(data, root)?;
        anyhow::ensure!(
            rows.iter().all(|row| {
                gents_protocol::request_admission::RequestPurpose::is_public(row.purpose)
                    && row.doc_id.as_deref().is_some_and(|id| !id.is_empty())
            }),
            "task request query returned a non-public or unidentified row"
        );
        requests.extend(rows);
    }
    Ok(requests)
}

pub(super) async fn complete_local_snapshot_requests(
    node: &EmbeddedNode,
    data: &Value,
    node_did: Option<&str>,
) -> Result<Vec<AgentRequestRow>> {
    let head_scopes = request_head_scopes(data)?;
    let mut seeds = snapshot_request_rows(data)?;
    let mut task_rows = Vec::new();
    for query in task_request_queries(data, node_did) {
        let response = execute_local_graphql_query(node, &query, "recent trigger requests").await?;
        task_rows.extend(parse_task_request_rows(&response)?);
    }
    for query in task_request_boundary_queries(&task_rows)? {
        let response =
            execute_local_graphql_query(node, &query, "task request cutoff ties").await?;
        seeds.extend(parse_task_request_rows(&response)?);
    }
    seeds.extend(task_rows);
    dedupe_request_seeds(&mut seeds);
    let missing_heads = missing_session_head_scopes(&head_scopes, &seeds);
    for query in session_head_queries(&missing_heads) {
        let response = execute_local_graphql_query(node, &query, "session request heads").await?;
        seeds.extend(scoped_session_heads(
            complete_request_rows(&response, "requests")?,
            &head_scopes,
        ));
    }
    dedupe_request_seeds(&mut seeds);
    let requests = if seeds.is_empty() {
        seeds
    } else {
        super::close_request_rows(node, seeds).await?
    };
    Ok(requests)
}

async fn complete_access_snapshot_requests(
    access: &gents::config_client::ConfigAccess,
    data: &Value,
    node_did: Option<&str>,
) -> Result<Vec<AgentRequestRow>> {
    let head_scopes = request_head_scopes(data)?;
    let mut seeds = snapshot_request_rows(data)?;
    let mut task_rows = Vec::new();
    for query in task_request_queries(data, node_did) {
        let response =
            execute_access_graphql_query(access, &query, "recent trigger requests").await?;
        task_rows.extend(parse_task_request_rows(&response)?);
    }
    for query in task_request_boundary_queries(&task_rows)? {
        let response =
            execute_access_graphql_query(access, &query, "task request cutoff ties").await?;
        seeds.extend(parse_task_request_rows(&response)?);
    }
    seeds.extend(task_rows);
    dedupe_request_seeds(&mut seeds);
    let missing_heads = missing_session_head_scopes(&head_scopes, &seeds);
    for query in session_head_queries(&missing_heads) {
        let response =
            execute_access_graphql_query(access, &query, "session request heads").await?;
        seeds.extend(scoped_session_heads(
            complete_request_rows(&response, "requests")?,
            &head_scopes,
        ));
    }
    dedupe_request_seeds(&mut seeds);
    let requests = if seeds.is_empty() {
        seeds
    } else {
        super::close_request_rows_on(access, seeds).await?
    };
    Ok(requests)
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

pub(super) fn parse_query_rows_with_completeness<T>(
    data: &Value,
    root: &str,
) -> Result<(Vec<T>, bool)>
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
    let requests = complete_local_snapshot_requests(node, &data, None).await?;
    snapshot_rows_with_requests(&data, None, requests)
}

pub async fn load_full_snapshot_with_peer_records(
    node: &EmbeddedNode,
    peers: &[PeerRecord],
    _requester_did: &str,
    identity_interaction: crate::local_runtime::OperatorIdentityInteraction,
) -> Result<ClientStore> {
    let mut store = load_full_snapshot(node).await?;
    for peer in peers {
        let Some(graphql) = identity_interaction.endpoint(peer) else {
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

#[derive(Clone, Copy)]
enum OperatorConfigScope {
    Full,
    Inference,
}

async fn load_operator_config(
    access: &gents::config_client::ConfigAccess,
    node_did: &str,
) -> Result<ClientStore> {
    load_operator_config_scoped(access, node_did, OperatorConfigScope::Full).await
}

pub async fn load_inference_configuration(
    access: &gents::config_client::ConfigAccess,
    node_did: &str,
) -> Result<ClientStore> {
    load_operator_config_scoped(access, node_did, OperatorConfigScope::Inference).await
}

async fn load_operator_config_scoped(
    access: &gents::config_client::ConfigAccess,
    node_did: &str,
    scope: OperatorConfigScope,
) -> Result<ClientStore> {
    let did = escape_graphql_string(node_did);
    let did_filter = format!("filter: {{ node_did: {{ _eq: \"{did}\" }} }}");
    let request_args = snapshot_request_args(Some(node_did));
    let (trigger_fields, _) =
        gents::config_client::config_projection(gents::Collection::Trigger, None)?;
    let (backend_fields, _) =
        gents::config_client::config_projection(gents::Collection::InferenceBackend, None)?;
    let trigger_fields = trigger_fields.join(" ");
    let backend_fields = backend_fields.join(" ");
    let common = format!(
        "nodes: {NODE_NAME}({did_filter}) {{ {NODE_FIELDS} }}
            agents: {AGENT_NAME}({did_filter}) {{ {AGENT_FIELDS} }}
            inference_backends: InferenceBackend({did_filter}) {{ {backend_fields} {INFERENCE_BACKEND_OBSERVATION_FIELDS} }}
            inference_profiles: InferenceProfile({did_filter}) {{ {INFERENCE_PROFILE_FIELDS} }}"
    );
    let remaining = match scope {
        OperatorConfigScope::Inference => String::new(),
        OperatorConfigScope::Full => format!(
            "runtimes: {NODE_RUNTIME_NAME}({did_filter}) {{ {NODE_RUNTIME_FIELDS} }}
            node_readiness: {NODE_READINESS_NAME}({did_filter}) {{ {NODE_READINESS_FIELDS} }}
            contexts: AgentContext({did_filter}) {{ {AGENT_CONTEXT_FIELDS} }}
            tools: {TOOLS_NAME}({did_filter}) {{ {TOOLS_FIELDS} }}
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
            requests: {AGENT_REQUEST_NAME}{request_args} {{ {AGENT_REQUEST_FIELDS} }}"
        ),
    };
    let query = format!("query DesktopOperatorConfig {{ {common} {remaining} }}");
    let data = execute_access_graphql_query(access, &query, "operator config").await?;
    let requests = match scope {
        OperatorConfigScope::Full => {
            complete_access_snapshot_requests(access, &data, Some(node_did)).await?
        }
        OperatorConfigScope::Inference => Vec::new(),
    };
    operator_config_store_from_data_with_requests(&data, node_did, requests, scope)
}

#[cfg(test)]
pub(crate) fn operator_config_store_from_data(data: &Value, node_did: &str) -> Result<ClientStore> {
    operator_config_store_from_data_with_requests(
        data,
        node_did,
        snapshot_request_rows(data)?,
        OperatorConfigScope::Full,
    )
}

fn operator_config_store_from_data_with_requests(
    data: &Value,
    node_did: &str,
    requests: Vec<AgentRequestRow>,
    scope: OperatorConfigScope,
) -> Result<ClientStore> {
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
    let (backend_observations, backend_observation_source_node_dids) =
        parse_observation_query_rows_with_source(
            data,
            "inference_backends",
            INFERENCE_BACKEND_OBSERVATION_FIELDS,
            Some("node_did"),
        )?;
    let mut common = ClientStoreRows {
        nodes,
        agents,
        inference_profiles,
        inference_backends,
        backend_observations,
        backend_observation_source_node_dids,
        ..Default::default()
    };
    if nodes_complete && agents_complete && profiles_complete && backends_complete {
        common
            .operator_config_authoritative_node_dids
            .insert(node_did.to_owned());
    }
    if matches!(scope, OperatorConfigScope::Inference) {
        return Ok(ClientStore::from_rows(common));
    }
    let (schedule_observations, schedule_observation_source_node_dids) =
        parse_observation_query_rows_with_source(
            data,
            "triggers",
            SCHEDULE_OBSERVATION_FIELDS,
            Some("node_did"),
        )?;
    let (trigger_observations, trigger_observation_source_node_dids) =
        parse_observation_query_rows_with_source(
            data,
            "triggers",
            TRIGGER_OBSERVATION_FIELDS,
            Some("node_did"),
        )?;
    let rows = ClientStoreRows {
        runtimes: parse_query_rows(data, "runtimes")?,
        node_readiness: parse_query_rows(data, "node_readiness")?,
        contexts: parse_query_rows(data, "contexts")?,
        tools: parse_query_rows(data, "tools")?,
        inference_sampling: parse_query_rows(data, "inference_sampling")?,
        inference_execution: parse_query_rows(data, "inference_execution")?,
        tasks: parse_query_rows(data, "tasks")?,
        schedules: parse_query_rows(data, "schedules")?,
        schedule_observations,
        schedule_observation_source_node_dids,
        triggers: parse_projected_query_rows(
            data,
            "triggers",
            TRIGGER_FIELDS,
            &format!("{TRIGGER_OBSERVATION_FIELDS} {SCHEDULE_OBSERVATION_FIELDS}"),
        )?,
        trigger_observations,
        trigger_observation_source_node_dids,
        skills: parse_query_rows(data, "skills")?,
        compactions: parse_query_rows(data, "compactions")?,
        tool_service_registries: parse_query_rows(data, "tool_service_registries")?,
        event_sources: parse_query_rows(data, "event_sources")?,
        agent_targets: parse_query_rows(data, "agent_targets")?,
        datastore_tool_surfaces: parse_query_rows(data, "datastore_tool_surfaces")?,
        chain_key_bindings: parse_query_rows(data, "chain_key_bindings")?,
        sessions: parse_query_rows(data, "sessions")?,
        requests,
        ..common
    };
    Ok(ClientStore::from_rows(rows))
}

pub async fn load_node_scoped_snapshot_with_peer_records(
    node: &EmbeddedNode,
    node_did: &str,
    peers: &[PeerRecord],
    _requester_did: &str,
    identity_interaction: crate::local_runtime::OperatorIdentityInteraction,
) -> Result<ClientStore> {
    let mut store = load_node_scoped_snapshot(node, node_did).await?;
    if let Some(graphql) = peers
        .iter()
        .find(|peer| peer.node_did == node_did)
        .and_then(|record| identity_interaction.endpoint(record))
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
