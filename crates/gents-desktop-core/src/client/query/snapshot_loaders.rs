#[cfg(test)]
use super::document_patches::load_rows;
use super::*;
use rapidhash::{HashSetExt, RapidHashSet};

fn node_filter(node_did: &str) -> String {
    format!(
        "node_did: {{ _eq: \"{}\" }}",
        escape_graphql_string(node_did)
    )
}

fn snapshot_request_args(node_did: &str) -> String {
    let node = node_filter(node_did);
    let live = gents_protocol::request_lifecycle::RequestLifecycleState::nonterminal_graphql_list();
    format!("(filter: {{ {node} lifecycle_state: {{ _in: {live} }} }})")
}

pub(crate) fn snapshot_query(node_did: &str) -> Result<String> {
    let args = format!("(filter: {{ {} }})", node_filter(node_did));
    let request_args = snapshot_request_args(node_did);
    let mailbox_node = node_filter(node_did);
    let mailbox_args = format!("(filter: {{ {mailbox_node} status: {{ _eq: \"open\" }} }})");
    let trigger_fields =
        gents_documents::config_projection(gents_store::collection::Collection::Trigger, None)?
            .0
            .join(" ");
    let backend_fields = INFERENCE_BACKEND_FIELDS;
    Ok(format!("query DesktopSnapshot {{
      nodes: {NODE_NAME}{args} {{ {NODE_FIELDS} }}
      default_agent_selections: {DEFAULT_AGENT_SELECTION_NAME}{args} {{ {DEFAULT_AGENT_SELECTION_FIELDS} }}
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
      inference_backends: {INFERENCE_BACKEND_NAME}{args} {{ {backend_fields} }}
      backend_observations: {INFERENCE_BACKEND_OBSERVATION_NAME}{args} {{ node_did {INFERENCE_BACKEND_OBSERVATION_FIELDS} }}
      inference_profiles: {INFERENCE_PROFILE_NAME}{args} {{ {INFERENCE_PROFILE_FIELDS} }}
      inference_sampling: InferenceSampling{args} {{ {INFERENCE_SAMPLING_FIELDS} }}
      inference_execution: InferenceExecution{args} {{ {INFERENCE_EXECUTION_FIELDS} }}
      tool_service_registries: {TOOL_SERVICE_REGISTRY_NAME}{args} {{ {TOOL_SERVICE_REGISTRY_FIELDS} }}
      event_sources: EventSource{args} {{ {EVENT_SOURCE_FIELDS} }}
      agent_targets: {AGENT_TARGET_NAME}{args} {{ {AGENT_TARGET_FIELDS} }}
      datastore_tool_surfaces: DatastoreToolSurface{args} {{ {DATASTORE_TOOL_SURFACE_FIELDS} }}
    }}"))
}

pub(super) fn snapshot_rows_with_requests(
    data: &Value,
    requests: Vec<AgentRequestRow>,
) -> Result<ClientStore> {
    let trigger_observations: Vec<TriggerObservation> =
        parse_observation_query_rows(data, "triggers", TRIGGER_OBSERVATION_FIELDS)?;
    let schedule_observations: Vec<ScheduleObservation> =
        parse_observation_query_rows(data, "triggers", SCHEDULE_OBSERVATION_FIELDS)?;
    let backend_observations = parse_backend_observation_rows(data, "backend_observations")?;
    let triggers: Vec<Trigger> = parse_projected_query_rows(
        data,
        "triggers",
        TRIGGER_FIELDS,
        &format!("{TRIGGER_OBSERVATION_FIELDS} {SCHEDULE_OBSERVATION_FIELDS}"),
    )?;
    let metadata: Vec<gents_documents::InferenceBackendMetadata> =
        parse_projected_query_rows(data, "inference_backends", INFERENCE_BACKEND_FIELDS, "")?;
    let inference_backends: Vec<InferenceBackend> = metadata
        .into_iter()
        .map(|metadata| metadata.into_backend().map_err(anyhow::Error::from))
        .collect::<Result<_>>()?;
    macro_rules! rows {
        ($ty:ty, $key:literal) => {
            parse_query_rows::<$ty>(data, $key)?
        };
    }
    Ok(ClientStore::from_rows(ClientStoreRows {
        nodes: rows!(Node, "nodes"),
        default_agent_selections: rows!(DefaultAgentSelection, "default_agent_selections"),
        agents: rows!(Agent, "agents"),
        runtimes: rows!(NodeRuntimeRow, "runtimes"),
        node_readiness: rows!(NodeReadinessRow, "node_readiness"),
        requests,
        mailbox_items: rows!(MailboxItemRow, "mailbox_items"),
        goals: rows!(GoalRow, "goals"),
        sessions: rows!(AgentSession, "sessions"),
        tasks: rows!(Task, "tasks"),
        schedules: rows!(Schedule, "schedules"),
        schedule_observations,
        triggers,
        trigger_observations,
        skills: rows!(SkillDocument, "skills"),
        tools: rows!(Tools, "tools"),
        contexts: rows!(AgentContext, "contexts"),
        compactions: rows!(CompactionConfig, "compactions"),
        inference_backends,
        backend_observations,
        inference_profiles: rows!(InferenceProfile, "inference_profiles"),
        inference_sampling: rows!(InferenceSampling, "inference_sampling"),
        inference_execution: rows!(InferenceExecution, "inference_execution"),
        tool_service_registries: rows!(ToolServiceRegistry, "tool_service_registries"),
        event_sources: rows!(EventSource, "event_sources"),
        agent_targets: rows!(AgentTargetDocument, "agent_targets"),
        datastore_tool_surfaces: rows!(DatastoreToolSurfaceDocument, "datastore_tool_surfaces"),
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

pub(super) fn task_request_queries(data: &Value, node_did: &str) -> Vec<String> {
    let Some(triggers) = data.get("triggers").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut scopes = triggers
        .iter()
        .filter_map(|row| {
            let node = row.get("node_did")?.as_str()?;
            if node != node_did {
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
    node_did: &str,
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
    access: &gents_store::config_client::ConfigAccess,
    data: &Value,
    node_did: &str,
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
    parse_rows_with_completeness(data, root, fields, sibling_fields, false).map(|(rows, _)| rows)
}

pub(super) fn parse_backend_observation_rows(
    data: &Value,
    root: &str,
) -> Result<Vec<InferenceBackendObservation>> {
    let rows: Vec<Value> =
        parse_observation_query_rows(data, root, INFERENCE_BACKEND_OBSERVATION_FIELDS)?;
    let mut observations = Vec::with_capacity(rows.len());
    for row in rows {
        match gents_inference::backends::registry::decode_backend_observation(row) {
            Ok(observation) => observations.push(observation),
            Err(error) => {
                tracing::warn!(target: "gents_desktop_core::query", root, error = %error,
                    "backend catalog observation unavailable");
            }
        }
    }
    Ok(observations)
}

pub(crate) fn parse_observation_query_rows<T>(
    data: &Value,
    root: &str,
    fields: &str,
) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    parse_rows_with_completeness(data, root, fields, "", true).map(|(rows, _)| rows)
}

pub(super) fn parse_query_rows_with_completeness<T>(
    data: &Value,
    root: &str,
) -> Result<(Vec<T>, bool)>
where
    T: DeserializeOwned,
{
    parse_rows_with_completeness(data, root, "", "", false)
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
    parse_rows_with_completeness(data, root, fields, sibling_fields, false)
}

fn parse_rows_with_completeness<T>(
    data: &Value,
    root: &str,
    fields: &str,
    sibling_fields: &str,
    exact_projection: bool,
) -> Result<(Vec<T>, bool)>
where
    T: DeserializeOwned,
{
    let rows = data
        .get(root)
        .ok_or_else(|| anyhow!("query result missing root field {root}"))?;
    match rows {
        Value::Null => Ok((Vec::new(), true)),
        Value::Array(rows) => {
            let fields: Vec<&str> = fields.split_whitespace().collect();
            let sibling_fields: Vec<&str> = sibling_fields.split_whitespace().collect();
            let mut parsed = Vec::with_capacity(rows.len());
            let mut complete = true;
            for row in rows {
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
                    Ok(row) => parsed.push(row),
                    Err(error) => {
                        complete = false;
                        tracing::warn!(target: "gents_desktop_core::query", root, error = %error, "skipping malformed query row")
                    }
                }
            }
            Ok((parsed, complete))
        }
        other => Err(anyhow!(
            "query result for {root} returned non-array payload: {other}"
        )),
    }
}

/// The top-level aliases of [`snapshot_query`]; a served runtime omits the
/// ones the reader may not read.
pub(crate) const SNAPSHOT_ROOTS: &[&str] = &[
    "nodes",
    "default_agent_selections",
    "agents",
    "runtimes",
    "node_readiness",
    "requests",
    "mailbox_items",
    "goals",
    "sessions",
    "tools",
    "contexts",
    "compactions",
    "triggers",
    "tasks",
    "schedules",
    "skills",
    "inference_backends",
    "backend_observations",
    "inference_profiles",
    "inference_sampling",
    "inference_execution",
    "tool_service_registries",
    "event_sources",
    "agent_targets",
    "datastore_tool_surfaces",
];

/// Reads an omitted snapshot root as an empty one.
///
/// External premise: the pinned served runtime answers a reader whose grant
/// was revoked with HTTP 200, no `errors`, and `data` without the roots it
/// may not read, while rows ACP filters for an authorized reader come back
/// as an empty list. Both are native ACP hiding what the reader may not
/// read (`RuntimeConnectionObservation.SnapshotOutcome`): the client shows
/// nothing there and never infers a denial or reconnects for it.
fn omitted_roots_read_empty(mut data: Value) -> Value {
    if let Some(object) = data.as_object_mut() {
        for root in SNAPSHOT_ROOTS {
            object.entry(*root).or_insert(Value::Null);
        }
    }
    data
}

/// How a runtime snapshot read failed
/// (`RuntimeConnectionObservation.SnapshotOutcome`). The observer reconnects
/// a refused source and keeps an armed source for any other failure, so the
/// kind is decided here, by the access owner's classification, and nowhere
/// else. An omitted root is not a failure (`omitted_roots_read_empty`).
#[derive(Debug, thiserror::Error)]
pub enum SnapshotReadError {
    /// The access owner refused the read (HTTP 401/403 or a
    /// `PERMISSION_DENIED` envelope): the runtime does not accept this
    /// client's credentials.
    #[error("runtime refused the snapshot read: {0}")]
    Refused(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// Any other failure: the runtime is unreachable, timed out, or answered
    /// something the reader could not decode.
    #[error("{0}")]
    Failed(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl From<anyhow::Error> for SnapshotReadError {
    fn from(error: anyhow::Error) -> Self {
        if error.permission_denied() {
            Self::Refused(error.into())
        } else {
            Self::Failed(error.into())
        }
    }
}

impl SnapshotReadError {
    pub fn refused(&self) -> bool {
        matches!(self, Self::Refused(_))
    }
}

pub async fn load_node_snapshot_on(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
) -> std::result::Result<ClientStore, SnapshotReadError> {
    let query = snapshot_query(node_did)?;
    let data = omitted_roots_read_empty(
        execute_access_graphql_query(access, &query, "runtime desktop snapshot").await?,
    );
    let requests = complete_access_snapshot_requests(access, &data, node_did).await?;
    Ok(if access.actor_did().is_some() {
        authenticated_node_snapshot_rows(&data, node_did, requests)?
    } else {
        snapshot_rows_with_requests(&data, requests)?
    })
}

pub(super) fn authenticated_node_snapshot_rows(
    data: &Value,
    node_did: &str,
    requests: Vec<AgentRequestRow>,
) -> Result<ClientStore> {
    let mut snapshot = snapshot_rows_with_requests(data, requests)?;
    let config = operator_config_store_from_data(data, node_did)?;
    snapshot.operator_config_authoritative_node_dids =
        config.operator_config_authoritative_node_dids;
    Ok(snapshot)
}

pub async fn load_inference_configuration(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
) -> Result<ClientStore> {
    let did = escape_graphql_string(node_did);
    let did_filter = format!("filter: {{ node_did: {{ _eq: \"{did}\" }} }}");
    let backend_fields = INFERENCE_BACKEND_FIELDS;
    let query = format!(
        "query DesktopInferenceConfig {{ nodes: {NODE_NAME}({did_filter}) {{ {NODE_FIELDS} }}
            default_agent_selections: {DEFAULT_AGENT_SELECTION_NAME}({did_filter}) {{ {DEFAULT_AGENT_SELECTION_FIELDS} }}
            agents: {AGENT_NAME}({did_filter}) {{ {AGENT_FIELDS} }}
            inference_backends: {INFERENCE_BACKEND_NAME}({did_filter}) {{ {backend_fields} }}
            backend_observations: {INFERENCE_BACKEND_OBSERVATION_NAME}({did_filter}) {{ node_did {INFERENCE_BACKEND_OBSERVATION_FIELDS} }}
            inference_profiles: InferenceProfile({did_filter}) {{ {INFERENCE_PROFILE_FIELDS} }} }}"
    );
    let data = execute_access_graphql_query(access, &query, "inference config").await?;
    operator_config_store_from_data(&data, node_did)
}

fn operator_config_store_from_data(data: &Value, node_did: &str) -> Result<ClientStore> {
    let (nodes, nodes_complete) = parse_query_rows_with_completeness(data, "nodes")?;
    let default_agent_selections = parse_query_rows(data, "default_agent_selections")?;
    let (agents, agents_complete) = parse_query_rows_with_completeness(data, "agents")?;
    let (inference_profiles, profiles_complete) =
        parse_query_rows_with_completeness(data, "inference_profiles")?;
    let (metadata, backends_complete): (Vec<gents_documents::InferenceBackendMetadata>, _) =
        parse_projected_query_rows_with_completeness(
            data,
            "inference_backends",
            INFERENCE_BACKEND_FIELDS,
            "",
        )?;
    let inference_backends = metadata
        .into_iter()
        .map(|metadata| metadata.into_backend().map_err(anyhow::Error::from))
        .collect::<Result<_>>()?;
    let backend_observations = parse_backend_observation_rows(data, "backend_observations")?;
    let mut common = ClientStoreRows {
        nodes,
        default_agent_selections,
        agents,
        inference_profiles,
        inference_backends,
        backend_observations,
        ..Default::default()
    };
    if nodes_complete && agents_complete && profiles_complete && backends_complete {
        common
            .operator_config_authoritative_node_dids
            .insert(node_did.to_owned());
    }
    Ok(ClientStore::from_rows(common))
}

pub async fn load_node_scoped_snapshot_with_peer_records(
    node_did: &str,
    peers: &[PeerRecord],
    capability: &gents_store::config_client::ClientSigningCapability,
) -> Result<ClientStore> {
    let endpoint = peers
        .iter()
        .find(|peer| peer.node_did == node_did)
        .and_then(|peer| crate::local_runtime::operator_endpoint(peer, capability))
        .with_context(|| format!("runtime {node_did} has no authenticated database endpoint"))?;
    Ok(load_node_snapshot_on(
        &gents_store::config_client::ConfigAccess::Graphql(endpoint),
        node_did,
    )
    .await?)
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
