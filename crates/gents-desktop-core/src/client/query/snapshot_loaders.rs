use super::*;
use gents_documents::views::agents::AgentList;
use gents_documents::views::config::{
    AgentContextList, AgentTargetList, CompactionConfigList, DatastoreToolSurfaceList,
    DefaultAgentSelectionList, NodeList, SkillList, ToolsList,
};
use gents_documents::views::tasks::{EventSourceList, ScheduleList, TaskList, TriggerList};
use gents_inference::views::backends::{BackendList, ObservationList};
use gents_inference::views::profiles::ProfileList;
use gents_inference::views::settings::{ExecutionList, SamplingList};
use gents_session::views::requests::{RequestOrder, RequestRows};
use gents_session::views::transcript::Scope;
use gents_store::config_client::ConfigAccess;
use gents_store::views::runtime::{NodeReadinessList, NodeRuntimeList};
use rapidhash::{HashSetExt, RapidHashSet};

/// A session head request's doc id, node DID, session id, requester DID and
/// request id; the field order is the sort order.
pub(super) type RequestHeadScope = (String, String, String, Option<String>, String);

fn request_head_scopes(sessions: &[AgentSession]) -> Vec<RequestHeadScope> {
    let mut scopes = sessions
        .iter()
        .filter_map(|session| {
            let head = session.observation.as_ref()?.latest_request.as_ref()?;
            Some((
                head.request_doc_id.clone(),
                session.node_did.clone(),
                session.session_id.clone(),
                session.requester_did.clone(),
                head.request_id.clone(),
            ))
        })
        .collect::<Vec<_>>();
    scopes.sort();
    scopes.dedup();
    scopes
}

pub(super) fn scoped_session_heads(
    rows: Vec<AgentRequestRow>,
    scopes: &[RequestHeadScope],
) -> Vec<AgentRequestRow> {
    let expected = scopes
        .iter()
        .map(|(doc, node, session, requester, id)| {
            (
                doc.as_str(),
                node.as_str(),
                session.as_str(),
                requester.as_deref(),
                id.as_str(),
            )
        })
        .collect::<RapidHashSet<_>>();
    rows.into_iter()
        .filter(|row| {
            row.purpose == Some(gents_protocol::request_admission::RequestPurpose::Normal)
                && row
                    .doc_id
                    .as_deref()
                    .zip(row.node_did.as_deref())
                    .zip(row.session_id.as_deref())
                    .is_some_and(|((doc, node), session)| {
                        expected.contains(&(
                            doc,
                            node,
                            session,
                            row.requester_did.as_deref(),
                            row.request_id.as_str(),
                        ))
                    })
        })
        .collect()
}

pub(super) async fn complete_access_snapshot_requests(
    access: &ConfigAccess,
    node_did: &str,
    sessions: &[AgentSession],
    triggers: &[Trigger],
    mut seeds: Vec<AgentRequestRow>,
) -> Result<Vec<AgentRequestRow>> {
    let head_scopes = request_head_scopes(sessions);
    let mut trigger_ids = triggers
        .iter()
        .filter(|trigger| trigger.node_did == node_did)
        .map(|trigger| trigger.trigger_id.clone())
        .collect::<Vec<_>>();
    trigger_ids.sort();
    trigger_ids.dedup();
    let views = trigger_ids
        .into_iter()
        .map(|trigger| RequestRows::trigger_candidates(Scope::Node(node_did.to_owned()), trigger))
        .collect::<Vec<_>>();
    let mut task_rows = Vec::new();
    for batch in views.chunks(32) {
        task_rows.extend(access.read_views(batch).await?.into_iter().flatten());
    }
    let mut groups = std::collections::BTreeMap::new();
    for row in &task_rows {
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
    let mut boundaries = Vec::new();
    for ((node, trigger), rows) in groups {
        if rows.len() < 8 {
            continue;
        }
        anyhow::ensure!(rows.len() == 8, "task candidate page exceeded its limit");
        let boundary = rows.last().expect("nonempty candidate page");
        boundaries.push(
            RequestRows::new(Scope::Node(node.to_owned()))
                .public()
                .trigger(trigger)
                .created_at(boundary.created_at.clone())
                .request_ids(vec![boundary.request_id.clone()])
                .session_id(boundary.session_id.clone())
                .order(RequestOrder::Physical)
                .limit(8),
        );
    }
    for batch in boundaries.chunks(32) {
        seeds.extend(access.read_views(batch).await?.into_iter().flatten());
    }
    seeds.extend(task_rows);
    let mut seen = RapidHashSet::new();
    seeds.retain(|row| row.doc_id.as_ref().is_none_or(|id| seen.insert(id.clone())));
    let missing = head_scopes
        .iter()
        .filter(|(doc, _, _, _, _)| !seen.contains(doc))
        .map(|(doc, _, _, _, _)| doc.clone())
        .collect::<Vec<_>>();
    for batch in missing.chunks(128) {
        let rows = access
            .read_view(&RequestRows::new(Scope::Node(node_did.to_owned())).doc_ids(batch.to_vec()))
            .await?;
        seeds.extend(scoped_session_heads(rows, &head_scopes));
    }
    let mut seen = RapidHashSet::new();
    seeds.retain(|row| row.doc_id.as_ref().is_none_or(|id| seen.insert(id.clone())));
    super::close_request_rows_on(access, seeds).await
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
    access: &ConfigAccess,
    node_did: &str,
) -> std::result::Result<ClientStore, SnapshotReadError> {
    load_node_snapshot(access, node_did)
        .await
        .map_err(SnapshotReadError::from)
}

async fn load_node_snapshot(access: &ConfigAccess, node_did: &str) -> Result<ClientStore> {
    let (
        nodes,
        default_agent_selections,
        agents,
        runtimes,
        node_readiness,
        requests,
        mailbox_items,
        goals,
        sessions,
        tools,
        contexts,
        compactions,
        trigger_rows,
        tasks,
        schedules,
        skills,
        backends,
        observations,
        inference_profiles,
        inference_sampling,
        inference_execution,
        tool_service_registries,
        event_sources,
        agent_targets,
        datastore_tool_surfaces,
    ) = access
        .read_views(&(
            NodeList::new(node_did),
            DefaultAgentSelectionList::new(node_did),
            AgentList::new(node_did),
            NodeRuntimeList::new(node_did),
            NodeReadinessList::new(node_did),
            RequestRows::new(Scope::Node(node_did.to_owned())).nonterminal(),
            gents_automation::views::MailboxRows::new(node_did).open(),
            gents_automation::views::GoalRows::new(node_did),
            gents_session::views::SessionList {
                input: gents_protocol::operations::session::SessionListInput {
                    node_did: Some(node_did.to_owned()),
                include_archived: true,
                    ..Default::default()
                },
            },
            ToolsList::new(node_did),
            AgentContextList::new(node_did),
            CompactionConfigList::new(node_did),
            TriggerList::new(node_did),
            TaskList::new(node_did),
            ScheduleList::new(node_did),
            SkillList::new(node_did),
            BackendList::new(node_did),
            ObservationList::new(node_did),
            ProfileList::new(node_did),
            SamplingList::new(node_did),
            ExecutionList::new(node_did),
            gents_tools::mcp::views::Services {
                node_did,
                service_id: None,
                enabled_only: false,
            },
            EventSourceList::new(node_did),
            AgentTargetList::new(node_did),
            DatastoreToolSurfaceList::new(node_did),
        ))
        .await?;
    let mut triggers = Vec::with_capacity(trigger_rows.len());
    let mut trigger_observations = Vec::with_capacity(trigger_rows.len());
    let mut schedule_observations = Vec::with_capacity(trigger_rows.len());
    for row in trigger_rows {
        let (trigger, observation, schedule) = row.into_parts();
        triggers.push(trigger);
        trigger_observations.push(observation);
        schedule_observations.push(schedule);
    }
    let sessions = sessions
        .into_iter()
        .map(|row| row.session)
        .collect::<Vec<_>>();
    let requests =
        complete_access_snapshot_requests(access, node_did, &sessions, &triggers, requests).await?;
    let inference_backends = backends
        .into_iter()
        .map(|row| row.metadata.into_backend().map_err(anyhow::Error::from))
        .collect::<Result<Vec<_>>>()?;
    let backend_observations = observations
        .into_iter()
        .map(|row| row.observation)
        .collect::<Vec<_>>();
    let tasks = tasks.into_iter().map(|row| row.task).collect::<Vec<_>>();
    let schedules = schedules
        .into_iter()
        .map(|row| row.schedule)
        .collect::<Vec<_>>();
    let skills = skills
        .into_iter()
        .map(|row| row.document)
        .collect::<Vec<_>>();
    let tools = tools
        .into_iter()
        .map(|row| row.document)
        .collect::<Vec<_>>();
    let contexts = contexts
        .into_iter()
        .map(|row| row.document)
        .collect::<Vec<_>>();
    let compactions = compactions
        .into_iter()
        .map(|row| row.document)
        .collect::<Vec<_>>();
    let inference_profiles = inference_profiles
        .into_iter()
        .map(|row| row.profile)
        .collect::<Vec<_>>();
    let inference_sampling = inference_sampling
        .into_iter()
        .map(|row| row.settings)
        .collect::<Vec<_>>();
    let inference_execution = inference_execution
        .into_iter()
        .map(|row| row.settings)
        .collect::<Vec<_>>();
    let event_sources = event_sources
        .into_iter()
        .map(|row| row.event_source)
        .collect::<Vec<_>>();
    let agent_targets = agent_targets
        .into_iter()
        .map(|row| row.document)
        .collect::<Vec<_>>();
    let datastore_tool_surfaces = datastore_tool_surfaces
        .into_iter()
        .map(|row| row.document)
        .collect::<Vec<_>>();
    Ok(ClientStore::from_rows(ClientStoreRows {
        nodes: nodes.into_iter().map(|row| row.document).collect(),
        default_agent_selections: default_agent_selections
            .into_iter()
            .map(|row| row.document)
            .collect(),
        agents: agents.into_iter().map(|row| row.agent).collect(),
        runtimes,
        node_readiness,
        requests,
        mailbox_items,
        goals,
        sessions,
        tasks,
        schedules,
        schedule_observations,
        triggers,
        trigger_observations,
        skills,
        tools,
        contexts,
        compactions,
        inference_backends,
        backend_observations,
        inference_profiles,
        inference_sampling,
        inference_execution,
        tool_service_registries,
        event_sources,
        agent_targets,
        datastore_tool_surfaces,
        operator_config_authoritative_node_dids: [node_did.to_owned()].into_iter().collect(),
        ..Default::default()
    }))
}

pub async fn load_inference_configuration(
    access: &ConfigAccess,
    node_did: &str,
) -> Result<ClientStore> {
    let (nodes, selections, agents, backends, observations, profiles) = access
        .read_views(&(
            NodeList::new(node_did),
            DefaultAgentSelectionList::new(node_did),
            AgentList::new(node_did),
            BackendList::new(node_did),
            ObservationList::new(node_did),
            ProfileList::new(node_did),
        ))
        .await?;
    let backend_observations = observations
        .into_iter()
        .map(|row| row.observation)
        .collect::<Vec<_>>();
    Ok(ClientStore::from_rows(ClientStoreRows {
        nodes: nodes.into_iter().map(|row| row.document).collect(),
        default_agent_selections: selections.into_iter().map(|row| row.document).collect(),
        agents: agents.into_iter().map(|row| row.agent).collect(),
        inference_backends: backends
            .into_iter()
            .map(|row| row.metadata.into_backend().map_err(anyhow::Error::from))
            .collect::<Result<_>>()?,
        inference_profiles: profiles.into_iter().map(|row| row.profile).collect(),
        backend_observations,
        operator_config_authoritative_node_dids: [node_did.to_owned()].into_iter().collect(),
        ..Default::default()
    }))
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

