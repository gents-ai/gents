use super::collection_roots::{root, Root};
use super::*;
use futures::future::BoxFuture;
use gents_documents::views::config::NodeList;
use gents_documents::views::tasks::TriggerList;
use gents_session::views::requests::{RequestOrder, RequestRows};
use gents_session::views::transcript::Scope;
use gents_store::config_client::ConfigAccess;
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

/// `rows` whose physical documents no earlier seed carried.
fn fresh(rows: Vec<AgentRequestRow>, seen: &mut RapidHashSet<String>) -> Vec<AgentRequestRow> {
    rows.into_iter()
        .filter(|row| row.doc_id.as_ref().is_none_or(|id| seen.insert(id.clone())))
        .collect()
}

/// The snapshot's requests: the nonterminal `seeds`, each trigger's recent
/// task requests, each session's observed head, and every row those need.
/// Reads that do not wait on each other share a request: the task pages and
/// heads go with the seeds' first dependency round, and the task page
/// cutoffs with the next.
pub(super) async fn complete_access_snapshot_requests<'a>(
    access: &'a ConfigAccess,
    node_did: &str,
    sessions: &[AgentSession],
    triggers: &[Trigger],
    seeds: Vec<AgentRequestRow>,
) -> Result<Vec<AgentRequestRow>> {
    let mut execute =
        move |views: Vec<RequestRows>| -> BoxFuture<'a, Result<Vec<Vec<AgentRequestRow>>>> {
            Box::pin(async move { Ok(access.read_views(&views).await?) })
        };
    let mut walk = super::request_closure::RequestClosure::default();
    let mut seen = RapidHashSet::new();
    walk.seed(fresh(seeds, &mut seen));
    let head_scopes = request_head_scopes(sessions);
    let missing = head_scopes
        .iter()
        .filter(|(doc, _, _, _, _)| !seen.contains(doc))
        .map(|(doc, _, _, _, _)| doc.clone())
        .collect::<Vec<_>>();

    let mut trigger_ids = triggers
        .iter()
        .filter(|trigger| trigger.node_did == node_did)
        .map(|trigger| trigger.trigger_id.clone())
        .collect::<Vec<_>>();
    trigger_ids.sort();
    trigger_ids.dedup();
    let trigger_count = trigger_ids.len();
    let views = trigger_ids
        .into_iter()
        .map(|trigger| RequestRows::trigger_candidates(Scope::Node(node_did.to_owned()), trigger))
        .chain(missing.chunks(128).map(|batch| {
            RequestRows::new(Scope::Node(node_did.to_owned())).doc_ids(batch.to_vec())
        }))
        .collect::<Vec<_>>();
    let mut answers = walk
        .round(views, &mut execute)
        .await?
        .unwrap_or_default()
        .into_iter();
    let task_rows = answers
        .by_ref()
        .take(trigger_count)
        .flatten()
        .collect::<Vec<_>>();
    let heads = answers
        .flat_map(|rows| scoped_session_heads(rows, &head_scopes))
        .collect::<Vec<_>>();

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
    walk.seed(fresh(task_rows, &mut seen));
    walk.seed(fresh(heads, &mut seen));
    if let Some(cutoffs) = walk.round(boundaries, &mut execute).await? {
        walk.seed(fresh(cutoffs.into_iter().flatten().collect(), &mut seen));
        while walk.round(Vec::new(), &mut execute).await?.is_some() {}
    }
    Ok(walk.finish())
}

/// How a runtime snapshot read failed
/// (`RuntimeConnectionObservation.SnapshotOutcome`). The observer reconnects
/// a refused source and keeps an armed source for any other failure, so the
/// kind is decided here, by the access owner's classification, and nowhere
/// else. The selected Node must be visible for a usable snapshot.
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
    #[error("Access to this Node is unavailable. Connect this desktop to request access again.")]
    NodeUnavailable,
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
        matches!(self, Self::Refused(_) | Self::NodeUnavailable)
    }
}

pub async fn load_node_snapshot_on(
    access: &ConfigAccess,
    node_did: &str,
) -> std::result::Result<ClientStore, SnapshotReadError> {
    read_node_snapshot_root(access, node_did)
        .await?
        .finish()
        .await
}

pub struct NodeSnapshotRoot<'a> {
    access: &'a ConfigAccess,
    node_did: &'a str,
    rows: ClientStoreRows,
    requests: Vec<AgentRequestRow>,
}

impl NodeSnapshotRoot<'_> {
    pub async fn finish(mut self) -> std::result::Result<ClientStore, SnapshotReadError> {
        self.rows.requests = complete_access_snapshot_requests(
            self.access,
            self.node_did,
            &self.rows.sessions,
            &self.rows.triggers,
            self.requests,
        )
        .await?;
        Ok(ClientStore::from_rows(self.rows))
    }
}

pub async fn read_node_snapshot_root<'a>(
    access: &'a ConfigAccess,
    node_did: &'a str,
) -> std::result::Result<NodeSnapshotRoot<'a>, SnapshotReadError> {
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
            root::DefaultAgentSelections::view(node_did),
            root::Agents::view(node_did),
            root::Runtimes::view(node_did),
            root::NodeReadiness::view(node_did),
            RequestRows::new(Scope::Node(node_did.to_owned())).nonterminal(),
            root::MailboxItems::view(node_did),
            root::Goals::view(node_did),
            gents_session::views::SessionList {
                input: gents_protocol::operations::session::SessionListInput {
                    node_did: Some(node_did.to_owned()),
                    include_archived: true,
                    ..Default::default()
                },
            },
            root::Tools::view(node_did),
            root::Contexts::view(node_did),
            root::Compactions::view(node_did),
            TriggerList::new(node_did),
            root::Tasks::view(node_did),
            root::Schedules::view(node_did),
            root::Skills::view(node_did),
            root::InferenceBackends::view(node_did),
            root::BackendObservations::view(node_did),
            root::InferenceProfiles::view(node_did),
            root::InferenceSampling::view(node_did),
            root::InferenceExecution::view(node_did),
            root::ToolServiceRegistries::view(node_did),
            root::EventSources::view(node_did),
            root::AgentTargets::view(node_did),
            root::DatastoreToolSurfaces::view(node_did),
        ))
        .await
        .map_err(anyhow::Error::from)?;
    selected_node_root(!nodes.is_empty())?;
    let mut rows = ClientStoreRows {
        nodes: nodes.into_iter().map(|row| row.document).collect(),
        sessions: sessions.into_iter().map(|row| row.session).collect(),
        operator_config_authoritative_node_dids: [node_did.to_owned()].into_iter().collect(),
        ..Default::default()
    };
    root::DefaultAgentSelections::store(default_agent_selections, &mut rows)?;
    root::Agents::store(agents, &mut rows)?;
    root::Runtimes::store(runtimes, &mut rows)?;
    root::NodeReadiness::store(node_readiness, &mut rows)?;
    root::MailboxItems::store(mailbox_items, &mut rows)?;
    root::Goals::store(goals, &mut rows)?;
    root::Tools::store(tools, &mut rows)?;
    root::Contexts::store(contexts, &mut rows)?;
    root::Compactions::store(compactions, &mut rows)?;
    root::Tasks::store(tasks, &mut rows)?;
    root::Schedules::store(schedules, &mut rows)?;
    root::Skills::store(skills, &mut rows)?;
    root::InferenceBackends::store(backends, &mut rows)?;
    root::BackendObservations::store(observations, &mut rows)?;
    root::InferenceProfiles::store(inference_profiles, &mut rows)?;
    root::InferenceSampling::store(inference_sampling, &mut rows)?;
    root::InferenceExecution::store(inference_execution, &mut rows)?;
    root::ToolServiceRegistries::store(tool_service_registries, &mut rows)?;
    root::EventSources::store(event_sources, &mut rows)?;
    root::AgentTargets::store(agent_targets, &mut rows)?;
    root::DatastoreToolSurfaces::store(datastore_tool_surfaces, &mut rows)?;
    for row in trigger_rows {
        let (trigger, observation, schedule) = row.into_parts();
        rows.triggers.push(trigger);
        rows.trigger_observations.push(observation);
        rows.schedule_observations.push(schedule);
    }
    Ok(NodeSnapshotRoot {
        access,
        node_did,
        rows,
        requests,
    })
}

pub async fn load_inference_configuration(
    access: &ConfigAccess,
    node_did: &str,
) -> Result<ClientStore> {
    let (nodes, selections, agents, backends, observations, profiles) = access
        .read_views(&(
            NodeList::new(node_did),
            root::DefaultAgentSelections::view(node_did),
            root::Agents::view(node_did),
            root::InferenceBackends::view(node_did),
            root::BackendObservations::view(node_did),
            root::InferenceProfiles::view(node_did),
        ))
        .await?;
    let mut rows = ClientStoreRows {
        nodes: nodes.into_iter().map(|row| row.document).collect(),
        operator_config_authoritative_node_dids: [node_did.to_owned()].into_iter().collect(),
        ..Default::default()
    };
    root::DefaultAgentSelections::store(selections, &mut rows)?;
    root::Agents::store(agents, &mut rows)?;
    root::InferenceBackends::store(backends, &mut rows)?;
    root::BackendObservations::store(observations, &mut rows)?;
    root::InferenceProfiles::store(profiles, &mut rows)?;
    Ok(ClientStore::from_rows(rows))
}

pub async fn load_node_scoped_snapshot_with_connections(
    node_did: &str,
    peers: &[gents_protocol::runtime_connection::RuntimeConnection],
    endpoints: &gents_store::config_client::runtime_endpoints::RuntimeEndpoints,
) -> std::result::Result<ClientStore, SnapshotReadError> {
    let endpoint = peers
        .iter()
        .find(|peer| peer.node_did == node_did)
        .and_then(|peer| endpoints.endpoint(&peer.node_did, &peer.graphql).ok())
        .with_context(|| format!("runtime {node_did} has no authenticated database endpoint"))?;
    load_node_snapshot_on(
        &gents_store::config_client::ConfigAccess::Graphql(endpoint),
        node_did,
    )
    .await
}

pub(super) fn selected_node_root(visible: bool) -> std::result::Result<(), SnapshotReadError> {
    if visible {
        Ok(())
    } else {
        Err(SnapshotReadError::NodeUnavailable)
    }
}

#[cfg(test)]
mod root_contract {
    #[test]
    fn selected_node_root_matches_lean() {
        gents_lean_vocab::families::runtime_connection_observation_selected_root_snapshot::replay::<
            String,
        >(|case| {
            let visible = case.visible;
            match super::selected_node_root(visible) {
                Ok(()) => "succeeded",
                Err(error) if error.refused() => "refused",
                Err(_) => "unreachable",
            }
            .to_owned()
        });
    }
}
