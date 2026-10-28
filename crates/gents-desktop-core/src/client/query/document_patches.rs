use super::*;
use gents::session::canonical_rows::{
    AGENT_MESSAGE_FIELDS, AGENT_OUTPUT_SEGMENT_FIELDS, decode_output_segment_row,
    decode_transcript_message_row,
};

pub struct DocumentPatch {
    pub store: ClientStore,
    pub observed_documents: usize,
    pub missing_doc_ids: Option<Vec<String>>,
}

pub async fn fetch_doc_patch(
    node: &EmbeddedNode,
    collection_name: &str,
    doc_ids: &[&str],
) -> Result<DocumentPatch> {
    if doc_ids.is_empty() {
        return Ok(DocumentPatch {
            store: ClientStore::default(),
            observed_documents: 0,
            missing_doc_ids: (collection_name == AGENT_REQUEST_NAME).then(Vec::new),
        });
    }

    let in_clause = doc_ids
        .iter()
        .map(|id| format!("\"{}\"", escape_graphql_string(id)))
        .collect::<Vec<_>>()
        .join(", ");

    let mut rows = ClientStoreRows::default();
    let mut observed_physical_documents = None;
    match collection_name {
        NODE_NAME => {
            rows.nodes = load_rows(
                node,
                NODE_NAME,
                &format!("query {{ {NODE_NAME}(docIDs: [{in_clause}]) {{ {NODE_FIELDS} }} }}"),
            )
            .await?;
        }
        AGENT_NAME => {
            rows.agents = load_rows(
                node,
                AGENT_NAME,
                &format!("query {{ {AGENT_NAME}(docIDs: [{in_clause}]) {{ {AGENT_FIELDS} }} }}"),
            )
            .await?;
        }
        NODE_RUNTIME_NAME => {
            rows.runtimes = load_rows(
                node,
                NODE_RUNTIME_NAME,
                &format!("query {{ {NODE_RUNTIME_NAME}(docIDs: [{in_clause}]) {{ {NODE_RUNTIME_FIELDS} }} }}"),
            )
            .await?;
        }
        NODE_READINESS_NAME => {
            rows.node_readiness = load_rows(
                node,
                NODE_READINESS_NAME,
                &format!("query {{ {NODE_READINESS_NAME}(docIDs: [{in_clause}]) {{ {NODE_READINESS_FIELDS} }} }}"),
            )
            .await?;
        }
        AGENT_REQUEST_NAME => {
            rows.requests = load_rows(
                node,
                AGENT_REQUEST_NAME,
                &format!("query {{ {AGENT_REQUEST_NAME}(docIDs: [{in_clause}]) {{ {AGENT_REQUEST_FIELDS} }} }}"),
            )
            .await?;
        }
        MAILBOX_ITEM_NAME => {
            rows.mailbox_items = load_rows(
                node,
                MAILBOX_ITEM_NAME,
                &format!("query {{ {MAILBOX_ITEM_NAME}(docIDs: [{in_clause}]) {{ {MAILBOX_ITEM_FIELDS} }} }}"),
            )
            .await?;
        }
        AGENT_MESSAGE_NAME => {
            let data = execute_local_graphql_query(
                node,
                &format!("query {{ {AGENT_MESSAGE_NAME}(docIDs: [{in_clause}]) {{ {AGENT_MESSAGE_FIELDS} }} }}"),
                "canonical message patch",
            ).await?;
            rows.transcript_messages =
                parse_canonical_rows(&data, AGENT_MESSAGE_NAME, decode_transcript_message_row)?;
        }
        AGENT_OUTPUT_SEGMENT_NAME => {
            let data = execute_local_graphql_query(
                node,
                &format!("query {{ {AGENT_OUTPUT_SEGMENT_NAME}(docIDs: [{in_clause}]) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"),
                "canonical output segment patch",
            ).await?;
            rows.output_segments =
                parse_canonical_rows(&data, AGENT_OUTPUT_SEGMENT_NAME, decode_output_segment_row)?;
        }
        AGENT_SESSION_NAME => {
            rows.sessions = load_rows(
                node,
                AGENT_SESSION_NAME,
                &format!("query {{ {AGENT_SESSION_NAME}(docIDs: [{in_clause}]) {{ {AGENT_SESSION_FIELDS} }} }}"),
            )
            .await?;
        }
        GOAL_NAME => {
            rows.goals = load_rows(
                node,
                GOAL_NAME,
                &format!("query {{ {GOAL_NAME}(docIDs: [{in_clause}]) {{ {GOAL_FIELDS} }} }}"),
            )
            .await?;
        }
        AGENT_TOOL_CALL_NAME => {
            rows.tool_calls = load_rows(
                node,
                AGENT_TOOL_CALL_NAME,
                &format!("query {{ {AGENT_TOOL_CALL_NAME}(docIDs: [{in_clause}]) {{ {AGENT_TOOL_CALL_FIELDS} }} }}"),
            )
            .await?;
        }
        COMPACTION_ENTRY_NAME => {
            rows.compaction_entries = load_rows(
                node,
                COMPACTION_ENTRY_NAME,
                &format!("query {{ {COMPACTION_ENTRY_NAME}(docIDs: [{in_clause}]) {{ {COMPACTION_ENTRY_FIELDS} }} }}"),
            )
            .await?;
        }
        TASK_NAME => {
            rows.tasks = load_rows(
                node,
                TASK_NAME,
                &format!("query {{ {TASK_NAME}(docIDs: [{in_clause}]) {{ {TASK_FIELDS} }} }}"),
            )
            .await?;
        }
        SCHEDULE_NAME => {
            rows.schedules = load_rows(
                node,
                SCHEDULE_NAME,
                &format!("query {{ {SCHEDULE_NAME}(docIDs: [{in_clause}]) {{ {SCHEDULE_FIELDS} }} }}"),
            )
            .await?;
        }
        TRIGGER_NAME => {
            let data = execute_local_graphql_query(
                node,
                &format!("query {{ {TRIGGER_NAME}(docIDs: [{in_clause}]) {{ {TRIGGER_FIELDS} {TRIGGER_OBSERVATION_FIELDS} {SCHEDULE_OBSERVATION_FIELDS} }} }}"),
                "trigger document patch",
            ).await?;
            observed_physical_documents = Some(
                data.get(TRIGGER_NAME)
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len),
            );
            (rows.triggers, rows.trigger_source_node_dids) = parse_projected_query_rows_with_source(
                &data,
                TRIGGER_NAME,
                TRIGGER_FIELDS,
                &format!("{TRIGGER_OBSERVATION_FIELDS} {SCHEDULE_OBSERVATION_FIELDS}"),
                Some("node_did"),
            )?;
            (rows.trigger_observations, rows.trigger_observation_source_node_dids) = parse_observation_query_rows_with_source(
                &data,
                TRIGGER_NAME,
                TRIGGER_OBSERVATION_FIELDS,
                Some("node_did"),
            )?;
            (rows.schedule_observations, rows.schedule_observation_source_node_dids) = parse_observation_query_rows_with_source(
                &data,
                TRIGGER_NAME,
                SCHEDULE_OBSERVATION_FIELDS,
                Some("node_did"),
            )?;
        }
        SKILL_NAME => {
            rows.skills = load_rows(
                node,
                SKILL_NAME,
                &format!("query {{ {SKILL_NAME}(docIDs: [{in_clause}]) {{ {SKILL_FIELDS} }} }}"),
            )
            .await?;
        }
        TOOLS_NAME => {
            rows.tools = load_rows(
                node,
                TOOLS_NAME,
                &format!("query {{ {TOOLS_NAME}(docIDs: [{in_clause}]) {{ {TOOLS_FIELDS} }} }}"),
            )
            .await?;
        }
        INFERENCE_BACKEND_NAME => {
            let data = execute_local_graphql_query(
                node,
                &format!("query {{ {INFERENCE_BACKEND_NAME}(docIDs: [{in_clause}]) {{ {INFERENCE_BACKEND_FIELDS} {INFERENCE_BACKEND_OBSERVATION_FIELDS} }} }}"),
                "inference backend document patch",
            ).await?;
            observed_physical_documents = Some(
                data.get(INFERENCE_BACKEND_NAME)
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len),
            );
            (rows.inference_backends, rows.inference_backend_source_node_dids) = parse_projected_query_rows_with_source(
                &data,
                INFERENCE_BACKEND_NAME,
                INFERENCE_BACKEND_FIELDS,
                INFERENCE_BACKEND_OBSERVATION_FIELDS,
                Some("node_did"),
            )?;
            (rows.backend_observations, rows.backend_observation_source_node_dids) =
                parse_backend_observation_rows(&data, INFERENCE_BACKEND_NAME)?;
        }
        "AgentContext" => rows.contexts = load_rows(node, "AgentContext", &format!("query {{ AgentContext(docIDs: [{in_clause}]) {{ {AGENT_CONTEXT_FIELDS} }} }}")).await?,
        "CompactionConfig" => rows.compactions = load_rows(node, "CompactionConfig", &format!("query {{ CompactionConfig(docIDs: [{in_clause}]) {{ {COMPACTION_CONFIG_FIELDS} }} }}")).await?,
        "InferenceSampling" => rows.inference_sampling = load_rows(node, "InferenceSampling", &format!("query {{ InferenceSampling(docIDs: [{in_clause}]) {{ {INFERENCE_SAMPLING_FIELDS} }} }}")).await?,
        "InferenceExecution" => rows.inference_execution = load_rows(node, "InferenceExecution", &format!("query {{ InferenceExecution(docIDs: [{in_clause}]) {{ {INFERENCE_EXECUTION_FIELDS} }} }}")).await?,
        "EventSource" => rows.event_sources = load_rows(node, "EventSource", &format!("query {{ EventSource(docIDs: [{in_clause}]) {{ {EVENT_SOURCE_FIELDS} }} }}")).await?,
        AGENT_TARGET_NAME => rows.agent_targets = load_rows(node, AGENT_TARGET_NAME, &format!("query {{ {AGENT_TARGET_NAME}(docIDs: [{in_clause}]) {{ {AGENT_TARGET_FIELDS} }} }}")).await?,
        "DatastoreToolSurface" => rows.datastore_tool_surfaces = load_rows(node, "DatastoreToolSurface", &format!("query {{ DatastoreToolSurface(docIDs: [{in_clause}]) {{ {DATASTORE_TOOL_SURFACE_FIELDS} }} }}")).await?,
        "ChainKeyBinding" => rows.chain_key_bindings = load_rows(node, "ChainKeyBinding", &format!("query {{ ChainKeyBinding(docIDs: [{in_clause}]) {{ {CHAIN_KEY_BINDING_FIELDS} }} }}")).await?,
        INFERENCE_PROFILE_NAME => {
            rows.inference_profiles = load_rows(
                node,
                INFERENCE_PROFILE_NAME,
                &format!("query {{ {INFERENCE_PROFILE_NAME}(docIDs: [{in_clause}]) {{ {INFERENCE_PROFILE_FIELDS} }} }}"),
            )
            .await?;
        }
        TOOL_SERVICE_REGISTRY_NAME => {
            rows.tool_service_registries = load_rows(
                node,
                TOOL_SERVICE_REGISTRY_NAME,
                &format!("query {{ {TOOL_SERVICE_REGISTRY_NAME}(docIDs: [{in_clause}]) {{ {TOOL_SERVICE_REGISTRY_FIELDS} }} }}"),
            )
            .await?;
        }
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
    rows.chain_key_binding_source_node_dids = rows
        .chain_key_bindings
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
    let request_count = rows.requests.len();
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
    let store = ClientStore::from_rows(rows);
    let observed_documents = if collection_name == AGENT_REQUEST_NAME {
        request_count
    } else {
        observed_physical_documents.unwrap_or_else(|| store.row_count())
    };
    Ok(DocumentPatch {
        store,
        observed_documents,
        missing_doc_ids,
    })
}

pub(crate) fn supports_doc_patch_collection(collection_name: &str) -> bool {
    matches!(
        collection_name,
        NODE_NAME
            | AGENT_NAME
            | NODE_RUNTIME_NAME
            | NODE_READINESS_NAME
            | AGENT_REQUEST_NAME
            | MAILBOX_ITEM_NAME
            | AGENT_MESSAGE_NAME
            | AGENT_OUTPUT_SEGMENT_NAME
            | AGENT_SESSION_NAME
            | GOAL_NAME
            | AGENT_TOOL_CALL_NAME
            | COMPACTION_ENTRY_NAME
            | TASK_NAME
            | SCHEDULE_NAME
            | TRIGGER_NAME
            | SKILL_NAME
            | TOOLS_NAME
            | INFERENCE_BACKEND_NAME
            | INFERENCE_PROFILE_NAME
            | TOOL_SERVICE_REGISTRY_NAME
            | "AgentContext"
            | "CompactionConfig"
            | "InferenceSampling"
            | "InferenceExecution"
            | "EventSource"
            | AGENT_TARGET_NAME
            | "DatastoreToolSurface"
            | "ChainKeyBinding"
    )
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
