use anyhow::Context;
use gents_protocol::request_admission::{AgentRequestAdmissionKind, AgentRequestCreate};

use gents_store::config_client::ConfigAccess;

/// A failed client submission retains whether native commit acknowledgement was
/// lost. Callers must reconcile the original signed request before replacing an
/// unconfirmed submission.
#[derive(Debug, thiserror::Error)]
pub enum ClientSubmissionError {
    #[error("client submission rejected")]
    Rejected(#[source] anyhow::Error),
    #[error("client submission commit is unconfirmed")]
    Unconfirmed(#[source] anyhow::Error),
    /// The request identity is held by a request this send cannot be: another
    /// signer's, another chat's, or more than one. Resending cannot change it.
    #[error("client request identity is held by another request")]
    IdentityConflict(#[source] anyhow::Error),
}

impl ClientSubmissionError {
    pub fn is_unconfirmed(&self) -> bool {
        matches!(self, Self::Unconfirmed(_))
    }
}

fn local_self_publication_allowed(actor: &str, requester: &str, signer: &str, agent: &str) -> bool {
    actor == signer && signer == requester && requester == agent
}

fn publication_admission_allowed(create: &AgentRequestCreate, actor: &str) -> bool {
    match create.admission.kind {
        AgentRequestAdmissionKind::Client => actor == create.admission.signer_did,
        AgentRequestAdmissionKind::LocalSelf => local_self_publication_allowed(
            actor,
            &create.requester_did,
            &create.admission.signer_did,
            &create.agent_did,
        ),
        _ => false,
    }
}

fn request_actor(access: &ConfigAccess, create: &AgentRequestCreate) -> anyhow::Result<String> {
    let actor = access
        .actor_did()
        .context("client submission requires an identity")?;
    anyhow::ensure!(
        actor == create.admission.signer_did,
        "client submission actor differs from its signer"
    );
    Ok(actor)
}

pub async fn stage_request_relationships_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    request: &str,
    agent: &str,
    session: &str,
    agent_did: &str,
) -> anyhow::Result<()> {
    txn.add_resource_relationship("AgentRequest", request, "parent", "agents", agent)
        .await?;
    txn.add_resource_relationship("AgentRequest", request, "session", "sessions", session)
        .await?;
    txn.add_actor_relationship(
        "AgentRequest",
        request,
        "working_agent",
        &identity::Did::new(agent_did)?,
    )
    .await?;
    Ok(())
}

/// Commit a selected Agent's signed runtime request with its physical parents.
pub async fn stage_runtime_request_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    create: &AgentRequestCreate,
    session: &str,
    agent: &str,
) -> anyhow::Result<String> {
    anyhow::ensure!(
        create.admission.kind == AgentRequestAdmissionKind::RuntimeInternal
            && txn.actor_did() == Some(create.agent_did.as_str())
            && create.admission.signer_did == create.agent_did
            && create.session_doc_id.as_deref() == Some(session)
            && create.agent_doc_id.as_deref() == Some(agent),
        "runtime request differs from its signing Agent or physical targets"
    );
    anyhow::ensure!(
        gents_store::identity::verify_did_signature(
            &create.admission.signer_did,
            &create.signing_payload(),
            &create.admission.signature,
        )?,
        "runtime request requires its valid signed receipt"
    );
    let observed = {
        use gents_protocol::document_query::{
            fields::agent_session as session_fields, Filter, Query, Selection,
        };
        txn.run(
            &Query::select(Selection::doc_id().field(session_fields::RELATIONSHIPS_PENDING))
                .filter(Filter::eq(session_fields::DOC_ID, session))
                .limit(2)
                .render(),
        )
        .await?
    };
    let sessions = observed["data"]["AgentSession"]
        .as_array()
        .context("runtime request session check omitted rows")?;
    anyhow::ensure!(
        matches!(sessions.as_slice(), [row] if row["_docID"] == session && row["relationships_pending"] == false),
        "runtime request session relationships are incomplete"
    );
    let fields = create.graphql_input_fields().map_err(anyhow::Error::msg)?;
    let response = txn.execute(&format!(
        "mutation {{ create_AgentRequest(input: {{ {fields}, relationships_pending: false }}) {{ _docID }} }}"
    )).await?;
    let request = gents_store::graphql::created_doc_id(&response, "AgentRequest")?;
    stage_request_relationships_in_txn(txn, &request, agent, session, &create.agent_did).await?;
    Ok(request)
}

/// Create and link a client or local-self request within the transaction that
/// verifies its physical session and selected Agent. Legacy pending publications
/// retain their separate authenticated recovery path.
pub async fn stage_client_request_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    create: &AgentRequestCreate,
    session: &str,
    agent: &str,
) -> anyhow::Result<String> {
    anyhow::ensure!(
        txn.actor_did()
            .is_some_and(|actor| publication_admission_allowed(create, actor))
            && create.session_doc_id.as_deref() == Some(session)
            && create.agent_doc_id.as_deref() == Some(agent),
        "request differs from its authenticated signer or physical targets"
    );
    anyhow::ensure!(
        gents_store::identity::verify_did_signature(
            &create.admission.signer_did,
            &create.signing_payload(),
            &create.admission.signature,
        )?,
        "client request requires its valid signed receipt"
    );
    let observed = {
        use gents_protocol::document_query::{
            fields::agent_session as session_fields, Filter, Query, Selection,
        };
        txn.run(
            &Query::select(Selection::doc_id().field(session_fields::RELATIONSHIPS_PENDING))
                .filter(Filter::eq(session_fields::DOC_ID, session))
                .limit(2)
                .render(),
        )
        .await?
    };
    let sessions = observed["data"]["AgentSession"]
        .as_array()
        .context("client request session check omitted rows")?;
    anyhow::ensure!(
        sessions.len() == 1
            && sessions[0]["_docID"] == session
            && sessions[0]["relationships_pending"] == false,
        "client request session relationships are incomplete"
    );
    // `AgentRequest.request_id` is only `@index`: a client create never adds
    // a second request under an identity the actor can already read.
    let held = {
        use gents_protocol::document_query::{
            fields::agent_request as request_fields, Filter, Query, Selection,
        };
        txn.run(
            &Query::select(Selection::doc_id())
                .filter(Filter::eq(request_fields::REQUEST_ID, &create.request_id))
                .limit(1)
                .render(),
        )
        .await?
    };
    anyhow::ensure!(
        held["data"]["AgentRequest"]
            .as_array()
            .context("client request identity check omitted rows")?
            .is_empty(),
        "client request identity {} is already held",
        create.request_id
    );
    let input = gents_store::config_client::client_request_input(create)?;
    let response = txn.execute_client_signed_create(
        "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
        &serde_json::json!({"input": input}),
        gents_protocol::schemas::AGENT_REQUEST,
    ).await?;
    let request = gents_store::graphql::created_doc_id(&response, "AgentRequest")?;
    stage_request_relationships_in_txn(txn, &request, agent, session, &create.agent_did).await?;
    Ok(request)
}

/// Stage an Agent-authored child while its new session still awaits the
/// existing creator-owned relationship publication protocol.
pub(crate) async fn stage_pending_child_request_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    create: &AgentRequestCreate,
    session: &str,
    agent: &str,
) -> anyhow::Result<String> {
    anyhow::ensure!(
        create.admission.kind == AgentRequestAdmissionKind::Client
            && txn
                .actor_did()
                .is_some_and(|actor| publication_admission_allowed(create, actor))
            && txn
                .managed_agent_signer()
                .is_some_and(|signer| signer.did() == create.admission.signer_did)
            && create.session_doc_id.as_deref() == Some(session)
            && create.agent_doc_id.as_deref() == Some(agent),
        "pending child differs from its signing Agent or physical targets"
    );
    anyhow::ensure!(
        gents_store::identity::verify_did_signature(
            &create.admission.signer_did,
            &create.signing_payload(),
            &create.admission.signature,
        )?,
        "pending child requires its valid signed receipt"
    );
    let observed = {
        use gents_protocol::document_query::{
            fields::agent_session as session_fields, Filter, Query, Selection,
        };
        txn.run(
            &Query::select(Selection::doc_id().field(session_fields::RELATIONSHIPS_PENDING))
                .filter(Filter::eq(session_fields::DOC_ID, session))
                .limit(2)
                .render(),
        )
        .await?
    };
    let sessions = observed["data"]["AgentSession"]
        .as_array()
        .context("pending child session check omitted rows")?;
    anyhow::ensure!(
        matches!(sessions.as_slice(), [row] if row["_docID"] == session && row["relationships_pending"] == true),
        "pending child session is unavailable or already published"
    );
    let response = txn
        .execute(
            &create
                .graphql_client_publication_mutation()
                .map_err(anyhow::Error::msg)?,
        )
        .await?;
    Ok(gents_store::graphql::created_doc_id(
        &response,
        "AgentRequest",
    )?)
}

/// How another owner's transaction publishes a signed request before it is
/// staged into its session.
pub struct StageOptions<'a> {
    /// The request document this signed create already published (an
    /// idempotent retry); `None` publishes it in this transaction.
    pub published_doc_id: Option<&'a str>,
    /// A verified client graph publishes the request under its own signer.
    pub signer: Option<&'a std::sync::Arc<db::block::builder::VerifiedClientCreate>>,
    pub reserved_runtime_session: bool,
}

/// Publish an admitted request through its admission kind, reread it, and
/// stage its session with the created-session provenance, in that order.
pub async fn stage_admitted_request_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    create: &AgentRequestCreate,
    session_doc: &str,
    agent_doc: &str,
    options: StageOptions<'_>,
) -> anyhow::Result<String> {
    let doc_id = if let Some(existing) = options.published_doc_id {
        existing.to_owned()
    } else if let Some(graph) = options.signer {
        let session = create
            .session_doc_id
            .as_deref()
            .context("client Goal request has no physical session")?;
        let agent = create
            .agent_doc_id
            .as_deref()
            .context("client Goal request has no physical Agent")?;
        txn.stage_verified_edit_request(create, graph.clone(), session, agent)
            .await?
    } else {
        match create.admission.kind {
            AgentRequestAdmissionKind::RuntimeInternal => {
                stage_runtime_request_in_txn(txn, create, session_doc, agent_doc).await?
            }
            AgentRequestAdmissionKind::Client | AgentRequestAdmissionKind::LocalSelf => {
                let response = {
                    use gents_protocol::document_query::{
                        fields::agent_session as session_fields, Filter, Query, Selection,
                    };
                    txn.run(
                        &Query::select(
                            Selection::new().field(session_fields::RELATIONSHIPS_PENDING),
                        )
                        .filter(Filter::eq(session_fields::DOC_ID, session_doc))
                        .limit(2)
                        .render(),
                    )
                    .await?
                };
                let rows = response["data"]["AgentSession"]
                    .as_array()
                    .context("goal session publication state unavailable")?;
                let [row] = rows.as_slice() else {
                    anyhow::bail!("goal session publication state ambiguous")
                };
                if row["relationships_pending"] == true {
                    stage_pending_child_request_in_txn(txn, create, session_doc, agent_doc).await?
                } else {
                    stage_client_request_in_txn(txn, create, session_doc, agent_doc).await?
                }
            }
            _ => anyhow::bail!("unsupported Goal-backed request admission"),
        }
    };
    let response = txn
        .execute(&format!(
            r#"{{ AgentRequest(docIDs: ["{}"]) {{ {} }} }}"#,
            gents_store::graphql::escape_graphql_string(&doc_id),
            gents_protocol::row::AGENT_REQUEST_FIELDS,
        ))
        .await?;
    let rows = response["data"]["AgentRequest"]
        .as_array()
        .context("staged request lookup omitted rows")?;
    anyhow::ensure!(rows.len() == 1, "staged request is missing or ambiguous");
    let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(rows[0].clone())?;
    let request = gents_protocol::row::AgentRequest::try_from(row)?;
    let provenance = super::materialize::created_session_provenance_in_txn(txn, &request).await?;
    if options.reserved_runtime_session {
        crate::session::finalize_reserved_runtime_session_in_txn(
            txn,
            session_doc,
            &request,
            provenance.clone(),
        )
        .await?;
    }
    crate::session::ensure_session_in_txn(
        txn,
        &create.session_id,
        &create.node_did,
        &create.agent_did,
        Some(&create.requester_did),
        create.input.initial_title.clone(),
        provenance,
        &create.created_at,
    )
    .await?;
    Ok(doc_id)
}

pub async fn ensure_request_session_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    create: &AgentRequestCreate,
    actor: &str,
) -> anyhow::Result<(String, String, bool)> {
    let existing = gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
        txn,
        &create.node_did,
        &create.session_id,
    )
    .await?;
    if let Some(existing) = existing {
        anyhow::ensure!(
            existing.session.requester_did.as_deref() == Some(create.requester_did.as_str()),
            "client request changes session requester provenance"
        );
        anyhow::ensure!(
            existing.session.agent_did == create.agent_did,
            "client request changes selected session agent"
        );
    } else {
        anyhow::ensure!(
            create.requester_did == actor,
            "new client session requester differs from its creator"
        );
    }
    let created = crate::session::ensure_reserved_session_in_txn(
        txn,
        &create.session_id,
        &create.node_did,
        &create.agent_did,
        Some(&create.requester_did),
        create.input.initial_title.clone(),
        None,
        &create.created_at,
    )
    .await?;
    let session = gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
        txn,
        &create.node_did,
        &create.session_id,
    )
    .await?
    .context("reserved client session disappeared")?
    .doc_id;
    let (agent, _) = gents_documents::read_desired_state_record_in_txn(
        txn,
        gents_store::collection::Collection::Agent,
        &create.node_did,
        &create.agent_did,
    )
    .await?
    .context("selected agent is unavailable")?;
    if created {
        crate::session::stage_session_relationships_in_txn(
            txn,
            &session,
            &agent,
            &identity::Did::new(&create.agent_did)?,
        )
        .await?;
    }
    Ok((session, agent, created))
}

/// Reserve a session and its selected agent relationship as the authenticated
/// creator before signing a goal command. Repeating it retains the exact
/// existing session, including after an unconfirmed commit.
pub async fn reserve_goal_session(
    access: &ConfigAccess,
    create: &AgentRequestCreate,
) -> Result<String, ClientSubmissionError> {
    let (session, _) = reserve_client_targets(access, create).await?;
    Ok(session)
}

pub async fn reserve_client_targets(
    access: &ConfigAccess,
    create: &AgentRequestCreate,
) -> Result<(String, String), ClientSubmissionError> {
    let actor = request_actor(access, create).map_err(ClientSubmissionError::Rejected)?;
    // The reservation reads the session and selected Agent that the runtime
    // and the User's configuration keep writing; the callback re-reads them
    // and is transaction-only, so a confirmed conflict is rebuilt and an
    // existing session is bound again (ClientSubmissionContention.tla).
    let (session, agent, created) = access
        .transact_signed_conflict_rebuild_with_finality("request.client.reserve_session", |txn| {
            let actor = &actor;
            Box::pin(async move { ensure_request_session_in_txn(txn, create, actor).await })
        })
        .await
        .map_err(|failure| {
            if failure.unconfirmed {
                ClientSubmissionError::Unconfirmed(failure.error)
            } else {
                ClientSubmissionError::Rejected(failure.error)
            }
        })?;
    if !created {
        crate::session::grant_created_session_relationships(
            access,
            &session,
            &agent,
            &create.agent_did,
        )
        .await
        .map_err(ClientSubmissionError::Unconfirmed)?;
    }
    Ok((session, agent))
}

/// Reserve the physical session and selected Agent before signing client work.
pub async fn build_signed_client_request(
    access: &ConfigAccess,
    spec: super::RequestSpec,
    signer: super::RequestSigner<'_>,
) -> anyhow::Result<AgentRequestCreate> {
    let mut create = super::materialize::build_request(spec)?;
    let actor = request_actor(access, &create)?;
    anyhow::ensure!(
        publication_admission_allowed(&create, &actor),
        "request publication admission is not allowed"
    );
    let (session, agent) = reserve_client_targets(access, &create).await?;
    create.session_doc_id = Some(session);
    create.agent_doc_id = Some(agent);
    super::materialize::sign_request(&mut create, signer).await?;
    Ok(create)
}

/// Bind a retry to the existing targets inside its publication transaction.
pub async fn build_signed_client_request_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    spec: super::RequestSpec,
    signer: super::RequestSigner<'_>,
) -> anyhow::Result<AgentRequestCreate> {
    let mut create = super::materialize::build_request(spec)?;
    bind_client_targets_in_txn(txn, &mut create).await?;
    super::materialize::sign_request(&mut create, signer).await?;
    Ok(create)
}

pub(crate) async fn build_signed_edit_request_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    mut create: AgentRequestCreate,
    signer: super::RequestSigner<'_>,
) -> anyhow::Result<AgentRequestCreate> {
    anyhow::ensure!(
        txn.actor_did() == Some(create.admission.signer_did.as_str()),
        "edit signer differs from transaction actor"
    );
    bind_existing_targets_in_txn(txn, &mut create).await?;
    super::materialize::sign_request(&mut create, signer).await?;
    Ok(create)
}

pub async fn bind_client_targets_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    create: &mut AgentRequestCreate,
) -> anyhow::Result<()> {
    if create.admission.kind == AgentRequestAdmissionKind::Client {
        bind_existing_targets_in_txn(txn, create).await?;
    }
    Ok(())
}

async fn bind_existing_targets_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    create: &mut AgentRequestCreate,
) -> anyhow::Result<()> {
    let session = gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
        txn,
        &create.node_did,
        &create.session_id,
    )
    .await?
    .context("client request session is unavailable")?;
    anyhow::ensure!(
        session.session.agent_did == create.agent_did
            && session.session.requester_did.as_deref() == Some(create.requester_did.as_str()),
        "client request session scope changed"
    );
    let (agent, _) = gents_documents::read_desired_state_record_in_txn(
        txn,
        gents_store::collection::Collection::Agent,
        &create.node_did,
        &create.agent_did,
    )
    .await?
    .context("selected agent is unavailable")?;
    create.session_doc_id = Some(session.doc_id);
    create.agent_doc_id = Some(agent);
    Ok(())
}

/// Publishes a signed request against its previously reserved physical targets.
/// Existing session provenance is immutable; its requester is not the acting
/// client's identity. Native ACP authorizes the transaction's explicit actor.
pub async fn submit_client_request(
    access: &ConfigAccess,
    create: &AgentRequestCreate,
) -> Result<(), ClientSubmissionError> {
    let actor = request_actor(access, create).map_err(ClientSubmissionError::Rejected)?;
    if !publication_admission_allowed(create, &actor) {
        return Err(ClientSubmissionError::Rejected(anyhow::anyhow!(
            "request publication admission is not allowed"
        )));
    }
    // The working Agent refreshes the session observation as it publishes
    // each message, so a submission during a turn can lose its session read;
    // the callback re-reads its targets and is transaction-only, so a
    // confirmed conflict is rebuilt (ClientSubmissionContention.tla).
    access
        .transact_signed_conflict_rebuild_with_finality("request.client.submit", |txn| {
            let actor = &actor;
            Box::pin(async move {
                let (session, agent, _created) =
                    ensure_request_session_in_txn(txn, create, actor).await?;
                anyhow::ensure!(
                    create.session_doc_id.as_deref() == Some(session.as_str())
                        && create.agent_doc_id.as_deref() == Some(agent.as_str()),
                    "client request physical targets changed before publication"
                );
                stage_client_request_in_txn(txn, create, &session, &agent).await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .map_err(|failure| {
            if failure.unconfirmed {
                ClientSubmissionError::Unconfirmed(failure.error)
            } else {
                ClientSubmissionError::Rejected(failure.error)
            }
        })
}

/// The owner's idempotency key for a client-authored send: the signer and
/// the request identity the client picked when the person sent. The key is
/// part of the signed create, and `AgentRequest.retry_key` is a unique index,
/// so two concurrent creates of one identity commit once (Lean
/// `ClientOptimisticSend.create`).
pub fn client_request_key(signer_did: &str, request_id: &str) -> String {
    format!("client-request:{signer_did}:{request_id}")
}

/// The chat a client send's identity must belong to.
#[derive(Debug, Clone, Copy)]
pub struct ClientRequestScope<'a> {
    pub node_did: &'a str,
    pub session_id: &'a str,
    pub requester_did: &'a str,
    pub request_id: &'a str,
}

impl<'a> ClientRequestScope<'a> {
    pub fn of(create: &'a AgentRequestCreate) -> Self {
        Self {
            node_did: &create.node_did,
            session_id: &create.session_id,
            requester_did: &create.requester_did,
            request_id: &create.request_id,
        }
    }
}

/// The request the runtime holds under a client send's identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldClientRequest {
    pub request_id: String,
    pub session_id: String,
    pub agent_did: Option<String>,
}

/// Send a signed client create, idempotent on its request identity (Lean
/// `ClientOptimisticSend.create`): a repeat of an identity the runtime
/// already holds is that request, whichever send of it committed (a retry
/// after a lost reply, two windows, a retry racing the first send still
/// committing, a send committed before the key existed). Returns the
/// identity of the held request. A failure with no request under the
/// identity is the send's failure; the caller shows it failed and may resend
/// the same identity.
pub async fn send_client_request(
    access: &ConfigAccess,
    create: &AgentRequestCreate,
) -> Result<String, ClientSubmissionError> {
    let key = client_request_key(&create.admission.signer_did, &create.request_id);
    if create.retry_key.as_deref() != Some(key.as_str()) {
        return Err(ClientSubmissionError::Rejected(anyhow::anyhow!(
            "a client send's idempotency key must be its request identity's key"
        )));
    }
    let scope = ClientRequestScope::of(create);
    if let Some(held) = held_client_request(access, scope).await? {
        return Ok(held.request_id);
    }
    let error = match submit_client_request(access, create).await {
        Ok(()) => return Ok(create.request_id.clone()),
        Err(error) => error,
    };
    match held_client_request(access, scope).await {
        Ok(Some(held)) => Ok(held.request_id),
        Ok(None) => Err(error),
        Err(ClientSubmissionError::Unconfirmed(read)) => Err(ClientSubmissionError::Unconfirmed(
            anyhow::Error::from(error).context(format!("{read:#}")),
        )),
        Err(other) => Err(other),
    }
}

/// Send a signed client retry of a failed turn, idempotent on its parent:
/// the `retry:doc:<parent>` key is a unique index, so a repeat of the retry
/// (another click, another window, a resend after a lost reply, a concurrent
/// retry under another successor identity) returns the one successor the
/// runtime holds ([`reconcile_client_retry`]) instead of creating a second.
/// A failure with no successor held is the retry's failure.
pub async fn send_client_retry(
    access: &ConfigAccess,
    create: &AgentRequestCreate,
) -> Result<String, ClientSubmissionError> {
    let parent_doc_id = create
        .retry_parent_request_doc_id
        .as_deref()
        .filter(|parent| !parent.is_empty())
        .ok_or_else(|| {
            ClientSubmissionError::Rejected(anyhow::anyhow!(
                "a client retry must name its parent document"
            ))
        })?;
    if create.retry_key.as_deref() != Some(format!("retry:doc:{parent_doc_id}").as_str()) {
        return Err(ClientSubmissionError::Rejected(anyhow::anyhow!(
            "a client retry's idempotency key must be its parent's retry key"
        )));
    }
    let held = || {
        reconcile_client_retry(
            access,
            &create.node_did,
            &create.session_id,
            &create.requester_did,
            parent_doc_id,
        )
    };
    if let Some(successor) = held().await? {
        return Ok(successor);
    }
    let error = match submit_client_request(access, create).await {
        Ok(()) => return Ok(create.request_id.clone()),
        Err(error) => error,
    };
    match held().await {
        Ok(Some(successor)) => Ok(successor),
        Ok(None) => Err(error),
        Err(ClientSubmissionError::Unconfirmed(read)) => Err(ClientSubmissionError::Unconfirmed(
            anyhow::Error::from(error).context(format!("{read:#}")),
        )),
        Err(other) => Err(other),
    }
}

/// The request the runtime holds under a client send's identity, with its
/// relationships completed. It is found by `request_id`, so a request
/// committed before its idempotency key existed is found too. A request
/// under the identity that is not this signer's request in `scope` is an
/// [`ClientSubmissionError::IdentityConflict`], never a retryable failure.
pub async fn held_client_request(
    access: &ConfigAccess,
    scope: ClientRequestScope<'_>,
) -> Result<Option<HeldClientRequest>, ClientSubmissionError> {
    let actor = access
        .actor_did()
        .context("a client request lookup requires an identity")
        .map_err(ClientSubmissionError::Rejected)?;
    let found = find_signed_client_request(access, "request_id", scope.request_id, &actor)
        .await
        .map_err(ClientSubmissionError::Unconfirmed)?;
    let (row, pending) = match found {
        None => return Ok(None),
        Some(SignedClientRow::Conflict(reason)) => {
            return Err(ClientSubmissionError::IdentityConflict(anyhow::anyhow!(
                "request identity {}: {reason}",
                scope.request_id
            )))
        }
        Some(SignedClientRow::Owned {
            row,
            relationships_pending,
        }) => (row, relationships_pending),
    };
    if row.node_did.as_deref() != Some(scope.node_did)
        || row.session_id.as_deref() != Some(scope.session_id)
        || row.requester_did.as_deref() != Some(scope.requester_did)
    {
        return Err(ClientSubmissionError::IdentityConflict(anyhow::anyhow!(
            "request identity {} is held by a request in another chat",
            scope.request_id
        )));
    }
    match pending {
        Some(false) => {}
        Some(true) => {
            let doc = row
                .doc_id
                .as_deref()
                .context("client request lookup omitted document identity")
                .map_err(ClientSubmissionError::Unconfirmed)?;
            complete_client_request_relationships(access, doc)
                .await
                .map_err(ClientSubmissionError::Unconfirmed)?;
        }
        None => {
            return Err(ClientSubmissionError::Unconfirmed(anyhow::anyhow!(
                "client request publication marker is unavailable"
            )))
        }
    }
    Ok(Some(HeldClientRequest {
        request_id: row.request_id,
        session_id: row.session_id.unwrap_or_default(),
        agent_did: row.agent_did,
    }))
}

/// A signed client request the actor can read under one lookup field.
enum SignedClientRow {
    /// Created and natively owned by the actor, with a valid creation receipt.
    Owned {
        row: Box<gents_protocol::row::AgentRequestRow>,
        relationships_pending: Option<bool>,
    },
    /// Another signer's or owner's request, or more than one request.
    Conflict(&'static str),
}

/// The one signed client request under `field = value`, verified against its
/// creation receipt and its native document owner. `field` is a code-owned
/// `AgentRequest` field name.
async fn find_signed_client_request(
    access: &ConfigAccess,
    field: &'static str,
    value: &str,
    actor: &str,
) -> anyhow::Result<Option<SignedClientRow>> {
    let escaped = gents_store::graphql::escape_graphql_string(value);
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ {field}: {{ _eq: "{escaped}" }} }}, limit: 2) {{ {} relationships_pending }} }}"#,
            gents_protocol::request_admission::SIGNED_REQUEST_FIELDS,
        ))
        .await?;
    let rows = response["data"]["AgentRequest"]
        .as_array()
        .context("client request lookup omitted rows")?;
    let value = match rows.as_slice() {
        [] => return Ok(None),
        [value] => value,
        _ => {
            return Ok(Some(SignedClientRow::Conflict(
                "it names more than one request",
            )))
        }
    };
    let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(value.clone())?;
    gents_store::identity::request_receipt::verify_request_receipt_signature(&row)?;
    if row.admission_kind.as_deref() != Some("client")
        || row.admission_signer_did.as_deref() != Some(actor)
    {
        return Ok(Some(SignedClientRow::Conflict(
            "it is held by another signer's request",
        )));
    }
    let doc = row
        .doc_id
        .clone()
        .context("client request lookup omitted document identity")?;
    let owned = access
        .transact_readonly("request.client.held_owner", |txn| {
            let doc = doc.as_str();
            Box::pin(async move {
                let owners = txn
                    .document_owners("AgentRequest", &[doc.to_owned()])
                    .await?;
                Ok::<_, anyhow::Error>(
                    owners.len() == 1
                        && owners[0]
                            .owner
                            .as_ref()
                            .is_some_and(|owner| owner.as_str() == actor),
                )
            })
        })
        .await?;
    if !owned {
        return Ok(Some(SignedClientRow::Conflict(
            "its request is owned by another identity",
        )));
    }
    Ok(Some(SignedClientRow::Owned {
        relationships_pending: value["relationships_pending"].as_bool(),
        row: Box::new(row),
    }))
}

async fn publish_request_relationships(
    access: &ConfigAccess,
    agent_did: &str,
    session: &str,
    agent: &str,
    request_doc: &str,
) -> anyhow::Result<()> {
    let actor = access
        .actor_did()
        .context("request relationship publication requires an actor")?;
    let pending = access
        .transact_readonly("request.client.relationships_precheck", |txn| {
            let actor = actor.as_str();
            Box::pin(async move {
                validate_request_relationship_binding(
                    txn,
                    actor,
                    agent_did,
                    session,
                    agent,
                    request_doc,
                )
                .await
            })
        })
        .await?;
    if !pending.0 {
        return Ok(());
    }
    crate::session::grant_created_session_relationships(access, session, agent, agent_did).await?;
    let worker = identity::Did::new(agent_did)?;
    access
        .transact_write("request.client.relationships_published", |txn| {
            let actor = actor.as_str();
            let worker = &worker;
            Box::pin(async move {
                let (pending, session_ready) = validate_request_relationship_binding(
                    txn, actor, agent_did, session, agent, request_doc,
                ).await?;
                if !pending {
                    return Ok::<_, anyhow::Error>(());
                }
                anyhow::ensure!(session_ready, "request session relationships are not yet published");
                let doc = gents_store::graphql::escape_graphql_string(request_doc);
                let changed = txn.execute_client_signed_update(&format!(
                    r#"mutation($input: AgentRequestMutationInputArg!) {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }}, relationships_pending: {{ _eq: true }} }}, input: $input) {{ _docID }} }}"#,
                ), &serde_json::json!({"input": {"relationships_pending": false}}), gents_protocol::schemas::AGENT_REQUEST).await?;
                anyhow::ensure!(changed["data"]["update_AgentRequest"].as_array().is_some_and(|rows|
                    rows.len() == 1 && rows[0]["_docID"].as_str() == Some(request_doc)),
                    "request relationship completion was not recorded");
                stage_request_relationships_in_txn(txn, request_doc, agent, session, worker.as_str()).await?;
                Ok(())
            })
        })
        .await
}

async fn validate_request_relationship_binding(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    actor: &str,
    agent_did: &str,
    session: &str,
    agent: &str,
    request_doc: &str,
) -> anyhow::Result<(bool, bool)> {
    anyhow::ensure!(
        txn.actor_did() == Some(actor),
        "request publication actor changed"
    );
    let response = {
        use gents_protocol::document_query::{
            fields::{
                agent as agent_fields, agent_request as request_fields,
                agent_session as session_fields,
            },
            Filter, Query, QuerySet, Selection,
        };
        txn.run(
            &QuerySet::new()
                .plain(
                    &Query::select(
                        Selection::owned(gents_protocol::request_admission::SIGNED_REQUEST_FIELDS)
                            .field(request_fields::RELATIONSHIPS_PENDING),
                    )
                    .filter(Filter::eq(request_fields::DOC_ID, request_doc))
                    .limit(2),
                )
                .plain(
                    &Query::select(
                        Selection::doc_id()
                            .field(session_fields::NODE_DID)
                            .field(session_fields::SESSION_ID)
                            .field(session_fields::REQUESTER_DID)
                            .field(session_fields::AGENT_DID)
                            .field(session_fields::RELATIONSHIPS_PENDING),
                    )
                    .filter(Filter::eq(session_fields::DOC_ID, session))
                    .limit(2),
                )
                .plain(
                    &Query::select(
                        Selection::doc_id()
                            .field(agent_fields::NODE_DID)
                            .field(agent_fields::AGENT_DID),
                    )
                    .filter(Filter::eq(agent_fields::DOC_ID, agent))
                    .limit(2),
                )
                .render(),
        )
        .await?
    };
    let rows = response["data"]["AgentRequest"]
        .as_array()
        .context("request publication omitted request rows")?;
    let [value] = rows.as_slice() else {
        anyhow::bail!("request publication requires one visible request");
    };
    let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(value.clone())?;
    gents_store::identity::request_receipt::verify_request_receipt_signature(&row)?;
    anyhow::ensure!(
        row.doc_id.as_deref() == Some(request_doc)
            && row.admission_signer_did.as_deref() == Some(actor)
            && row.session_doc_id.as_deref() == Some(session)
            && row.agent_doc_id.as_deref() == Some(agent)
            && row.agent_did.as_deref() == Some(agent_did)
            && matches!(row.admission_kind.as_deref(), Some("client" | "local-self")),
        "request publication differs from the signed actor or physical targets"
    );
    if row.admission_kind.as_deref() == Some("local-self") {
        anyhow::ensure!(
            row.requester_did.as_deref() == Some(actor) && actor == agent_did,
            "local-self publication differs from its selected agent"
        );
    }
    let owners = txn
        .document_owners("AgentRequest", &[request_doc.to_owned()])
        .await?;
    anyhow::ensure!(
        owners.len() == 1
            && owners[0].doc_id == request_doc
            && owners[0].visible
            && owners[0]
                .owner
                .as_ref()
                .is_some_and(|owner| owner.as_str() == actor),
        "request publication native owner differs from its signer"
    );
    let sessions = response["data"]["AgentSession"]
        .as_array()
        .context("request publication omitted session rows")?;
    let [target] = sessions.as_slice() else {
        anyhow::bail!("request publication requires one visible session");
    };
    anyhow::ensure!(
        target["_docID"].as_str() == Some(session)
            && target["node_did"].as_str() == row.node_did.as_deref()
            && target["session_id"].as_str() == row.session_id.as_deref()
            && target["requester_did"].as_str() == row.requester_did.as_deref()
            && target["agent_did"].as_str() == Some(agent_did),
        "request publication session differs from its signed target"
    );
    let agents = response["data"]["Agent"]
        .as_array()
        .context("request publication omitted Agent rows")?;
    let [target_agent] = agents.as_slice() else {
        anyhow::bail!("request publication requires one visible Agent");
    };
    anyhow::ensure!(
        target_agent["_docID"].as_str() == Some(agent)
            && target_agent["node_did"].as_str() == row.node_did.as_deref()
            && target_agent["agent_did"].as_str() == Some(agent_did),
        "request publication Agent differs from its signed target"
    );
    let pending = value["relationships_pending"]
        .as_bool()
        .context("request relationship publication marker is unavailable")?;
    Ok((pending, target["relationships_pending"] == false))
}

/// Resolve a retained signed client submission and finish its pending grants.
/// An absent request is not proof that an earlier commit cannot still arrive.
/// Completed markers are never replayed, preserving subsequent revocations.
pub async fn reconcile_client_request(
    access: &ConfigAccess,
    create: &AgentRequestCreate,
) -> Result<bool, ClientSubmissionError> {
    let actor = request_actor(access, create).map_err(ClientSubmissionError::Rejected)?;
    if create.admission.kind != AgentRequestAdmissionKind::Client {
        return Err(ClientSubmissionError::Rejected(anyhow::anyhow!(
            "client reconciliation requires Client admission"
        )));
    }
    reconcile_client_request_inner(access, create, &actor)
        .await
        .map_err(ClientSubmissionError::Unconfirmed)
}

async fn reconcile_client_request_inner(
    access: &ConfigAccess,
    create: &AgentRequestCreate,
    actor: &str,
) -> anyhow::Result<bool> {
    let response = {
        use gents_protocol::document_query::{
            fields::agent_request as request_fields, session_scope, Filter, Query, Selection,
        };
        access
            .read(
                &Query::select(
                    Selection::owned(gents_protocol::request_admission::SIGNED_REQUEST_FIELDS)
                        .field(request_fields::RELATIONSHIPS_PENDING),
                )
                .filter(session_scope(
                    &create.node_did,
                    &create.session_id,
                    Some(create.requester_did.as_str()),
                ))
                .filter(Filter::eq(request_fields::REQUEST_ID, &create.request_id))
                .limit(2)
                .render(),
            )
            .await?
    };
    let rows = response["data"]["AgentRequest"]
        .as_array()
        .context("request recovery omitted rows")?;
    anyhow::ensure!(rows.len() <= 1, "ambiguous client request recovery");
    let Some(value) = rows.first() else {
        return Ok(false);
    };
    let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(value.clone())?;
    gents_store::identity::request_receipt::verify_request_receipt_signature(&row)?;
    let signature = bs58::encode(&create.admission.signature).into_string();
    anyhow::ensure!(
        row.request_id == create.request_id
            && row.node_did.as_deref() == Some(create.node_did.as_str())
            && row.session_id.as_deref() == Some(create.session_id.as_str())
            && row.requester_did.as_deref() == Some(create.requester_did.as_str())
            && row.agent_did.as_deref() == Some(create.agent_did.as_str())
            && row.session_doc_id == create.session_doc_id
            && row.agent_doc_id == create.agent_doc_id
            && row.admission_signer_did.as_deref() == Some(actor)
            && row.admission_signature.as_deref() == Some(signature.as_str()),
        "client request recovery differs from retained signed request"
    );
    match value["relationships_pending"].as_bool() {
        Some(false) => return Ok(true),
        Some(true) => {}
        None => anyhow::bail!("client request publication marker is unavailable"),
    }
    let request_doc = row
        .doc_id
        .context("client request recovery omitted document identity")?;
    let (session, agent) = access
        .transact_readonly("request.client.recovery_scope", |txn| {
            Box::pin(async move {
                let session =
                    gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                        txn,
                        &create.node_did,
                        &create.session_id,
                    )
                    .await?
                    .context("client request session is unavailable")?;
                anyhow::ensure!(
                    session.session.agent_did == create.agent_did
                        && session.session.requester_did.as_deref()
                            == Some(create.requester_did.as_str()),
                    "client request session differs from retained request"
                );
                let (agent, _) = gents_documents::read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Agent,
                    &create.node_did,
                    &create.agent_did,
                )
                .await?
                .context("selected agent is unavailable")?;
                anyhow::ensure!(
                    create.session_doc_id.as_deref() == Some(session.doc_id.as_str())
                        && create.agent_doc_id.as_deref() == Some(agent.as_str()),
                    "client request physical targets changed before recovery"
                );
                Ok::<_, anyhow::Error>((session.doc_id, agent))
            })
        })
        .await?;
    publish_request_relationships(access, &create.agent_did, &session, &agent, &request_doc)
        .await?;
    Ok(true)
}

/// Complete a committed client-owned request's native grants from its signed
/// physical targets. A completed marker is never replayed after a revocation.
pub async fn complete_client_request_relationships(
    access: &ConfigAccess,
    request_doc: &str,
) -> anyhow::Result<()> {
    let actor = access
        .actor_did()
        .context("request relationship recovery requires an actor")?;
    let response = {
        use gents_protocol::document_query::{
            fields::agent_request as request_fields, Filter, Query, Selection,
        };
        access
            .read(
                &Query::select(
                    Selection::owned(gents_protocol::request_admission::SIGNED_REQUEST_FIELDS)
                        .field(request_fields::RELATIONSHIPS_PENDING),
                )
                .filter(Filter::eq(request_fields::DOC_ID, request_doc))
                .limit(2)
                .render(),
            )
            .await?
    };
    let rows = response["data"]["AgentRequest"]
        .as_array()
        .context("request relationship recovery omitted rows")?;
    let [value] = rows.as_slice() else {
        anyhow::bail!("request relationship recovery requires one visible document");
    };
    let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(value.clone())?;
    gents_store::identity::request_receipt::verify_request_receipt_signature(&row)?;
    anyhow::ensure!(
        row.doc_id.as_deref() == Some(request_doc),
        "request physical identity changed"
    );
    match value["relationships_pending"].as_bool() {
        Some(false) => return Ok(()),
        Some(true) => {}
        None => anyhow::bail!("request publication marker is unavailable"),
    }
    anyhow::ensure!(
        row.admission_kind.as_deref() == Some("client")
            && row.admission_signer_did.as_deref() == Some(actor.as_str()),
        "pending request grants require the authenticated client signer"
    );
    let node_did = row.node_did.as_deref().context("request has no node")?;
    let session_id = row
        .session_id
        .as_deref()
        .context("request has no session")?;
    let requester_did = row
        .requester_did
        .as_deref()
        .context("request has no requester")?;
    let agent_did = row.agent_did.as_deref().context("request has no agent")?;
    let session_doc = row
        .session_doc_id
        .as_deref()
        .context("request has no session document")?;
    let agent_doc = row
        .agent_doc_id
        .as_deref()
        .context("request has no agent document")?;
    access
        .transact_readonly("request.client.publication_recovery_scope", |txn| {
            let actor = actor.as_str();
            Box::pin(async move {
                let owners = txn
                    .document_owners("AgentRequest", &[request_doc.to_owned()])
                    .await?;
                anyhow::ensure!(
                    owners.len() == 1
                        && owners[0]
                            .owner
                            .as_ref()
                            .is_some_and(|owner| owner.as_str() == actor),
                    "request creator differs from the authenticated signer"
                );
                let session =
                    gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                        txn, node_did, session_id,
                    )
                    .await?
                    .context("request session is unavailable")?;
                let (agent, _) = gents_documents::read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Agent,
                    node_did,
                    agent_did,
                )
                .await?
                .context("selected agent is unavailable")?;
                anyhow::ensure!(
                    session.doc_id == session_doc
                        && agent == agent_doc
                        && session.session.agent_did == agent_did
                        && session.session.requester_did.as_deref() == Some(requester_did),
                    "request physical targets changed before recovery"
                );
                Ok::<_, anyhow::Error>(())
            })
        })
        .await?;
    publish_request_relationships(access, agent_did, session_doc, agent_doc, request_doc).await
}

/// Recover detached grants for an agent's committed `agent_new` publications.
/// The parent agent is the native request owner; the selected child agent cannot
/// discover the request until these relationships have been published.
#[cfg(test)]
async fn recover_delegated_client_request_relationships(
    node: &std::sync::Arc<defra_node::Node>,
    node_did: &str,
    actor: &identity::Did,
) -> anyhow::Result<usize> {
    recover_delegated_client_request_relationships_with_access(
        &ConfigAccess::local_as(node.clone(), actor.clone()),
        node_did,
        actor,
    )
    .await
}

pub(crate) async fn recover_delegated_client_request_relationships_with_access(
    access: &ConfigAccess,
    node_did: &str,
    actor: &identity::Did,
) -> anyhow::Result<usize> {
    use gents_protocol::request_input::SessionCreationIntent;

    anyhow::ensure!(
        access.actor_did().as_deref() == Some(actor.as_str()),
        "request recovery actor differs from access actor"
    );
    let owner = gents_store::graphql::escape_graphql_string(node_did);
    let signer = gents_store::graphql::escape_graphql_string(actor.as_str());
    let filter = format!(
        r#"node_did: {{ _eq: "{owner}" }}, requester_did: {{ _eq: "{signer}" }}, admission_signer_did: {{ _eq: "{signer}" }}, admission_kind: {{ _eq: "client" }}, relationships_pending: {{ _eq: true }}, caused_by_parent_tool_call_doc_id: {{ _ne: null }}"#
    );
    let pending = access
        .transact_readonly("request.client.recover_delegated_publications", |txn| {
            let filter = &filter;
            Box::pin(async move {
                txn.cursor_rows(
                    "AgentRequest",
                    gents_protocol::request_admission::SIGNED_REQUEST_FIELDS,
                    filter,
                )
                .await
            })
        })
        .await?;
    let mut recovered = 0;
    let mut failure = None;
    for value in pending {
        let row: gents_protocol::row::AgentRequestRow = match serde_json::from_value(value) {
            Ok(row) => row,
            Err(error) => {
                failure.get_or_insert_with(|| anyhow::anyhow!(error));
                continue;
            }
        };
        let Some(input) = row.input.as_ref() else {
            continue;
        };
        if input.session_creation != Some(SessionCreationIntent::AgentNew) {
            continue;
        }
        let Some(source) = input.started_by.as_ref() else {
            continue;
        };
        let (Some(request_doc), Some(row_node), Some(session_id)) = (
            row.doc_id.as_deref(),
            row.node_did.as_deref(),
            row.session_id.as_deref(),
        ) else {
            continue;
        };
        let target = gents_protocol::session::SessionScope {
            node_did: row_node.to_owned(),
            session_id: session_id.to_owned(),
            requester_did: row.requester_did.clone(),
        };
        let caller = gents_protocol::session::SessionScope::from(source);
        match crate::session::origin::verified_agent_new_creation_for_opening(
            access,
            &target,
            &caller,
            request_doc,
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                failure.get_or_insert(error);
                continue;
            }
        }
        match complete_client_request_relationships(access, request_doc).await {
            Ok(()) => recovered += 1,
            Err(error) => {
                failure.get_or_insert(error);
            }
        }
    }
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(recovered)
}

/// Recover the unique signed retry of a physical parent using its creator's
/// authenticated access. A completed publication is never replayed.
pub async fn reconcile_client_retry(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: &str,
    parent_doc_id: &str,
) -> Result<Option<String>, ClientSubmissionError> {
    async fn recover(
        access: &ConfigAccess,
        node_did: &str,
        session_id: &str,
        requester_did: &str,
        parent_doc_id: &str,
    ) -> anyhow::Result<Option<String>> {
        let actor = access
            .actor_did()
            .context("retry recovery requires an identity")?;
        let key = format!("retry:doc:{parent_doc_id}");
        let (row, relationships_pending) =
            match find_signed_client_request(access, "retry_key", &key, &actor).await? {
                None => return Ok(None),
                Some(SignedClientRow::Conflict(reason)) => {
                    anyhow::bail!("retry recovery differs from the authenticated signer: {reason}")
                }
                Some(SignedClientRow::Owned {
                    row,
                    relationships_pending,
                }) => (row, relationships_pending),
            };
        anyhow::ensure!(
            row.node_did.as_deref() == Some(node_did)
                && row.session_id.as_deref() == Some(session_id)
                && row.requester_did.as_deref() == Some(requester_did)
                && row.retry_parent_request_doc_id.as_deref() == Some(parent_doc_id)
                && row.retry_key.as_deref() == Some(key.as_str()),
            "retry recovery differs from the authenticated parent scope"
        );
        let request_doc = row
            .doc_id
            .as_deref()
            .context("retry has no document identity")?;
        match relationships_pending {
            Some(false) => return Ok(Some(row.request_id)),
            Some(true) => {}
            None => anyhow::bail!("retry publication marker is unavailable"),
        }
        let agent_did = row.agent_did.as_deref().context("retry has no agent")?;
        let session_doc = row
            .session_doc_id
            .as_deref()
            .context("retry has no session document")?;
        let agent_doc = row
            .agent_doc_id
            .as_deref()
            .context("retry has no agent document")?;
        access
            .transact_readonly("request.client.retry_recovery_scope", |txn| {
                Box::pin(async move {
                    let session =
                        gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                            txn, node_did, session_id,
                        )
                        .await?
                        .context("retry session is unavailable")?;
                    let (agent, _) = gents_documents::read_desired_state_record_in_txn(
                        txn,
                        gents_store::collection::Collection::Agent,
                        node_did,
                        agent_did,
                    )
                    .await?
                    .context("retry agent is unavailable")?;
                    anyhow::ensure!(
                        session.doc_id == session_doc
                            && agent == agent_doc
                            && session.session.agent_did == agent_did
                            && session.session.requester_did.as_deref() == Some(requester_did),
                        "retry physical targets changed before recovery"
                    );
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        publish_request_relationships(access, agent_did, session_doc, agent_doc, request_doc)
            .await?;
        Ok(Some(row.request_id))
    }
    recover(access, node_did, session_id, requester_did, parent_doc_id)
        .await
        .map_err(ClientSubmissionError::Unconfirmed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{grant_operator_user, seed_owned_agent, serve_client_node};
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
    use gents_store::identity::{KeyIdentity, NodeIdentity};
    use std::sync::Arc;

    use crate::test_support::assert_block_signer;

    #[path = "claim_tests.rs"]
    mod claim_tests;
    #[path = "mailbox_claim_tests.rs"]
    mod mailbox_claim_tests;

    #[tokio::test]
    async fn client_signature_binds_modeled_physical_targets() {
        let home = tempfile::tempdir().unwrap();
        let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
        for case in
            gents_lean_vocab::families::cross_actor_authorization_request_signed_targets::cases()
        {
            let input = &case.input;
            let mut request = AgentRequestCreate::base(
                RequestPurpose::Normal,
                "request",
                user.did(),
                user.did(),
                "agent",
                "session",
                "hello",
                "interactive",
                "2026-11-04T00:00:00Z",
                AgentRequestAdmissionRecord::client(user.did()),
            );
            request.session_doc_id = Some(input.signed_session.clone());
            request.agent_doc_id = Some(input.signed_agent_document.clone());
            crate::request::admission::sign_agent_request_create(&user, &mut request)
                .await
                .unwrap();
            let mut changed = request.clone();
            changed.session_doc_id = Some(input.session.clone());
            changed.agent_doc_id = Some(input.agent_document.clone());
            let verified = matches!(
                user.verify(
                    user.did(),
                    &changed.signing_payload(),
                    &request.admission.signature,
                )
                .await,
                Ok(true)
            );
            case.check(verified);
        }
    }

    #[test]
    fn local_self_publication_matches_model() {
        gents_lean_vocab::families::session_child_local_self_publication_cases::replay(|input| {
            local_self_publication_allowed(
                &input.actor,
                &input.requester,
                &input.signer,
                &input.agent,
            )
        });
    }

    #[tokio::test]
    async fn local_self_materialization_publishes_bound_session_before_claim() {
        let db = gents_inference::test_support::test_db(
            "local-self-materialization-publishes-bou",
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
        let node = db.node.clone();
        let runtime: Arc<dyn NodeIdentity> = Arc::new(
            gents_store::test_support::held_runtime_identity(db.node_identity.did()),
        );
        let runtime_did = identity::Did::new(runtime.did()).unwrap();
        let runtime_access = ConfigAccess::local_as(node.clone(), runtime_did.clone());
        let escaped_runtime = gents_store::graphql::escape_graphql_string(runtime.did());
        runtime_access
            .write(
                "test.local_self.node",
                &format!(r#"mutation {{ create_Node(input: {{ node_did: "{escaped_runtime}" }}) {{ _docID }} }}"#),
            )
            .await
            .unwrap();
        let binding = gents_store::identity::provision_agent_identity_authorized(
            &node,
            runtime.as_ref(),
            &runtime_did,
        )
        .await
        .unwrap();
        gents_inference::test_support::install_test_agent_binding(
            &node,
            runtime.did(),
            "one-shot fixture",
            &binding,
        )
        .await;
        gents_store::identity::GrantSet::AgentBaseline
            .grant(
                &runtime_access,
                &identity::Did::new(&binding.agent_did).unwrap(),
            )
            .await
            .unwrap();
        let agent =
            gents_store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap();
        let mut lifecycle = crate::request::lifecycle::RequestLifecycle::materialize_pending_with_execution_binding(
            node.clone(),
            &binding.agent_did,
            runtime.clone(),
            "hello",
            60,
            crate::request::lifecycle::ExecutionOrigin::Interactive,
            "fixture-backend",
            crate::request::lifecycle::TriggerLineage::default(),
            Some(agent.clone()),
        )
        .await
        .expect("materialize a bound local-self request");
        let access = ConfigAccess::local_managed_agent(
            node.clone(),
            binding.agent_did.parse().unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        let rows = access.execute(
            "{ AgentSession { _docID agent_did requester_did relationships_pending } \
              AgentRequest { _docID session_doc_id agent_doc_id admission_kind admission_signer_did relationships_pending } }",
        ).await.unwrap();
        let session = &rows["data"]["AgentSession"][0];
        let request = &rows["data"]["AgentRequest"][0];
        assert_eq!(request["session_doc_id"], session["_docID"]);
        assert!(request["agent_doc_id"].as_str().is_some());
        assert_eq!(request["admission_kind"], "local-self");
        assert_eq!(request["admission_signer_did"], binding.agent_did);
        assert_eq!(request["relationships_pending"], false);
        assert_eq!(session["relationships_pending"], false);
        assert_eq!(session["agent_did"], binding.agent_did);
        assert_eq!(session["requester_did"], binding.agent_did);
        assert_eq!(
            lifecycle.claim_with_identity().await.unwrap(),
            crate::request::lifecycle::ClaimOutcome::Claimed,
        );
        let writer = crate::request::streaming::DefraStreamWriter::new_managed(
            node.clone(),
            agent.as_ref(),
            std::time::Duration::ZERO,
        )
        .unwrap();
        lifecycle.begin_owned_execution(&writer).await.unwrap();
        lifecycle
            .terminalize_owned(
                crate::request::lifecycle::RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::NoMessage,
                None,
            )
            .await
            .unwrap();
        node.shutdown().await.unwrap();
    }

    /// Grants a fixture Agent the node permissions production projects from
    /// its live managed Agent document; document ACP still decides visibility.
    async fn grant_agent_baseline(
        node: &Arc<defra_node::Node>,
        runtime: &dyn NodeIdentity,
        agent_did: &str,
    ) {
        let admin =
            ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did()).unwrap());
        gents_store::identity::GrantSet::AgentBaseline
            .grant(&admin, &identity::Did::new(agent_did).unwrap())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn goal_reservation_retains_user_owned_session_after_unconfirmed_commit() {
        for modeled in
            gents_lean_vocab::families::goal_client_start_contracts_reservation::cases::<String>()
        {
            modeled.check(observe_goal_reservations().await);
        }
    }

    async fn observe_goal_reservations(
    ) -> gents_lean_vocab::families::goal_client_start_contracts_reservation::ReservationOutput
    {
        let home = tempfile::tempdir().unwrap();
        let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
        let (db, runtime) = serve_client_node(
            "goal-reservation-retains-user-owned-sess",
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
        let node = db.node.clone();
        grant_operator_user(&node, &runtime, &user).await;
        let agent = gents_store::identity::provision_agent_identity(&runtime).unwrap();
        let access = db.client_access(&user);
        seed_owned_agent(&access, runtime.did(), &agent).await;
        grant_agent_baseline(&node, &runtime, &agent.agent_did).await;
        let request = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "goal-first",
            runtime.did(),
            user.did(),
            &agent.agent_did,
            "goal-session",
            "work",
            "interactive",
            "2026-11-04T00:00:00Z",
            AgentRequestAdmissionRecord::client(user.did()),
        );
        let (result, fired) = gents_store::config_client::ConfigApplyTxn::with_post_commit_receipt_loss_for_operation(
                Some("request.client.reserve_session"), reserve_goal_session(&access, &request),
            ).await;
        assert!(fired && result.unwrap_err().is_unconfirmed());
        let before = access
            .execute("{ AgentSession { _docID } AgentRequest { _docID } Goal { _docID } }")
            .await
            .unwrap();
        let doc = before["data"]["AgentSession"][0]["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(before["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(before["data"]["Goal"].as_array().unwrap().is_empty());
        let retried = reserve_goal_session(&access, &request).await.unwrap();
        let retry_reuses_session = retried == doc;
        let owners = access
            .transact_readonly("test.goal.owner", |txn| {
                let doc = doc.clone();
                Box::pin(async move { txn.document_owners("AgentSession", &[doc]).await })
            })
            .await
            .unwrap();
        let user_owned = owners[0]
            .owner
            .as_ref()
            .is_some_and(|owner| owner.as_str() == user.did());
        let worker = ConfigAccess::local_as(node.clone(), agent.agent_did.parse().unwrap())
            .execute("{ AgentSession { _docID } }")
            .await
            .unwrap();
        let agent_granted = !worker["data"]["AgentSession"]
            .as_array()
            .unwrap()
            .is_empty();
        let agent_key = gents_store::identity::resolve_agent_identity(&runtime, &agent).unwrap();
        let agent_access = ConfigAccess::local_managed_agent(
            node.clone(),
            agent.agent_did.parse().unwrap(),
            agent_key.as_ref(),
        )
        .unwrap();
        let mut autonomous = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "agent-goal-first",
            runtime.did(),
            &agent.agent_did,
            &agent.agent_did,
            "agent-goal-session",
            "work",
            "interactive",
            "2026-11-04T00:00:00Z",
            AgentRequestAdmissionRecord::local_self(&agent.agent_did),
        );
        crate::request::admission::sign_agent_request_create(agent_key.as_ref(), &mut autonomous)
            .await
            .unwrap();
        let autonomous_doc = reserve_goal_session(&agent_access, &autonomous)
            .await
            .unwrap();
        let owners = agent_access
            .transact_readonly("test.agent_goal.owner", |txn| {
                let doc = autonomous_doc.clone();
                Box::pin(async move { txn.document_owners("AgentSession", &[doc]).await })
            })
            .await
            .unwrap();
        let agent_is_owner = owners[0]
            .owner
            .as_ref()
            .is_some_and(|owner| owner.as_str() == agent.agent_did);
        let inherited = access
            .execute("{ AgentSession { session_id } }")
            .await
            .unwrap();
        assert!(inherited["data"]["AgentSession"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["session_id"] == "agent-goal-session"));
        node.shutdown().await.expect("shut down test node");
        gents_lean_vocab::families::goal_client_start_contracts_reservation::ReservationOutput {
            user_owned,
            agent_granted,
            retry_reuses_session,
            agent_is_owner,
        }
    }

    #[tokio::test]
    async fn client_session_and_request_commit_or_roll_back_together() {
        let home = tempfile::tempdir().unwrap();
        let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
        let (db, runtime) = serve_client_node(
            "client-session-and-request-commit-or-rol",
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
        let node = db.node.clone();
        grant_operator_user(&node, &runtime, &user).await;
        let agent = gents_store::identity::provision_agent_identity(&runtime).unwrap();
        let access = db.client_access(&user);
        seed_owned_agent(&access, runtime.did(), &agent).await;
        grant_agent_baseline(&node, &runtime, &agent.agent_did).await;
        let node_owner = access
            .transact_readonly("test.client.node_owner", |txn| {
                Box::pin(async move {
                    let response = txn.execute("{ Node { _docID } }").await?;
                    let doc = response["data"]["Node"][0]["_docID"]
                        .as_str()
                        .context("missing Node")?
                        .to_owned();
                    Ok::<_, anyhow::Error>(txn.document_owners("Node", &[doc]).await?)
                })
            })
            .await
            .unwrap();
        assert_eq!(
            node_owner[0].owner.as_ref().map(identity::Did::as_str),
            Some(user.did())
        );
        let mut first = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "first",
            runtime.did(),
            user.did(),
            &agent.agent_did,
            "first-session",
            "hello",
            "interactive",
            "2026-11-04T00:00:00Z",
            AgentRequestAdmissionRecord::client(user.did()),
        );
        first.retry_key = Some("unique-client-retry".to_owned());
        let (session, agent_doc) = reserve_client_targets(&access, &first).await.unwrap();
        first.session_doc_id = Some(session);
        first.agent_doc_id = Some(agent_doc);
        crate::request::admission::sign_agent_request_create(&user, &mut first)
            .await
            .unwrap();
        let (observed_access, request_grantee) = (&access, agent.agent_did.as_str());
        let observe = move || async move {
            let rows = observed_access
                .execute("{ AgentRequest { _docID relationships_pending } }")
                .await
                .unwrap();
            let row = rows["data"]["AgentRequest"]
                .as_array()
                .unwrap()
                .first()
                .cloned();
            let grants = match &row {
                Some(row) => observed_access
                    .list_document_relationships("AgentRequest", row["_docID"].as_str().unwrap())
                    .await
                    .unwrap()
                    .iter()
                    .any(|relationship| {
                        relationship.relation == "working_agent"
                            && relationship.subject == request_grantee
                    }),
                None => false,
            };
            gents_lean_vocab::families::session_child_atomic_client_request_unconfirmed::ClientRequestPublication {
                committed: row.is_some(),
                pending: row
                    .as_ref()
                    .is_some_and(|row| row["relationships_pending"].as_bool().unwrap()),
                grants,
            }
        };
        for case in
            gents_lean_vocab::families::session_child_atomic_client_request_unconfirmed::cases()
        {
            assert!(case.input.fresh && case.input.commit, "{}", case.input.name);
            let (result, fired) = gents_store::config_client::ConfigApplyTxn::with_post_commit_receipt_loss_for_operation(
                Some("request.client.submit"), submit_client_request(&access, &first),
            ).await;
            assert!(fired && result.unwrap_err().is_unconfirmed());
            let unconfirmed = observe().await;
            assert!(reconcile_client_request(&access, &first).await.unwrap());
            let reconciled = observe().await;
            assert_eq!(
                (
                    unconfirmed.committed,
                    unconfirmed.pending,
                    unconfirmed.grants
                ),
                (reconciled.committed, reconciled.pending, reconciled.grants)
            );
            case.check(reconciled);
        }
        assert!(reconcile_client_request(&access, &first).await.unwrap());
        let wrong = ConfigAccess::local_as(node.clone(), agent.agent_did.parse().unwrap());
        assert!(reconcile_client_request(&wrong, &first).await.is_err());
        let agent_key = gents_store::identity::resolve_agent_identity(&runtime, &agent).unwrap();
        let agent_access = ConfigAccess::local_managed_agent(
            node.clone(),
            agent.agent_did.parse().unwrap(),
            agent_key.as_ref(),
        )
        .unwrap();
        let sibling = gents_store::identity::provision_agent_identity(&runtime).unwrap();
        grant_agent_baseline(&node, &runtime, &sibling.agent_did).await;
        let sibling_access =
            ConfigAccess::local_as(node.clone(), sibling.agent_did.parse().unwrap());
        let runtime_access = ConfigAccess::local_as(node.clone(), runtime.did().parse().unwrap());
        let session_query = "{ AgentSession { session_id } }";
        for (reader, expected) in [
            (&access, 1),
            (&agent_access, 1),
            (&sibling_access, 0),
            (&runtime_access, 0),
        ] {
            let result = reader.execute(session_query).await.unwrap();
            assert_eq!(
                result["data"]["AgentSession"].as_array().unwrap().len(),
                expected
            );
        }
        assert_eq!(
            crate::session::load_session_requester_scope(
                &node,
                runtime.did(),
                "first-session",
                &agent.agent_did,
            )
            .await
            .unwrap()
            .as_deref(),
            Some(user.did()),
        );
        assert_eq!(
            crate::session::load_session_requester_scope(
                &node,
                runtime.did(),
                "first-session",
                &sibling.agent_did,
            )
            .await
            .unwrap(),
            None,
        );
        let mut second = first.clone();
        second.request_id = "second".to_owned();
        second.session_id = "rolled-back-session".to_owned();
        crate::request::admission::sign_agent_request_create(&user, &mut second)
            .await
            .unwrap();
        let error = submit_client_request(&access, &second).await.unwrap_err();
        assert!(!error.is_unconfirmed());
        let rows = access.execute("{ AgentSession { session_id requester_did agent_did } AgentRequest { request_id } }")
            .await.unwrap();
        assert_eq!(rows["data"]["AgentSession"].as_array().unwrap().len(), 1);
        assert_eq!(
            rows["data"]["AgentSession"][0]["session_id"],
            "first-session"
        );
        assert_eq!(rows["data"]["AgentSession"][0]["requester_did"], user.did());
        assert_eq!(
            rows["data"]["AgentSession"][0]["agent_did"],
            agent.agent_did
        );
        assert_eq!(rows["data"]["AgentRequest"].as_array().unwrap().len(), 1);
        agent_access
            .transact_write("test.client.unattended", |txn| {
                Box::pin(async {
                    crate::session::ensure_session_in_txn(
                        txn,
                        "unattended",
                        runtime.did(),
                        &agent.agent_did,
                        Some(user.did()),
                        None,
                        None,
                        "2026-11-04T00:00:00Z",
                    )
                    .await?;
                    let session =
                        gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                            txn,
                            runtime.did(),
                            "unattended",
                        )
                        .await?
                        .context("unattended session missing")?;
                    let (agent_doc, _) = gents_documents::read_desired_state_record_in_txn(
                        txn,
                        gents_store::collection::Collection::Agent,
                        runtime.did(),
                        &agent.agent_did,
                    )
                    .await?
                    .context("selected agent missing")?;
                    crate::session::stage_created_session_relationships_in_txn(
                        txn,
                        &session.doc_id,
                        &agent_doc,
                        &identity::Did::new(&agent.agent_did)?,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
        let user_sessions = access
            .execute("{ AgentSession(filter:{session_id:{_eq:\"unattended\"}}) { _docID } }")
            .await
            .unwrap();
        let unattended = user_sessions["data"]["AgentSession"][0]["_docID"]
            .as_str()
            .unwrap();
        let hidden = runtime_access
            .execute("{ AgentSession(filter:{session_id:{_eq:\"unattended\"}}) { _docID } }")
            .await
            .unwrap();
        assert!(hidden["data"]["AgentSession"]
            .as_array()
            .unwrap()
            .is_empty());
        access
            .transact_write("test.client.delete_unattended", |txn| {
                Box::pin(async move { txn.delete_session_for_native_fixture(unattended).await })
            })
            .await
            .unwrap();
        let after = agent_access
            .execute("{ AgentSession(filter:{session_id:{_eq:\"unattended\"}}) { _docID } }")
            .await
            .unwrap();
        assert!(after["data"]["AgentSession"].as_array().unwrap().is_empty());
        node.shutdown().await.expect("shut down test node");
    }

    #[tokio::test]
    async fn client_retry_recovers_only_its_authenticated_signed_publication() {
        let home = tempfile::tempdir().unwrap();
        let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
        let other = KeyIdentity::load_or_create(home.path().join("other.key"), None).unwrap();
        let (db, runtime) = serve_client_node(
            "client-retry-recovers-only-its-authentic",
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
        let node = db.node.clone();
        grant_operator_user(&node, &runtime, &user).await;
        let agent = gents_store::identity::provision_agent_identity(&runtime).unwrap();
        let access = db.client_access(&user);
        seed_owned_agent(&access, runtime.did(), &agent).await;
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "retry",
            runtime.did(),
            user.did(),
            &agent.agent_did,
            "retry-session",
            "hello",
            "interactive",
            "2026-11-04T00:00:00Z",
            AgentRequestAdmissionRecord::client(user.did()),
        );
        let (session, agent_doc) = reserve_client_targets(&access, &create).await.unwrap();
        create.session_doc_id = Some(session);
        create.agent_doc_id = Some(agent_doc);
        create.retry_parent_request = Some("parent".into());
        create.retry_parent_request_doc_id = Some("parent-doc".into());
        create.retry_root_request = Some("parent".into());
        create.retry_key = Some("retry:doc:parent-doc".into());
        crate::request::admission::sign_agent_request_create(&user, &mut create)
            .await
            .unwrap();
        let mut input = gents_store::config_client::client_request_input(&create).unwrap();
        input["relationships_pending"] = true.into();
        access
            .transact_write("test.retry.pending", |txn| {
                let input = &input;
                Box::pin(async move {
                    txn.execute_client_signed_create(
                        "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                        &serde_json::json!({"input": input}),
                        gents_protocol::schemas::AGENT_REQUEST,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
        // The other actor and the Agent are authenticated node readers; only
        // document ACP decides whether each can observe this user's retry.
        for reader in [other.did(), agent.agent_did.as_str()] {
            ConfigAccess::local_as(node.clone(), runtime.did().parse().unwrap())
                .grant_node_permission(
                    &reader.parse().unwrap(),
                    gents_store::config_client::NodePermission::DocumentRead,
                )
                .await
                .unwrap();
        }
        let wrong = ConfigAccess::local_as(node.clone(), other.did().parse().unwrap());
        assert!(reconcile_client_retry(
            &access,
            runtime.did(),
            "retry-session",
            "other-requester",
            "parent-doc"
        )
        .await
        .is_err());
        let agent_access = ConfigAccess::local_as(node.clone(), agent.agent_did.parse().unwrap());
        for case in gents_lean_vocab::families::session_child_client_request_recovery::cases() {
            let recovered = reconcile_client_retry(
                if case.input.same_signer {
                    &access
                } else {
                    &wrong
                },
                runtime.did(),
                "retry-session",
                user.did(),
                "parent-doc",
            )
            .await
            .unwrap();
            assert_eq!(
                recovered.as_deref(),
                case.input.same_signer.then_some("retry")
            );
            let rows = access
                .execute("{ AgentRequest { relationships_pending } }")
                .await
                .unwrap();
            let visible = agent_access
                .execute("{ AgentRequest { request_id } }")
                .await
                .unwrap();
            case.check(
                gents_lean_vocab::families::session_child_client_request_recovery::ClientRequestOutput {
                    pending: rows["data"]["AgentRequest"][0]["relationships_pending"]
                        .as_bool()
                        .unwrap(),
                    grants: visible["data"]["AgentRequest"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|row| row["request_id"] == "retry"),
                },
            );
        }
        assert_eq!(
            reconcile_client_retry(
                &access,
                runtime.did(),
                "retry-session",
                user.did(),
                "parent-doc"
            )
            .await
            .unwrap()
            .as_deref(),
            Some("retry")
        );
        node.shutdown().await.expect("shut down test node");
    }

    /// `ClientOptimisticSend.create`: repeats of one identity, rebuilt and
    /// re-signed, racing each other, or following a create committed without
    /// the key, are one request; a raw create cannot add a second request
    /// under a held identity; an identity held in another chat is a named
    /// conflict.
    #[tokio::test]
    async fn client_send_is_idempotent_on_its_request_identity() {
        let home = tempfile::tempdir().unwrap();
        let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
        let (db, runtime) = serve_client_node(
            "client-send-is-idempotent-on-its-request",
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
        let node = db.node.clone();
        grant_operator_user(&node, &runtime, &user).await;
        let agent = gents_store::identity::provision_agent_identity(&runtime).unwrap();
        let access = db.client_access(&user);
        seed_owned_agent(&access, runtime.did(), &agent).await;
        let signed_in = |session: &'static str,
                         request_id: &'static str,
                         created_at: &'static str| {
            let access = &access;
            let user = &user;
            let runtime = &runtime;
            let agent = &agent;
            async move {
                let mut create = AgentRequestCreate::base(
                    RequestPurpose::Normal,
                    request_id,
                    runtime.did(),
                    user.did(),
                    &agent.agent_did,
                    session,
                    "hello",
                    "interactive",
                    created_at,
                    AgentRequestAdmissionRecord::client(user.did()),
                );
                create.retry_key = Some(client_request_key(user.did(), request_id));
                let (session, agent_doc) = reserve_client_targets(access, &create).await.unwrap();
                create.session_doc_id = Some(session);
                create.agent_doc_id = Some(agent_doc);
                crate::request::admission::sign_agent_request_create(user, &mut create)
                    .await
                    .unwrap();
                create
            }
        };
        let signed = |request_id: &'static str, created_at: &'static str| {
            signed_in("idempotent-session", request_id, created_at)
        };
        let count = |request_id: &'static str| {
            let access = &access;
            async move {
                let rows = access
                    .execute(&format!(
                        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ _docID relationships_pending }} }}"#
                    ))
                    .await
                    .unwrap();
                let rows = rows["data"]["AgentRequest"].as_array().unwrap().clone();
                assert!(rows.iter().all(|row| row["relationships_pending"] == false));
                rows.len()
            }
        };
        let first = signed("first", "2026-11-04T00:00:00Z").await;
        assert_eq!(send_client_request(&access, &first).await.unwrap(), "first");
        assert_eq!(send_client_request(&access, &first).await.unwrap(), "first");
        let rebuilt = signed("first", "2026-11-04T00:00:07Z").await;
        assert_ne!(rebuilt.admission.signature, first.admission.signature);
        assert_eq!(
            send_client_request(&access, &rebuilt).await.unwrap(),
            "first"
        );
        assert_eq!(count("first").await, 1);

        let left = signed("raced", "2026-11-04T00:01:00Z").await;
        let right = signed("raced", "2026-11-04T00:01:01Z").await;
        let (left, right) = tokio::join!(
            send_client_request(&access, &left),
            send_client_request(&access, &right)
        );
        assert_eq!(left.unwrap(), "raced");
        assert_eq!(right.unwrap(), "raced");
        assert_eq!(count("raced").await, 1);

        let mut keyless = signed("keyless", "2026-11-04T00:02:00Z").await;
        keyless.retry_key = None;
        crate::request::admission::sign_agent_request_create(&user, &mut keyless)
            .await
            .unwrap();
        assert!(matches!(
            send_client_request(&access, &keyless).await,
            Err(ClientSubmissionError::Rejected(_))
        ));
        assert_eq!(count("keyless").await, 0);
        let mut foreign_key = signed("keyless", "2026-11-04T00:02:01Z").await;
        foreign_key.retry_key = Some("another-key".into());
        crate::request::admission::sign_agent_request_create(&user, &mut foreign_key)
            .await
            .unwrap();
        assert!(send_client_request(&access, &foreign_key).await.is_err());
        assert_eq!(count("keyless").await, 0);

        // A create committed before the key existed is the held request.
        submit_client_request(&access, &keyless).await.unwrap();
        assert_eq!(count("keyless").await, 1);
        let keyed = signed("keyless", "2026-11-04T00:02:02Z").await;
        assert_eq!(
            send_client_request(&access, &keyed).await.unwrap(),
            "keyless"
        );
        assert_eq!(count("keyless").await, 1);
        // The raw create does not admit a second request under a held identity.
        assert!(submit_client_request(&access, &foreign_key).await.is_err());
        assert!(submit_client_request(&access, &rebuilt).await.is_err());
        assert_eq!(count("keyless").await, 1);
        assert_eq!(count("first").await, 1);

        let elsewhere = signed_in("other-session", "first", "2026-11-04T00:03:00Z").await;
        assert!(matches!(
            send_client_request(&access, &elsewhere).await,
            Err(ClientSubmissionError::IdentityConflict(_))
        ));
        assert_eq!(count("first").await, 1);
        node.shutdown().await.expect("shut down test node");
    }

    #[tokio::test]
    async fn delegated_recovery_does_not_grant_a_signed_request_without_its_creation_receipt() {
        for case in gents_lean_vocab::families::delegated_publication_recovery_receipt::cases() {
            let foreign_owner = case
                .input
                .actions
                .iter()
                .any(|action| action.kind == "observe" && action.owner == Some(false));
            let name = format!("delegated-recovery-{}", case.input.name);
            let db = gents_inference::test_support::test_db(
                &name,
                crate::test_support::no_lifecycle_plugins(),
            )
            .await;
            let (db, user, user_access, parent) =
                gents_inference::test_support::signed_test_node_from_db(db, &name).await;
            let node = db.node.clone();
            let runtime = db.node_identity.clone();
            let target = gents_store::identity::provision_agent_identity_authorized(
                &node,
                runtime.as_ref(),
                &identity::Did::new(user.did()).unwrap(),
            )
            .await
            .unwrap();
            gents_inference::test_support::install_test_agent_binding_with_access(
                runtime.did(),
                "target",
                &target,
                &user_access,
            )
            .await;
            let escaped = gents_store::graphql::escape_graphql_string(&target.agent_did);
            let target_row = user_access
                .execute(&format!(
                    r#"{{ Agent(filter: {{ agent_did: {{ _eq: "{escaped}" }} }}) {{ _docID }} }}"#
                ))
                .await
                .unwrap();
            let target_doc = target_row["data"]["Agent"][0]["_docID"].as_str().unwrap();
            user_access
                .add_actor_relationship(
                    "Agent",
                    target_doc,
                    "session_writer",
                    &parent.agent_did.parse().unwrap(),
                )
                .await
                .unwrap();
            let signer =
                gents_store::identity::resolve_agent_identity(runtime.as_ref(), &parent).unwrap();
            let actor: identity::Did = parent.agent_did.parse().unwrap();
            let access =
                ConfigAccess::local_managed_agent(node.clone(), actor.clone(), signer.as_ref())
                    .unwrap();
            let mut forged = AgentRequestCreate::base(
                RequestPurpose::Normal,
                "forged-delegated-request",
                runtime.did(),
                &parent.agent_did,
                &target.agent_did,
                "forged-delegated-session",
                "hello",
                "interactive",
                "2026-11-04T00:00:00Z",
                AgentRequestAdmissionRecord::client(&parent.agent_did),
            );
            forged.input.session_creation =
                Some(gents_protocol::request_input::SessionCreationIntent::AgentNew);
            forged.input.started_by = Some(gents_protocol::session::SessionReference {
                node_did: runtime.did().to_owned(),
                session_id: "missing-parent-session".to_owned(),
                requester_did: Some(user.did().to_owned()),
            });
            forged.caused_by_parent_request_id = Some("missing-parent-request".to_owned());
            forged.caused_by_parent_request_doc_id = Some("missing-parent-doc".to_owned());
            forged.caused_by_parent_tool_call_id = Some("missing-tool-call".to_owned());
            forged.caused_by_parent_tool_call_doc_id = Some("missing-tool-doc".to_owned());
            let (session, agent) = reserve_client_targets(&access, &forged).await.unwrap();
            forged.session_doc_id = Some(session);
            forged.agent_doc_id = Some(agent);
            crate::request::admission::sign_agent_request_create(signer.as_ref(), &mut forged)
                .await
                .unwrap();
            let writer = if foreign_owner { &user_access } else { &access };
            let mutation = forged.graphql_client_publication_mutation().unwrap();
            let written = if foreign_owner {
                user_access
                    .transact_write("test.forged_delegated_request", |txn| {
                        let forged = &forged;
                        Box::pin(async move {
                            let mut input =
                                gents_store::config_client::client_request_input(forged)?;
                            input["relationships_pending"] = true.into();
                            txn.execute_client_signed_create(
                                "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                                &serde_json::json!({"input": input}),
                                gents_protocol::schemas::AGENT_REQUEST,
                            )
                            .await
                            .map_err(anyhow::Error::from)
                        })
                    })
                    .await
                    .unwrap()
            } else {
                access
                    .write("test.forged_delegated_request", &mutation)
                    .await
                    .unwrap()
            };
            let request_doc =
                gents_store::graphql::created_doc_id(&written, "AgentRequest").unwrap();
            let observed = if foreign_owner {
                complete_client_request_relationships(&access, &request_doc)
                    .await
                    .is_ok()
            } else {
                match recover_delegated_client_request_relationships(&node, runtime.did(), &actor)
                    .await
                    .unwrap()
                {
                    0 => false,
                    1 => true,
                    recovered => panic!("{} recovered {recovered} publications", case.input.name),
                }
            };
            let rows = writer
                .execute(&format!(
                    r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ relationships_pending }} }}"#,
                    gents_store::graphql::escape_graphql_string(&request_doc)
                ))
                .await
                .unwrap();
            let target_signer =
                gents_store::identity::resolve_agent_identity(runtime.as_ref(), &target).unwrap();
            let target_access = ConfigAccess::local_managed_agent(
                node.clone(),
                target.agent_did.parse().unwrap(),
                target_signer.as_ref(),
            )
            .unwrap();
            let visible = target_access
                .execute("{ AgentRequest { request_id } }")
                .await
                .unwrap();
            case.check(
                gents_lean_vocab::families::delegated_publication_recovery_receipt::RecoveryOutput {
                    observed,
                    request_pending: rows["data"]["AgentRequest"][0]["relationships_pending"]
                        .as_bool()
                        .unwrap(),
                    target_read: !visible["data"]["AgentRequest"]
                        .as_array()
                        .unwrap()
                        .is_empty(),
                },
            );
            node.shutdown().await.unwrap();
        }
    }
}
