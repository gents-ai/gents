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
    let request_id = request_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let session_id = uuid::Uuid::new_v4().to_string();
    let create = build_signed_pending_agent_request_with_lineage_workspace_and_conversation_title(
        agent_did,
        behavior_id,
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
        return publish_graph_root_request(node, &create).await;
    }
    let escaped_request_id = escape_graphql_string(&request_id);
    let mutation = create.graphql_mutation().map_err(anyhow::Error::msg)?;
    let mutation = &mutation;

    // A trigger fire is not replayable: `event_kind: created` is first-seen, so
    // dropping this create on a transient conflict loses the stage for good.
    let response = crate::config_client::ConfigAccess::write_local(
        node,
        "lifecycle.materialize_pending",
        &mutation,
    )
    .await?;
    let response: defra_node::QueryResponse = serde_json::from_value(response)?;

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
    node: &EmbeddedNode,
    create: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<EnqueuedAgentRequest> {
    let mutation = create.graphql_mutation().map_err(anyhow::Error::msg)?;
    let mutation = &mutation;
    let doc_id = crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "lifecycle.publish_graph_root",
        move |txn| {
            Box::pin(async move {
                crate::graph_pipeline::fence_graph_root_request_in_txn(&txn, create).await?;
                let response = txn.execute(&mutation).await?;
                let child = response
                    .pointer("/data/create_AgentRequest")
                    .or_else(|| response.pointer("/data/add_AgentRequest"))
                    .context("graph root create omitted result")?;
                let doc_id = child
                    .get("_docID")
                    .or_else(|| child.get(0).and_then(|row| row.get("_docID")))
                    .and_then(serde_json::Value::as_str)
                    .context("graph root create omitted document ID")?
                    .to_owned();
                Ok::<_, anyhow::Error>(doc_id)
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
///
/// Callers which need to stage additional controller documents in the same
/// transaction can precompute the request/session/retry identity, then pass the
/// returned immutable create document to their atomic submit seam. The ordinary
/// trigger path above deliberately keeps its existing create behavior.
#[allow(clippy::too_many_arguments)]
pub async fn build_signed_pending_agent_request_with_lineage_workspace_and_conversation_title(
    agent_did: &str,
    behavior_id: &str,
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
    if requester_did.is_some_and(|did| did.trim() != agent_did) {
        tracing::debug!(
            target_agent_did = agent_did,
            "runtime trigger requester provenance remains in signed trigger context"
        );
    }
    let identity = RequestIdentity {
        requester_did: None,
        request_id: request_id.to_string(),
        agent_did: agent_did.to_string(),
        behavior_id: behavior_id.to_string(),
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
        ..RequestSpec::new(identity, admission)
    };
    build_signed_request(spec, RequestSigner::RegisteredTarget).await
}

/// The identity fields every writer decides for a fresh `AgentRequest`:
/// who it is for, what session it belongs to, and what it says.
pub struct RequestIdentity {
    /// Defaults to the target agent when omitted.
    pub requester_did: Option<String>,
    pub request_id: String,
    pub agent_did: String,
    pub behavior_id: String,
    pub session_id: String,
    pub content: String,
    pub execution_origin: ExecutionOrigin,
    pub created_at: String,
}

/// Parent-request linkage: the logical and physical identifiers of the
/// request (and, for a subagent spawn, the tool call) that caused this one,
/// plus its resulting depth. Used both for subagent spawns and for other
/// requests that are simply linked to one parent at some depth (a control
/// continuation, a background-wake redrive successor).
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
/// signed. This is the single seam every production writer should build
/// through; `build_signed_request` alone owns which of its fields become
/// which stamped columns.
pub struct RequestSpec {
    pub identity: RequestIdentity,
    pub admission: gents_protocol::request_admission::AgentRequestAdmissionRecord,
    pub initial_lifecycle_state: RequestLifecycleState,
    /// Correlation/context/kind/id/source-doc lineage of the trigger that
    /// caused this request. `trigger_doc_id` is carried separately because,
    /// unlike the rest of `TriggerLineage`, it is not part of the signed
    /// trigger-provenance payload validated by `validate_trigger_provenance`.
    pub trigger_lineage: TriggerLineage,
    pub trigger_doc_id: Option<String>,
    pub workspace: Option<WorkspaceLineage>,
    pub subagent: Option<ParentLink>,
    /// `None` means this is not a retry: `retry_root_request` defaults to
    /// this request's own id and `max_retries` to `DEFAULT_REQUEST_MAX_RETRIES`.
    pub retry: Option<RetryLink>,
    pub input: gents_protocol::request_input::RequestInput,
    pub retry_key: Option<String>,
    pub valid_until: Option<String>,
}

impl RequestSpec {
    /// A `RequestSpec` with only identity and admission decided; every
    /// other field takes the default a writer wants when it isn't a
    /// trigger-lineage-carrying, workspace-bound, subagent-linked, retried,
    /// request. Callers set only what they need
    /// via struct-update syntax:
    /// `RequestSpec { retry_key: Some(key), ..RequestSpec::new(identity, admission) }`.
    pub fn new(
        identity: RequestIdentity,
        admission: gents_protocol::request_admission::AgentRequestAdmissionRecord,
    ) -> Self {
        Self {
            identity,
            admission,
            initial_lifecycle_state: RequestLifecycleState::Pending,
            trigger_lineage: TriggerLineage::default(),
            trigger_doc_id: None,
            workspace: None,
            subagent: None,
            retry: None,
            input: Default::default(),
            retry_key: None,
            valid_until: None,
        }
    }
}

/// How the built `AgentRequestCreate` is signed: as the already-registered
/// runtime principal named by `spec.identity.agent_did` (the common case for
/// runtime-authored requests), or with an explicit caller-held identity.
pub enum RequestSigner<'a> {
    RegisteredTarget,
    Identity(&'a dyn crate::identity::AgentIdentity),
}

/// Build and stamp one `AgentRequestCreate`, unsigned. This is the sole
/// owner of the mapping from a writer's decisions (`RequestSpec`) to the
/// DTO's stamped columns; every production writer should build through it
/// (or `build_signed_request`, below) rather than hand-rolling
/// `AgentRequestCreate::base` (see below) and stamping fields itself.
///
/// Split out from signing so a caller that needs to inspect the built DTO
/// before deciding whether to persist it (e.g. a retry-key dedupe lookup
/// keyed on a fingerprint of the pre-signature fields) does not pay for a
/// signature it may discard.
pub(crate) fn build_request(
    spec: RequestSpec,
) -> Result<gents_protocol::request_admission::AgentRequestCreate> {
    let RequestSpec {
        identity,
        admission,
        initial_lifecycle_state,
        trigger_lineage,
        trigger_doc_id,
        workspace,
        subagent,
        retry,
        input,
        retry_key,
        valid_until,
    } = spec;

    let request_id = identity.request_id.clone();
    let agent_did = identity.agent_did.clone();

    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        identity.request_id,
        identity.agent_did,
        identity.requester_did.unwrap_or(agent_did),
        identity.behavior_id,
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
        create.workspace_owner_agent_did = workspace.workspace_owner_agent_did;
        create.workspace_authority = workspace.workspace_authority;
        create.workspace_seal_hash = workspace.workspace_seal_hash;
    }

    create.subagent_depth = subagent.as_ref().map_or(0, |link| link.depth);
    if let Some(link) = subagent {
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
    create.max_retries = retry
        .as_ref()
        .map_or(i64::from(DEFAULT_REQUEST_MAX_RETRIES), |link| {
            link.max_retries
        });
    create.retry_count = retry.as_ref().map_or(0, |link| link.retry_count);
    if let Some(link) = retry {
        create.retry_parent_request = link.parent_request_id;
        create.retry_parent_request_doc_id = link.parent_request_doc_id;
    }

    Ok(create)
}

/// Sign an `AgentRequestCreate` built by `build_request`, either as the
/// already-registered runtime principal named by `create.agent_did` or with
/// an explicit caller-held identity.
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

/// Build, stamp, and sign one `AgentRequestCreate` in one call. Equivalent
/// to `build_request` followed by `sign_request`; use the two-step form
/// directly when the built DTO must be inspected (e.g. fingerprinted for a
/// dedupe lookup) before a signature is worth computing.
pub async fn build_signed_request(
    spec: RequestSpec,
    signer: RequestSigner<'_>,
) -> Result<gents_protocol::request_admission::AgentRequestCreate> {
    let mut create = build_request(spec)?;
    sign_request(&mut create, signer).await?;
    Ok(create)
}

pub async fn activate_workspace_bound_request(
    node: &EmbeddedNode,
    request_doc_id: &str,
) -> Result<()> {
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{
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
        _agent_name: &str,
        agent_did: &str,
        request: AgentRequest,
        deadline_duration_secs: u64,
        execution_origin: ExecutionOrigin,
        backend_id: impl Into<String>,
    ) -> Self {
        let behavior_id = request.behavior_id.clone();
        Self {
            node,
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
            configured_max_total_tokens: None,
            claimed_deadline_at: None,
            background_completion_input_through_sequence: None,
            state: LocalLifecycleState::Pending,
            valid_until_at_claim: None,
            execution_lease: None,
            execution_lease_duration_secs: crate::config::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
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
        let mut lifecycle = Self::materialize_pending_with_execution_binding(
            node,
            agent_name,
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
        let admission =
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(&agent_did);
        let request_identity = RequestIdentity {
            requester_did: None,
            request_id: request_id.clone(),
            agent_did: agent_did.clone(),
            behavior_id: behavior_id.clone(),
            session_id: session_id.clone(),
            content: content.to_string(),
            execution_origin,
            created_at: created_at.clone(),
        };
        let spec = RequestSpec {
            trigger_lineage,
            ..RequestSpec::new(request_identity, admission)
        };
        let create = build_signed_request(spec, RequestSigner::Identity(identity.as_ref())).await?;
        let mutation = create.graphql_mutation().map_err(anyhow::Error::msg)?;
        let resp = crate::config_client::ConfigAccess::write_local(
            node.as_ref(),
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
            doc_id,
            request_id,
            agent_did: agent_did.clone(),
            requester_did: Some(agent_did.clone()),
            behavior_id,
            session_id,
            content: content.to_string(),
            max_total_tokens: None,
            input: create.input.clone(),
            execution_origin: Some(execution_origin.as_str().to_string()),
            created_at,
            deadline: None,
            execution_generation: None,
            execution_lease_expires_at: None,
            execution_progress_seq: 0,
            subagent_depth: 0,
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
            workspace_owner_agent_did: None,
            workspace_authority: None,
            workspace_seal_hash: None,
        };
        let request = crate::request_admission::verify_fresh_local_self_request(
            node.as_ref(),
            identity.as_ref(),
            &queued_request,
            agent_name,
        )
        .await?;
        let lifecycle = Self::new_with_execution_binding(
            node,
            agent_name,
            &agent_did,
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

    pub fn response_doc_id(&self) -> Option<&str> {
        self.response_doc_id.as_deref()
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

    pub fn behavior_id(&self) -> &str {
        &self.behavior_id
    }
}

/// Materialize the single session through its owner in the claim transaction.
/// Creation intent is consumed once; request observations come from the exact
/// authoritative request row after the claim mutation.
pub(super) async fn apply_request_session_projection(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    request: &AgentRequest,
    now: &str,
) -> Result<()> {
    if let Some(existing) = session::load_agent_session_row_in_txn(
        txn,
        &request.agent_did,
        &request.session_id,
        request.requester_did.as_deref(),
    )
    .await?
    {
        if existing.session.behavior_id != request.behavior_id {
            return Err(ClaimAdmissionError::SessionBehaviorMismatch {
                session_id: request.session_id.clone(),
                existing_behavior_id: existing.session.behavior_id,
                requested_behavior_id: request.behavior_id.clone(),
            }
            .into());
        }
    }
    session::ensure_session_in_txn(
        txn,
        &request.session_id,
        &request.agent_did,
        &request.behavior_id,
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
    session::reopen_session_in_txn(
        txn,
        &request.session_id,
        &request.agent_did,
        request.requester_did.as_deref(),
        now,
    )
    .await?;
    let owner = session::load_agent_session_row_in_txn(
        txn,
        &request.agent_did,
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
    session::advance_session_request_observation_in_txn(txn, incoming, &request.content, now)
        .await?;
    Ok(())
}

#[cfg(test)]
mod pin_tests {
    //! Pins today's `AgentRequestCreate::graphql_input_fields()` output for
    //! each production writer, per fixed inputs, before the writers are
    //! switched onto `build_signed_request` (#1336 Task 2). Every test here
    //! uses a deterministic signing identity (a hardcoded raw Ed25519 key,
    //! shared with the other pinning modules via `lifecycle::test_support`)
    //! so the emitted `admission_signature` is stable across runs; the only
    //! other source of nondeterminism in these writers is an internally
    //! generated `created_at` (and, at the subagent site, an internally
    //! generated `session_id`), which each test either normalizes out of
    //! the comparison or avoids by reproducing the writer's pure
    //! DTO-construction statements with a fixed timestamp in place of
    //! `Utc::now()`.
    //!
    //! Sites that require a live node beyond signing (parent/tool-call
    //! lookups, retry-key dedupe queries, claim) are exercised by
    //! reproducing their DTO-construction statements verbatim rather than
    //! by invoking the full function, since the field-stamping logic itself
    //! has no node dependency; see the per-site comment at each test.

    use super::*;
    use crate::identity::AgentIdentity;
    use crate::lifecycle::test_support::{pin_fixed_signing_identity, PIN_FIXED_DID};

    /// Replace the internally generated `created_at` and `admission_signature`
    /// field text with stable placeholders, so the rest of the field set can
    /// still be pinned with a literal `assert_eq!` even though the writer
    /// calls `Utc::now()` (and therefore signs a different payload) on every
    /// invocation.
    fn normalize_dynamic_fields(
        create: &gents_protocol::request_admission::AgentRequestCreate,
        fields: &str,
    ) -> String {
        let created_at_field = format!(
            "created_at: \"{}\"",
            escape_graphql_string(&create.created_at)
        );
        let signature_field = format!(
            "admission_signature: \"{}\"",
            bs58::encode(&create.admission.signature).into_string()
        );
        fields
            .replacen(&created_at_field, "created_at: \"<CREATED_AT>\"", 1)
            .replacen(&signature_field, "admission_signature: \"<SIGNATURE>\"", 1)
    }

    // --- Site 1: materialize.rs `build_signed_pending_agent_request_with_lineage_workspace_and_conversation_title` ---
    // Pure and public; called directly. `created_at`/`admission_signature`
    // are internally generated (`Utc::now()`), so they are normalized out.

    #[tokio::test]
    async fn pin_materialize_pending_manual_trigger() {
        let tempdir = tempfile::tempdir().unwrap();
        let _identity = pin_fixed_signing_identity(tempdir.path());

        let create =
            build_signed_pending_agent_request_with_lineage_workspace_and_conversation_title(
                PIN_FIXED_DID,
                "behavior-1",
                "hello agent",
                ExecutionOrigin::Interactive,
                TriggerLineage {
                    trigger_id: None,
                    trigger_kind: Some("manual".to_string()),
                    source_doc_id: None,
                    correlation: None,
                    trigger_context: None,
                },
                Some("My Conversation"),
                None,
                "req-materialize-pending-manual",
                "sess-materialize-pending-manual",
                None,
                None,
                None,
            )
            .await
            .expect("build signed pending manual request");

        let fields = create.graphql_input_fields().expect("graphql_input_fields");
        let normalized = normalize_dynamic_fields(&create, &fields);
        assert_eq!(
            normalized,
            "request_id: \"req-materialize-pending-manual\", agent_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", requester_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", behavior_id: \"behavior-1\", session_id: \"sess-materialize-pending-manual\", retry_root_request: \"req-materialize-pending-manual\", content: \"hello agent\", input: { initial_title: { source: \"task\", text: \"My Conversation\" } }, execution_origin: \"interactive\", caused_by_trigger_kind: \"manual\", created_at: \"<CREATED_AT>\", retry_count: 0, max_retries: 3, subagent_depth: 0, admission_kind: \"local-self\", admission_signer_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", admission_signature: \"<SIGNATURE>\", lifecycle_state: \"pending\", failure_reason: \"\""
        );
    }

    #[tokio::test]
    async fn pin_materialize_pending_event_trigger_with_workspace() {
        let tempdir = tempfile::tempdir().unwrap();
        let _identity = pin_fixed_signing_identity(tempdir.path());

        let trigger_lineage = TriggerLineage {
            trigger_id: Some("trigger-1".to_string()),
            trigger_kind: Some("event".to_string()),
            source_doc_id: Some("source-doc-1".to_string()),
            correlation: Some("corr-1".to_string()),
            trigger_context: Some(r#"{"k":"v"}"#.to_string()),
        };
        let workspace_lineage = WorkspaceLineage {
            workspace_id: Some("ws-1".to_string()),
            workspace_owner_agent_did: Some("did:key:workspace-owner".to_string()),
            workspace_authority: Some("readWrite".to_string()),
            workspace_seal_hash: Some("seal-1".to_string()),
        };

        let create =
            build_signed_pending_agent_request_with_lineage_workspace_and_conversation_title(
                PIN_FIXED_DID,
                "behavior-1",
                "hello agent",
                ExecutionOrigin::Scheduled,
                trigger_lineage,
                Some("My Conversation"),
                Some(&workspace_lineage),
                "req-materialize-pending-event",
                "sess-materialize-pending-event",
                Some("retry-key-1"),
                None,
                Some("trigger-doc-1"),
            )
            .await
            .expect("build signed pending event-triggered workspace-bound request");

        let fields = create.graphql_input_fields().expect("graphql_input_fields");
        let normalized = normalize_dynamic_fields(&create, &fields);
        assert_eq!(
            normalized,
            "request_id: \"req-materialize-pending-event\", agent_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", requester_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", behavior_id: \"behavior-1\", session_id: \"sess-materialize-pending-event\", retry_root_request: \"req-materialize-pending-event\", retry_key: \"retry-key-1\", content: \"hello agent\", input: { initial_title: { source: \"task\", text: \"My Conversation\" } }, execution_origin: \"scheduled\", caused_by_trigger_id: \"trigger-1\", caused_by_trigger_doc_id: \"trigger-doc-1\", caused_by_trigger_kind: \"event\", caused_by_correlation: \"corr-1\", caused_by_trigger_context: \"{\\\"k\\\":\\\"v\\\"}\", caused_by_source_doc_id: \"source-doc-1\", created_at: \"<CREATED_AT>\", retry_count: 0, max_retries: 3, subagent_depth: 0, workspace_id: \"ws-1\", workspace_owner_agent_did: \"did:key:workspace-owner\", workspace_authority: \"readWrite\", workspace_seal_hash: \"seal-1\", admission_kind: \"runtime-internal\", admission_signer_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", admission_signature: \"<SIGNATURE>\", runtime_issuer_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", runtime_source_request_id: \"trigger-1\", runtime_source_kind: \"automated-trigger\", lifecycle_state: \"workspaceBindingPending\", failure_reason: \"\""
        );
    }

    // --- Site 2: materialize.rs `RequestLifecycle::materialize_claimed_with_execution_binding` ---
    // This associate function claims the request against a live node in the
    // same call, so it cannot be driven directly in a field-stamping test.
    // Driven through `build_signed_request` with the equivalent `RequestSpec`
    // and `RequestSigner::Identity` (matching the production function's
    // `sign_agent_request_create(identity.as_ref(), ...)`), asserting against
    // the output pinned by reproducing the production statements directly.

    #[tokio::test]
    async fn pin_materialize_claimed_with_execution_binding() {
        let tempdir = tempfile::tempdir().unwrap();
        let identity = pin_fixed_signing_identity(tempdir.path());

        let agent_did = identity.did().to_string();
        let trigger_lineage = TriggerLineage {
            trigger_id: Some("trigger-1".to_string()),
            trigger_kind: Some("event".to_string()),
            source_doc_id: Some("source-doc-1".to_string()),
            correlation: Some("corr-1".to_string()),
            trigger_context: Some(r#"{"k":"v"}"#.to_string()),
        };

        let request_identity = RequestIdentity {
            requester_did: None,
            request_id: "req-materialize-claimed".to_string(),
            agent_did: agent_did.clone(),
            behavior_id: "agent-name-1".to_string(),
            session_id: "sess-materialize-claimed".to_string(),
            content: "resume the run".to_string(),
            execution_origin: ExecutionOrigin::Scheduled,
            created_at: "2030-01-01T00:00:00Z".to_string(),
        };
        let admission =
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(&agent_did);
        let spec = RequestSpec {
            trigger_lineage,
            ..RequestSpec::new(request_identity, admission)
        };
        let create = build_signed_request(spec, RequestSigner::Identity(&identity))
            .await
            .expect("sign claimed request");

        let fields = create.graphql_input_fields().expect("graphql_input_fields");
        assert_eq!(
            fields,
            "request_id: \"req-materialize-claimed\", agent_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", requester_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", behavior_id: \"agent-name-1\", session_id: \"sess-materialize-claimed\", retry_root_request: \"req-materialize-claimed\", content: \"resume the run\", backend_id: \"backend-1\", execution_origin: \"scheduled\", caused_by_trigger_id: \"trigger-1\", caused_by_trigger_kind: \"event\", caused_by_correlation: \"corr-1\", caused_by_trigger_context: \"{\\\"k\\\":\\\"v\\\"}\", caused_by_source_doc_id: \"source-doc-1\", created_at: \"2030-01-01T00:00:00Z\", retry_count: 0, max_retries: 3, subagent_depth: 0, admission_kind: \"local-self\", admission_signer_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", admission_signature: \"4DPGJDS77o4K6koAPWqJP5iQU55UV919NvMny273iJRN5uQYZZh3Tr76Jwh7FKQ2GDTirX8wWw5sZbHr9gd8QiqY\", lifecycle_state: \"pending\", failure_reason: \"\""
        );
    }
}
