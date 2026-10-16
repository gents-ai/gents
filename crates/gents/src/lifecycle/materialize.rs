use super::*;
use anyhow::Context;

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
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}, limit: 2) {{ _docID request_id }} }}"#
    );
    let query_resp = node.execute(&query).await;
    if query_resp.has_errors() {
        anyhow::bail!("{lookup_error}: {:?}", query_resp.errors);
    }

    let rows: Vec<gents_protocol::row::AgentRequestRow> =
        crate::graphql::rows(&query_resp, "AgentRequest")?;
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

/// Write one signed pending request. `session_id` names an existing session
/// the request continues; absent mints a new session.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn write_pending_agent_request_with_lineage_workspace_and_conversation_title(
    node: &std::sync::Arc<EmbeddedNode>,
    actor: ::identity::Did,
    node_did: &str,
    agent_id: &str,
    content: &str,
    execution_origin: ExecutionOrigin,
    trigger_lineage: TriggerLineage,
    conversation_title: Option<&str>,
    workspace_lineage: Option<&WorkspaceLineage>,
    request_id: Option<&str>,
    requester_did: Option<&str>,
    trigger_doc_id: Option<&str>,
    session_id: Option<&str>,
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
    let create = build_signed_pending_agent_request_with_lineage_workspace_and_conversation_title(
        node_did,
        agent_id,
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
    )
    .await?;
    if create
        .caused_by_trigger_id
        .as_deref()
        .is_some_and(crate::graph_pipeline::graph_artifact_is_reserved)
    {
        return publish_graph_root_request(node, actor, &create).await;
    }
    let escaped_request_id = escape_graphql_string(&request_id);
    let mutation = create.graphql_mutation().map_err(anyhow::Error::msg)?;
    let mutation = &mutation;

    let response = crate::config_client::ConfigAccess::transact_local_write(
        node,
        Some(actor),
        "lifecycle.materialize_pending",
        |txn| Box::pin(async move { txn.execute_local_response(mutation).await }),
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

/// Retry the complete graph publication transaction on native conflicts. A
/// first-seen trigger event cannot be replayed, so retrying only its row create
/// would either lose the stage or bypass a newly committed graph closure.
async fn publish_graph_root_request(
    node: &std::sync::Arc<EmbeddedNode>,
    actor: ::identity::Did,
    create: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<EnqueuedAgentRequest> {
    let mutation = create.graphql_mutation().map_err(anyhow::Error::msg)?;
    let mutation = &mutation;
    let doc_id = crate::config_client::ConfigAccess::transact_local_write(
        node,
        Some(actor),
        "lifecycle.publish_graph_root",
        move |txn| {
            Box::pin(async move {
                crate::graph_pipeline::fence_graph_root_request_in_txn(&txn, create).await?;
                let response = txn.execute(&mutation).await?;
                crate::graphql::created_doc_id(&response, "AgentRequest")
            })
        },
    )
    .await?;
    Ok(EnqueuedAgentRequest {
        doc_id,
        request_id: create.request_id.clone(),
        session_id: create.session_id.clone(),
    })
}

/// Build and sign the canonical pending request used by trigger materialization.
/// Callers that stage controller documents in the same transaction precompute
/// the request/session/retry identity and pass the returned immutable create
/// document to their atomic submit seam. `requester_did` is the requester that
/// owns the existing session a runtime fire is delivered into; `None` is the
/// target itself.
#[allow(clippy::too_many_arguments)]
pub async fn build_signed_pending_agent_request_with_lineage_workspace_and_conversation_title(
    node_did: &str,
    agent_id: &str,
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
    let prompt_selection = crate::skills::prompt_slash_skill_selection(content);
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
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(node_did)
        }
        Some("event" | "schedule") => {
            let source = trigger_lineage.trigger_id.as_deref().ok_or_else(|| {
                anyhow::anyhow!("runtime trigger request requires a durable trigger id")
            })?;
            gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_automated_trigger(
                node_did, source,
            )
        }
        Some(kind) => anyhow::bail!("unsupported runtime request trigger kind {kind}"),
    };
    let identity = RequestIdentity {
        requester_did: requester_did.map(str::to_owned),
        request_id: request_id.to_string(),
        node_did: node_did.to_string(),
        agent_id: agent_id.to_string(),
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
    build_signed_request(spec, RequestSigner::RegisteredTarget).await
}

/// Identity fields every writer decides for a fresh `AgentRequest`.
pub struct RequestIdentity {
    pub requester_did: Option<String>,
    pub request_id: String,
    pub node_did: String,
    pub agent_id: String,
    pub session_id: String,
    pub content: String,
    pub execution_origin: ExecutionOrigin,
    pub created_at: String,
}

/// Causal lineage: the logical and physical identifiers of the request (and,
/// for `agent_new`/`agent_message`, the tool call) that caused this one.
#[derive(Default)]
pub struct ParentLink {
    pub depth: u32,
    pub parent_request_id: String,
    pub parent_request_doc_id: String,
    pub parent_tool_call_id: Option<String>,
    pub parent_tool_call_doc_id: Option<String>,
}

/// Retry linkage: the failed request this one supersedes, the root of the
/// retry chain, and the counters carried forward from it.
pub struct RetryLink {
    pub parent_request_id: Option<String>,
    pub parent_request_doc_id: Option<String>,
    pub root_request_id: String,
    pub retry_count: i64,
    pub max_retries: i64,
}

/// Every input a writer decides before an `AgentRequestCreate` is built and
/// signed; `build_request` alone owns which of its fields become which
/// stamped columns.
pub struct RequestSpec {
    pub purpose: gents_protocol::request_admission::RequestPurpose,
    pub identity: RequestIdentity,
    pub admission: gents_protocol::request_admission::AgentRequestAdmissionRecord,
    pub initial_lifecycle_state: RequestLifecycleState,
    /// Correlation/context/kind/id/source-doc lineage of the trigger that
    /// caused this request; `trigger_doc_id` is not part of the signed
    /// trigger-provenance payload validated by `validate_trigger_provenance`.
    pub trigger_lineage: TriggerLineage,
    pub trigger_doc_id: Option<String>,
    pub workspace: Option<WorkspaceLineage>,
    pub parent: Option<ParentLink>,
    /// `None` means this is not a retry.
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

/// How the built `AgentRequestCreate` is signed: as the already-registered
/// runtime node named by `spec.identity.node_did` (the common case for
/// runtime-authored requests), or with an explicit caller-held identity.
pub enum RequestSigner<'a> {
    RegisteredTarget,
    Identity(&'a dyn crate::identity::NodeIdentity),
}

/// Build and stamp one `AgentRequestCreate`, unsigned. Sole owner of the
/// mapping from a writer's decisions (`RequestSpec`) to the DTO's stamped
/// columns, split from signing so a caller may inspect the built DTO before
/// paying for a signature.
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
    let node_did = identity.node_did.clone();

    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        purpose,
        identity.request_id,
        identity.node_did,
        identity.requester_did.unwrap_or(node_did),
        identity.agent_id,
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
            gents_protocol::request_admission::RequestPurpose::TitleAudit => 0,
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

/// Sign an `AgentRequestCreate` built by `build_request`.
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

/// Build, stamp, and sign one `AgentRequestCreate` in one call.
pub async fn build_signed_request(
    spec: RequestSpec,
    signer: RequestSigner<'_>,
) -> Result<gents_protocol::request_admission::AgentRequestCreate> {
    let mut create = build_request(spec)?;
    sign_request(&mut create, signer).await?;
    Ok(create)
}

/// The parent link is provenance only. The persisted pending request is the
/// crash-recoverable owner of title inference, including after its parent ends.
pub(crate) async fn write_pending_title_request(
    node: &std::sync::Arc<EmbeddedNode>,
    parent: &AgentRequest,
    content: String,
) -> Result<AgentRequest> {
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
    anyhow::ensure!(
        parent.purpose == RequestPurpose::Normal,
        "a title audit request cannot recursively create title work"
    );
    let identity = RequestIdentity {
        request_id: uuid::Uuid::new_v4().to_string(),
        node_did: parent.node_did.clone(),
        requester_did: Some(parent.node_did.clone()),
        agent_id: parent.agent_id.clone(),
        session_id: parent.session_id.clone(),
        content,
        execution_origin: ExecutionOrigin::Interactive,
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };
    let admission =
        AgentRequestAdmissionRecord::runtime_local_control(&parent.node_did, &parent.request_id);
    let spec = RequestSpec {
        parent: Some(ParentLink {
            parent_request_id: parent.request_id.clone(),
            parent_request_doc_id: parent.doc_id.clone(),
            ..Default::default()
        }),
        ..RequestSpec::new(RequestPurpose::TitleAudit, identity, admission)
    };
    let create = build_signed_request(spec, RequestSigner::RegisteredTarget).await?;
    let mutation = create
        .graphql_mutation_selecting(crate::watcher::AGENT_REQUEST_FIELDS)
        .map_err(anyhow::Error::msg)?;
    let response = crate::config_client::ConfigAccess::write_local_response(
        node,
        "lifecycle.materialize_title_audit",
        &mutation,
    )
    .await?;
    crate::watcher::agent_request_from_mutation_response(&response, "create_AgentRequest")?
        .context("title request creation omitted its exact durable request")
}

/// Why a request exists, for its causal hop (Lean `CausalHop.Cause`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestHopCause {
    Root,
    CrossSession { cause_hop: u32 },
    Return,
    Continuation,
}

/// Lean `CausalHop.nextHop`: `own` is the hop of the request this one
/// continues in its own session, `0` for a new session.
pub fn next_request_hop(cause: RequestHopCause, own: u32) -> u32 {
    match cause {
        RequestHopCause::Root => 0,
        RequestHopCause::CrossSession { cause_hop } => own.max(cause_hop.saturating_add(1)),
        RequestHopCause::Return | RequestHopCause::Continuation => own,
    }
}

/// Lean `CausalHop.admitHop`.
pub fn request_hop_within_bound(max_request_hop: u32, hop: u32) -> bool {
    hop <= max_request_hop
}

/// The calling edge an `agent_new`/`agent_message` request records: the
/// caller's DID (its requester and signer), request and tool call.
#[derive(Debug, Clone)]
pub(crate) struct SessionMessageCause {
    pub(crate) caller_node_did: String,
    pub(crate) caller_request_id: String,
    pub(crate) caller_request_doc_id: String,
    pub(crate) caller_hop: u32,
    pub(crate) tool_call_id: String,
    pub(crate) tool_call_doc_id: String,
    pub(crate) correlation: Option<String>,
}

/// Where a session-message request runs. The target node's own
/// agent configures it; nothing is inherited from the caller.
#[derive(Debug, Clone)]
pub(crate) struct SessionMessageTarget {
    pub(crate) node_did: String,
    pub(crate) agent_id: String,
    pub(crate) session_id: String,
}

/// Build and sign the request an `agent_new`/`agent_message` call
/// materializes at `hop` (Lean `DurableLineage.sessionMessageWrite`). This is
/// the single writer of the calling edge (`caused_by_parent_*`). The caller is
/// the requester and signer: its own node admits it as LocalSelf, any
/// other target as Peer under that target's ACP. A steering delivery carries
/// `queue`, which orders it after the busy session's active request.
pub(crate) async fn build_session_message_request(
    cause: &SessionMessageCause,
    target: &SessionMessageTarget,
    content: &str,
    title: Option<&str>,
    request_id: &str,
    retry_key: Option<String>,
    hop: u32,
    queue: Option<gents_protocol::request_input::RequestQueue>,
) -> Result<gents_protocol::request_admission::AgentRequestCreate> {
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
    anyhow::ensure!(
        !cause.caller_request_id.trim().is_empty()
            && !cause.caller_request_doc_id.trim().is_empty()
            && !cause.tool_call_id.trim().is_empty()
            && !cause.tool_call_doc_id.trim().is_empty(),
        "session-message lineage requires the full calling request and tool call edge"
    );
    let prompt_selection = crate::skills::prompt_slash_skill_selection(content);
    let admission = if target.node_did == cause.caller_node_did {
        AgentRequestAdmissionRecord::local_self(&cause.caller_node_did)
    } else {
        AgentRequestAdmissionRecord::peer(&cause.caller_node_did)
    };
    let identity = RequestIdentity {
        requester_did: Some(cause.caller_node_did.clone()),
        request_id: request_id.to_owned(),
        node_did: target.node_did.clone(),
        agent_id: target.agent_id.clone(),
        session_id: target.session_id.clone(),
        content: prompt_selection.prompt.clone(),
        execution_origin: ExecutionOrigin::Interactive,
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };
    let input = gents_protocol::request_input::RequestInput {
        selected_skill_ids: prompt_selection.selected_skill_ids,
        initial_title: title
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(|text| gents_protocol::session::SessionTitle {
                text: text.to_owned(),
                source: gents_protocol::session::SessionTitleSource::Task,
            }),
        queue,
        ..Default::default()
    };
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
    let signer = crate::identity::RegisteredIdentity::from_registered_did(
        cause.caller_node_did.clone(),
        None,
    )
    .context("load the caller's registered identity to sign its session message")?;
    build_signed_request(spec, RequestSigner::Identity(&signer)).await
}

pub async fn activate_workspace_bound_request(
    node: &std::sync::Arc<EmbeddedNode>,
    request_doc_id: &str,
) -> Result<()> {
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
    let response = crate::config_client::ConfigAccess::write_local(
        node,
        "lifecycle.activate_workspace_bound",
        &mutation,
    )
    .await?;
    let response: defra_node::QueryResponse = serde_json::from_value(response)?;
    if crate::graphql::single_mutation_document(&response, "update_AgentRequest")?.is_none() {
        let query = format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{
                request_id lifecycle_state workspace_id
            }} }}"#,
            doc_id = escape_graphql_string(request_doc_id),
        );
        let response = crate::graphql::graphql_with_transaction_retry(
            node,
            &query,
            "recover workspace-bound request activation",
        )
        .await?;
        let row = crate::graphql::first_row::<gents_protocol::row::AgentRequestRow>(
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
        agent_id: &str,
        node_did: &str,
        request: AgentRequest,
        deadline_duration_secs: u64,
    ) -> Self {
        Self::new_with_execution_binding(
            node,
            agent_id,
            node_did,
            request,
            deadline_duration_secs,
            ExecutionOrigin::Interactive,
            "",
        )
    }

    pub fn new_with_execution_binding(
        node: Arc<EmbeddedNode>,
        _agent_id: &str,
        _node_did: &str,
        request: AgentRequest,
        deadline_duration_secs: u64,
        execution_origin: ExecutionOrigin,
        backend_id: impl Into<String>,
    ) -> Self {
        let agent_id = request.agent_id.clone();
        Self {
            node,
            agent_id,
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
            fold_admitted: Vec::new(),
            folded_selection: Vec::new(),
            natural_finish_allowed: false,
            execution_lease_duration_secs: crate::config::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn materialize_claimed_with_execution_binding(
        node: Arc<EmbeddedNode>,
        agent_id: &str,
        identity: Arc<dyn crate::identity::NodeIdentity>,
        content: &str,
        deadline_duration_secs: u64,
        execution_origin: ExecutionOrigin,
        backend_id: impl Into<String>,
        trigger_lineage: TriggerLineage,
    ) -> Result<Self> {
        let mut lifecycle = Self::materialize_pending_with_execution_binding(
            node,
            agent_id,
            identity,
            content,
            deadline_duration_secs,
            execution_origin,
            backend_id,
            trigger_lineage,
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
        agent_id: &str,
        identity: Arc<dyn crate::identity::NodeIdentity>,
        content: &str,
        deadline_duration_secs: u64,
        execution_origin: ExecutionOrigin,
        backend_id: impl Into<String>,
        trigger_lineage: TriggerLineage,
    ) -> Result<Self> {
        let node_did = identity.did().to_string();
        let backend_id = backend_id.into();
        let agent_id = agent_id.to_string();
        let request_id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4().to_string();
        validate_trigger_provenance(&trigger_lineage)?;
        let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let admission =
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(&node_did);
        let request_identity = RequestIdentity {
            requester_did: None,
            request_id: request_id.clone(),
            node_did: node_did.clone(),
            agent_id: agent_id.clone(),
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
        let create = build_signed_request(spec, RequestSigner::Identity(identity.as_ref())).await?;
        let mutation = create.graphql_mutation().map_err(anyhow::Error::msg)?;
        let resp = crate::config_client::ConfigAccess::write_local(
            &node,
            "lifecycle.materialize_before_claim",
            &mutation,
        )
        .await?;
        let resp: defra_node::QueryResponse = serde_json::from_value(resp)?;

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
            retry_parent_request_doc_id: create.retry_parent_request_doc_id.clone(),
            purpose: create.purpose,
            doc_id,
            request_id,
            node_did: node_did.clone(),
            requester_did: Some(node_did.clone()),
            agent_id: agent_id.clone(),
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
        let request = crate::request_admission::verify_fresh_local_self_request(
            &node,
            identity.as_ref(),
            &queued_request,
            &agent_id,
        )
        .await?;
        let lifecycle = Self::new_with_execution_binding(
            node,
            &agent_id,
            &node_did,
            request,
            deadline_duration_secs,
            execution_origin,
            backend_id,
        );
        Ok(lifecycle)
    }

    pub fn request(&self) -> &AgentRequest {
        &self.request
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

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }
}

/// Session rows sharing the request's `(node, session)` label, decoded one
/// row at a time so rows that fail canonical decode stay separable.
pub(super) struct LabeledSessionRows {
    pub(super) decoded: Vec<session::SessionOwnerRow>,
    pub(super) undecodable: Vec<anyhow::Error>,
}

/// Resolve the session rows sharing the request's `(node, session)` label.
/// Requester scope is deliberately absent from the filter: the claim boundary
/// must see a foreign-owned label to refuse it, and the refusal boundary must
/// see it to leave it untouched.
async fn labeled_session_rows_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    request: &AgentRequest,
) -> Result<LabeledSessionRows> {
    let response = txn
        .execute(&format!(
            r#"{{ AgentSession(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            escape_graphql_string(&request.node_did),
            escape_graphql_string(&request.session_id),
            session::AGENT_SESSION_FIELDS,
        ))
        .await?;
    let rows = response["data"]["AgentSession"]
        .as_array()
        .context("AgentSession query omitted rows")?;
    let mut decoded = Vec::new();
    let mut undecodable = Vec::new();
    for row in rows {
        match session::decode_session_row(row) {
            Ok(row) => decoded.push(row),
            Err(error) => undecodable.push(error),
        }
    }
    Ok(LabeledSessionRows {
        decoded,
        undecodable,
    })
}

/// Materialize the single session through its owner in the claim transaction.
/// Creation intent is consumed once; request observations come from the exact
/// authoritative request row after the claim mutation.
pub(super) async fn apply_request_session_projection(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    request: &AgentRequest,
    now: &str,
) -> Result<()> {
    // The session belongs to one requester; a request under any other scope
    // is refused rather than attached.
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
        if existing.session.agent_id != request.agent_id {
            return Err(ClaimAdmissionError::SessionAgentMismatch {
                session_id: request.session_id.clone(),
                existing_agent_id: existing.session.agent_id.clone(),
                requested_agent_id: request.agent_id.clone(),
            }
            .into());
        }
    }
    session::ensure_session_in_txn(
        txn,
        &request.session_id,
        &request.node_did,
        &request.agent_id,
        request.requester_did.as_deref(),
        request.input.initial_title.clone(),
        request
            .caused_by_parent_request_doc_id
            .as_ref()
            .map(|parent| gents_protocol::session::SessionProvenance {
                parent_request_doc_id: Some(parent.clone()),
                ..Default::default()
            }),
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
    let owner = session::load_agent_session_row_in_txn(
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
    let advanced =
        session::advance_session_request_observation_in_txn(txn, incoming, &request.content, now)
            .await?;
    if !reopened && !advanced {
        session::touch_session_in_txn(txn, &owner, now).await?;
    }
    Ok(())
}

/// Project a refused request into its session inside the refusal transaction,
/// so the failed turn stays reachable from the session after the client stops
/// tracking it. Guards mirror `AgentSession.advance` and leave the session
/// untouched instead of failing the refusal: a refusal never reopens a closed
/// session, and the pending -> failed write must not roll back if the
/// projection cannot decode or read unrelated durable rows.
pub(crate) async fn project_refused_request_session_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    request: &AgentRequest,
    now: &str,
) -> Result<()> {
    if request.purpose != gents_protocol::request_admission::RequestPurpose::Normal
        || request.node_did.trim().is_empty()
        || request.session_id.trim().is_empty()
        || request.agent_id.trim().is_empty()
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
            || existing.session.agent_id != request.agent_id
        {
            return Ok(());
        }
    }
    project_refused_request_onto_session_in_txn(txn, request, now).await
}

async fn project_refused_request_onto_session_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    request: &AgentRequest,
    now: &str,
) -> Result<()> {
    session::ensure_session_in_txn(
        txn,
        &request.session_id,
        &request.node_did,
        &request.agent_id,
        request.requester_did.as_deref(),
        request.input.initial_title.clone(),
        request
            .caused_by_parent_request_doc_id
            .as_ref()
            .map(|parent| gents_protocol::session::SessionProvenance {
                parent_request_doc_id: Some(parent.clone()),
                ..Default::default()
            }),
        now,
    )
    .await?;
    let owner = session::load_agent_session_row_in_txn(
        txn,
        &request.node_did,
        &request.session_id,
        request.requester_did.as_deref(),
    )
    .await?
    .context("refused session disappeared")?;
    let incoming = async {
        let facts = session::load_scoped_request_facts_in_txn(txn, &owner.session, false).await?;
        facts
            .iter()
            .find(|fact| {
                fact.observed.request_doc_id == request.doc_id
                    && fact.observed.request_id == request.request_id
            })
            .cloned()
            .context("refused request missing from its exact session scope")
    }
    .await;
    let incoming = match incoming {
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
    session::advance_refused_request_observation_in_txn(txn, &incoming, &request.content, now)
        .await?;
    Ok(())
}

/// Canonical identity shared by automated and manually invoked Task fires.
pub fn task_fire_key(identity: &gents_protocol::trigger_delivery::FireIdentity) -> String {
    crate::trigger_engine::durable::fire_key(identity)
}

pub struct TaskDeliveryAdmission {
    pub request: EnqueuedAgentRequest,
    pub duplicate: bool,
}

pub async fn write_task_delivery(
    access: &crate::config_client::ConfigAccess,
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
    crate::config_client::ConfigAccess::transact_local_write(
        node,
        Some(actor),
        "lifecycle.admit_task_delivery",
        |txn| {
            Box::pin(async move { stage_task_delivery(txn, fire, continue_existing, create).await })
        },
    )
    .await
}

pub(crate) async fn write_trigger_delivery(
    node: &std::sync::Arc<EmbeddedNode>,
    actor: ::identity::Did,
    prepared: &crate::trigger_engine::durable::PreparedFire,
    create: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<TaskDeliveryAdmission> {
    write_task_delivery_local(
        node,
        actor,
        &prepared.receipt,
        prepared.target_existing,
        create,
    )
    .await
}

async fn stage_task_delivery(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    fire: &gents_protocol::trigger_delivery::TriggerFire,
    continue_existing: bool,
    create: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<TaskDeliveryAdmission> {
    let fresh = crate::trigger_engine::durable::stage_fire_receipt(txn, fire).await?;
    if fresh {
        if create.caused_by_trigger_kind.as_deref() == Some("event") {
            anyhow::ensure!(
                create.caused_by_trigger_id.as_deref() == Some(fire.identity.trigger_id.as_str()),
                "event request and receipt trigger identity disagree"
            );
            crate::config_client::event_source_cursor::validate_event_admission(txn, fire).await?;
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
                anyhow::ensure!(
                    goal_id
                        == &crate::goal::deterministic_goal_id(
                            &create.node_did,
                            &create.session_id
                        ),
                    "Task receipt has a noncanonical Goal identity"
                );
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
        let session = crate::session::load_agent_session_row_in_txn(
            txn,
            &create.node_did,
            &create.session_id,
            Some(create.requester_did.as_str()),
        )
        .await?;
        if continue_existing {
            anyhow::ensure!(
                crate::trigger_engine::durable::resolve_session_id(
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
                    || crate::trigger_engine::durable::resolve_session_id(
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
                session.session.agent_id == create.agent_id,
                "Task target session has a different agent"
            );
            anyhow::ensure!(
                session.session.closed_at.is_none(),
                "Task target session is closed"
            );
        }
        crate::graph_pipeline::fence_graph_root_request_in_txn(txn, create).await?;
        txn.execute(&create.graphql_mutation().map_err(anyhow::Error::msg)?)
            .await?;
    }
    let query = format!("{{ AgentRequest(filter: {{node_did: {{_eq: \"{}\"}}, request_id: {{_eq: \"{}\"}}}}) {{ _docID session_id }} }}",
        escape_graphql_string(&fire.identity.owner_did), escape_graphql_string(&fire.request_id));
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
    use crate::lifecycle::test_support::{pin_fixed_signing_identity, PIN_FIXED_DID};

    #[tokio::test]
    async fn task_admission_is_atomic_idempotent_and_defers_goal_assignment() {
        let identity_dir = tempfile::tempdir().unwrap();
        let _identity = pin_fixed_signing_identity(identity_dir.path());
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let access = crate::config_client::ConfigAccess::local(node.clone());
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
            goal_id: Some(crate::goal::deterministic_goal_id(
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
        let create =
            build_signed_pending_agent_request_with_lineage_workspace_and_conversation_title(
                PIN_FIXED_DID,
                "agent",
                "complete assignment",
                ExecutionOrigin::Interactive,
                TriggerLineage {
                    trigger_id: None,
                    trigger_kind: Some("manual".into()),
                    source_doc_id: None,
                    correlation: None,
                    trigger_context: None,
                },
                None,
                None,
                &fire.request_id,
                &fire.session_id,
                Some(&fire.fire_key),
                None,
                None,
            )
            .await
            .unwrap();
        assert!(write_task_delivery(&access, &fire, true, &create)
            .await
            .is_err());
        let before = access
            .execute("{ TriggerFire { fire_key } AgentRequest { request_id } }")
            .await
            .unwrap();
        assert_eq!(before["data"]["TriggerFire"].as_array().unwrap().len(), 0);
        assert_eq!(before["data"]["AgentRequest"].as_array().unwrap().len(), 0);
        let first = write_task_delivery(&access, &fire, false, &create)
            .await
            .unwrap();
        let retry = write_task_delivery(&access, &fire, false, &create)
            .await
            .unwrap();
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
    }
}
