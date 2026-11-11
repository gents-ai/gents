use super::*;
use anyhow::Context;
use gents_protocol::request_lifecycle::{
    EnqueuedAgentRequest, SessionMessageCause, SessionMessageTarget,
};

/// A pending fire or EventGroup state persisted before the upgrade can still
/// name a retired graph trigger as its cause; such a request is never created.
pub fn refuse_retired_reserved_cause(
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
    access: &gents_store::config_client::ConfigAccess,
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
pub async fn write_pending_agent_request_with_lineage_workspace_and_conversation_title(
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
    identity: &dyn gents_store::identity::NodeIdentity,
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
pub async fn write_pending_agent_request_with_identity(
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
    identity: Option<&dyn gents_store::identity::NodeIdentity>,
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
    let access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        actor,
        identity,
    )?;
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
pub fn pending_agent_request_spec(
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
    let prompt_selection = gents_documents::skills::prompt_slash_skill_selection(content);
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
    Identity(&'a dyn gents_store::identity::NodeIdentity),
}

pub fn build_request(
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

pub async fn sign_request(
    create: &mut gents_protocol::request_admission::AgentRequestCreate,
    signer: RequestSigner<'_>,
) -> Result<()> {
    match signer {
        RequestSigner::RegisteredTarget => {
            crate::request::admission::sign_agent_request_create_as_registered_target(create)
                .await?;
        }
        RequestSigner::Identity(identity) => {
            crate::request::admission::sign_agent_request_create(identity, create).await?;
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
pub async fn write_pending_auxiliary_request(
    node: &std::sync::Arc<EmbeddedNode>,
    parent: &AgentRequest,
    purpose: gents_protocol::request_admission::RequestPurpose,
    content: String,
    valid_until: Option<String>,
    signer: &gents_store::identity::ManagedAgentSigner,
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
    let response = gents_store::config_client::ConfigAccess::local_with_agent_signer(
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
pub fn session_message_sender_header(cause: &SessionMessageCause, at: &str) -> String {
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
    signer: &gents_store::identity::ManagedAgentSigner,
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
pub async fn build_session_message_request_with_input(
    cause: &SessionMessageCause,
    target: &SessionMessageTarget,
    content: &str,
    title: Option<&str>,
    request_id: &str,
    retry_key: Option<String>,
    hop: u32,
    mut input: gents_protocol::request_input::RequestInput,
    signer: &gents_store::identity::ManagedAgentSigner,
) -> Result<gents_protocol::request_admission::AgentRequestCreate> {
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
    anyhow::ensure!(
        !cause.caller_request_id.trim().is_empty()
            && !cause.caller_request_doc_id.trim().is_empty()
            && !cause.tool_call_id.trim().is_empty()
            && !cause.tool_call_doc_id.trim().is_empty(),
        "session-message lineage requires the full calling request and tool call edge"
    );
    let prompt_selection = gents_documents::skills::prompt_slash_skill_selection(content);
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

pub async fn activate_workspace_bound_request_with_identity(
    node: &std::sync::Arc<EmbeddedNode>,
    request_doc_id: &str,
    actor: identity::Did,
    identity: Option<&dyn gents_store::identity::NodeIdentity>,
) -> Result<()> {
    let access = gents_store::config_client::ConfigAccess::local_with_optional_agent_signer(
        node.clone(),
        actor,
        identity.and_then(|identity| identity.managed_agent_signer()),
    )?;
    let mutation = {
        use gents_protocol::document_query::{
            fields::agent_request as fields, Filter, Input, Update,
        };
        Update::doc(
            request_doc_id,
            Input::new().set(
                fields::LIFECYCLE_STATE,
                RequestLifecycleState::Pending.as_str(),
            ),
        )
        .filter(Filter::eq(fields::DOC_ID, request_doc_id).and(Filter::eq(
            fields::LIFECYCLE_STATE,
            RequestLifecycleState::WorkspaceBindingPending.as_str(),
        )))
        .render()
    };
    let response = access
        .write_rendered("lifecycle.activate_workspace_bound", &mutation)
        .await?;
    let response: defra_node::QueryResponse = serde_json::from_value(response)?;
    if gents_store::graphql::single_mutation_document(&response, "update_AgentRequest")?.is_none() {
        let query = {
            use gents_protocol::document_query::{
                fields::agent_request as fields, Filter, Query, Selection,
            };
            Query::select(
                Selection::new()
                    .field(fields::REQUEST_ID)
                    .field(fields::LIFECYCLE_STATE)
                    .field(fields::WORKSPACE_ID),
            )
            .filter(Filter::eq(fields::DOC_ID, request_doc_id))
            .limit(1)
            .render()
        };
        let response = serde_json::from_value(access.read(&query).await?)?;
        let row = gents_store::graphql::first_row::<gents_protocol::row::AgentRequestRow>(
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
        request: AgentRequest,
        deadline_duration_secs: u64,
    ) -> Self {
        Self::new_with_execution_binding(
            node,
            request,
            deadline_duration_secs,
            ExecutionOrigin::Interactive,
            "",
        )
    }

    pub fn new_with_execution_binding(
        node: Arc<EmbeddedNode>,
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
            resumed_from_backend: false,
            execution_lease_duration_secs:
                gents_documents::inference_defaults::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn materialize_claimed_with_execution_binding(
        node: Arc<EmbeddedNode>,
        agent_did: &str,
        identity: Arc<dyn gents_store::identity::NodeIdentity>,
        agent_identity: Arc<dyn gents_store::identity::NodeIdentity>,
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
    pub async fn materialize_pending_with_execution_binding(
        node: Arc<EmbeddedNode>,
        agent_did: &str,
        identity: Arc<dyn gents_store::identity::NodeIdentity>,
        content: &str,
        deadline_duration_secs: u64,
        execution_origin: ExecutionOrigin,
        backend_id: impl Into<String>,
        trigger_lineage: TriggerLineage,
        managed_identity: Option<Arc<dyn gents_store::identity::NodeIdentity>>,
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
        let access = gents_store::config_client::ConfigAccess::local_managed_agent(
            node.clone(),
            actor,
            identity.as_ref(),
        )?;
        let signer = RequestSigner::Identity(identity.as_ref());
        let create = super::client::build_signed_client_request(&access, spec, signer).await?;
        super::client::submit_client_request(&access, &create).await?;
        let query = {
            use gents_protocol::document_query::{
                fields::agent_request as fields, Filter, Query, Selection,
            };
            Query::select(Selection::doc_id())
                .filter(Filter::eq(fields::REQUEST_ID, &request_id))
                .limit(2)
                .render()
        };
        let response = access.read(&query).await?;
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

    pub fn claimed_tool_context(
        &self,
    ) -> anyhow::Result<gents_loop::tool::ToolRequestContext> {
        anyhow::ensure!(
            matches!(
                self.state,
                LocalLifecycleState::Claimed | LocalLifecycleState::Streaming
            ),
            "tool context requires a committed request claim"
        );
        let admitted_client = self.claimed_client_provenance().map(|provenance| {
            gents_loop::tool::AdmittedClientContext {
                signer_did: provenance.signer_did,
                request_doc_id: provenance.request_doc_id,
                session_doc_id: provenance.session_doc_id,
                agent_doc_id: provenance.agent_doc_id,
            }
        });
        Ok(gents_loop::tool::ToolRequestContext {
            request_id: self.request.request_id.clone(),
            request_doc_id: self.request.doc_id.clone(),
            node_did: self.request.node_did.clone(),
            agent_did: self.request.agent_did.clone(),
            session_id: self.request.session_id.clone(),
            caused_by_source_doc_id: self.request.caused_by_source_doc_id.clone(),
            admitted_client,
        })
    }

    pub fn execution_generation(&self) -> anyhow::Result<&str> {
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
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    request: &AgentRequest,
) -> Result<LabeledSessionRows> {
    let query = {
        use gents_protocol::document_query::{
            fields::agent_session as fields, Filter, Query, Selection,
        };
        Query::select(Selection::owned(
            gents_protocol::session::AGENT_SESSION_FIELDS,
        ))
        .filter(Filter::eq(fields::NODE_DID, &request.node_did))
        .filter(Filter::eq(fields::SESSION_ID, &request.session_id))
        .render()
    };
    let response = txn.run(&query).await?;
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
pub fn created_session_provenance(
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

pub async fn created_session_provenance_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    request: &AgentRequest,
) -> Result<Option<gents_protocol::session::SessionProvenance>> {
    let mut provenance = created_session_provenance(request);
    let kind = request.caused_by_trigger_kind.as_deref();
    if !matches!(kind, Some("manual" | "event" | "schedule")) {
        return Ok(provenance);
    }
    let query = {
        use gents_protocol::document_query::{
            fields::trigger_fire as fields, Filter, Query, Selection,
        };
        Query::select(
            Selection::new()
                .field(fields::FIRE_KEY)
                .field(fields::OWNER_DID)
                .field(fields::REQUEST_ID)
                .field(fields::SESSION_ID)
                .field(fields::TRIGGER_ID)
                .field(fields::TASK_ID)
                .field(fields::SOURCE_COLLECTION)
                .field(fields::SOURCE_DOC_ID),
        )
        .filter(Filter::eq(fields::OWNER_DID, &request.node_did))
        .filter(Filter::eq(fields::REQUEST_ID, &request.request_id))
        .limit(2)
        .render()
    };
    let response = txn.run(&query).await?;
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

pub async fn apply_request_session_projection(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
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
    let owner = gents_store::config_client::agent_session::load_agent_session_row_in_txn(
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
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
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
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
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
    let owner = gents_store::config_client::agent_session::load_agent_session_row_in_txn(
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
        let binding = gents_store::identity::provision_agent_identity(&identity).unwrap();
        let agent = gents_store::identity::resolve_agent_identity(&identity, &binding).unwrap();
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
        let binding = gents_store::identity::provision_agent_identity(&identity).unwrap();
        let agent = gents_store::identity::resolve_agent_identity(&identity, &binding).unwrap();
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

#[cfg(test)]
mod session_provenance_tests {
    use super::*;

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
            &gents_store::config_client::ConfigAccess::local(admission.node.clone()),
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
        let cases = &gents_lean_vocab::lean_contract_snapshot()
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
}

async fn is_client_request_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    request: &AgentRequest,
) -> Result<bool> {
    let query = {
        use gents_protocol::document_query::{
            fields::agent_request as fields, Filter, Query, Selection,
        };
        Query::select(Selection::new().field(fields::ADMISSION_KIND))
            .filter(Filter::eq(fields::DOC_ID, &request.doc_id))
            .limit(1)
            .render()
    };
    let response = txn.run(&query).await?;
    let row = response
        .pointer("/data/AgentRequest/0")
        .context("session projection request disappeared")?;
    Ok(row
        .get("admission_kind")
        .and_then(serde_json::Value::as_str)
        == Some("client"))
}
