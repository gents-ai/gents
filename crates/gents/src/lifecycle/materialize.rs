use super::*;

#[derive(Debug, Clone)]
pub struct EnqueuedAgentRequest {
    pub doc_id: String,
    pub request_id: String,
    pub session_id: String,
}

fn validate_trigger_lineage(
    trigger_lineage: &TriggerLineage,
    trigger_doc_id: Option<&str>,
) -> Result<()> {
    validate_trigger_provenance(trigger_lineage)?;
    let trigger_kind = trigger_lineage
        .trigger_kind
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let trigger_doc_id = trigger_doc_id.map(str::trim);
    match (trigger_kind, trigger_doc_id) {
        (Some("event" | "schedule"), Some(value)) if !value.is_empty() => {}
        (Some("event" | "schedule"), _) => {
            anyhow::bail!("Automated trigger lineage requires trigger_doc_id")
        }
        (_, Some(_)) => anyhow::bail!("Only automated trigger lineage may carry trigger_doc_id"),
        _ => {}
    }
    Ok(())
}

fn validate_trigger_provenance(trigger_lineage: &TriggerLineage) -> Result<()> {
    let trigger_kind = trigger_lineage
        .trigger_kind
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let source_doc_id = trigger_lineage.source_doc_id.as_deref().map(str::trim);
    match (trigger_kind, source_doc_id) {
        (Some("event"), Some(value)) if !value.is_empty() => {}
        (Some("event"), _) => anyhow::bail!("Event trigger lineage requires source_doc_id"),
        (_, Some(_)) => anyhow::bail!("Only Event trigger lineage may carry source_doc_id"),
        _ => {}
    }
    Ok(())
}

async fn resolve_created_agent_request_doc_id(
    node: &EmbeddedNode,
    mutation_response: &defra_node::QueryResponse,
    mutation_field: &str,
    escaped_request_id: &str,
    lookup_error: &str,
    missing_doc_id_error: &str,
) -> Result<String> {
    if let Some(doc_id) = extract_single_doc_id(mutation_response, mutation_field) {
        return Ok(doc_id);
    }

    let query = format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}, limit: 2) {{ _docID }} }}"#
    );
    let query_resp = node.execute(&query).await;
    if query_resp.has_errors() {
        anyhow::bail!("{lookup_error}: {:?}", query_resp.errors);
    }

    let rows = query_resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    if rows.len() != 1 {
        anyhow::bail!(
            "{missing_doc_id_error}: request_id lookup returned {} documents",
            rows.len()
        );
    }
    rows.first()
        .and_then(|row| row.get("_docID"))
        .and_then(|doc_id| doc_id.as_str())
        .ok_or_else(|| anyhow::anyhow!("{missing_doc_id_error}"))
        .map(str::to_string)
}

pub(crate) async fn write_pending_agent_request_with_lineage_and_conversation_title(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
    content: &str,
    execution_origin: ExecutionOrigin,
    trigger_lineage: TriggerLineage,
    conversation_title: Option<&str>,
) -> Result<EnqueuedAgentRequest> {
    write_pending_agent_request_with_lineage_workspace_and_conversation_title(
        node,
        agent_did,
        behavior_id,
        content,
        execution_origin,
        trigger_lineage,
        conversation_title,
        None,
        None,
        None,
        None,
    )
    .await
}

pub(crate) async fn write_pending_agent_request_with_lineage_workspace_and_conversation_title(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
    content: &str,
    execution_origin: ExecutionOrigin,
    trigger_lineage: TriggerLineage,
    conversation_title: Option<&str>,
    workspace_lineage: Option<&WorkspaceLineage>,
    request_id: Option<&str>,
    requester_did: Option<&str>,
    trigger_doc_id: Option<&str>,
) -> Result<EnqueuedAgentRequest> {
    validate_trigger_lineage(&trigger_lineage, trigger_doc_id)?;
    if trigger_lineage.trigger_kind.as_deref() == Some("manual")
        && trigger_lineage.trigger_id.is_some()
    {
        anyhow::bail!("Manual trigger enqueue must not carry trigger_id");
    }
    if let Some(workspace) = workspace_lineage {
        workspace.require_authority_if_workspace_id()?;
    }

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let request_id = request_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let session_id = uuid::Uuid::new_v4().to_string();

    let escaped_request_id = escape_graphql_string(&request_id);
    let prompt_selection = crate::skills::prompt_slash_skill_selection(content);
    let content = prompt_selection.prompt.as_str();
    let initial_status = if workspace_lineage.is_some_and(WorkspaceLineage::is_bound) {
        "workspace_binding_pending"
    } else {
        "pending"
    };
    let conversation_title = conversation_title.and_then(|title| {
        let title = title.trim();
        (!title.is_empty()).then(|| title.to_string())
    });
    let mut metadata = serde_json::Map::new();
    if !prompt_selection.selected_skill_ids.is_empty() {
        metadata.insert(
            "selected_skill_ids".to_string(),
            serde_json::json!(prompt_selection.selected_skill_ids),
        );
    }
    if let Some(title) = conversation_title.as_deref() {
        metadata.insert(
            "conversation_title".to_string(),
            serde_json::Value::String(title.to_string()),
        );
    }
    let admission = match trigger_lineage.trigger_kind.as_deref() {
        Some("manual") | None => {
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(agent_did)
        }
        Some("event" | "schedule") => {
            let source = trigger_lineage.trigger_id.as_deref().ok_or_else(|| {
                anyhow::anyhow!("runtime trigger request requires a durable trigger id")
            })?;
            gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_automated_trigger(
                agent_did, source,
            )
        }
        Some(kind) => anyhow::bail!("unsupported runtime request trigger kind {kind}"),
    };
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        request_id.clone(),
        agent_did,
        agent_did,
        behavior_id,
        session_id.clone(),
        content,
        execution_origin.as_str(),
        now,
        admission,
    );
    create.metadata =
        (!metadata.is_empty()).then(|| serde_json::Value::Object(metadata).to_string());
    create.initial_status = initial_status.to_string();
    create.caused_by_trigger_id = trigger_lineage.trigger_id.clone();
    create.caused_by_trigger_kind = trigger_lineage.trigger_kind.clone();
    create.caused_by_trigger_doc_id = trigger_doc_id.map(str::to_owned);
    create.caused_by_source_doc_id = trigger_lineage.source_doc_id.clone();
    create.caused_by_correlation = trigger_lineage.correlation.clone();
    create.caused_by_trigger_context = trigger_lineage.trigger_context.clone();
    create.max_retries = i64::from(DEFAULT_REQUEST_MAX_RETRIES);
    if let Some(workspace) = workspace_lineage {
        create.workspace_id = workspace.workspace_id.clone();
        create.workspace_authority = workspace.workspace_authority.clone();
        create.workspace_owner_deployment_id = workspace.workspace_owner_deployment_id.clone();
        create.workspace_seal_hash = workspace.workspace_seal_hash.clone();
    }
    if requester_did.is_some_and(|did| did.trim() != agent_did) {
        tracing::debug!(
            target_agent_did = agent_did,
            "runtime trigger requester provenance remains in signed trigger context"
        );
    }
    crate::sign_agent_request_create_as_registered_target(&mut create).await?;
    let mutation = create.graphql_mutation().map_err(anyhow::Error::msg)?;

    // A trigger fire is not replayable: `event_kind: created` is first-seen, so
    // dropping this create on a transient conflict loses the stage for good.
    let response = crate::graphql::graphql_mutation_with_transaction_retry(
        node,
        &mutation,
        "materialize_pending_agent_request",
    )
    .await?;

    let doc_id = resolve_created_agent_request_doc_id(
        node,
        &response,
        "create_AgentRequest",
        &escaped_request_id,
        "querying created pending AgentRequest doc id failed",
        "pending AgentRequest create returned no _docID",
    )
    .await?;

    Ok(EnqueuedAgentRequest {
        doc_id,
        request_id,
        session_id,
    })
}

pub(crate) async fn activate_workspace_bound_request(
    node: &EmbeddedNode,
    request_doc_id: &str,
) -> Result<()> {
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{
                    _docID: {{ _eq: "{doc_id}" }},
                    status: {{ _eq: "workspace_binding_pending" }},
                    lifecycle_state: {{ _eq: "pending" }}
                }},
                input: {{ status: "pending" }}
            ) {{ _docID }}
        }}"#,
        doc_id = escape_graphql_string(request_doc_id),
    );
    let response = crate::graphql::graphql_mutation_with_transaction_retry(
        node,
        &mutation,
        "activate_workspace_bound_request",
    )
    .await?;
    if crate::graphql::single_mutation_document(&response, "update_AgentRequest")?.is_none() {
        anyhow::bail!(
            "workspace-bound AgentRequest {request_doc_id} was not staged for activation"
        );
    }
    Ok(())
}

impl RequestLifecycle {
    pub fn new_with_agent_did(
        node: Arc<EmbeddedNode>,
        agent_name: &str,
        agent_did: &str,
        request: AgentRequest,
        deadline_duration_secs: u64,
    ) -> Self {
        Self::new_with_execution_binding(
            node,
            agent_name,
            agent_did,
            request,
            deadline_duration_secs,
            ExecutionOrigin::Interactive,
            "",
        )
    }

    pub fn new_with_execution_binding(
        node: Arc<EmbeddedNode>,
        agent_name: &str,
        agent_did: &str,
        request: AgentRequest,
        deadline_duration_secs: u64,
        execution_origin: ExecutionOrigin,
        backend_id: impl Into<String>,
    ) -> Self {
        let behavior_id = resolve_behavior_id(agent_name, request.behavior_id.as_deref());
        Self {
            node,
            agent_name: agent_name.to_string(),
            agent_did: agent_did.to_string(),
            behavior_id,
            execution_origin,
            backend_id: backend_id.into(),
            failure_reason: None,
            request,
            request_commit_cid: None,
            response_doc_id: None,
            progress_seq: 0,
            deadline_duration_secs,
            claimed_deadline_at: None,
            background_completion_input_through_sequence: None,
            state: LocalLifecycleState::Pending,
            valid_until_at_claim: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn materialize_claimed_with_execution_binding(
        node: Arc<EmbeddedNode>,
        agent_name: &str,
        identity: Arc<dyn crate::identity::AgentIdentity>,
        content: &str,
        deadline_duration_secs: u64,
        execution_origin: ExecutionOrigin,
        backend_id: impl Into<String>,
        trigger_lineage: TriggerLineage,
    ) -> Result<Self> {
        let agent_did = identity.did().to_string();
        let backend_id = backend_id.into();
        let behavior_id = agent_name.to_string();
        let request_id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4().to_string();
        validate_trigger_provenance(&trigger_lineage)?;
        let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
            request_id.clone(),
            &agent_did,
            &agent_did,
            behavior_id.clone(),
            session_id.clone(),
            content,
            execution_origin.as_str(),
            created_at.clone(),
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(&agent_did),
        );
        create.backend_id = (!backend_id.is_empty()).then(|| backend_id.clone());
        create.caused_by_trigger_id = trigger_lineage.trigger_id.clone();
        create.caused_by_trigger_kind = trigger_lineage.trigger_kind.clone();
        create.caused_by_source_doc_id = trigger_lineage.source_doc_id.clone();
        create.caused_by_correlation = trigger_lineage.correlation.clone();
        create.caused_by_trigger_context = trigger_lineage.trigger_context.clone();
        create.max_retries = i64::from(DEFAULT_REQUEST_MAX_RETRIES);
        crate::sign_agent_request_create(identity.as_ref(), &mut create).await?;
        let mutation = create.graphql_mutation().map_err(anyhow::Error::msg)?;
        let resp = crate::graphql::graphql_mutation_with_transaction_retry(
            node.as_ref(),
            &mutation,
            "materialize_signed_pending_request_before_owned_claim",
        )
        .await?;

        let doc_id = resolve_created_agent_request_doc_id(
            node.as_ref(),
            &resp,
            "create_AgentRequest",
            &escape_graphql_string(&request_id),
            "querying created AgentRequest doc id failed",
            "create_AgentRequest returned no _docID",
        )
        .await?;
        let queued_request = AgentRequest {
            doc_id,
            request_id,
            agent_did: agent_did.clone(),
            requester_did: Some(agent_did.clone()),
            behavior_id: Some(behavior_id),
            session_id,
            content: content.to_string(),
            temperature: None,
            top_p: None,
            top_k: None,
            seed: None,
            max_tokens: None,
            max_total_tokens: None,
            metadata: None,
            execution_origin: Some(execution_origin.as_str().to_string()),
            created_at,
            deadline: None,
            subagent_depth: 0,
            caused_by_parent_request_id: None,
            caused_by_parent_request_doc_id: None,
            caused_by_parent_tool_call_id: None,
            caused_by_parent_tool_call_doc_id: None,
            caused_by_trigger_id: trigger_lineage.trigger_id,
            caused_by_trigger_kind: trigger_lineage.trigger_kind,
            caused_by_source_doc_id: trigger_lineage.source_doc_id,
            caused_by_correlation: trigger_lineage.correlation,
            caused_by_trigger_context: trigger_lineage.trigger_context,
            workspace_id: None,
            workspace_authority: None,
            workspace_owner_deployment_id: None,
            workspace_seal_hash: None,
        };
        let request = crate::request_admission::verify_fresh_local_self_request(
            node.as_ref(),
            identity.as_ref(),
            &queued_request,
            agent_name,
        )
        .await?;
        let mut lifecycle = Self::new_with_execution_binding(
            node,
            agent_name,
            &agent_did,
            request,
            deadline_duration_secs,
            execution_origin,
            backend_id,
        );
        match lifecycle.claim_with_identity().await? {
            ClaimOutcome::Claimed => Ok(lifecycle),
            outcome => {
                anyhow::bail!("newly materialized signed AgentRequest was not claimed: {outcome:?}")
            }
        }
    }

    pub fn request(&self) -> &AgentRequest {
        &self.request
    }

    pub fn response_doc_id(&self) -> Option<&str> {
        self.response_doc_id.as_deref()
    }

    pub fn backend_id(&self) -> &str {
        &self.backend_id
    }

    pub fn behavior_id(&self) -> &str {
        &self.behavior_id
    }
}

fn conversation_title_from_metadata(metadata: Option<&str>) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(metadata?)
        .ok()?
        .get("conversation_title")?
        .as_str()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_string)
}

pub(super) struct RequestSessionProjection {
    session_id: String,
    behavior_id: String,
    session_update: String,
    session_create: String,
    conversation_update: String,
    conversation_create: String,
}

pub(super) fn request_session_projection(
    request: &AgentRequest,
    agent_name: &str,
    agent_did: &str,
    behavior_id: &str,
    started: &str,
) -> RequestSessionProjection {
    let session_id = escape_graphql_string(&request.session_id);
    let escaped_agent_name = escape_graphql_string(agent_name);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_behavior_id = escape_graphql_string(behavior_id);
    let escaped_started = escape_graphql_string(started);
    let requester_did_field = session::requester_did_create_field(request.requester_did.as_deref());
    let session_update = format!(
        r#"mutation {{
            update_AgentSession(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                input: {{
                    agent_name: "{escaped_agent_name}",
                    behavior_id: "{escaped_behavior_id}",
                    status: "active",
                    ended: null
                }}
            ) {{ _docID }}
        }}"#
    );
    let session_create = format!(
        r#"mutation {{
            create_AgentSession(input: {{
                session_id: "{session_id}",
                agent_name: "{escaped_agent_name}",
                agent_did: "{escaped_agent_did}",
                {requester_did_field}
                behavior_id: "{escaped_behavior_id}",
                started: "{escaped_started}",
                status: "active"
            }}) {{ _docID }}
        }}"#
    );
    let title = conversation_title_from_metadata(request.metadata.as_deref());
    let preview = session::derive_conversation_preview(&request.content);
    let (title, title_source) = title
        .as_deref()
        .map(|title| (title, session::CONVERSATION_TITLE_SOURCE_TASK))
        .unwrap_or(("", session::CONVERSATION_TITLE_SOURCE_FALLBACK));
    let escaped_title = escape_graphql_string(title);
    let escaped_title_source = escape_graphql_string(title_source);
    let escaped_preview = escape_graphql_string(&preview);
    let escaped_request_id = escape_graphql_string(&request.request_id);
    let conversation_update = format!(
        r#"mutation {{
            update_AgentConversation(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                input: {{
                    agent_name: "{escaped_agent_name}",
                    preview_text: "{escaped_preview}",
                    status: "processing",
                    updated_at: "{escaped_started}",
                    latest_request_id: "{escaped_request_id}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let conversation_create = format!(
        r#"mutation {{
            create_AgentConversation(input: {{
                session_id: "{session_id}",
                agent_name: "{escaped_agent_name}",
                agent_did: "{escaped_agent_did}",
                {requester_did_field}
                behavior_id: "{escaped_behavior_id}",
                title: "{escaped_title}",
                title_source: "{escaped_title_source}",
                preview_text: "{escaped_preview}",
                status: "processing",
                created_at: "{escaped_started}",
                updated_at: "{escaped_started}",
                latest_request_id: "{escaped_request_id}"
            }}) {{ _docID }}
        }}"#
    );
    RequestSessionProjection {
        session_id: request.session_id.clone(),
        behavior_id: behavior_id.to_string(),
        session_update,
        session_create,
        conversation_update,
        conversation_create,
    }
}

pub(super) async fn apply_request_session_projection(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    projection: &RequestSessionProjection,
) -> Result<()> {
    let escaped_session_id = escape_graphql_string(&projection.session_id);
    let binding_query = format!(
        r#"{{
            AgentSession(filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }}) {{
                behavior_id
            }}
        }}"#
    );
    let binding = txn.execute_local_response(&binding_query).await?;
    let sessions = binding
        .data
        .as_ref()
        .and_then(|data| data.get("AgentSession"))
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if let Some(existing_behavior_id) = sessions
        .iter()
        .filter_map(|row| row.get("behavior_id").and_then(serde_json::Value::as_str))
        .map(str::trim)
        .find(|value| !value.is_empty() && *value != projection.behavior_id)
    {
        return Err(ClaimAdmissionError::SessionBehaviorMismatch {
            session_id: projection.session_id.clone(),
            existing_behavior_id: existing_behavior_id.to_string(),
            requested_behavior_id: projection.behavior_id.clone(),
        }
        .into());
    }
    if sessions.is_empty() {
        txn.execute_local_response(&projection.session_create)
            .await?;
    } else {
        txn.execute_local_response(&projection.session_update)
            .await?;
    }

    let conversation = txn
        .execute_local_response(&projection.conversation_update)
        .await?;
    if !conversation
        .data
        .as_ref()
        .and_then(|data| data.get("update_AgentConversation"))
        .is_some_and(response_has_documents)
    {
        txn.execute_local_response(&projection.conversation_create)
            .await?;
    }
    Ok(())
}
