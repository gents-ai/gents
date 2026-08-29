use super::*;

/// Fetch the rows for a specific set of `(collection, doc_id)` pairs and
/// return them as a single-collection `ClientStore` patch suitable for
/// `ObservedStore::merge_snapshot`. Empty `doc_ids` returns an empty store.
/// Unknown `collection_name` errors so callers can fall back to a scoped
/// reload.
pub async fn fetch_doc_patch(
    node: &EmbeddedNode,
    collection_name: &str,
    doc_ids: &[&str],
) -> Result<ClientStore> {
    if doc_ids.is_empty() {
        return Ok(ClientStore::default());
    }

    let in_clause = doc_ids
        .iter()
        .map(|id| format!("\"{}\"", escape_graphql_string(id)))
        .collect::<Vec<_>>()
        .join(", ");

    let mut rows = ClientStoreRows::default();
    match collection_name {
        AGENT_PRINCIPAL_NAME => {
            rows.agent_principals = load_rows(
                node,
                AGENT_PRINCIPAL_NAME,
                &format!("query {{ {AGENT_PRINCIPAL_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {AGENT_PRINCIPAL_FIELDS} }} }}"),
            )
            .await?;
        }
        AGENT_BEHAVIOR_NAME => {
            rows.behaviors = load_rows(
                node,
                AGENT_BEHAVIOR_NAME,
                &format!("query {{ {AGENT_BEHAVIOR_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {AGENT_BEHAVIOR_FIELDS} }} }}"),
            )
            .await?;
        }
        AGENT_RUNTIME_NAME => {
            rows.runtimes = load_rows(
                node,
                AGENT_RUNTIME_NAME,
                &format!("query {{ {AGENT_RUNTIME_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {AGENT_RUNTIME_FIELDS} }} }}"),
            )
            .await?;
        }
        AGENT_BEHAVIOR_READINESS_NAME => {
            rows.behavior_readiness = load_rows(
                node,
                AGENT_BEHAVIOR_READINESS_NAME,
                &format!("query {{ {AGENT_BEHAVIOR_READINESS_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {AGENT_BEHAVIOR_READINESS_FIELDS} }} }}"),
            )
            .await?;
        }
        AGENT_CONVERSATION_NAME => {
            rows.conversations = load_rows(
                node,
                AGENT_CONVERSATION_NAME,
                &format!("query {{ {AGENT_CONVERSATION_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {AGENT_CONVERSATION_FIELDS} }} }}"),
            )
            .await?;
        }
        AGENT_REQUEST_NAME => {
            rows.requests = load_rows(
                node,
                AGENT_REQUEST_NAME,
                &format!("query {{ {AGENT_REQUEST_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {AGENT_REQUEST_FIELDS} }} }}"),
            )
            .await?;
        }
        MAILBOX_ITEM_NAME => {
            rows.mailbox_items = load_rows(
                node,
                MAILBOX_ITEM_NAME,
                &format!("query {{ {MAILBOX_ITEM_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {MAILBOX_ITEM_FIELDS} }} }}"),
            )
            .await?;
        }
        AGENT_RESPONSE_NAME => {
            rows.responses = load_rows(
                node,
                AGENT_RESPONSE_NAME,
                &format!("query {{ {AGENT_RESPONSE_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {AGENT_RESPONSE_FIELDS} }} }}"),
            )
            .await?;
        }
        AGENT_MESSAGE_NAME => {
            rows.messages = load_rows(
                node,
                AGENT_MESSAGE_NAME,
                &format!("query {{ {AGENT_MESSAGE_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {AGENT_MESSAGE_FIELDS} }} }}"),
            )
            .await?;
        }
        AGENT_SESSION_NAME => {
            rows.sessions = load_rows(
                node,
                AGENT_SESSION_NAME,
                &format!("query {{ {AGENT_SESSION_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {AGENT_SESSION_FIELDS} }} }}"),
            )
            .await?;
        }
        GOAL_NAME => {
            rows.goals = load_rows(
                node,
                GOAL_NAME,
                &format!("query {{ {GOAL_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {GOAL_FIELDS} }} }}"),
            )
            .await?;
        }
        AGENT_TOOL_CALL_NAME => {
            rows.tool_calls = load_rows(
                node,
                AGENT_TOOL_CALL_NAME,
                &format!("query {{ {AGENT_TOOL_CALL_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {AGENT_TOOL_CALL_FIELDS} }} }}"),
            )
            .await?;
        }
        AGENT_TOOL_RESULT_NAME => {
            rows.tool_results = load_rows(
                node,
                AGENT_TOOL_RESULT_NAME,
                &format!("query {{ {AGENT_TOOL_RESULT_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {AGENT_TOOL_RESULT_FIELDS} }} }}"),
            )
            .await?;
        }
        COMPACTION_ENTRY_NAME => {
            rows.compaction_entries = load_rows(
                node,
                COMPACTION_ENTRY_NAME,
                &format!("query {{ {COMPACTION_ENTRY_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {COMPACTION_ENTRY_FIELDS} }} }}"),
            )
            .await?;
        }
        TASK_NAME => {
            rows.tasks = load_rows(
                node,
                TASK_NAME,
                &format!("query {{ {TASK_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {TASK_FIELDS} }} }}"),
            )
            .await?;
        }
        SCHEDULE_NAME => {
            rows.schedules = load_rows(
                node,
                SCHEDULE_NAME,
                &format!("query {{ {SCHEDULE_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {SCHEDULE_FIELDS} }} }}"),
            )
            .await?;
        }
        EVENT_TRIGGER_NAME => {
            rows.event_triggers = load_rows(
                node,
                EVENT_TRIGGER_NAME,
                &format!("query {{ {EVENT_TRIGGER_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {EVENT_TRIGGER_FIELDS} }} }}"),
            )
            .await?;
        }
        SKILL_NAME => {
            rows.skills = load_rows(
                node,
                SKILL_NAME,
                &format!("query {{ {SKILL_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {SKILL_FIELDS} }} }}"),
            )
            .await?;
        }
        TOOL_SELECTION_NAME => {
            rows.tool_selections = load_rows(
                node,
                TOOL_SELECTION_NAME,
                &format!("query {{ {TOOL_SELECTION_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {TOOL_SELECTION_FIELDS} }} }}"),
            )
            .await?;
        }
        INFERENCE_BACKEND_NAME => {
            rows.inference_backends = load_rows(
                node,
                INFERENCE_BACKEND_NAME,
                &format!("query {{ {INFERENCE_BACKEND_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {INFERENCE_BACKEND_FIELDS} }} }}"),
            )
            .await?;
        }
        INFERENCE_PROFILE_NAME => {
            rows.inference_profiles = load_rows(
                node,
                INFERENCE_PROFILE_NAME,
                &format!("query {{ {INFERENCE_PROFILE_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {INFERENCE_PROFILE_FIELDS} }} }}"),
            )
            .await?;
        }
        TOOL_SERVICE_REGISTRY_NAME => {
            rows.tool_service_registries = load_rows(
                node,
                TOOL_SERVICE_REGISTRY_NAME,
                &format!("query {{ {TOOL_SERVICE_REGISTRY_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {TOOL_SERVICE_REGISTRY_FIELDS} }} }}"),
            )
            .await?;
        }
        other => bail!("fetch_doc_patch: unknown collection {other}"),
    }
    Ok(ClientStore::from_rows(rows))
}

pub(crate) fn supports_doc_patch_collection(collection_name: &str) -> bool {
    matches!(
        collection_name,
        AGENT_PRINCIPAL_NAME
            | AGENT_BEHAVIOR_NAME
            | AGENT_RUNTIME_NAME
            | AGENT_BEHAVIOR_READINESS_NAME
            | AGENT_CONVERSATION_NAME
            | AGENT_REQUEST_NAME
            | MAILBOX_ITEM_NAME
            | AGENT_RESPONSE_NAME
            | AGENT_MESSAGE_NAME
            | AGENT_SESSION_NAME
            | GOAL_NAME
            | AGENT_TOOL_CALL_NAME
            | AGENT_TOOL_RESULT_NAME
            | COMPACTION_ENTRY_NAME
            | TASK_NAME
            | SCHEDULE_NAME
            | EVENT_TRIGGER_NAME
            | SKILL_NAME
            | TOOL_SELECTION_NAME
            | INFERENCE_BACKEND_NAME
            | INFERENCE_PROFILE_NAME
            | TOOL_SERVICE_REGISTRY_NAME
    )
}

pub(crate) fn is_transcript_content_collection(collection_name: &str) -> bool {
    matches!(
        collection_name,
        AGENT_MESSAGE_NAME | AGENT_TOOL_CALL_NAME | AGENT_TOOL_RESULT_NAME | COMPACTION_ENTRY_NAME
    )
}
