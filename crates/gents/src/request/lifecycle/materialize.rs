use super::*;
use anyhow::Context;
use gents_protocol::request_lifecycle::{
    EnqueuedAgentRequest, SessionMessageCause, SessionMessageTarget,
};

/// A pending fire or EventGroup state persisted before the upgrade can still
/// name a retired graph trigger as its cause; such a request is never created.
fn refuse_retired_reserved_cause(
    create: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<()> {
    if let Some(id) = create
        .caused_by_trigger_id
        .as_deref()
        .filter(|id| gents_protocol::canonical::is_retired_reserved_id(id))
    {
        anyhow::bail!("trigger {id} belongs to a retired graph revision; it no longer runs");
    }
    Ok(())
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
    access: &crate::ConfigAccess,
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
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}, limit: 2) {{ _docID request_id }} }}"#
    );
    let query_resp = access
        .execute(&query)
        .await
        .context(lookup_error.to_owned())?;
    let rows: Vec<gents_protocol::row::AgentRequestRow> =
        serde_json::from_value(query_resp["data"]["AgentRequest"].clone())?;
    if rows.len() != 1 {
        anyhow::bail!(
            "{missing_doc_id_error}: request_id lookup returned {} documents",
            rows.len()
        );
    }
    rows.first()
        .and_then(|row| row.doc_id.as_deref())
        .ok_or_else(|| anyhow::anyhow!("{missing_doc_id_error}"))
        .map(str::to_string)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn write_pending_agent_request_with_lineage_workspace_and_conversation_title(
    node: &std::sync::Arc<EmbeddedNode>,
    actor: ::identity::Did,
    node_did: &str,
    agent_did: &str,
    content: &str,
    execution_origin: ExecutionOrigin,
    trigger_lineage: TriggerLineage,
    conversation_title: Option<&str>,
    workspace_lineage: Option<&WorkspaceLineage>,
    request_id: Option<&str>,
    requester_did: Option<&str>,
    trigger_doc_id: Option<&str>,
    session_id: Option<&str>,
    identity: &dyn crate::store::identity::NodeIdentity,
) -> Result<EnqueuedAgentRequest> {
    write_pending_agent_request_with_identity(
        node,
        actor,
        node_did,
        agent_did,
        content,
        execution_origin,
        trigger_lineage,
        conversation_title,
        workspace_lineage,
        request_id,
        requester_did,
        trigger_doc_id,
        session_id,
        Some(identity),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn write_pending_agent_request_with_identity(
    node: &std::sync::Arc<EmbeddedNode>,
    actor: ::identity::Did,
    node_did: &str,
    agent_did: &str,
    content: &str,
    execution_origin: ExecutionOrigin,
    trigger_lineage: TriggerLineage,
    conversation_title: Option<&str>,
    workspace_lineage: Option<&WorkspaceLineage>,
    request_id: Option<&str>,
    requester_did: Option<&str>,
    trigger_doc_id: Option<&str>,
    session_id: Option<&str>,
    identity: Option<&dyn crate::store::identity::NodeIdentity>,
) -> Result<EnqueuedAgentRequest> {
    let request_id = request_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let session_id = session_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let spec = pending_agent_request_spec(
        node_did,
        agent_did,
        content,
        execution_origin,
        trigger_lineage,
        conversation_title,
        workspace_lineage,
        &request_id,
        &session_id,
        None,
        requester_did,
        trigger_doc_id,
    )?;
    let identity = identity.context("request materialization requires a managed Agent identity")?;
    let access = crate::ConfigAccess::local_managed_agent(node.clone(), actor, identity)?;
    let create = build_signed_request(spec, RequestSigner::Identity(identity)).await?;
    refuse_retired_reserved_cause(&create)?;
    let escaped_request_id = escape_graphql_string(&request_id);
    let mutation = create.graphql_mutation().map_err(anyhow::Error::msg)?;
    let mutation = &mutation;

    let response = access
        .transact_write("lifecycle.materialize_pending", |txn| {
            Box::pin(async move { txn.execute_local_response(mutation).await })
        })
        .await?;

    let doc_id = resolve_created_agent_request_doc_id(
        &access,
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

#[allow(clippy::too_many_arguments)]
pub async fn build_signed_pending_agent_request_with_lineage_workspace_and_conversation_title(
    node_did: &str,
    agent_did: &str,
    content: &str,
    execution_origin: ExecutionOrigin,
    trigger_lineage: TriggerLineage,
    conversation_title: Option<&str>,
    workspace_lineage: Option<&WorkspaceLineage>,
    request_id: &str,
    session_id: &str,
    retry_key: Option<&str>,
    requester_did: Option<&str>,
    trigger_doc_id: Option<&str>,
) -> Result<gents_protocol::request_admission::AgentRequestCreate> {
    let spec = pending_agent_request_spec(
        node_did,
        agent_did,
        content,
        execution_origin,
        trigger_lineage,
        conversation_title,
        workspace_lineage,
        request_id,
        session_id,
        retry_key,
        requester_did,
        trigger_doc_id,
    )?;
    build_signed_request(spec, RequestSigner::RegisteredTarget).await
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn pending_agent_request_spec(
    node_did: &str,
    agent_did: &str,
    content: &str,
    execution_origin: ExecutionOrigin,
    trigger_lineage: TriggerLineage,
    conversation_title: Option<&str>,
    workspace_lineage: Option<&WorkspaceLineage>,
    request_id: &str,
    session_id: &str,
    retry_key: Option<&str>,
    requester_did: Option<&str>,
    trigger_doc_id: Option<&str>,
) -> Result<RequestSpec> {
    validate_trigger_lineage(&trigger_lineage, trigger_doc_id)?;
    if trigger_lineage.trigger_kind.as_deref() == Some("manual")
        && trigger_lineage.trigger_id.is_some()
    {
        anyhow::bail!("Manual trigger enqueue must not carry trigger_id");
    }
    if let Some(workspace) = workspace_lineage {
        workspace.require_authority_if_workspace_id()?;
    }

    let request_id = request_id.trim();
    let session_id = session_id.trim();
    anyhow::ensure!(!request_id.is_empty(), "request_id must be non-empty");
    anyhow::ensure!(!session_id.is_empty(), "session_id must be non-empty");
    let retry_key = retry_key.map(str::trim);
    anyhow::ensure!(
        retry_key.is_none_or(|value| !value.is_empty()),
        "retry_key must be non-empty when supplied"
    );
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let prompt_selection = crate::documents::skills::prompt_slash_skill_selection(content);
    let content = prompt_selection.prompt.as_str();
    let initial_lifecycle_state = if workspace_lineage.is_some_and(WorkspaceLineage::is_bound) {
        RequestLifecycleState::WorkspaceBindingPending
    } else {
        RequestLifecycleState::Pending
    };
    let conversation_title = conversation_title.and_then(|title| {
        let title = title.trim();
        (!title.is_empty()).then(|| title.to_string())
    });
    let input = gents_protocol::request_input::RequestInput {
        selected_skill_ids: prompt_selection.selected_skill_ids,
        initial_title: conversation_title.map(|text| gents_protocol::session::SessionTitle {
            text,
            source: gents_protocol::session::SessionTitleSource::Task,
        }),
        ..Default::default()
    };
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
    let identity = RequestIdentity {
        requester_did: requester_did.map(str::to_owned),
        request_id: request_id.to_string(),
        node_did: node_did.to_string(),
        agent_did: agent_did.to_string(),
        session_id: session_id.to_string(),
        content: content.to_string(),
        execution_origin,
        created_at: now,
    };
    let spec = RequestSpec {
        initial_lifecycle_state,
        trigger_lineage,
        trigger_doc_id: trigger_doc_id.map(str::to_owned),
        workspace: workspace_lineage.cloned(),
        input,
        retry_key: retry_key.map(str::to_owned),
        ..RequestSpec::new(
            gents_protocol::request_admission::RequestPurpose::Normal,
            identity,
            admission,
        )
    };
    Ok(spec)
}

pub struct RequestIdentity {
    pub requester_did: Option<String>,
    pub request_id: String,
    pub node_did: String,
    pub agent_did: String,
    pub session_id: String,
    pub content: String,
    pub execution_origin: ExecutionOrigin,
    pub created_at: String,
}

#[derive(Default)]
pub struct ParentLink {
    pub depth: u32,
    pub parent_request_id: String,
    pub parent_request_doc_id: String,
    pub parent_tool_call_id: Option<String>,
    pub parent_tool_call_doc_id: Option<String>,
}

pub struct RetryLink {
    pub parent_request_id: Option<String>,
    pub parent_request_doc_id: Option<String>,
    pub root_request_id: String,
    pub retry_count: i64,
    pub max_retries: i64,
}

pub struct RequestSpec {
    pub purpose: gents_protocol::request_admission::RequestPurpose,
    pub identity: RequestIdentity,
    pub admission: gents_protocol::request_admission::AgentRequestAdmissionRecord,
    pub initial_lifecycle_state: RequestLifecycleState,
    pub trigger_lineage: TriggerLineage,
    pub trigger_doc_id: Option<String>,
    pub workspace: Option<WorkspaceLineage>,
    pub parent: Option<ParentLink>,
    pub retry: Option<RetryLink>,
    pub input: gents_protocol::request_input::RequestInput,
    pub retry_key: Option<String>,
    pub valid_until: Option<String>,
}

impl RequestSpec {
    pub fn new(
        purpose: gents_protocol::request_admission::RequestPurpose,
        identity: RequestIdentity,
        admission: gents_protocol::request_admission::AgentRequestAdmissionRecord,
    ) -> Self {
        Self {
            purpose,
            identity,
            admission,
            initial_lifecycle_state: RequestLifecycleState::Pending,
            trigger_lineage: TriggerLineage::default(),
            trigger_doc_id: None,
            workspace: None,
            parent: None,
            retry: None,
            input: Default::default(),
            retry_key: None,
            valid_until: None,
        }
    }
}

pub enum RequestSigner<'a> {
    RegisteredTarget,
    Identity(&'a dyn crate::store::identity::NodeIdentity),
}

pub(crate) fn build_request(
    spec: RequestSpec,
) -> Result<gents_protocol::request_admission::AgentRequestCreate> {
    let RequestSpec {
        purpose,
        identity,
        admission,
        initial_lifecycle_state,
        trigger_lineage,
        trigger_doc_id,
        workspace,
        parent,
        retry,
        input,
        retry_key,
        valid_until,
    } = spec;

    let request_id = identity.request_id.clone();
    let acting_agent = identity.agent_did.clone();

    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        purpose,
        identity.request_id,
        identity.node_did,
        identity.requester_did.unwrap_or(acting_agent),
        identity.agent_did,
        identity.session_id,
        identity.content,
        identity.execution_origin.as_str(),
        identity.created_at,
        admission,
    );

    create.initial_lifecycle_state = initial_lifecycle_state;
    create.input = input;
    create.retry_key = retry_key;
    create.valid_until = valid_until;

    create.caused_by_trigger_id = trigger_lineage.trigger_id;
    create.caused_by_trigger_kind = trigger_lineage.trigger_kind;
    create.caused_by_trigger_doc_id = trigger_doc_id;
    create.caused_by_source_doc_id = trigger_lineage.source_doc_id;
    create.caused_by_correlation = trigger_lineage.correlation;
    create.caused_by_trigger_context = trigger_lineage.trigger_context;

    if let Some(workspace) = workspace {
        create.workspace_id = workspace.workspace_id;
        create.workspace_owner_node_did = workspace.workspace_owner_node_did;
        create.workspace_authority = workspace.workspace_authority;
        create.workspace_seal_hash = workspace.workspace_seal_hash;
    }

    create.request_hop = parent.as_ref().map_or(0, |link| link.depth);
    if let Some(link) = parent {
        create.caused_by_parent_request_id = Some(link.parent_request_id);
        create.caused_by_parent_request_doc_id = Some(link.parent_request_doc_id);
        create.caused_by_parent_tool_call_id = link.parent_tool_call_id;
        create.caused_by_parent_tool_call_doc_id = link.parent_tool_call_doc_id;
    }

    create.retry_root_request = Some(
        retry
            .as_ref()
            .map_or_else(|| request_id.clone(), |link| link.root_request_id.clone()),
    );
    create.max_retries = retry.as_ref().map_or(
        match purpose {
            gents_protocol::request_admission::RequestPurpose::Normal => {
                i64::from(DEFAULT_REQUEST_MAX_RETRIES)
            }
            gents_protocol::request_admission::RequestPurpose::TitleAudit
            | gents_protocol::request_admission::RequestPurpose::AccessReview => 0,
        },
        |link| link.max_retries,
    );
    create.retry_count = retry.as_ref().map_or(0, |link| link.retry_count);
    if let Some(link) = retry {
        create.retry_parent_request = link.parent_request_id;
        create.retry_parent_request_doc_id = link.parent_request_doc_id;
    }

    Ok(create)
}

pub(crate) async fn sign_request(
    create: &mut gents_protocol::request_admission::AgentRequestCreate,
    signer: RequestSigner<'_>,
) -> Result<()> {
    match signer {
        RequestSigner::RegisteredTarget => {
            crate::sign_agent_request_create_as_registered_target(create).await?;
        }
        RequestSigner::Identity(identity) => {
            crate::sign_agent_request_create(identity, create).await?;
        }
    }
    Ok(())
}

pub async fn build_signed_request(
    spec: RequestSpec,
    signer: RequestSigner<'_>,
) -> Result<gents_protocol::request_admission::AgentRequestCreate> {
    let mut create = build_request(spec)?;
    sign_request(&mut create, signer).await?;
    Ok(create)
}

/// A review exists only while its caller is waiting. Its finite validity lets
/// the preclaim owner settle abandoned work without replaying provider calls.
pub(crate) async fn write_pending_auxiliary_request(
    node: &std::sync::Arc<EmbeddedNode>,
    parent: &AgentRequest,
    purpose: gents_protocol::request_admission::RequestPurpose,
    content: String,
    valid_until: Option<String>,
    signer: &crate::store::identity::ManagedAgentSigner,
) -> Result<AgentRequest> {
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
    anyhow::ensure!(
        parent.purpose == RequestPurpose::Normal && purpose.is_auxiliary(),
        "auxiliary requests require an ordinary parent"
    );
    anyhow::ensure!(
        purpose != RequestPurpose::AccessReview || valid_until.is_some(),
        "access review requires finite validity"
    );
    let identity = RequestIdentity {
        request_id: uuid::Uuid::new_v4().to_string(),
        node_did: parent.node_did.clone(),
        requester_did: Some(parent.agent_did.clone()),
        agent_did: parent.agent_did.clone(),
        session_id: parent.session_id.clone(),
        content,
        execution_origin: ExecutionOrigin::Interactive,
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };
    let admission =
        AgentRequestAdmissionRecord::runtime_local_control(&parent.agent_did, &parent.request_id);
    let spec = RequestSpec {
        valid_until,
        parent: Some(ParentLink {
            parent_request_id: parent.request_id.clone(),
            parent_request_doc_id: parent.doc_id.clone(),
            ..Default::default()
        }),
        ..RequestSpec::new(purpose, identity, admission)
    };
    anyhow::ensure!(
        signer.did() == parent.agent_did,
        "auxiliary signer differs from parent Agent"
    );
    let create = build_signed_request(spec, RequestSigner::Identity(signer)).await?;
    let mutation = create
        .graphql_mutation_selecting(gents_protocol::row::AGENT_REQUEST_FIELDS)
        .map_err(anyhow::Error::msg)?;
    let response = crate::store::config_client::ConfigAccess::local_with_agent_signer(
        node.clone(),
        identity::Did::new(parent.agent_did.clone())?,
        signer.clone(),
    )?
    .write("lifecycle.materialize_auxiliary", &mutation)
    .await?;
    crate::request::watcher::agent_request_from_mutation_response(
        &serde_json::from_value(response)?,
        "create_AgentRequest",
    )?
    .context("auxiliary request creation omitted its exact durable request")
}

/// A same-node session message is signed by the operator's own DID, so this
/// line is how the target model tells another agent's text from its operator's.
pub(crate) fn session_message_sender_header(cause: &SessionMessageCause, at: &str) -> String {
    super::session_message_presentation::sender_header(
        &cause.caller_node_did,
        &cause.caller_agent_did,
        &cause.caller_session_id,
        &cause.caller_request_id,
        at,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "called from session_message with this shape"
)]
pub(crate) async fn build_session_message_request(
    cause: &SessionMessageCause,
    target: &SessionMessageTarget,
    content: &str,
    title: Option<&str>,
    request_id: &str,
    retry_key: Option<String>,
    hop: u32,
    queue: Option<gents_protocol::request_input::RequestQueue>,
    cwd: Option<String>,
    signer: &crate::store::identity::ManagedAgentSigner,
) -> Result<gents_protocol::request_admission::AgentRequestCreate> {
    build_session_message_request_with_input(
        cause,
        target,
        content,
        title,
        request_id,
        retry_key,
        hop,
        gents_protocol::request_input::RequestInput {
            queue,
            cwd,
            ..Default::default()
        },
        signer,
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "called from session_message and plugin_job effects with this shape"
)]
pub(crate) async fn build_session_message_request_with_input(
    cause: &SessionMessageCause,
    target: &SessionMessageTarget,
    content: &str,
    title: Option<&str>,
    request_id: &str,
    retry_key: Option<String>,
    hop: u32,
    mut input: gents_protocol::request_input::RequestInput,
    signer: &crate::store::identity::ManagedAgentSigner,
) -> Result<gents_protocol::request_admission::AgentRequestCreate> {
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
    anyhow::ensure!(
        !cause.caller_request_id.trim().is_empty()
            && !cause.caller_request_doc_id.trim().is_empty()
            && !cause.tool_call_id.trim().is_empty()
            && !cause.tool_call_doc_id.trim().is_empty(),
        "session-message lineage requires the full calling request and tool call edge"
    );
    let prompt_selection = crate::documents::skills::prompt_slash_skill_selection(content);
    let admission = if target.node_did == cause.caller_node_did {
        AgentRequestAdmissionRecord::local_self(&cause.caller_agent_did)
    } else {
        AgentRequestAdmissionRecord::peer(&cause.caller_agent_did)
    };
    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let identity = RequestIdentity {
        requester_did: Some(cause.caller_agent_did.clone()),
        request_id: request_id.to_owned(),
        node_did: target.node_did.clone(),
        agent_did: target.agent_did.clone(),
        session_id: target.session_id.clone(),
        content: format!(
            "{}\n\n{}",
            session_message_sender_header(cause, &created_at),
            prompt_selection.prompt
        ),
        execution_origin: ExecutionOrigin::Interactive,
        created_at,
    };
    input.started_by = Some(gents_protocol::session::SessionReference {
        node_did: cause.caller_node_did.clone(),
        session_id: cause.caller_session_id.clone(),
        requester_did: cause.caller_requester_did.clone(),
    });
    input.selected_skill_ids = prompt_selection.selected_skill_ids;
    input.initial_title = title
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(|text| gents_protocol::session::SessionTitle {
            text: text.to_owned(),
            source: gents_protocol::session::SessionTitleSource::Task,
        });
    let spec = RequestSpec {
        trigger_lineage: TriggerLineage {
            correlation: cause.correlation.clone(),
            ..Default::default()
        },
        parent: Some(ParentLink {
            depth: hop,
            parent_request_id: cause.caller_request_id.clone(),
            parent_request_doc_id: cause.caller_request_doc_id.clone(),
            parent_tool_call_id: Some(cause.tool_call_id.clone()),
            parent_tool_call_doc_id: Some(cause.tool_call_doc_id.clone()),
        }),
        input,
        retry_key,
        ..RequestSpec::new(RequestPurpose::Normal, identity, admission)
    };
    anyhow::ensure!(
        signer.did() == cause.caller_agent_did,
        "session-message signer differs from caller"
    );
    build_signed_request(spec, RequestSigner::Identity(signer)).await
}

pub async fn activate_workspace_bound_request(
    node: &std::sync::Arc<EmbeddedNode>,
    request_doc_id: &str,
    actor: identity::Did,
) -> Result<()> {
    activate_workspace_bound_request_with_identity(node, request_doc_id, actor, None).await
}

pub async fn activate_workspace_bound_request_with_identity(
    node: &std::sync::Arc<EmbeddedNode>,
    request_doc_id: &str,
    actor: identity::Did,
    identity: Option<&dyn crate::store::identity::NodeIdentity>,
) -> Result<()> {
    let access = crate::ConfigAccess::local_with_optional_agent_signer(
        node.clone(),
        actor,
        identity.and_then(|identity| identity.managed_agent_signer()),
    )?;
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                docID: "{doc_id}", filter: {{
                    _docID: {{ _eq: "{doc_id}" }},
                    lifecycle_state: {{ _eq: "{workspace_binding_pending}" }}
                }},
                input: {{ lifecycle_state: "{pending}" }}
            ) {{ _docID }}
        }}"#,
        doc_id = escape_graphql_string(request_doc_id),
        workspace_binding_pending = RequestLifecycleState::WorkspaceBindingPending.as_str(),
        pending = RequestLifecycleState::Pending.as_str(),
    );
    let response = access
        .write("lifecycle.activate_workspace_bound", &mutation)
        .await?;
    let response: defra_node::QueryResponse = serde_json::from_value(response)?;
    if crate::store::graphql::single_mutation_document(&response, "update_AgentRequest")?.is_none()
    {
        let query = format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{
                request_id lifecycle_state workspace_id
            }} }}"#,
            doc_id = escape_graphql_string(request_doc_id),
        );
        let response = serde_json::from_value(access.execute(&query).await?)?;
        let row = crate::store::graphql::first_row::<gents_protocol::row::AgentRequestRow>(
            &response,
            "AgentRequest",
        )?;
        let activation_already_visible = row.is_some_and(|row| {
            row.workspace_id
                .as_deref()
                .is_some_and(|workspace_id| !workspace_id.trim().is_empty())
                && row.lifecycle_state != Some(RequestLifecycleState::WorkspaceBindingPending)
        });
        if activation_already_visible {
            return Ok(());
        }
        anyhow::bail!(
            "workspace-bound AgentRequest {request_doc_id} was not staged for activation"
        );
    }
    Ok(())
}

impl RequestLifecycle {
    pub fn set_execution_lease_duration(&mut self, duration: std::time::Duration) {
        assert_eq!(
            self.state,
            LocalLifecycleState::Pending,
            "execution lease duration must be configured before claim"
        );
        self.execution_lease_duration_secs = duration.as_secs().max(1);
    }

    pub fn new_with_node_did(
        node: Arc<EmbeddedNode>,
        agent_did: &str,
        node_did: &str,
        request: AgentRequest,
        deadline_duration_secs: u64,
    ) -> Self {
        Self::new_with_execution_binding(
            node,
            agent_did,
            node_did,
            request,
            deadline_duration_secs,
            ExecutionOrigin::Interactive,
            "",
        )
    }

    pub fn new_with_execution_binding(
        node: Arc<EmbeddedNode>,
        _agent_did: &str,
        _node_did: &str,
        request: AgentRequest,
        deadline_duration_secs: u64,
        execution_origin: ExecutionOrigin,
        backend_id: impl Into<String>,
    ) -> Self {
        let agent_did = request.agent_did.clone();
        Self {
            node,
            agent_did,
            agent_signer: None,
            execution_origin,
            backend_id: backend_id.into(),
            failure_reason: None,
            request,
            request_commit_cid: None,
            deadline_duration_secs,
            configured_max_total_tokens: None,
            claimed_deadline_at: None,
            background_completion_input_through_sequence: None,
            state: LocalLifecycleState::Pending,
            valid_until_at_claim: None,
            execution_lease: None,
            renewal_task: None,
            client_claim_admission: None,
            steering_admission: None,
            fold_admitted: Vec::new(),
            folded_selection: Vec::new(),
            natural_finish_allowed: false,
            execution_lease_duration_secs:
                crate::inference::resolved::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn materialize_claimed_with_execution_binding(
        node: Arc<EmbeddedNode>,
        agent_did: &str,
        identity: Arc<dyn crate::store::identity::NodeIdentity>,
        agent_identity: Arc<dyn crate::store::identity::NodeIdentity>,
        content: &str,
        deadline_duration_secs: u64,
        execution_origin: ExecutionOrigin,
        backend_id: impl Into<String>,
        trigger_lineage: TriggerLineage,
    ) -> Result<Self> {
        let mut lifecycle = Self::materialize_pending_with_execution_binding(
            node,
            agent_did,
            identity,
            content,
            deadline_duration_secs,
            execution_origin,
            backend_id,
            trigger_lineage,
            Some(agent_identity),
        )
        .await?;
        match lifecycle.claim_with_identity().await? {
            ClaimOutcome::Claimed => Ok(lifecycle),
            outcome => {
                anyhow::bail!("newly materialized signed AgentRequest was not claimed: {outcome:?}")
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn materialize_pending_with_execution_binding(
        node: Arc<EmbeddedNode>,
        agent_did: &str,
        identity: Arc<dyn crate::store::identity::NodeIdentity>,
        content: &str,
        deadline_duration_secs: u64,
        execution_origin: ExecutionOrigin,
        backend_id: impl Into<String>,
        trigger_lineage: TriggerLineage,
        managed_identity: Option<Arc<dyn crate::store::identity::NodeIdentity>>,
    ) -> Result<Self> {
        let node_did = identity.did().to_string();
        let backend_id = backend_id.into();
        let agent_did = agent_did.to_string();
        let request_id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4().to_string();
        validate_trigger_provenance(&trigger_lineage)?;
        let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let admission =
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(&agent_did);
        let request_identity = RequestIdentity {
            requester_did: None,
            request_id: request_id.clone(),
            node_did: node_did.clone(),
            agent_did: agent_did.clone(),
            session_id: session_id.clone(),
            content: content.to_string(),
            execution_origin,
            created_at: created_at.clone(),
        };
        let spec = RequestSpec {
            trigger_lineage,
            ..RequestSpec::new(
                gents_protocol::request_admission::RequestPurpose::Normal,
                request_identity,
                admission,
            )
        };
        let actor = identity::Did::new(agent_did.clone())?;
        let identity = managed_identity
            .as_ref()
            .context("request materialization requires a managed Agent identity")?;
        let access =
            crate::ConfigAccess::local_managed_agent(node.clone(), actor, identity.as_ref())?;
        let signer = RequestSigner::Identity(identity.as_ref());
        let create = super::client::build_signed_client_request(&access, spec, signer).await?;
        super::client::submit_client_request(&access, &create).await?;
        let escaped_request = escape_graphql_string(&request_id);
        let response = access.execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request}" }} }}, limit: 2) {{ _docID }} }}"#,
        )).await?;
        let [row] = response["data"]["AgentRequest"]
            .as_array()
            .context("published AgentRequest query omitted rows")?
            .as_slice()
        else {
            anyhow::bail!("published AgentRequest is missing or ambiguous");
        };
        let doc_id = row["_docID"]
            .as_str()
            .context("published AgentRequest omitted document identity")?
            .to_owned();
        let queued_request = AgentRequest {
            retry_parent_request_doc_id: create.retry_parent_request_doc_id.clone(),
            purpose: create.purpose,
            doc_id,
            request_id,
            node_did: node_did.clone(),
            requester_did: Some(agent_did.clone()),
            agent_did: agent_did.clone(),
            session_id,
            content: content.to_string(),
            max_total_tokens: None,
            input: create.input.clone(),
            execution_origin: Some(execution_origin.as_str().to_string()),
            created_at,
            deadline: None,
            execution_generation: None,
            execution_lease_expires_at: None,
            execution_lease_secs: None,
            request_hop: 0,
            caused_by_parent_request_id: None,
            caused_by_parent_request_doc_id: None,
            caused_by_parent_tool_call_id: None,
            caused_by_parent_tool_call_doc_id: None,
            caused_by_trigger_id: create.caused_by_trigger_id,
            caused_by_trigger_kind: create.caused_by_trigger_kind,
            caused_by_source_doc_id: create.caused_by_source_doc_id,
            caused_by_correlation: create.caused_by_correlation,
            caused_by_trigger_context: create.caused_by_trigger_context,
            workspace_id: None,
            workspace_owner_node_did: None,
            workspace_authority: None,
            workspace_seal_hash: None,
        };
        let request = crate::request::admission::verify_fresh_local_self_request(
            &node,
            identity.as_ref(),
            &queued_request,
            &agent_did,
        )
        .await?;
        let mut lifecycle = Self::new_with_execution_binding(
            node,
            &agent_did,
            &node_did,
            request,
            deadline_duration_secs,
            execution_origin,
            backend_id,
        );
        if let Some(identity) = managed_identity {
            lifecycle.bind_managed_agent(identity.as_ref())?;
        }
        Ok(lifecycle)
    }

    pub fn request(&self) -> &AgentRequest {
        &self.request
    }

    pub(crate) fn claimed_client_provenance(
        &self,
    ) -> Option<crate::request::admission::ClaimedClientProvenance> {
        if !matches!(
            self.state,
            LocalLifecycleState::Claimed | LocalLifecycleState::Streaming
        ) {
            return None;
        }
        self.client_claim_admission
            .as_ref()?
            .claimed_provenance(&self.request)
    }

    pub(crate) fn claimed_tool_context(
        &self,
    ) -> anyhow::Result<crate::inference::llm::tool::ToolRequestContext> {
        anyhow::ensure!(
            matches!(
                self.state,
                LocalLifecycleState::Claimed | LocalLifecycleState::Streaming
            ),
            "tool context requires a committed request claim"
        );
        let admitted_client = self.claimed_client_provenance().map(|provenance| {
            crate::inference::llm::tool::AdmittedClientContext {
                signer_did: provenance.signer_did,
                request_doc_id: provenance.request_doc_id,
                session_doc_id: provenance.session_doc_id,
                agent_doc_id: provenance.agent_doc_id,
            }
        });
        Ok(crate::inference::llm::tool::ToolRequestContext {
            request_id: self.request.request_id.clone(),
            request_doc_id: self.request.doc_id.clone(),
            node_did: self.request.node_did.clone(),
            agent_did: self.request.agent_did.clone(),
            session_id: self.request.session_id.clone(),
            caused_by_source_doc_id: self.request.caused_by_source_doc_id.clone(),
            admitted_client,
        })
    }

    pub(crate) fn execution_generation(&self) -> anyhow::Result<&str> {
        self.execution_lease
            .as_ref()
            .map(|lease| lease.generation.as_str())
            .ok_or_else(|| anyhow::anyhow!("request has no active execution generation"))
    }

    pub fn backend_id(&self) -> &str {
        &self.backend_id
    }

    pub fn agent_did(&self) -> &str {
        &self.agent_did
    }
}

pub(super) struct LabeledSessionRows {
    pub(super) decoded: Vec<gents_protocol::session::SessionOwnerRow>,
    pub(super) undecodable: Vec<anyhow::Error>,
}

async fn labeled_session_rows_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    request: &AgentRequest,
) -> Result<LabeledSessionRows> {
    let response = txn
        .execute(&format!(
            r#"{{ AgentSession(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            escape_graphql_string(&request.node_did),
            escape_graphql_string(&request.session_id),
            gents_protocol::session::AGENT_SESSION_FIELDS,
        ))
        .await?;
    let rows = response["data"]["AgentSession"]
        .as_array()
        .context("AgentSession query omitted rows")?;
    let mut decoded = Vec::new();
    let mut undecodable = Vec::new();
    for row in rows {
        match gents_protocol::session::decode_session_row(row) {
            Ok(row) => decoded.push(row),
            Err(error) => undecodable.push(error.into()),
        }
    }
    Ok(LabeledSessionRows {
        decoded,
        undecodable,
    })
}

/// The calling request and opening document recorded when a request creates a
/// session. Task delivery evidence is added in the same creation transaction.
/// `ensure_session_in_txn` preserves existing session provenance.
pub(crate) fn created_session_provenance(
    request: &AgentRequest,
) -> Option<gents_protocol::session::SessionProvenance> {
    let parent = request.caused_by_parent_request_doc_id.clone();
    let opening = request.caused_by_parent_tool_call_doc_id.is_some()
        || request.has_automated_trigger_lineage();
    if parent.is_none() && !opening {
        return None;
    }
    Some(gents_protocol::session::SessionProvenance {
        parent_request_doc_id: parent,
        started_by: request.input.started_by.clone(),
        opening_request_doc_id: opening.then(|| request.doc_id.clone()),
        ..Default::default()
    })
}

struct TaskFireEvidence<'a> {
    node: &'a str,
    session: &'a str,
    request: &'a str,
    task: &'a str,
    valid: bool,
}

fn task_for_opening<'a>(
    node: &str,
    session: &str,
    request: &str,
    fires: &'a [TaskFireEvidence<'a>],
) -> Option<&'a str> {
    let [fire] = fires else { return None };
    (fire.valid && fire.node == node && fire.session == session && fire.request == request)
        .then_some(fire.task)
}

pub(crate) async fn created_session_provenance_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    request: &AgentRequest,
) -> Result<Option<gents_protocol::session::SessionProvenance>> {
    let mut provenance = created_session_provenance(request);
    let kind = request.caused_by_trigger_kind.as_deref();
    if !matches!(kind, Some("manual" | "event" | "schedule")) {
        return Ok(provenance);
    }
    let query = format!(
        r#"{{ TriggerFire(filter: {{owner_did: {{_eq: "{}"}}, request_id: {{_eq: "{}"}}}}, limit: 2) {{fire_key owner_did request_id session_id trigger_id task_id source_collection source_doc_id}} }}"#,
        escape_graphql_string(&request.node_did),
        escape_graphql_string(&request.request_id),
    );
    let response = txn.execute(&query).await?;
    let rows = response
        .pointer("/data/TriggerFire")
        .and_then(serde_json::Value::as_array)
        .context("Task fire query omitted rows")?;
    let evidence: Vec<_> = rows
        .iter()
        .map(|row| {
            let node = row["owner_did"].as_str().unwrap_or_default();
            let session = row["session_id"].as_str().unwrap_or_default();
            let request_id = row["request_id"].as_str().unwrap_or_default();
            let trigger = row["trigger_id"].as_str().unwrap_or_default();
            let task = row["task_id"].as_str().unwrap_or_default();
            let kind_matches = match kind {
                Some("manual") => {
                    request.caused_by_trigger_id.is_none()
                        && row["source_collection"].as_str() == Some("Task")
                        && trigger.starts_with(&format!("manual:{task}:"))
                }
                Some("event") => {
                    request.caused_by_trigger_id.as_deref() == Some(trigger)
                        && request.caused_by_source_doc_id.as_deref()
                            == row["source_doc_id"].as_str()
                }
                Some("schedule") => request.caused_by_trigger_id.as_deref() == Some(trigger),
                _ => false,
            };
            TaskFireEvidence {
                node,
                session,
                request: request_id,
                task,
                valid: kind_matches
                    && gents_protocol::trigger_delivery::verified_task_fire(
                        row, node, session, request_id, trigger,
                    )
                    .is_some(),
            }
        })
        .collect();
    if let Some(task) = task_for_opening(
        &request.node_did,
        &request.session_id,
        &request.request_id,
        &evidence,
    ) {
        let origin = provenance.get_or_insert_with(Default::default);
        origin.task_id = Some(task.to_owned());
        origin.opening_request_doc_id = Some(request.doc_id.clone());
    }
    Ok(provenance)
}

pub(super) async fn apply_request_session_projection(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    request: &AgentRequest,
    now: &str,
) -> Result<()> {
    let rows = labeled_session_rows_in_txn(txn, request).await?;
    if let Some(error) = rows.undecodable.into_iter().next() {
        return Err(error
            .context("AgentSession row under the claimed session label failed canonical decode"));
    }
    if rows.decoded.len() > 1 {
        return Err(ClaimAdmissionError::SessionScopeMismatch {
            session_id: request.session_id.clone(),
            reason: "ambiguous session owner".to_owned(),
        }
        .into());
    }
    if let Some(existing) = rows.decoded.first() {
        if existing.session.requester_did != request.requester_did {
            return Err(ClaimAdmissionError::SessionScopeMismatch {
                session_id: request.session_id.clone(),
                reason: "the request's requester does not own the session".to_owned(),
            }
            .into());
        }
        if existing.session.agent_did != request.agent_did {
            return Err(ClaimAdmissionError::SessionAgentMismatch {
                session_id: request.session_id.clone(),
                existing_agent_did: existing.session.agent_did.clone(),
                requested_agent_did: request.agent_did.clone(),
            }
            .into());
        }
    }
    anyhow::ensure!(
        !rows.decoded.is_empty() || !is_client_request_in_txn(txn, request).await?,
        "Client request is missing its atomically created session"
    );
    let provenance = if rows.decoded.is_empty() {
        created_session_provenance_in_txn(txn, request).await?
    } else {
        created_session_provenance(request)
    };
    session::ensure_session_in_txn(
        txn,
        &request.session_id,
        &request.node_did,
        &request.agent_did,
        request.requester_did.as_deref(),
        request.input.initial_title.clone(),
        provenance,
        now,
    )
    .await?;
    let reopened = session::reopen_session_in_txn(
        txn,
        &request.session_id,
        &request.node_did,
        request.requester_did.as_deref(),
        now,
    )
    .await?;
    let owner = crate::store::config_client::agent_session::load_agent_session_row_in_txn(
        txn,
        &request.node_did,
        &request.session_id,
        request.requester_did.as_deref(),
    )
    .await?
    .context("claimed session disappeared")?;
    let facts = session::load_scoped_request_facts_in_txn(txn, &owner.session, false).await?;
    let incoming = facts
        .iter()
        .find(|fact| {
            fact.observed.request_doc_id == request.doc_id
                && fact.observed.request_id == request.request_id
        })
        .context("claimed request missing from its exact session scope")?;
    let preview = session::request_preview_in_txn(txn, request).await?;
    let advanced =
        session::advance_session_request_observation_in_txn(txn, incoming, preview, now).await?;
    if !reopened && !advanced {
        session::touch_session_in_txn(txn, &owner, now).await?;
    }
    Ok(())
}

pub(crate) async fn project_refused_request_session_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    request: &AgentRequest,
    now: &str,
) -> Result<()> {
    if request.purpose != gents_protocol::request_admission::RequestPurpose::Normal
        || request.node_did.trim().is_empty()
        || request.session_id.trim().is_empty()
        || request.agent_did.trim().is_empty()
    {
        return Ok(());
    }
    let rows = labeled_session_rows_in_txn(txn, request).await?;
    if !rows.undecodable.is_empty() {
        tracing::warn!(
            target: session::REFUSAL_PROJECTION_EVENT_TARGET,
            session_id = %request.session_id,
            request_id = %request.request_id,
            undecodable_rows = rows.undecodable.len(),
            "refused request leaves its session untouched: an AgentSession row under the label \
             failed canonical decode"
        );
        return Ok(());
    }
    if let Some(existing) = rows.decoded.first() {
        if rows.decoded.len() > 1
            || existing.session.requester_did != request.requester_did
            || existing.session.agent_did != request.agent_did
        {
            return Ok(());
        }
    }
    project_refused_request_onto_session_in_txn(txn, request, now, rows.decoded.is_empty()).await
}

async fn project_refused_request_onto_session_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    request: &AgentRequest,
    now: &str,
    new_session: bool,
) -> Result<()> {
    if new_session && is_client_request_in_txn(txn, request).await? {
        return Ok(());
    }
    let provenance = if new_session {
        created_session_provenance_in_txn(txn, request).await?
    } else {
        created_session_provenance(request)
    };
    session::ensure_session_in_txn(
        txn,
        &request.session_id,
        &request.node_did,
        &request.agent_did,
        request.requester_did.as_deref(),
        request.input.initial_title.clone(),
        provenance,
        now,
    )
    .await?;
    let owner = crate::store::config_client::agent_session::load_agent_session_row_in_txn(
        txn,
        &request.node_did,
        &request.session_id,
        request.requester_did.as_deref(),
    )
    .await?
    .context("refused session disappeared")?;
    let incoming: Result<_> = async {
        let facts = session::load_scoped_request_facts_in_txn(txn, &owner.session, false).await?;
        let incoming = facts
            .iter()
            .find(|fact| {
                fact.observed.request_doc_id == request.doc_id
                    && fact.observed.request_id == request.request_id
            })
            .cloned()
            .context("refused request missing from its exact session scope")?;
        let preview = session::request_preview_in_txn(txn, request).await?;
        Ok((incoming, preview))
    }
    .await;
    let (incoming, preview) = match incoming {
        Ok(incoming) => incoming,
        Err(error) => {
            tracing::warn!(
                target: session::REFUSAL_PROJECTION_EVENT_TARGET,
                session_id = %request.session_id,
                request_id = %request.request_id,
                error = %error,
                "refused request skipped its session projection; the refusal itself persists in \
                 this transaction"
            );
            return Ok(());
        }
    };
    session::advance_refused_request_observation_in_txn(txn, &incoming, preview, now).await?;
    Ok(())
}

#[cfg(test)]
mod session_message_tests {
    use super::*;
    use crate::request::lifecycle::test_support::{pin_fixed_signing_identity, PIN_FIXED_DID};

    #[tokio::test]
    async fn session_message_request_carries_the_cwd() {
        let tempdir = tempfile::tempdir().unwrap();
        let identity = pin_fixed_signing_identity(tempdir.path());
        let binding = crate::store::identity::provision_agent_identity(&identity).unwrap();
        let agent = crate::store::identity::resolve_agent_identity(&identity, &binding).unwrap();
        let signer = agent.managed_agent_signer().unwrap();
        let cause = SessionMessageCause {
            caller_node_did: PIN_FIXED_DID.to_owned(),
            caller_agent_did: binding.agent_did.clone(),
            caller_session_id: "caller-session".to_owned(),
            caller_requester_did: None,
            caller_request_id: "caller-request".to_owned(),
            caller_request_doc_id: "caller-request-doc".to_owned(),
            caller_hop: 0,
            tool_call_id: "tool-call".to_owned(),
            tool_call_doc_id: "tool-call-doc".to_owned(),
            correlation: None,
        };
        let target = SessionMessageTarget {
            node_did: PIN_FIXED_DID.to_owned(),
            agent_did: "agent-1".to_owned(),
            session_id: "session-1".to_owned(),
        };
        let build = |cwd: Option<String>| {
            build_session_message_request(
                &cause, &target, "hello", None, "req-cwd", None, 1, None, cwd, &signer,
            )
        };
        let carried = build(Some("/work/tree".to_owned())).await.unwrap();
        assert_eq!(carried.input.cwd.as_deref(), Some("/work/tree"));
        let source = carried.input.started_by.as_ref().unwrap();
        assert_eq!(source.node_did, cause.caller_node_did);
        assert_eq!(source.session_id, cause.caller_session_id);
        assert_eq!(source.requester_did, cause.caller_requester_did);
        assert_eq!(
            carried.input.reissued("physical-source").started_by,
            carried.input.started_by
        );
        assert!(carried.input.for_fresh_session().started_by.is_none());
        let unset = build(None).await.unwrap();
        assert_eq!(unset.input.cwd, None);
    }

    #[tokio::test]
    async fn session_message_content_names_its_sender_after_skill_selection() {
        let tempdir = tempfile::tempdir().unwrap();
        let identity = pin_fixed_signing_identity(tempdir.path());
        let binding = crate::store::identity::provision_agent_identity(&identity).unwrap();
        let agent = crate::store::identity::resolve_agent_identity(&identity, &binding).unwrap();
        let signer = agent.managed_agent_signer().unwrap();
        let cause = SessionMessageCause {
            caller_node_did: PIN_FIXED_DID.to_owned(),
            caller_agent_did: binding.agent_did.clone(),
            caller_session_id: "sender-s2".to_owned(),
            caller_requester_did: None,
            caller_request_id: "caller-request".to_owned(),
            caller_request_doc_id: "caller-request-doc".to_owned(),
            caller_hop: 0,
            tool_call_id: "tool-call".to_owned(),
            tool_call_doc_id: "tool-call-doc".to_owned(),
            correlation: None,
        };
        let target = SessionMessageTarget {
            node_did: PIN_FIXED_DID.to_owned(),
            agent_did: "agent-1".to_owned(),
            session_id: "session-1".to_owned(),
        };
        let create = build_session_message_request(
            &cause,
            &target,
            "/triage\nlook at it",
            None,
            "req-sender",
            None,
            1,
            None,
            None,
            &signer,
        )
        .await
        .unwrap();
        assert_eq!(create.input.selected_skill_ids, ["triage"]);
        assert_eq!(
            create.content,
            format!(
                "[message agent:{} session:sender-s2 node:did:key:z6Mkmuzz..rco7 · at:{} \
                 · cause:caller-request]\n\nlook at it",
                binding.agent_did, create.created_at
            )
        );
    }
}

pub fn task_fire_key(identity: &gents_protocol::trigger_delivery::FireIdentity) -> String {
    crate::triggers::engine::durable::fire_key(identity)
}

pub struct TaskDeliveryAdmission {
    pub request: EnqueuedAgentRequest,
    pub duplicate: bool,
}

pub async fn write_task_delivery(
    access: &crate::store::config_client::ConfigAccess,
    fire: &gents_protocol::trigger_delivery::TriggerFire,
    continue_existing: bool,
    create: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<TaskDeliveryAdmission> {
    access
        .transact_write("lifecycle.admit_task_delivery", |txn| {
            Box::pin(async move { stage_task_delivery(txn, fire, continue_existing, create).await })
        })
        .await
}

pub async fn write_task_delivery_local(
    node: &std::sync::Arc<EmbeddedNode>,
    actor: ::identity::Did,
    fire: &gents_protocol::trigger_delivery::TriggerFire,
    continue_existing: bool,
    create: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<TaskDeliveryAdmission> {
    let access = crate::store::config_client::ConfigAccess::local_as(node.clone(), actor);
    write_task_delivery(&access, fire, continue_existing, create).await
}

pub(crate) async fn write_trigger_delivery_with_identity(
    node: &std::sync::Arc<EmbeddedNode>,
    actor: ::identity::Did,
    prepared: &crate::triggers::engine::durable::PreparedFire,
    create: &gents_protocol::request_admission::AgentRequestCreate,
    identity: Option<&dyn crate::store::identity::NodeIdentity>,
) -> Result<TaskDeliveryAdmission> {
    let identity = identity.context("trigger delivery requires a managed Agent identity")?;
    let access = crate::ConfigAccess::local_managed_agent(node.clone(), actor, identity)?;
    access
        .transact_write("lifecycle.admit_trigger_delivery", |txn| {
            Box::pin(async move {
                let fresh =
                    crate::triggers::engine::durable::stage_fire_receipt(txn, &prepared.receipt)
                        .await?;
                let mut create = create.clone();
                if fresh {
                    if prepared.target_existing && prepared.receipt.goal_id.is_none() {
                        create.input = crate::goal::prepare_goal_input_in_txn(
                            txn,
                            &create.node_did,
                            &create.session_id,
                            &create.agent_did,
                            Some(&create.requester_did),
                            create.input,
                        )
                        .await?;
                    }
                    sign_request(&mut create, RequestSigner::Identity(identity)).await?;
                }
                let admitted = stage_task_delivery_with_receipt(
                    txn,
                    &prepared.receipt,
                    prepared.target_existing,
                    &create,
                    fresh,
                    prepared.event_source_generation.as_ref(),
                )
                .await?;
                if prepared.replace_prior && !admitted.duplicate {
                    supersede_trigger_requests_in_txn(
                        txn,
                        &prepared.receipt.identity.owner_did,
                        &prepared.receipt.identity.trigger_id,
                        Some(&prepared.receipt.request_id),
                    )
                    .await?;
                }
                Ok::<_, anyhow::Error>(admitted)
            })
        })
        .await
}

#[doc(hidden)]
pub async fn supersede_trigger_requests_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    trigger_id: &str,
    excluded_request_id: Option<&str>,
) -> Result<usize> {
    let owner_gql = escape_graphql_string(owner);
    let trigger_gql = escape_graphql_string(trigger_id);
    let excluded = excluded_request_id
        .map(|id| {
            format!(
                ", request_id: {{ _neq: \"{}\" }}",
                escape_graphql_string(id),
            )
        })
        .unwrap_or_default();
    let now = chrono::Utc::now().to_rfc3339();
    let unclaimed = gents_protocol::request_lifecycle::RequestLifecycleState::graphql_list([
        gents_protocol::request_lifecycle::RequestLifecycleState::Pending,
        gents_protocol::request_lifecycle::RequestLifecycleState::WorkspaceBindingPending,
    ]);
    let response = txn.execute(&format!(r#"mutation {{ update_AgentRequest(
        filter: {{ node_did: {{ _eq: "{owner_gql}" }}, caused_by_trigger_id: {{ _eq: "{trigger_gql}" }}{excluded}, lifecycle_state: {{ _in: {unclaimed} }} }},
        input: {{ lifecycle_state: "{superseded}", terminalized_at: "{}", terminal_redrive_attempts: 0 }}
    ) {{ _docID request_id }} }}"#, escape_graphql_string(&now), superseded = RequestLifecycleState::Superseded.as_str())).await?;
    let pending = response["data"]["update_AgentRequest"]
        .as_array()
        .context("supersession omitted pending requests")?;
    for row in pending {
        crate::triggers::engine::durable::publish_request_outcome(
            txn,
            owner,
            row["request_id"]
                .as_str()
                .context("superseded request lacks ID")?,
            RequestLifecycleState::Superseded,
            "newer trigger fire",
            &now,
        )
        .await?;
    }
    let active = gents_protocol::request_lifecycle::RequestLifecycleState::graphql_list([
        gents_protocol::request_lifecycle::RequestLifecycleState::Claimed,
        gents_protocol::request_lifecycle::RequestLifecycleState::Processing,
    ]);
    let response = txn.execute(&format!(r#"{{ AgentRequest(filter: {{
        node_did: {{ _eq: "{owner_gql}" }}, caused_by_trigger_id: {{ _eq: "{trigger_gql}" }}{excluded},
        lifecycle_state: {{ _in: {active} }}
    }}) {{ _docID request_id node_did requester_did agent_did session_id lifecycle_state
        execution_generation execution_lease_expires_at }} }}"#)).await?;
    let active: Vec<gents_protocol::row::AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone())?;
    for row in &active {
        anyhow::ensure!(
            super::execution_lease::stage_revoke_execution_preserving_output(
                txn,
                row,
                RequestTerminalOutcome::Superseded,
                "superseded by a newer trigger fire",
            )
            .await?
                == TerminalizeResult::Won,
            "execution changed during atomic trigger supersession",
        );
    }
    Ok(pending.len() + active.len())
}

async fn stage_task_delivery(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    fire: &gents_protocol::trigger_delivery::TriggerFire,
    continue_existing: bool,
    create: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<TaskDeliveryAdmission> {
    let fresh = crate::triggers::engine::durable::stage_fire_receipt(txn, fire).await?;
    stage_task_delivery_with_receipt(txn, fire, continue_existing, create, fresh, None).await
}

async fn stage_task_delivery_with_receipt(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    fire: &gents_protocol::trigger_delivery::TriggerFire,
    continue_existing: bool,
    create: &gents_protocol::request_admission::AgentRequestCreate,
    fresh: bool,
    expected_event_source: Option<&serde_json::Value>,
) -> Result<TaskDeliveryAdmission> {
    if fresh {
        if create.caused_by_trigger_kind.as_deref() == Some("event") {
            anyhow::ensure!(
                create.caused_by_trigger_id.as_deref() == Some(fire.identity.trigger_id.as_str()),
                "event request and receipt trigger identity disagree"
            );
            let generation = expected_event_source
                .context("event source generation is required for admission")?;
            crate::documents::event_consumer::validate_event_admission(
                txn, fire, generation,
            )
            .await?;
        }
        anyhow::ensure!(
            create.node_did == fire.identity.owner_did
                && create.request_id == fire.request_id
                && create.session_id == fire.session_id,
            "Task receipt and signed request identity disagree"
        );
        anyhow::ensure!(
            !fire.goal_assignment_applied,
            "a Task admission cannot apply its Goal assignment"
        );
        anyhow::ensure!(
            !fire.emit_outcome
                || fire
                    .source_handoff_id
                    .as_deref()
                    .is_some_and(|id| !id.trim().is_empty()),
            "outcome-enabled Task requires its source handoff identity"
        );
        match &fire.goal_id {
            Some(goal_id) => {
                crate::goal::validate_task_goal_identity_in_txn(
                    txn,
                    &create.node_did,
                    &create.session_id,
                    goal_id,
                    create.input.goal_assignment.as_ref(),
                )
                .await?;
                anyhow::ensure!(
                    fire.goal_objective
                        .as_deref()
                        .is_some_and(|objective| !objective.trim().is_empty()),
                    "Goal-backed Task requires an objective"
                );
                anyhow::ensure!(
                    fire.goal_token_budget.is_none_or(|budget| budget > 0),
                    "Task Goal budget must be positive"
                );
            }
            None => anyhow::ensure!(
                fire.goal_objective.is_none() && fire.goal_token_budget.is_none(),
                "ordinary Task receipt cannot declare a Goal assignment"
            ),
        }
        let manual_label = create.caused_by_trigger_kind.as_deref() == Some("manual")
            && create.caused_by_trigger_id.is_none()
            && fire.identity.source_collection == "Task"
            && fire
                .identity
                .trigger_id
                .starts_with(&format!("manual:{}:", fire.task_id));
        let session = crate::store::config_client::agent_session::load_agent_session_row_in_txn(
            txn,
            &create.node_did,
            &create.session_id,
            Some(create.requester_did.as_str()),
        )
        .await?;
        if continue_existing {
            anyhow::ensure!(
                crate::triggers::engine::durable::resolve_session_id(
                    &fire.identity,
                    Some(&create.session_id),
                    session.is_some()
                )
                .is_some(),
                "Task target session is missing or belongs to another owner"
            );
        } else {
            anyhow::ensure!(
                manual_label
                    || crate::triggers::engine::durable::resolve_session_id(
                        &fire.identity,
                        None,
                        false
                    )
                    .as_deref()
                        == Some(fire.session_id.as_str()),
                "new Task session must derive from its fire identity"
            );
        }
        if let Some(session) = session {
            anyhow::ensure!(
                session.session.agent_did == create.agent_did,
                "Task target session has a different agent"
            );
            anyhow::ensure!(
                session.session.closed_at.is_none(),
                "Task target session is closed"
            );
        }
        if create.admission.kind
            == gents_protocol::request_admission::AgentRequestAdmissionKind::Client
        {
            let mut resolved = create.clone();
            super::bind_client_targets_in_txn(txn, &mut resolved).await?;
            anyhow::ensure!(
                create.session_doc_id == resolved.session_doc_id
                    && create.agent_doc_id == resolved.agent_doc_id,
                "Task request physical targets changed before publication"
            );
            refuse_retired_reserved_cause(create)?;
            super::client::stage_client_request_in_txn(
                txn,
                create,
                create
                    .session_doc_id
                    .as_deref()
                    .context("client Task session is unavailable")?,
                create
                    .agent_doc_id
                    .as_deref()
                    .context("client Task agent is unavailable")?,
            )
            .await?;
        } else {
            refuse_retired_reserved_cause(create)?;
            txn.execute(&create.graphql_mutation().map_err(anyhow::Error::msg)?)
                .await?;
        }
    }
    let query = format!("{{ AgentRequest(filter: {{node_did: {{_eq: \"{}\"}}, request_id: {{_eq: \"{}\"}}}}) {{ {} }} }}",
        escape_graphql_string(&fire.identity.owner_did), escape_graphql_string(&fire.request_id),
        gents_protocol::row::AGENT_REQUEST_FIELDS);
    let response = txn.execute(&query).await?;
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(serde_json::Value::as_array)
        .context("admitted request query omitted rows")?;
    anyhow::ensure!(
        rows.len() == 1,
        "fire receipt requires exactly one admitted request"
    );
    let session_id = rows[0]["session_id"]
        .as_str()
        .context("admitted request lacks session ID")?;
    anyhow::ensure!(
        !fresh || session_id == fire.session_id,
        "admitted request changed its fire's session"
    );
    if fresh {
        let request = AgentRequest::try_from(serde_json::from_value::<
            gents_protocol::row::AgentRequestRow,
        >(rows[0].clone())?)?;
        let provenance = created_session_provenance_in_txn(txn, &request).await?;
        let created = session::ensure_session_in_txn(
            txn,
            &request.session_id,
            &request.node_did,
            &request.agent_did,
            request.requester_did.as_deref(),
            request.input.initial_title.clone(),
            provenance,
            &request.created_at,
        )
        .await?;
        let agent_owned = matches!(
            create.admission.kind,
            gents_protocol::request_admission::AgentRequestAdmissionKind::LocalSelf
                | gents_protocol::request_admission::AgentRequestAdmissionKind::RuntimeInternal
        );
        if created || agent_owned {
            let session = crate::store::config_client::agent_session::load_agent_session_row_in_txn(
                txn,
                &request.node_did,
                &request.session_id,
                request.requester_did.as_deref(),
            )
            .await?
            .context("Task session disappeared")?;
            let (agent, _) = crate::store::config_client::read_desired_state_record_in_txn(
                txn,
                crate::Collection::Agent,
                &request.node_did,
                &request.agent_did,
            )
            .await?
            .context("selected Task agent is unavailable")?;
            if agent_owned {
                anyhow::ensure!(
                    txn.actor_did() == Some(request.agent_did.as_str())
                        && create.admission.signer_did == request.agent_did
                        && request.doc_id == rows[0]["_docID"].as_str().unwrap_or_default()
                        && request.node_did == create.node_did
                        && request.agent_did == create.agent_did
                        && request.session_id == create.session_id
                        && request.request_id == create.request_id
                        && request.requester_did.as_deref() == Some(create.requester_did.as_str())
                        && session.session.agent_did == request.agent_did,
                    "Task request publication differs from its authenticated Agent or session"
                );
                anyhow::ensure!(
                    crate::store::identity::verify_did_signature(
                        &create.admission.signer_did,
                        &create.signing_payload(),
                        &create.admission.signature,
                    )?,
                    "Task request publication requires its valid signed receipt"
                );
                txn.add_resource_relationship(
                    "AgentRequest",
                    &request.doc_id,
                    "parent",
                    "agents",
                    &agent,
                )
                .await?;
                txn.add_resource_relationship(
                    "AgentRequest",
                    &request.doc_id,
                    "session",
                    "sessions",
                    &session.doc_id,
                )
                .await?;
                txn.add_actor_relationship(
                    "AgentRequest",
                    &request.doc_id,
                    "working_agent",
                    &identity::Did::new(&request.agent_did)?,
                )
                .await?;
            }
            if created {
                session::stage_created_session_relationships_in_txn(
                    txn,
                    &session.doc_id,
                    &agent,
                    &identity::Did::new(&request.agent_did)?,
                )
                .await?;
            }
        }
        let session = crate::store::config_client::agent_session::load_agent_session_row_in_txn(
            txn,
            &request.node_did,
            &request.session_id,
            request.requester_did.as_deref(),
        )
        .await?
        .context("Task receipt session is unavailable")?;
        let receipt = txn
            .execute(&format!(
                "{{ TriggerFire(filter: {{fire_key: {{_eq: \"{}\"}}}}, limit: 2) {{_docID}} }}",
                escape_graphql_string(&fire.fire_key),
            ))
            .await?;
        let rows = receipt["data"]["TriggerFire"]
            .as_array()
            .context("Task receipt query omitted rows")?;
        let [receipt] = rows.as_slice() else {
            anyhow::bail!("Task receipt is missing or ambiguous")
        };
        let doc_id = receipt["_docID"]
            .as_str()
            .context("Task receipt lacks physical ID")?;
        crate::triggers::engine::durable::stage_session_child_relationships(
            txn,
            "TriggerFire",
            doc_id,
            &session.doc_id,
            &request.agent_did,
        )
        .await?;
    }
    Ok(TaskDeliveryAdmission {
        duplicate: !fresh,
        request: EnqueuedAgentRequest {
            doc_id: rows[0]["_docID"]
                .as_str()
                .context("admitted request lacks document ID")?
                .into(),
            request_id: fire.request_id.clone(),
            session_id: session_id.into(),
        },
    })
}

#[cfg(test)]
mod task_delivery_tests {
    use super::*;
    use crate::request::lifecycle::test_support::{pin_fixed_signing_identity, PIN_FIXED_DID};

    #[tokio::test]
    async fn pending_receipt_lookup_uses_the_selected_agent() {
        use crate::tools::call::lifecycle::admission_fixture::{
            published_admission_with_owner, PublishedAdmissionOptions,
        };

        let (admission, parent) = published_admission_with_owner(PublishedAdmissionOptions {
            name: "pending-receipt-agent".into(),
            ..Default::default()
        })
        .await
        .unwrap();
        let access = admission.tool.access().unwrap();
        let receipt = defra_node::QueryResponse::success(serde_json::json!({
            "create_AgentRequest": [{}]
        }));
        let escaped = escape_graphql_string(&parent.request().request_id);
        let resolved = resolve_created_agent_request_doc_id(
            &access,
            &receipt,
            "create_AgentRequest",
            &escaped,
            "lookup failed",
            "missing receipt",
        )
        .await
        .unwrap();
        assert_eq!(resolved, parent.request().doc_id);
        assert!(resolve_created_agent_request_doc_id(
            &crate::ConfigAccess::local(admission.node.clone()),
            &receipt,
            "create_AgentRequest",
            &escaped,
            "lookup failed",
            "missing receipt",
        )
        .await
        .is_err());
        admission.node.shutdown().await.unwrap();
    }

    #[test]
    fn task_provenance_matches_generated_opening_cases() {
        let cases = &crate::lean_vocab_test::lean_contract_snapshot()
            .session_document_cases
            .projection;
        let cases: Vec<_> = cases
            .iter()
            .filter(|case| case["operation"] == "task_opening")
            .collect();
        assert!(!cases.is_empty());
        for case in cases {
            let values: Vec<_> = case["fires"]
                .as_array()
                .unwrap()
                .iter()
                .map(|fire| {
                    (
                        fire["node"].as_u64().unwrap().to_string(),
                        fire["session"].as_u64().unwrap().to_string(),
                        fire["request"].as_u64().unwrap().to_string(),
                        fire["task"].as_u64().unwrap().to_string(),
                        fire["valid"].as_bool().unwrap(),
                    )
                })
                .collect();
            let fires: Vec<_> = values
                .iter()
                .map(|(node, session, request, task, valid)| TaskFireEvidence {
                    node,
                    session,
                    request,
                    task,
                    valid: *valid,
                })
                .collect();
            let actual =
                task_for_opening("1", "10", "20", &fires).map(|task| task.parse::<u64>().unwrap());
            assert_eq!(actual, case["task"].as_u64(), "{}", case["name"]);
        }
    }

    #[test]
    fn task_fire_requires_its_canonical_identity_and_exact_session() {
        let identity = gents_protocol::trigger_delivery::FireIdentity {
            owner_did: "node".into(),
            trigger_id: "manual:task:invocation".into(),
            source_collection: "Task".into(),
            source_doc_id: "invocation".into(),
        };
        let request = identity.request_id();
        let mut fire = serde_json::json!({
            "fire_key": identity.fire_key(), "trigger_id": identity.trigger_id,
            "source_collection": identity.source_collection, "source_doc_id": identity.source_doc_id,
            "session_id": "session", "task_id": "task"
        });
        assert_eq!(
            gents_protocol::trigger_delivery::verified_task_fire(
                &fire,
                "node",
                "session",
                &request,
                "manual:task:invocation"
            )
            .map(|(_, task)| task),
            Some("task")
        );
        assert!(gents_protocol::trigger_delivery::verified_task_fire(
            &fire,
            "node",
            "other",
            &request,
            "manual:task:invocation"
        )
        .is_none());
        fire["fire_key"] = serde_json::json!("tampered");
        assert!(gents_protocol::trigger_delivery::verified_task_fire(
            &fire,
            "node",
            "session",
            &request,
            "manual:task:invocation"
        )
        .is_none());
    }

    #[tokio::test]
    async fn task_admission_is_atomic_idempotent_and_defers_goal_assignment() {
        use crate::store::identity::NodeIdentity;
        let identity_dir = tempfile::tempdir().unwrap();
        let runtime = pin_fixed_signing_identity(identity_dir.path());
        let user =
            crate::KeyIdentity::load_or_create(identity_dir.path().join("user.key"), None).unwrap();
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(PIN_FIXED_DID)
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let binding = crate::store::identity::provision_agent_identity(&runtime).unwrap();
        let agent = crate::store::identity::resolve_agent_identity(&runtime, &binding).unwrap();
        let user_access = crate::ConfigAccess::local_as(node.clone(), user.did().parse().unwrap());
        let setup_access = crate::ConfigAccess::local(node.clone());
        let (node_doc, agent_doc) = setup_access.transact_write("test.task.agent", |txn| {
            let binding = &binding;
            Box::pin(async move {
                let created = txn.execute_with_variables(
                    "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
                    &serde_json::json!({"input": {"node_did": PIN_FIXED_DID}}),
                ).await?;
                let node_doc = crate::store::graphql::created_doc_id(&created, "Node")?;
                let created = txn.execute_with_variables(
                    "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                    &serde_json::json!({"input": {"node_did": PIN_FIXED_DID,
                        "agent_did": binding.agent_did, "creation_nonce": binding.creation_nonce,
                        "inference_profile_id": "test-profile"}}),
                ).await?;
                let agent_doc = crate::store::graphql::created_doc_id(&created, "Agent")?;
                Ok::<_, anyhow::Error>((node_doc, agent_doc))
            })
        }).await.unwrap();
        // Relationship writes need a signed actor; the node root owns the Agent it created.
        let node_actor = crate::ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(runtime.did()).unwrap(),
        );
        node_actor
            .add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node_doc)
            .await
            .unwrap();
        node_actor
            .add_actor_relationship(
                "Agent",
                &agent_doc,
                "executor",
                &identity::Did::new(&binding.agent_did).unwrap(),
            )
            .await
            .unwrap();
        node_actor
            .add_actor_relationship(
                "Agent",
                &agent_doc,
                "administrator",
                &identity::Did::new(user.did()).unwrap(),
            )
            .await
            .unwrap();
        let access = crate::ConfigAccess::local_managed_agent(
            node.clone(),
            agent.did().parse().unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        let identity = gents_protocol::trigger_delivery::FireIdentity {
            owner_did: PIN_FIXED_DID.into(),
            trigger_id: "manual:task:invocation-key".into(),
            source_collection: "Task".into(),
            source_doc_id: "invocation-key".into(),
        };
        let key = task_fire_key(&identity);
        let session_id = format!("trigger-session:{key}");
        let fire = gents_protocol::trigger_delivery::TriggerFire {
            fire_key: key.clone(),
            identity,
            task_id: "task".into(),
            request_id: format!("trigger-request:{key}"),
            session_id: session_id.clone(),
            goal_id: Some(gents_protocol::goal::deterministic_goal_id(
                PIN_FIXED_DID,
                &session_id,
            )),
            goal_objective: Some("complete assignment".into()),
            goal_token_budget: Some(100),
            goal_assignment_applied: false,
            emit_outcome: true,
            queued_serial: false,
            source_handoff_id: Some("invocation-key".into()),
            reply_session_id: None,
            shard_id: None,
            attempt: None,
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        let spec = pending_agent_request_spec(
            PIN_FIXED_DID,
            agent.did(),
            "complete assignment",
            ExecutionOrigin::Interactive,
            TriggerLineage {
                trigger_id: None,
                trigger_kind: Some("manual".into()),
                source_doc_id: None,
                correlation: None,
                trigger_context: None,
            },
            Some("task"),
            None,
            &fire.request_id,
            &fire.session_id,
            Some(&fire.fire_key),
            None,
            None,
        )
        .unwrap();
        let create = build_signed_request(spec, RequestSigner::Identity(agent.as_ref()))
            .await
            .unwrap();
        assert!(write_task_delivery(&access, &fire, true, &create)
            .await
            .is_err());
        let before = access
            .execute("{ TriggerFire { fire_key } AgentRequest { request_id } AgentSession { session_id } }")
            .await
            .unwrap();
        assert_eq!(before["data"]["TriggerFire"].as_array().unwrap().len(), 0);
        assert_eq!(before["data"]["AgentRequest"].as_array().unwrap().len(), 0);
        assert_eq!(before["data"]["AgentSession"].as_array().unwrap().len(), 0);
        let cases = &crate::lean_vocab_test::lean_contract_snapshot()
            .identity_task_session_publication_cases;
        let modeled = |name: &str| cases.iter().find(|case| case["name"] == name).unwrap();
        let aborted: anyhow::Result<()> = access
            .transact_write("test.task.abort_after_session_relationships", |txn| {
                let fire = &fire;
                let create = &create;
                Box::pin(async move {
                    stage_task_delivery(txn, fire, false, create).await?;
                    anyhow::bail!("forced abort after staged Task session publication")
                })
            })
            .await;
        assert!(aborted
            .unwrap_err()
            .to_string()
            .contains("forced abort after staged Task session publication"));
        let before = access.execute("{ TriggerFire { fire_key } AgentRequest { request_id } AgentSession { session_id } }").await.unwrap();
        assert_eq!(
            !before["data"]["AgentRequest"]
                .as_array()
                .unwrap()
                .is_empty(),
            modeled("aborted")["request"]
        );
        assert_eq!(
            !before["data"]["AgentSession"]
                .as_array()
                .unwrap()
                .is_empty(),
            modeled("aborted")["session"]
        );
        assert!(before["data"]["TriggerFire"].as_array().unwrap().is_empty());
        let first = write_task_delivery(&access, &fire, false, &create)
            .await
            .unwrap();
        let session_before_retry = access
            .execute("{ AgentSession { _docID relationships_pending } }")
            .await
            .unwrap();
        let session_doc = session_before_retry["data"]["AgentSession"][0]["_docID"]
            .as_str()
            .unwrap();
        assert_eq!(
            session_before_retry["data"]["AgentSession"][0]["relationships_pending"],
            false
        );
        let removed = node
            .delete_dac_actor_relationship(
                agent.did(),
                "AgentSession",
                session_doc,
                "working_agent",
                agent.did(),
            )
            .await
            .unwrap();
        assert_eq!(removed, modeled("created")["working_agent"]);
        let retry = write_task_delivery(&access, &fire, false, &create)
            .await
            .unwrap();
        let restored = node
            .delete_dac_actor_relationship(
                agent.did(),
                "AgentSession",
                session_doc,
                "working_agent",
                agent.did(),
            )
            .await
            .unwrap();
        assert_eq!(
            restored,
            modeled("duplicate-after-revocation")["working_agent"]
        );
        assert_eq!(first.request.doc_id, retry.request.doc_id);
        assert!(!first.duplicate);
        assert!(retry.duplicate);
        let after = access.execute("{ TriggerFire { fire_key goal_assignment_applied } AgentRequest { request_id lifecycle_state } Goal { goal_id } }").await.unwrap();
        assert_eq!(after["data"]["TriggerFire"].as_array().unwrap().len(), 1);
        assert_eq!(
            after["data"]["TriggerFire"][0]["goal_assignment_applied"],
            false
        );
        assert_eq!(after["data"]["AgentRequest"].as_array().unwrap().len(), 1);
        assert_eq!(
            after["data"]["AgentRequest"][0]["lifecycle_state"],
            "pending"
        );
        assert_eq!(after["data"]["Goal"].as_array().unwrap().len(), 0);
        let session = access
            .execute("{ AgentSession { _docID session_id title provenance } }")
            .await
            .unwrap();
        assert_eq!(session["data"]["AgentSession"].as_array().unwrap().len(), 1);
        let session = &session["data"]["AgentSession"][0];
        let modeled = crate::lean_vocab_test::lean_contract_snapshot()
            .session_document_cases
            .projection
            .iter()
            .find(|case| {
                case["operation"] == "atomic_admission"
                    && case["name"] == "agent_unattended_creation"
            })
            .expect("modeled unattended admission");
        assert_eq!(session["session_id"], session_id);
        assert_eq!(session["title"], modeled["after"]["session"]["title"]);
        assert_eq!(session["provenance"]["task_id"], "task");
        assert_eq!(
            session["provenance"]["opening_request_doc_id"],
            first.request.doc_id
        );
        let owner = access
            .transact_readonly("test.task.session_owner", |txn| {
                Box::pin(async move {
                    txn.document_owners(
                        "AgentSession",
                        &[session["_docID"].as_str().unwrap().to_owned()],
                    )
                    .await
                })
            })
            .await
            .unwrap();
        assert_eq!(
            owner[0].owner.as_ref().map(identity::Did::as_str),
            Some(agent.did())
        );
        let user_sessions = user_access
            .execute("{ AgentSession { _docID relationships_pending } }")
            .await
            .unwrap();
        assert_eq!(
            user_sessions["data"]["AgentSession"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            user_sessions["data"]["AgentSession"][0]["_docID"],
            session["_docID"]
        );
        assert_eq!(
            user_sessions["data"]["AgentSession"][0]["relationships_pending"],
            false
        );
        let household = crate::lean_vocab_test::lean_identity_household_cases()
            .first()
            .expect("modeled household");
        let modeled_user = &household
            .objects
            .iter()
            .find(|object| object.resource == "nodes" && object.id == "home")
            .unwrap()
            .owner;
        let modeled_agent = &household
            .objects
            .iter()
            .find(|object| {
                object.resource == "delivered_requests" && object.id == "automated-request"
            })
            .unwrap()
            .owner;
        let modeled_outsider = household.enrolled.last().expect("modeled outsider");
        let expected_read = |actor: &str| {
            household
                .actions
                .iter()
                .find_map(|action| match action {
                    crate::lean_vocab_test::LeanIdentityAction::Check {
                        actor: checked,
                        resource,
                        id,
                        permission,
                        expected,
                    } if checked == actor
                        && resource == "delivered_requests"
                        && id == "automated-request"
                        && permission == "read" =>
                    {
                        Some(*expected)
                    }
                    _ => None,
                })
                .expect("modeled automated request read")
        };
        let outsider =
            crate::KeyIdentity::load_or_create(identity_dir.path().join("outsider.key"), None)
                .unwrap();
        let outsider_access =
            crate::ConfigAccess::local_as(node.clone(), outsider.did().parse().unwrap());
        let request_query = format!("{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{}\" }} }}) {{ _docID lifecycle_state }} }}", escape_graphql_string(&first.request.doc_id));
        for (reader, modeled_actor) in [
            (&user_access, modeled_user.as_str()),
            (&access, modeled_agent.as_str()),
            (&outsider_access, modeled_outsider.as_str()),
        ] {
            let response = reader.execute(&request_query).await.unwrap();
            let visible = !response["data"]["AgentRequest"]
                .as_array()
                .unwrap()
                .is_empty();
            assert_eq!(
                visible,
                expected_read(modeled_actor),
                "Task request visibility for {modeled_actor}"
            );
        }
        access.write("test.task.finish", &format!(
            "mutation {{ update_AgentRequest(docID: \"{}\", input: {{ lifecycle_state: \"completed\" }}) {{ _docID }} }}",
            escape_graphql_string(&first.request.doc_id)
        )).await.unwrap();
        let observed = crate::eval::runner::embedded::observe::poll_request_with_access(
            &user_access,
            &first.request.request_id,
        )
        .await
        .unwrap()
        .expect("the user can observe the completed Task request");
        assert_eq!(observed.lifecycle_state.as_deref(), Some("completed"));
        let session_doc = session["_docID"].as_str().unwrap();
        let trace = crate::lean_vocab_test::lean_contract_snapshot()
            .identity_publication_cases
            .iter()
            .find(|case| case["name"] == "interrupted_then_repaired")
            .unwrap();
        access
            .delete_resource_relationship(
                "AgentSession",
                session_doc,
                "parent",
                "agents",
                &agent_doc,
            )
            .await
            .unwrap();
        access.write("test.session.interrupted", &format!(
            r#"mutation {{ update_AgentSession(docID: "{}", input: {{ relationships_pending: true }}) {{ _docID }} }}"#,
            escape_graphql_string(session_doc),
        )).await.unwrap();
        let actor = identity::Did::new(agent.did()).unwrap();
        assert_eq!(
            crate::session::recover_agent_session_relationships_with_access(
                &access,
                PIN_FIXED_DID,
                &actor
            )
            .await
            .unwrap(),
            1
        );
        let recovered = user_access
            .execute("{ AgentSession { relationships_pending } }")
            .await
            .unwrap();
        assert_eq!(
            recovered["data"]["AgentSession"][0]["relationships_pending"],
            trace["steps"][1]["after"]["pending"]
        );
        let request_row = access
            .execute(&format!(
                "{{ AgentRequest {{ {} }} }}",
                gents_protocol::row::AGENT_REQUEST_FIELDS
            ))
            .await
            .unwrap();
        let request = AgentRequest::try_from(
            serde_json::from_value::<gents_protocol::row::AgentRequestRow>(
                request_row["data"]["AgentRequest"][0].clone(),
            )
            .unwrap(),
        )
        .unwrap();
        let provenance = access
            .transact_readonly("test.task_provenance", |txn| {
                let request = &request;
                Box::pin(async move { created_session_provenance_in_txn(txn, request).await })
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(provenance.task_id.as_deref(), Some("task"));
        assert_eq!(
            provenance.opening_request_doc_id.as_deref(),
            Some(first.request.doc_id.as_str())
        );
        let receipt = access.execute("{TriggerFire {_docID}}").await.unwrap();
        let receipt_doc = receipt["data"]["TriggerFire"][0]["_docID"]
            .as_str()
            .unwrap();
        let receipt_contract = crate::lean_vocab_test::lean_identity_household_cases()
            .iter()
            .find(|case| case.name == "configuration-explicit-grants")
            .unwrap();
        let creator = &receipt_contract
            .objects
            .iter()
            .find(|object| object.id == "agent-created")
            .unwrap()
            .owner;
        let user = &receipt_contract
            .objects
            .iter()
            .find(|object| object.id == "home")
            .unwrap()
            .owner;
        let sibling = receipt_contract
            .task_receipt_cases
            .iter()
            .find(|case| {
                case["creator"] == *creator
                    && case["permission"] == "read"
                    && case["producer_reader"] == false
                    && case["expected"] == false
            })
            .unwrap()["actor"]
            .as_str()
            .unwrap();
        for (reader, actor) in [
            (&user_access, user.as_str()),
            (&access, creator.as_str()),
            (&outsider_access, sibling),
        ] {
            let expected = receipt_contract
                .task_receipt_cases
                .iter()
                .find(|case| {
                    case["creator"] == *creator
                        && case["actor"] == actor
                        && case["permission"] == "read"
                        && case["producer_reader"] == false
                })
                .unwrap()["expected"]
                .as_bool()
                .unwrap();
            let response = reader.execute("{TriggerFire {_docID}}").await.unwrap();
            assert_eq!(
                !response["data"]["TriggerFire"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
                expected
            );
        }
        access
            .delete_resource_relationship(
                "TriggerFire",
                receipt_doc,
                "parent",
                "sessions",
                session_doc,
            )
            .await
            .unwrap();
        assert!(
            write_task_delivery(&access, &fire, false, &create)
                .await
                .unwrap()
                .duplicate
        );
        assert!(
            user_access.execute("{TriggerFire {_docID}}").await.unwrap()["data"]["TriggerFire"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn reserved_trigger_cause_is_refused() {
        let identity_dir = tempfile::tempdir().unwrap();
        let identity = pin_fixed_signing_identity(identity_dir.path());
        let binding = crate::store::identity::provision_agent_identity(&identity).unwrap();
        let agent = crate::store::identity::resolve_agent_identity(&identity, &binding).unwrap();
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(PIN_FIXED_DID)
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let access = crate::store::config_client::ConfigAccess::local(node.clone());
        let message =
            "trigger graph-trigger-retired belongs to a retired graph revision; it no longer runs";
        let lineage = || TriggerLineage {
            trigger_id: Some("graph-trigger-retired".into()),
            trigger_kind: Some("schedule".into()),
            source_doc_id: None,
            correlation: None,
            trigger_context: None,
        };

        let actor = ::identity::Did::new(agent.did()).unwrap();
        let err = write_pending_agent_request_with_lineage_workspace_and_conversation_title(
            &node,
            actor,
            PIN_FIXED_DID,
            agent.did(),
            "work",
            ExecutionOrigin::Interactive,
            lineage(),
            None,
            None,
            None,
            None,
            Some("trigger-doc"),
            None,
            agent.as_ref(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), message);

        let identity = gents_protocol::trigger_delivery::FireIdentity {
            owner_did: PIN_FIXED_DID.into(),
            trigger_id: "manual:task".into(),
            source_collection: "Task".into(),
            source_doc_id: "invocation-key".into(),
        };
        let key = task_fire_key(&identity);
        let session_id = format!("trigger-session:{key}");
        let fire = gents_protocol::trigger_delivery::TriggerFire {
            fire_key: key.clone(),
            identity,
            task_id: "task".into(),
            request_id: format!("trigger-request:{key}"),
            session_id: session_id.clone(),
            goal_id: None,
            goal_objective: None,
            goal_token_budget: None,
            goal_assignment_applied: false,
            emit_outcome: false,
            queued_serial: false,
            source_handoff_id: None,
            reply_session_id: None,
            shard_id: None,
            attempt: None,
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        let spec = pending_agent_request_spec(
            PIN_FIXED_DID,
            agent.did(),
            "work",
            ExecutionOrigin::Interactive,
            lineage(),
            None,
            None,
            &fire.request_id,
            &fire.session_id,
            Some(&fire.fire_key),
            None,
            Some("trigger-doc"),
        )
        .unwrap();
        let create = build_signed_request(spec, RequestSigner::Identity(agent.as_ref()))
            .await
            .unwrap();
        let err = write_task_delivery(&access, &fire, false, &create)
            .await
            .err()
            .expect("reserved cause must be refused");
        assert_eq!(err.to_string(), message);

        let after = access
            .execute("{ TriggerFire { fire_key } AgentRequest { request_id } }")
            .await
            .unwrap();
        assert_eq!(after["data"]["TriggerFire"].as_array().unwrap().len(), 0);
        assert_eq!(after["data"]["AgentRequest"].as_array().unwrap().len(), 0);
    }
}

async fn is_client_request_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    request: &AgentRequest,
) -> Result<bool> {
    let response = txn.execute(&format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ admission_kind }} }}"#,
        escape_graphql_string(&request.doc_id),
    )).await?;
    let row = response
        .pointer("/data/AgentRequest/0")
        .context("session projection request disappeared")?;
    Ok(row
        .get("admission_kind")
        .and_then(serde_json::Value::as_str)
        == Some("client"))
}
