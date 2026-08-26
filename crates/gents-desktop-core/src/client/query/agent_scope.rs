use super::*;

/// Load the bounded observer projection for a specific `agent_did`.
/// Agent-keyed collections (including Goal) are filtered by `agent_did`;
/// session metadata is filtered by IDs derived from conversations/requests.
/// Transcript content is intentionally excluded and remains in DefraDB.
/// Control-plane
/// collections (InferenceBackend, InferenceProfile, ToolServiceRegistry,
/// Task, Schedule, EventTrigger) load in full — they're operator-authored
/// and small.
pub async fn load_agent_scoped_snapshot(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<ClientStore> {
    let did = escape_graphql_string(agent_did);
    let did_filter = format!("filter: {{ agent_did: {{ _eq: \"{did}\" }} }}");

    // Agent-keyed collections.
    let agent_principals: Vec<AgentPrincipalRow> = load_rows(
        node,
        AGENT_PRINCIPAL_NAME,
        &format!("query {{ {AGENT_PRINCIPAL_NAME}({did_filter}) {{ {AGENT_PRINCIPAL_FIELDS} }} }}"),
    )
    .await?;
    let behaviors: Vec<AgentBehaviorRow> = load_rows(
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
    let conversations: Vec<AgentConversationRow> = load_rows(
        node,
        AGENT_CONVERSATION_NAME,
        &format!(
            "query {{ {AGENT_CONVERSATION_NAME}({did_filter}) {{ {AGENT_CONVERSATION_FIELDS} }} }}"
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
    let tool_selections: Vec<ToolSelectionRow> = load_rows(
        node,
        TOOL_SELECTION_NAME,
        &format!("query {{ {TOOL_SELECTION_NAME}({did_filter}) {{ {TOOL_SELECTION_FIELDS} }} }}"),
    )
    .await?;

    // Derive session_id list from the agent's conversations and sessions.
    let mut session_ids: HashSet<String> = HashSet::new();
    for c in &conversations {
        session_ids.insert(c.session_id.clone());
    }
    for r in &requests {
        if let Some(sid) = r.session_id.as_deref() {
            session_ids.insert(sid.to_string());
        }
    }
    for goal in &goals {
        session_ids.insert(goal.session_id.clone());
    }

    // Session-keyed collections.
    let sessions = if session_ids.is_empty() {
        Vec::new()
    } else {
        let session_in = session_ids
            .iter()
            .map(|s| format!("\"{}\"", escape_graphql_string(s)))
            .collect::<Vec<_>>()
            .join(", ");
        let session_filter = format!("filter: {{ session_id: {{ _in: [{session_in}] }} }}");
        let sessions: Vec<AgentSessionRow> = load_rows(
            node,
            AGENT_SESSION_NAME,
            &format!(
                "query {{ {AGENT_SESSION_NAME}({session_filter}) {{ {AGENT_SESSION_FIELDS} }} }}"
            ),
        )
        .await?;
        sessions
    };

    // Control-plane (load in full; small).
    let tasks = load_tasks(node).await?;
    let schedules = load_schedules(node).await?;
    let event_triggers = load_event_triggers(node).await?;
    let skills = load_skills(node).await?;
    let inference_backends = load_inference_backends(node).await?;
    let inference_profiles = load_inference_profiles(node).await?;
    let tool_service_registries = load_tool_service_registries(node).await?;

    Ok(ClientStore::from_rows(ClientStoreRows {
        agent_principals,
        behaviors,
        runtimes,
        conversations,
        requests,
        mailbox_items,
        responses,
        sessions,
        goals,
        tasks,
        schedules,
        event_triggers,
        skills,
        tool_selections,
        inference_backends,
        inference_profiles,
        tool_service_registries,
        ..ClientStoreRows::default()
    }))
}
