use super::*;

#[cfg(test)]
pub struct DocumentPatch {
    pub store: ClientStore,
    pub observed_documents: usize,
    pub missing_doc_ids: Option<Vec<String>>,
}

#[cfg(test)]
pub async fn fetch_doc_patch(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    collection_name: &str,
    doc_ids: &[&str],
) -> Result<DocumentPatch> {
    use gents_documents::views::{agents::AgentList, config, tasks};
    use gents_inference::views::{backends, profiles::ProfileList, settings};
    use gents_session::views::{requests::RequestRows, transcript, SessionPage};
    use gents_store::views::runtime;
    if doc_ids.is_empty() {
        return Ok(DocumentPatch {
            store: ClientStore::default(),
            observed_documents: 0,
            missing_doc_ids: (collection_name == AGENT_REQUEST_NAME).then(Vec::new),
        });
    }
    let ids = doc_ids
        .iter()
        .map(|id| (*id).to_owned())
        .collect::<Vec<_>>();
    let scope = transcript::Scope::Node(node_did.to_owned());
    let mut rows = ClientStoreRows::default();
    let observed_documents;
    macro_rules! read {
        ($view:expr, $field:ident, $map:expr) => {{
            let selected = access.read_view(&$view).await?;
            observed_documents = selected.len();
            rows.$field = selected.into_iter().map($map).collect();
        }};
    }
    match collection_name {
        NODE_NAME => read!(config::NodeList::new(node_did).doc_ids(ids), nodes, |row| {
            row.document
        }),
        DEFAULT_AGENT_SELECTION_NAME => read!(
            config::DefaultAgentSelectionList::new(node_did).doc_ids(ids),
            default_agent_selections,
            |row| row.document
        ),
        AGENT_NAME => read!(AgentList::new(node_did).doc_ids(ids), agents, |row| row
            .agent),
        NODE_RUNTIME_NAME => read!(
            runtime::NodeRuntimeList::new(node_did).doc_ids(ids),
            runtimes,
            |row| row
        ),
        NODE_READINESS_NAME => read!(
            runtime::NodeReadinessList::new(node_did).doc_ids(ids),
            node_readiness,
            |row| row
        ),
        AGENT_REQUEST_NAME => read!(RequestRows::new(scope).doc_ids(ids), requests, |row| row),
        MAILBOX_ITEM_NAME => read!(
            gents_automation::views::MailboxRows::new(node_did).doc_ids(ids),
            mailbox_items,
            |row| row
        ),
        AGENT_MESSAGE_NAME => {
            let mut view = transcript::MessageRows::new(scope);
            view.document_ids = Some(ids);
            read!(view, transcript_messages, |row| row.into_canonical());
        }
        AGENT_OUTPUT_SEGMENT_NAME => {
            let mut view = transcript::SegmentRows::new(scope);
            view.document_ids = Some(ids);
            read!(view, output_segments, |row| row.into_canonical());
        }
        AGENT_SESSION_NAME => {
            let view = SessionPage {
                input: gents_protocol::operations::session::SessionListInput {
                    node_did: Some(node_did.to_owned()),
                    include_archived: true,
                    ..Default::default()
                },
                doc_ids: Some(ids),
                ..Default::default()
            };
            read!(view, sessions, |row| row.session);
        }
        GOAL_NAME => read!(
            gents_automation::views::GoalRows::new(node_did).doc_ids(ids),
            goals,
            |row| row
        ),
        AGENT_TOOL_CALL_NAME => {
            let mut view = transcript::ToolCallRows::new(scope);
            view.document_ids = Some(ids);
            read!(view, tool_calls, |row| row);
        }
        COMPACTION_ENTRY_NAME => {
            let mut view = transcript::CompactionRows::new(scope);
            view.document_ids = Some(ids);
            read!(view, compaction_entries, |row| row);
        }
        TASK_NAME => read!(tasks::TaskList::new(node_did).doc_ids(ids), tasks, |row| {
            row.task
        }),
        SCHEDULE_NAME => read!(
            tasks::ScheduleList::new(node_did).doc_ids(ids),
            schedules,
            |row| row.schedule
        ),
        TRIGGER_NAME => {
            let selected = access
                .read_view(&tasks::TriggerList::new(node_did).doc_ids(ids))
                .await?;
            observed_documents = selected.len();
            for row in selected {
                let source = Some(row.trigger.node_did.clone());
                let (trigger, observation, schedule) = row.into_parts();
                rows.triggers.push(trigger);
                rows.trigger_observations.push(observation);
                rows.schedule_observations.push(schedule);
                rows.trigger_source_node_dids.push(source.clone());
                rows.trigger_observation_source_node_dids
                    .push(source.clone());
                rows.schedule_observation_source_node_dids.push(source);
            }
        }
        SKILL_NAME => read!(
            config::SkillList::new(node_did).doc_ids(ids),
            skills,
            |row| row.document
        ),
        TOOLS_NAME => read!(
            config::ToolsList::new(node_did).doc_ids(ids),
            tools,
            |row| row.document
        ),
        INFERENCE_BACKEND_NAME => {
            let selected = access
                .read_view(&backends::BackendList::new(node_did).doc_ids(ids))
                .await?;
            observed_documents = selected.len();
            for row in selected {
                rows.inference_backend_source_node_dids
                    .push(Some(row.metadata.node_did.clone()));
                rows.inference_backends.push(row.metadata.into_backend()?);
            }
        }
        INFERENCE_BACKEND_OBSERVATION_NAME => {
            let selected = access
                .read_view(&backends::ObservationList::new(node_did).doc_ids(ids))
                .await?;
            observed_documents = selected.len();
            for row in selected {
                rows.backend_observation_source_node_dids
                    .push(Some(row.node_did));
                rows.backend_observations.push(row.observation);
            }
        }
        "AgentContext" => read!(
            config::AgentContextList::new(node_did).doc_ids(ids),
            contexts,
            |row| row.document
        ),
        "CompactionConfig" => read!(
            config::CompactionConfigList::new(node_did).doc_ids(ids),
            compactions,
            |row| row.document
        ),
        "InferenceSampling" => read!(
            settings::SamplingList::new(node_did).doc_ids(ids),
            inference_sampling,
            |row| row.settings
        ),
        "InferenceExecution" => read!(
            settings::ExecutionList::new(node_did).doc_ids(ids),
            inference_execution,
            |row| row.settings
        ),
        "EventSource" => read!(
            tasks::EventSourceList::new(node_did).doc_ids(ids),
            event_sources,
            |row| row.event_source
        ),
        AGENT_TARGET_NAME => read!(
            config::AgentTargetList::new(node_did).doc_ids(ids),
            agent_targets,
            |row| row.document
        ),
        "DatastoreToolSurface" => read!(
            config::DatastoreToolSurfaceList::new(node_did).doc_ids(ids),
            datastore_tool_surfaces,
            |row| row.document
        ),
        INFERENCE_PROFILE_NAME => read!(
            ProfileList::new(node_did).doc_ids(ids),
            inference_profiles,
            |row| row.profile
        ),
        TOOL_SERVICE_REGISTRY_NAME => read!(
            gents_tools::mcp::views::ServiceDocuments {
                node_did,
                doc_ids: &ids
            },
            tool_service_registries,
            |row| row.config
        ),
        other => bail!("fetch_doc_patch: unknown collection {other}"),
    }
    rows.context_source_node_dids = rows
        .contexts
        .iter()
        .map(|row| Some(row.node_did.clone()))
        .collect();
    rows.compaction_source_node_dids = rows
        .compactions
        .iter()
        .map(|row| Some(row.node_did.clone()))
        .collect();
    rows.inference_sampling_source_node_dids = rows
        .inference_sampling
        .iter()
        .map(|row| Some(row.node_did.clone()))
        .collect();
    rows.inference_execution_source_node_dids = rows
        .inference_execution
        .iter()
        .map(|row| Some(row.node_did.clone()))
        .collect();
    rows.event_source_source_node_dids = rows
        .event_sources
        .iter()
        .map(|row| Some(row.node_did.clone()))
        .collect();
    rows.agent_target_source_node_dids = rows
        .agent_targets
        .iter()
        .map(|row| Some(row.node_did.clone()))
        .collect();
    rows.datastore_tool_surface_source_node_dids = rows
        .datastore_tool_surfaces
        .iter()
        .map(|row| Some(row.node_did.clone()))
        .collect();
    rows.tool_service_registry_source_node_dids = rows
        .tool_service_registries
        .iter()
        .map(|row| Some(row.node_did.clone()))
        .collect();
    rows.inference_profile_source_node_dids = rows
        .inference_profiles
        .iter()
        .map(|row| Some(row.node_did.clone()))
        .collect();
    let missing_doc_ids = (collection_name == AGENT_REQUEST_NAME).then(|| {
        let found_doc_ids: rapidhash::RapidHashSet<&str> = rows
            .requests
            .iter()
            .filter_map(|row| row.doc_id.as_deref())
            .collect();
        doc_ids
            .iter()
            .filter(|doc_id| !found_doc_ids.contains(**doc_id))
            .map(|doc_id| (*doc_id).to_owned())
            .collect()
    });
    Ok(DocumentPatch {
        store: ClientStore::from_rows(rows),
        observed_documents,
        missing_doc_ids,
    })
}

pub(crate) const OBSERVED_COLLECTIONS: &[&str] = &[
    NODE_NAME,
    DEFAULT_AGENT_SELECTION_NAME,
    AGENT_NAME,
    NODE_RUNTIME_NAME,
    NODE_READINESS_NAME,
    AGENT_REQUEST_NAME,
    MAILBOX_ITEM_NAME,
    AGENT_MESSAGE_NAME,
    AGENT_OUTPUT_SEGMENT_NAME,
    AGENT_SESSION_NAME,
    GOAL_NAME,
    AGENT_TOOL_CALL_NAME,
    COMPACTION_ENTRY_NAME,
    TASK_NAME,
    SCHEDULE_NAME,
    TRIGGER_NAME,
    SKILL_NAME,
    TOOLS_NAME,
    INFERENCE_BACKEND_NAME,
    INFERENCE_BACKEND_OBSERVATION_NAME,
    INFERENCE_PROFILE_NAME,
    TOOL_SERVICE_REGISTRY_NAME,
    "AgentContext",
    "CompactionConfig",
    "InferenceSampling",
    "InferenceExecution",
    "EventSource",
    AGENT_TARGET_NAME,
    "DatastoreToolSurface",
];

#[cfg(test)]
pub(crate) fn supports_doc_patch_collection(collection_name: &str) -> bool {
    OBSERVED_COLLECTIONS.contains(&collection_name)
}

pub(crate) fn is_transcript_content_collection(collection_name: &str) -> bool {
    matches!(
        collection_name,
        AGENT_MESSAGE_NAME
            | AGENT_OUTPUT_SEGMENT_NAME
            | AGENT_TOOL_CALL_NAME
            | COMPACTION_ENTRY_NAME
    )
}
