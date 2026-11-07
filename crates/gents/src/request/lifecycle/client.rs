use anyhow::Context;
use gents_protocol::request_admission::{AgentRequestAdmissionKind, AgentRequestCreate};

use crate::store::config_client::ConfigAccess;

/// A failed client submission retains whether native commit acknowledgement was
/// lost. Callers must reconcile the original signed request before replacing an
/// unconfirmed submission.
#[derive(Debug, thiserror::Error)]
pub enum ClientSubmissionError {
    #[error("client submission rejected")]
    Rejected(#[source] anyhow::Error),
    #[error("client submission commit is unconfirmed")]
    Unconfirmed(#[source] anyhow::Error),
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

pub(crate) async fn stage_request_relationships_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
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
pub(crate) async fn stage_runtime_request_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
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
        crate::store::identity::verify_did_signature(
            &create.admission.signer_did,
            &create.signing_payload(),
            &create.admission.signature,
        )?,
        "runtime request requires its valid signed receipt"
    );
    let escaped = crate::store::graphql::escape_graphql_string(session);
    let observed = txn.execute(&format!(
        r#"{{ AgentSession(filter: {{_docID: {{_eq: "{escaped}"}}}}, limit: 2) {{_docID relationships_pending}} }}"#,
    )).await?;
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
    let request = crate::store::graphql::created_doc_id(&response, "AgentRequest")?;
    stage_request_relationships_in_txn(txn, &request, agent, session, &create.agent_did).await?;
    Ok(request)
}

/// Create and link a client or local-self request within the transaction that
/// verifies its physical session and selected Agent. Legacy pending publications
/// retain their separate authenticated recovery path.
pub(crate) async fn stage_client_request_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
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
        crate::store::identity::verify_did_signature(
            &create.admission.signer_did,
            &create.signing_payload(),
            &create.admission.signature,
        )?,
        "client request requires its valid signed receipt"
    );
    let escaped = crate::store::graphql::escape_graphql_string(session);
    let observed = txn.execute(&format!(
        r#"{{ AgentSession(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 2) {{ _docID relationships_pending }} }}"#,
    )).await?;
    let sessions = observed["data"]["AgentSession"]
        .as_array()
        .context("client request session check omitted rows")?;
    anyhow::ensure!(
        sessions.len() == 1
            && sessions[0]["_docID"] == session
            && sessions[0]["relationships_pending"] == false,
        "client request session relationships are incomplete"
    );
    let input = crate::store::config_client::client_request_input(create)?;
    let response = txn.execute_client_signed_create(
        "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
        &serde_json::json!({"input": input}),
        gents_protocol::schemas::AGENT_REQUEST,
    ).await?;
    let request = crate::store::graphql::created_doc_id(&response, "AgentRequest")?;
    stage_request_relationships_in_txn(txn, &request, agent, session, &create.agent_did).await?;
    Ok(request)
}

/// Stage an Agent-authored child while its new session still awaits the
/// existing creator-owned relationship publication protocol.
pub(crate) async fn stage_pending_child_request_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
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
        crate::store::identity::verify_did_signature(
            &create.admission.signer_did,
            &create.signing_payload(),
            &create.admission.signature,
        )?,
        "pending child requires its valid signed receipt"
    );
    let escaped = crate::store::graphql::escape_graphql_string(session);
    let observed = txn.execute(&format!(
        r#"{{ AgentSession(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 2) {{ _docID relationships_pending }} }}"#,
    )).await?;
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
    Ok(crate::store::graphql::created_doc_id(
        &response,
        "AgentRequest",
    )?)
}

pub(crate) async fn ensure_request_session_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    create: &AgentRequestCreate,
    actor: &str,
) -> anyhow::Result<(String, String, bool)> {
    let existing =
        crate::session::load_agent_session_owner_in_txn(txn, &create.node_did, &create.session_id)
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
    let session =
        crate::session::load_agent_session_owner_in_txn(txn, &create.node_did, &create.session_id)
            .await?
            .context("reserved client session disappeared")?
            .doc_id;
    let (agent, _) = crate::store::config_client::read_desired_state_record_in_txn(
        txn,
        crate::Collection::Agent,
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

async fn reserve_client_targets(
    access: &ConfigAccess,
    create: &AgentRequestCreate,
) -> Result<(String, String), ClientSubmissionError> {
    let actor = request_actor(access, create).map_err(ClientSubmissionError::Rejected)?;
    let (session, agent, created) = access
        .transact_write_with_finality("request.client.reserve_session", |txn| {
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
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    spec: super::RequestSpec,
    signer: super::RequestSigner<'_>,
) -> anyhow::Result<AgentRequestCreate> {
    let mut create = super::materialize::build_request(spec)?;
    bind_client_targets_in_txn(txn, &mut create).await?;
    super::materialize::sign_request(&mut create, signer).await?;
    Ok(create)
}

pub(crate) async fn build_signed_edit_request_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
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

pub(crate) async fn bind_client_targets_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    create: &mut AgentRequestCreate,
) -> anyhow::Result<()> {
    if create.admission.kind == AgentRequestAdmissionKind::Client {
        bind_existing_targets_in_txn(txn, create).await?;
    }
    Ok(())
}

async fn bind_existing_targets_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    create: &mut AgentRequestCreate,
) -> anyhow::Result<()> {
    let session =
        crate::session::load_agent_session_owner_in_txn(txn, &create.node_did, &create.session_id)
            .await?
            .context("client request session is unavailable")?;
    anyhow::ensure!(
        session.session.agent_did == create.agent_did
            && session.session.requester_did.as_deref() == Some(create.requester_did.as_str()),
        "client request session scope changed"
    );
    let (agent, _) = crate::store::config_client::read_desired_state_record_in_txn(
        txn,
        crate::Collection::Agent,
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
    access
        .transact_write_with_finality("request.client.submit", |txn| {
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
                let doc = crate::store::graphql::escape_graphql_string(request_doc);
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
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
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
    let doc = crate::store::graphql::escape_graphql_string(request_doc);
    let session_doc = crate::store::graphql::escape_graphql_string(session);
    let agent_doc = crate::store::graphql::escape_graphql_string(agent);
    let response = txn.execute(&format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }} }}, limit: 2) {{ {} relationships_pending }} AgentSession(filter: {{ _docID: {{ _eq: "{session_doc}" }} }}, limit: 2) {{ _docID node_did session_id requester_did agent_did relationships_pending }} Agent(filter: {{ _docID: {{ _eq: "{agent_doc}" }} }}, limit: 2) {{ _docID node_did agent_did }} }}"#,
        crate::SIGNED_REQUEST_FIELDS,
    )).await?;
    let rows = response["data"]["AgentRequest"]
        .as_array()
        .context("request publication omitted request rows")?;
    let [value] = rows.as_slice() else {
        anyhow::bail!("request publication requires one visible request");
    };
    let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(value.clone())?;
    crate::verify_request_receipt_signature(&row)?;
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
    let scope = crate::session::session_scope_filter(
        &create.node_did,
        &create.session_id,
        Some(&create.requester_did),
    );
    let request = crate::store::graphql::escape_graphql_string(&create.request_id);
    let response = access.execute(&format!("{{ AgentRequest(filter: {{{scope}, request_id: {{ _eq: \"{request}\" }} }}, limit: 2) {{ {} relationships_pending }} }}", crate::SIGNED_REQUEST_FIELDS)).await?;
    let rows = response["data"]["AgentRequest"]
        .as_array()
        .context("request recovery omitted rows")?;
    anyhow::ensure!(rows.len() <= 1, "ambiguous client request recovery");
    let Some(value) = rows.first() else {
        return Ok(false);
    };
    let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(value.clone())?;
    crate::verify_request_receipt_signature(&row)?;
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
                let session = crate::session::load_agent_session_owner_in_txn(
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
                let (agent, _) = crate::store::config_client::read_desired_state_record_in_txn(
                    txn,
                    crate::Collection::Agent,
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
pub(crate) async fn complete_client_request_relationships(
    access: &ConfigAccess,
    request_doc: &str,
) -> anyhow::Result<()> {
    let actor = access
        .actor_did()
        .context("request relationship recovery requires an actor")?;
    let escaped = crate::store::graphql::escape_graphql_string(request_doc);
    let response = access.execute(&format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 2) {{ {} relationships_pending }} }}"#,
        crate::SIGNED_REQUEST_FIELDS,
    )).await?;
    let rows = response["data"]["AgentRequest"]
        .as_array()
        .context("request relationship recovery omitted rows")?;
    let [value] = rows.as_slice() else {
        anyhow::bail!("request relationship recovery requires one visible document");
    };
    let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(value.clone())?;
    crate::verify_request_receipt_signature(&row)?;
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
                    crate::session::load_agent_session_owner_in_txn(txn, node_did, session_id)
                        .await?
                        .context("request session is unavailable")?;
                let (agent, _) = crate::store::config_client::read_desired_state_record_in_txn(
                    txn,
                    crate::Collection::Agent,
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
    let owner = crate::store::graphql::escape_graphql_string(node_did);
    let signer = crate::store::graphql::escape_graphql_string(actor.as_str());
    let filter = format!(
        r#"node_did: {{ _eq: "{owner}" }}, requester_did: {{ _eq: "{signer}" }}, admission_signer_did: {{ _eq: "{signer}" }}, admission_kind: {{ _eq: "client" }}, relationships_pending: {{ _eq: true }}, caused_by_parent_tool_call_doc_id: {{ _ne: null }}"#
    );
    let pending = access
        .transact_readonly("request.client.recover_delegated_publications", |txn| {
            let filter = &filter;
            Box::pin(async move {
                txn.cursor_rows("AgentRequest", crate::SIGNED_REQUEST_FIELDS, filter)
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
        let target = crate::session::origin::SessionScope {
            node_did: row_node.to_owned(),
            session_id: session_id.to_owned(),
            requester_did: row.requester_did.clone(),
        };
        let caller = crate::session::origin::SessionScope::from(source);
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
        let escaped = crate::store::graphql::escape_graphql_string(&key);
        let response = access.execute(&format!(
            r#"{{ AgentRequest(filter: {{ retry_key: {{ _eq: "{escaped}" }} }}, limit: 2) {{ {} relationships_pending }} }}"#,
            crate::SIGNED_REQUEST_FIELDS,
        )).await?;
        let rows = response["data"]["AgentRequest"]
            .as_array()
            .context("retry recovery omitted rows")?;
        anyhow::ensure!(rows.len() <= 1, "ambiguous retry recovery");
        let Some(value) = rows.first() else {
            return Ok(None);
        };
        let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(value.clone())?;
        crate::verify_request_receipt_signature(&row)?;
        anyhow::ensure!(
            row.admission_kind.as_deref() == Some("client")
                && row.admission_signer_did.as_deref() == Some(actor.as_str())
                && row.node_did.as_deref() == Some(node_did)
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
        access
            .transact_readonly("request.client.retry_recovery_owner", |txn| {
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
                        "retry creator differs from the authenticated signer"
                    );
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        match value["relationships_pending"].as_bool() {
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
                        crate::session::load_agent_session_owner_in_txn(txn, node_did, session_id)
                            .await?
                            .context("retry session is unavailable")?;
                    let (agent, _) = crate::store::config_client::read_desired_state_record_in_txn(
                        txn,
                        crate::Collection::Agent,
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
    use crate::{KeyIdentity, NodeIdentity};
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
    use std::sync::Arc;

    async fn assert_block_signer(node: &defra_node::Node, cid: &str, signer: &str) {
        let (block, signature) = node
            .authorized_signed_block_bytes(cid, Some(signer))
            .await
            .unwrap();
        let block = defra_core::block::Block::from_dag_cbor(&block).unwrap();
        let signature = defra_core::block::Signature::from_dag_cbor(&signature).unwrap();
        assert_eq!(
            db::block::verify::verified_signature_signer_did(&block, &signature).unwrap(),
            signer
        );
    }

    #[path = "claim_tests.rs"]
    mod claim_tests;
    #[path = "mailbox_claim_tests.rs"]
    mod mailbox_claim_tests;

    #[tokio::test]
    async fn delegated_recovery_does_not_grant_a_signed_request_without_its_creation_receipt() {
        let db = crate::support::test_db("delegated-recovery").await;
        let (db, user, user_access, parent) =
            crate::test_support::signed_test_node_from_db(db, "delegated-recovery").await;
        let node = db.node.clone();
        let runtime = db.node_identity.clone();
        let target = crate::store::identity::provision_agent_identity_authorized(
            &node,
            runtime.as_ref(),
            &identity::Did::new(user.did()).unwrap(),
        )
        .await
        .unwrap();
        crate::test_support::install_test_agent_binding_with_access(
            runtime.did(),
            "target",
            &target,
            &user_access,
        )
        .await;
        let escaped = crate::store::graphql::escape_graphql_string(&target.agent_did);
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
            crate::store::identity::resolve_agent_identity(runtime.as_ref(), &parent).unwrap();
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
        crate::sign_agent_request_create(signer.as_ref(), &mut forged)
            .await
            .unwrap();
        access
            .write(
                "test.forged_delegated_request",
                &forged.graphql_client_publication_mutation().unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            recover_delegated_client_request_relationships(&node, runtime.did(), &actor)
                .await
                .unwrap(),
            0
        );
        let rows = access
            .execute("{ AgentRequest { request_id relationships_pending } }")
            .await
            .unwrap();
        let model = &crate::lean_vocab_test::lean_contract_snapshot().delegated_publication_cases;
        let wrong_receipt = model
            .iter()
            .find(|case| case["name"] == "wrong-receipt")
            .unwrap();
        assert_eq!(
            rows["data"]["AgentRequest"][0]["relationships_pending"],
            wrong_receipt["expected_request_pending"]
        );
        let target_signer =
            crate::store::identity::resolve_agent_identity(runtime.as_ref(), &target).unwrap();
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
        assert_eq!(
            !visible["data"]["AgentRequest"]
                .as_array()
                .unwrap()
                .is_empty(),
            wrong_receipt["expected_target_read"].as_bool().unwrap()
        );
        node.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn client_signature_binds_modeled_physical_targets() {
        let home = tempfile::tempdir().unwrap();
        let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
        let cases =
            &crate::lean_vocab_test::lean_contract_snapshot().cross_actor_authorization_cases;
        let baseline = cases
            .iter()
            .find(|case| case.name == "committed_claim")
            .unwrap();
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
        request.session_doc_id = Some(baseline.intent.session.clone());
        request.agent_doc_id = Some(baseline.intent.agent_document.clone());
        crate::sign_agent_request_create(&user, &mut request)
            .await
            .unwrap();
        for name in ["wrong_session", "wrong_agent_document"] {
            let case = cases.iter().find(|case| case.name == name).unwrap();
            let mut changed = request.clone();
            changed.session_doc_id = Some(case.observation.session.clone());
            changed.agent_doc_id = Some(case.observation.agent_document.clone());
            // A changed payload is rejected by the identity owner either as
            // Ok(false) or as a verification error; only Ok(true) verifies.
            let verified = matches!(
                user.verify(
                    user.did(),
                    &changed.signing_payload(),
                    &request.admission.signature,
                )
                .await,
                Ok(true)
            );
            assert_eq!(
                verified,
                case.decision == crate::lean_vocab_test::LeanCrossActorDecision::Claimed,
                "{name}"
            );
        }
        assert!(user
            .verify(
                user.did(),
                &request.signing_payload(),
                &request.admission.signature
            )
            .await
            .unwrap());
    }

    #[test]
    fn local_self_publication_matches_model() {
        for case in &crate::lean_vocab_test::lean_contract_snapshot().local_self_publication_cases {
            let field = |name: &str| case[name].as_str().unwrap();
            assert_eq!(
                local_self_publication_allowed(
                    field("actor"),
                    field("requester"),
                    field("signer"),
                    field("agent"),
                ),
                case["allowed"].as_bool().unwrap(),
                "{case}",
            );
        }
    }

    #[tokio::test]
    async fn local_self_materialization_publishes_bound_session_before_claim() {
        let home = tempfile::tempdir().unwrap();
        let runtime: Arc<dyn NodeIdentity> =
            Arc::new(KeyIdentity::load_or_create(home.path().join("runtime.key"), None).unwrap());
        let node = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(runtime.did())
                .with_node_acp_enabled()
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let runtime_did = identity::Did::new(runtime.did()).unwrap();
        let runtime_access = ConfigAccess::local_as(node.clone(), runtime_did.clone());
        let escaped_runtime = crate::store::graphql::escape_graphql_string(runtime.did());
        runtime_access
            .write(
                "test.local_self.node",
                &format!(r#"mutation {{ create_Node(input: {{ node_did: "{escaped_runtime}" }}) {{ _docID }} }}"#),
            )
            .await
            .unwrap();
        let binding = crate::store::identity::provision_agent_identity_authorized(
            &node,
            runtime.as_ref(),
            &runtime_did,
        )
        .await
        .unwrap();
        crate::test_support::install_test_agent_binding(
            &node,
            runtime.did(),
            "one-shot fixture",
            &binding,
        )
        .await;
        crate::store::identity::GrantSet::AgentBaseline
            .grant(
                &runtime_access,
                &identity::Did::new(&binding.agent_did).unwrap(),
            )
            .await
            .unwrap();
        let agent =
            crate::store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap();
        let mut lifecycle = crate::RequestLifecycle::materialize_pending_with_execution_binding(
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

    async fn seed_owned_agent(
        access: &ConfigAccess,
        runtime_did: &str,
        agent: &gents_protocol::agent_identity::AgentIdentityBinding,
    ) {
        let (node, agent_doc) = access.transact_write("test.client.owned_agent", |txn| Box::pin(async move {
            let node = txn.execute_client_signed_create(
                "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
                &serde_json::json!({"input":{"node_did":runtime_did}}),
                gents_protocol::schemas::NODE,
            ).await?;
            let node = crate::store::graphql::created_doc_id(&node, "Node")?;
            let created = txn.execute_client_signed_create(
                "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                &serde_json::json!({"input":{"node_did":runtime_did,"agent_did":agent.agent_did,"creation_nonce":agent.creation_nonce,"inference_profile_id":"fixture","enabled":true}}),
                gents_protocol::schemas::AGENT,
            ).await?;
            let agent_doc = crate::store::graphql::created_doc_id(&created, "Agent")?;
            Ok::<_, anyhow::Error>((node, agent_doc))
        })).await.unwrap();
        access
            .add_actor_relationship(
                "Node",
                &node,
                "runtime_reader",
                &identity::Did::new(runtime_did).unwrap(),
            )
            .await
            .unwrap();
        access
            .add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node)
            .await
            .unwrap();
        access
            .add_actor_relationship(
                "Agent",
                &agent_doc,
                "executor",
                &identity::Did::new(&agent.agent_did).unwrap(),
            )
            .await
            .unwrap();
    }

    /// Serves a fixture node over loopback HTTP. A user's configuration is
    /// published only through its client-signed endpoint; embedded access
    /// refuses non-runtime writes by design.
    async fn serve_client_node(runtime: &dyn NodeIdentity) -> (Arc<defra_node::Node>, String) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let node = Arc::new(
            defra_node::Node::builder()
                .enable_signing()
                .with_node_identity_did(runtime.did())
                .with_node_acp_enabled()
                .with_http(defra_node::HttpConfig::with_listener(listener).unwrap())
                .build()
                .await
                .unwrap(),
        );
        crate::store::schema::ensure_runtime_schemas(&node)
            .await
            .unwrap();
        (node, format!("http://{address}/api/v0/graphql"))
    }

    /// Grants the user the `OperatorUser` set as the runtime admin, as
    /// `signed_test_node` does. Without it, seeding the user's client-signed
    /// Node and Agent ends in an incomplete signing conversation.
    async fn grant_operator_user(
        node: &Arc<defra_node::Node>,
        runtime: &dyn NodeIdentity,
        user: &KeyIdentity,
    ) {
        let admin =
            ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did()).unwrap());
        crate::store::identity::GrantSet::OperatorUser
            .grant(&admin, &identity::Did::new(user.did()).unwrap())
            .await
            .unwrap();
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
        crate::store::identity::GrantSet::AgentBaseline
            .grant(&admin, &identity::Did::new(agent_did).unwrap())
            .await
            .unwrap();
    }

    fn client_access(endpoint: &str, identity: &KeyIdentity) -> ConfigAccess {
        ConfigAccess::Graphql(
            crate::store::config_client::GraphqlEndpoint::as_principal(endpoint, identity.did())
                .with_client_signing_capability(identity.client_signing_capability().unwrap())
                .unwrap(),
        )
    }

    async fn create_client_agent(
        access: &ConfigAccess,
        runtime_did: &str,
        agent: &gents_protocol::agent_identity::AgentIdentityBinding,
    ) -> String {
        access
            .transact_write("test.client.created_agent", |txn| {
                Box::pin(async move {
                    let created = txn
                        .execute_client_signed_create(
                            "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                            &serde_json::json!({"input":{"node_did":runtime_did,"agent_did":agent.agent_did,"creation_nonce":agent.creation_nonce,"inference_profile_id":"fixture","enabled":true}}),
                            gents_protocol::schemas::AGENT,
                        )
                        .await?;
                    Ok::<_, anyhow::Error>(crate::store::graphql::created_doc_id(&created, "Agent")?)
                })
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn agent_goal_submission_stages_the_session_before_its_goal_and_rolls_back_together() {
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().goal_backed_runtime_cases;
        let modeled = |name: &str| cases.iter().find(|case| case["name"] == name).unwrap();
        let (db, user, client, binding) =
            crate::test_support::signed_test_node("runtime-goal-opening").await;
        let node = db.node.clone();
        let agent =
            crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
                .unwrap();
        let access = ConfigAccess::local_managed_agent(
            node.clone(),
            identity::Did::new(agent.did()).unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        let mut request = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "task-opening",
            db.node_identity.did(),
            agent.did(),
            agent.did(),
            "task-session",
            "work",
            "scheduled",
            "2026-11-04T00:00:00Z",
            AgentRequestAdmissionRecord::runtime_automated_trigger(agent.did(), "trigger"),
        );
        request.retry_key = Some("task-retry".into());
        request.caused_by_trigger_id = Some("trigger".into());
        request.caused_by_trigger_kind = Some("schedule".into());
        request.caused_by_trigger_doc_id = Some("trigger-doc".into());
        crate::sign_agent_request_create(agent.as_ref(), &mut request)
            .await
            .unwrap();
        let submitted = crate::goal::submit_goal_backed_request_with_identity(
            &node,
            identity::Did::new(agent.did()).unwrap(),
            db.node_identity.did(),
            "task-session",
            "finish work",
            None,
            &request,
            Some(agent.as_ref()),
        )
        .await
        .unwrap();
        let rows = access.execute(&format!("{{ AgentSession {{_docID provenance}} AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ {} relationships_pending }} Goal {{_docID}} GoalCreationClaim {{_docID}} }}", crate::store::graphql::escape_graphql_string(&submitted.doc_id), crate::SIGNED_REQUEST_FIELDS)).await.unwrap();
        let session = &rows["data"]["AgentSession"][0];
        let row: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(rows["data"]["AgentRequest"][0].clone()).unwrap();
        assert_eq!(
            !rows["data"]["Goal"].as_array().unwrap().is_empty(),
            modeled("created")["goal"]
        );
        assert_eq!(
            !rows["data"]["AgentRequest"].as_array().unwrap().is_empty(),
            modeled("created")["request"]
        );
        assert_eq!(row.session_doc_id.as_deref(), session["_docID"].as_str());
        assert!(row
            .agent_doc_id
            .as_deref()
            .is_some_and(|doc| !doc.is_empty()));
        assert_eq!(
            session["provenance"]["opening_request_doc_id"],
            submitted.doc_id
        );
        crate::request::admission::verify_request_receipt_signature(&row).unwrap();
        let user_rows = client
            .execute("{ AgentSession {_docID} AgentRequest {_docID} }")
            .await
            .unwrap();
        assert_eq!(
            !user_rows["data"]["AgentSession"]
                .as_array()
                .unwrap()
                .is_empty(),
            modeled("created")["session"]
        );
        assert_eq!(
            !user_rows["data"]["AgentRequest"]
                .as_array()
                .unwrap()
                .is_empty(),
            modeled("created")["grants"]
        );
        let other_home = tempfile::tempdir().unwrap();
        let other = KeyIdentity::load_or_create(other_home.path().join("other.key"), None).unwrap();
        let admin = ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(db.node_identity.did()).unwrap(),
        );
        for permission in [
            crate::store::config_client::NodePermission::DocumentRead,
            crate::store::config_client::NodePermission::CollectionGet,
        ] {
            admin
                .grant_node_permission(&identity::Did::new(other.did()).unwrap(), permission)
                .await
                .unwrap();
        }
        let other_access =
            ConfigAccess::local_as(node.clone(), identity::Did::new(other.did()).unwrap());
        let hidden = other_access
            .execute("{ AgentRequest {_docID} }")
            .await
            .unwrap();
        assert!(hidden["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .is_empty());
        let retry = crate::goal::submit_goal_backed_request_with_identity(
            &node,
            identity::Did::new(agent.did()).unwrap(),
            db.node_identity.did(),
            "task-session",
            "finish work",
            None,
            &request,
            Some(agent.as_ref()),
        )
        .await
        .unwrap();
        assert_eq!(retry.doc_id, submitted.doc_id);
        for (field, name) in [("session", "wrong-session"), ("agent", "wrong-agent")] {
            let mut wrong = request.clone();
            if field == "session" {
                wrong.session_doc_id = Some("foreign-session".into());
            } else {
                wrong.agent_doc_id = Some("foreign-agent".into());
            }
            crate::sign_agent_request_create(agent.as_ref(), &mut wrong)
                .await
                .unwrap();
            assert!(
                crate::goal::submit_goal_backed_request_with_identity(
                    &node,
                    identity::Did::new(agent.did()).unwrap(),
                    db.node_identity.did(),
                    "task-session",
                    "finish work",
                    None,
                    &wrong,
                    Some(agent.as_ref()),
                )
                .await
                .is_err(),
                "{name}"
            );
            assert_eq!(modeled(name)["request"], false);
        }
        assert!(crate::goal::submit_goal_backed_request_with_identity(
            &node,
            identity::Did::new(db.node_identity.did()).unwrap(),
            db.node_identity.did(),
            "task-session",
            "finish work",
            None,
            &request,
            Some(db.node_identity.as_ref()),
        )
        .await
        .is_err());
        assert_eq!(modeled("foreign-actor")["request"], false);
        admin
            .grant_node_permission(
                &identity::Did::new(agent.did()).unwrap(),
                crate::store::config_client::NodePermission::DacRelationDelete,
            )
            .await
            .unwrap();
        access
            .delete_resource_relationship(
                "AgentRequest",
                &submitted.doc_id,
                "parent",
                "agents",
                row.agent_doc_id.as_deref().unwrap(),
            )
            .await
            .unwrap();
        access
            .delete_resource_relationship(
                "AgentRequest",
                &submitted.doc_id,
                "session",
                "sessions",
                row.session_doc_id.as_deref().unwrap(),
            )
            .await
            .unwrap();
        let retry = crate::goal::submit_goal_backed_request_with_identity(
            &node,
            identity::Did::new(agent.did()).unwrap(),
            db.node_identity.did(),
            "task-session",
            "finish work",
            None,
            &request,
            Some(agent.as_ref()),
        )
        .await
        .unwrap();
        assert_eq!(retry.doc_id, submitted.doc_id);
        let observed = client.execute("{ AgentRequest {_docID} }").await.unwrap();
        assert_eq!(
            !observed["data"]["AgentRequest"]
                .as_array()
                .unwrap()
                .is_empty(),
            modeled("duplicate-after-revocation")["grants"]
        );
        let mut rejected = request.clone();
        rejected.request_id = "rejected-opening".into();
        rejected.session_id = "rejected-session".into();
        rejected.retry_key = Some("rejected-retry".into());
        rejected.input.goal_assignment = Some(gents_protocol::request_input::GoalAssignmentInput {
            goal_id: "wrong-goal".into(),
            root_request_doc_id: None,
        });
        crate::sign_agent_request_create(agent.as_ref(), &mut rejected)
            .await
            .unwrap();
        assert!(crate::goal::submit_goal_backed_request_with_identity(
            &node,
            identity::Did::new(agent.did()).unwrap(),
            db.node_identity.did(),
            "rejected-session",
            "finish work",
            None,
            &rejected,
            Some(agent.as_ref()),
        )
        .await
        .is_err());
        let after = access.execute("{ AgentSession {_docID} AgentRequest {_docID} Goal {_docID} GoalCreationClaim {_docID} }").await.unwrap();
        for name in ["AgentSession", "AgentRequest", "Goal", "GoalCreationClaim"] {
            assert_eq!(after["data"][name].as_array().unwrap().len(), 1, "{name}");
        }
        assert_eq!(modeled("aborted")["session"], false);
        assert_eq!(modeled("aborted")["goal"], false);
        assert_eq!(modeled("aborted")["request"], false);
        drop(user);
    }

    #[tokio::test]
    async fn goal_reservation_retains_user_owned_session_after_unconfirmed_commit() {
        let modeled = crate::lean_vocab_test::lean_contract_snapshot()
            .goal_client_start_cases
            .iter()
            .find(|case| case["name"] == "reserved_session")
            .expect("modeled client goal reservation");
        let home = tempfile::tempdir().unwrap();
        let runtime = KeyIdentity::load_or_create(home.path().join("runtime.key"), None).unwrap();
        let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
        let (node, endpoint) = serve_client_node(&runtime).await;
        grant_operator_user(&node, &runtime, &user).await;
        let agent = crate::store::identity::provision_agent_identity(&runtime).unwrap();
        let access = client_access(&endpoint, &user);
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
        let (result, fired) = crate::store::config_client::ConfigApplyTxn::
            with_post_commit_receipt_loss_for_operation(
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
        assert_eq!(
            retried == doc,
            modeled["reservation"]["retry_reuses_session"]
                .as_bool()
                .unwrap()
        );
        let owners = access
            .transact_readonly("test.goal.owner", |txn| {
                let doc = doc.clone();
                Box::pin(async move { txn.document_owners("AgentSession", &[doc]).await })
            })
            .await
            .unwrap();
        assert_eq!(
            owners[0]
                .owner
                .as_ref()
                .is_some_and(|owner| owner.as_str() == user.did()),
            modeled["reservation"]["user_owned"].as_bool().unwrap()
        );
        let worker = ConfigAccess::local_as(node.clone(), agent.agent_did.parse().unwrap())
            .execute("{ AgentSession { _docID } }")
            .await
            .unwrap();
        assert_eq!(
            !worker["data"]["AgentSession"]
                .as_array()
                .unwrap()
                .is_empty(),
            modeled["reservation"]["agent_granted"].as_bool().unwrap()
        );
        let agent_key = crate::store::identity::resolve_agent_identity(&runtime, &agent).unwrap();
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
        crate::sign_agent_request_create(agent_key.as_ref(), &mut autonomous)
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
        assert_eq!(
            owners[0]
                .owner
                .as_ref()
                .is_some_and(|owner| owner.as_str() == agent.agent_did),
            modeled["reservation"]["agent_is_owner"].as_bool().unwrap()
        );
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
    }

    #[tokio::test]
    async fn client_session_and_request_commit_or_roll_back_together() {
        let home = tempfile::tempdir().unwrap();
        let runtime = KeyIdentity::load_or_create(home.path().join("runtime.key"), None).unwrap();
        let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
        let (node, endpoint) = serve_client_node(&runtime).await;
        grant_operator_user(&node, &runtime, &user).await;
        let agent = crate::store::identity::provision_agent_identity(&runtime).unwrap();
        let access = client_access(&endpoint, &user);
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
        crate::sign_agent_request_create(&user, &mut first)
            .await
            .unwrap();
        let (result, fired) = crate::store::config_client::ConfigApplyTxn::with_post_commit_receipt_loss_for_operation(
            Some("request.client.submit"), submit_client_request(&access, &first),
        ).await;
        assert!(fired && result.unwrap_err().is_unconfirmed());
        let model =
            &crate::lean_vocab_test::lean_contract_snapshot().client_request_publication_cases;
        let committed = crate::lean_vocab_test::lean_contract_snapshot()
            .identity_atomic_client_request_cases
            .iter()
            .find(|case| case["name"] == "created")
            .unwrap();
        let pending = access
            .execute("{ AgentRequest { relationships_pending } }")
            .await
            .unwrap();
        assert_eq!(
            pending["data"]["AgentRequest"][0]["relationships_pending"],
            committed["pending"]
        );
        assert!(reconcile_client_request(&access, &first).await.unwrap());
        let rows = access
            .execute("{ AgentRequest { _docID relationships_pending } }")
            .await
            .unwrap();
        assert_eq!(
            rows["data"]["AgentRequest"][0]["relationships_pending"],
            committed["pending"]
        );
        assert!(reconcile_client_request(&access, &first).await.unwrap());
        let wrong = ConfigAccess::local_as(node.clone(), agent.agent_did.parse().unwrap());
        let wrong_model = model
            .iter()
            .find(|case| case["name"] == "wrong_signer")
            .unwrap();
        assert_eq!(wrong_model["expected_pending"], true);
        assert!(reconcile_client_request(&wrong, &first).await.is_err());
        let agent_key = crate::store::identity::resolve_agent_identity(&runtime, &agent).unwrap();
        let agent_access = ConfigAccess::local_managed_agent(
            node.clone(),
            agent.agent_did.parse().unwrap(),
            agent_key.as_ref(),
        )
        .unwrap();
        let sibling = crate::store::identity::provision_agent_identity(&runtime).unwrap();
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
        crate::sign_agent_request_create(&user, &mut second)
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
                    let session = crate::session::load_agent_session_owner_in_txn(
                        txn,
                        runtime.did(),
                        "unattended",
                    )
                    .await?
                    .context("unattended session missing")?;
                    let (agent_doc, _) =
                        crate::store::config_client::read_desired_state_record_in_txn(
                            txn,
                            crate::Collection::Agent,
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
            .write(
                "test.client.delete_unattended",
                &format!(
                    "mutation {{ delete_AgentSession(docID: \"{}\") {{ _docID }} }}",
                    crate::store::graphql::escape_graphql_string(unattended)
                ),
            )
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
        let runtime = KeyIdentity::load_or_create(home.path().join("runtime.key"), None).unwrap();
        let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
        let other = KeyIdentity::load_or_create(home.path().join("other.key"), None).unwrap();
        let (node, endpoint) = serve_client_node(&runtime).await;
        grant_operator_user(&node, &runtime, &user).await;
        let agent = crate::store::identity::provision_agent_identity(&runtime).unwrap();
        let access = client_access(&endpoint, &user);
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
        crate::sign_agent_request_create(&user, &mut create)
            .await
            .unwrap();
        let publication = create.graphql_client_publication_mutation().unwrap();
        access
            .transact_write("test.retry.pending", |txn| {
                let publication = &publication;
                Box::pin(async move {
                    txn.execute(publication).await?;
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
                    crate::store::config_client::NodePermission::DocumentRead,
                )
                .await
                .unwrap();
        }
        let wrong = ConfigAccess::local_as(node.clone(), other.did().parse().unwrap());
        assert!(reconcile_client_retry(
            &wrong,
            runtime.did(),
            "retry-session",
            user.did(),
            "parent-doc"
        )
        .await
        .unwrap()
        .is_none());
        assert!(reconcile_client_retry(
            &access,
            runtime.did(),
            "retry-session",
            "other-requester",
            "parent-doc"
        )
        .await
        .is_err());
        let model =
            &crate::lean_vocab_test::lean_contract_snapshot().client_request_publication_cases;
        let granted = model
            .iter()
            .find(|case| case["name"] == "grant_succeeded")
            .unwrap();
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
        let rows = access
            .execute("{ AgentRequest { relationships_pending } }")
            .await
            .unwrap();
        assert_eq!(
            rows["data"]["AgentRequest"][0]["relationships_pending"],
            granted["expected_pending"]
        );
        let visible = ConfigAccess::local_as(node.clone(), agent.agent_did.parse().unwrap())
            .execute("{ AgentRequest { request_id } }")
            .await
            .unwrap();
        assert_eq!(visible["data"]["AgentRequest"][0]["request_id"], "retry");
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

    #[tokio::test]
    async fn http_client_creates_session_and_grants_only_its_selected_agent() {
        let home = tempfile::tempdir().unwrap();
        let runtime = KeyIdentity::load_or_create(home.path().join("runtime.key"), None).unwrap();
        let user = KeyIdentity::load_or_create(home.path().join("user.key"), None).unwrap();
        let other_user = KeyIdentity::load_or_create(home.path().join("other.key"), None).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let node = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(runtime.did())
                .with_node_acp_enabled()
                .with_http(defra_node::HttpConfig::with_listener(listener).unwrap())
                .build()
                .await
                .unwrap(),
        );
        crate::store::schema::ensure_runtime_schemas(&node)
            .await
            .unwrap();
        let agent = crate::store::identity::provision_agent_identity(&runtime).unwrap();
        let sibling = crate::store::identity::provision_agent_identity(&runtime).unwrap();
        let endpoint = format!("http://{address}/api/v0/graphql");
        let endpoint_for = |actor: &KeyIdentity| {
            crate::store::config_client::GraphqlEndpoint::as_principal(&endpoint, actor.did())
                .with_client_signing_capability(actor.client_signing_capability().unwrap())
                .unwrap()
        };
        let access = ConfigAccess::Graphql(endpoint_for(&user));
        let node_endpoint = endpoint_for(&runtime);
        assert!(ConfigAccess::graphql_as(&endpoint, user.did())
            .execute("{ AgentSession { _docID } }")
            .await
            .is_err());
        let client = reqwest::Client::new();
        for did in [
            user.did(),
            agent.agent_did.as_str(),
            sibling.agent_did.as_str(),
            other_user.did(),
        ] {
            for relation in [
                "read-document",
                "update-document",
                "add-dac-relation",
                "delete-dac-relation",
            ] {
                if relation == "delete-dac-relation" && did != user.did() {
                    continue;
                }
                let response = node_endpoint
                    .authorize(
                        client
                            .post(format!("http://{address}/api/v0/acp/node/relationship"))
                            .json(&serde_json::json!({"Relation": relation, "TargetActor": did})),
                    )
                    .unwrap()
                    .send()
                    .await
                    .unwrap();
                assert!(
                    response.status().is_success(),
                    "{}",
                    response.text().await.unwrap()
                );
            }
        }
        let schema = "type IdentitySchemaProbe { name: String }";
        assert!(access.add_collection(schema).await.is_err());
        ConfigAccess::Graphql(node_endpoint.clone())
            .add_collection(schema)
            .await
            .unwrap();
        seed_owned_agent(&access, runtime.did(), &agent).await;
        let mut request = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "http-request",
            runtime.did(),
            user.did(),
            &agent.agent_did,
            "http-session",
            "hello",
            "interactive",
            "2026-11-04T00:00:00Z",
            AgentRequestAdmissionRecord::client(user.did()),
        );
        let cases = &crate::lean_vocab_test::lean_contract_snapshot()
            .identity_client_session_reservation_cases;
        let modeled = |name: &str| cases.iter().find(|case| case["name"] == name).unwrap();
        let aborted: anyhow::Result<()> = access
            .transact_write("test.client.abort_session_reservation", |txn| {
                let request = &request;
                let actor = user.did();
                Box::pin(async move {
                    ensure_request_session_in_txn(txn, request, actor).await?;
                    anyhow::bail!("forced abort after client session relationships")
                })
            })
            .await;
        assert!(aborted
            .unwrap_err()
            .to_string()
            .contains("forced abort after client session relationships"));
        let before = access.execute("{ AgentSession { _docID } }").await.unwrap();
        assert_eq!(
            !before["data"]["AgentSession"]
                .as_array()
                .unwrap()
                .is_empty(),
            modeled("aborted")["session"]
        );
        let (session, agent_doc) = reserve_client_targets(&access, &request).await.unwrap();
        let created = access.execute("{ AgentSession { _docID relationships_pending _version { cid height fieldName } } }").await.unwrap();
        let row = &created["data"]["AgentSession"][0];
        assert_eq!(row["_docID"], session);
        assert_eq!(row["relationships_pending"], false);
        let version = crate::store::graphql::document_composite_version(row, "client session")
            .unwrap()
            .unwrap();
        assert_block_signer(&node, &version.cid, user.did()).await;
        let agent_access = ConfigAccess::local_as(node.clone(), agent.agent_did.parse().unwrap());
        let visible = agent_access
            .execute("{ AgentSession { _docID } }")
            .await
            .unwrap();
        assert_eq!(
            !visible["data"]["AgentSession"]
                .as_array()
                .unwrap()
                .is_empty(),
            modeled("created")["working_agent"]
        );
        access
            .delete_actor_relationship(
                "AgentSession",
                &session,
                "working_agent",
                &identity::Did::new(&agent.agent_did).unwrap(),
            )
            .await
            .unwrap();
        let duplicate = reserve_client_targets(&access, &request).await.unwrap();
        assert_eq!(duplicate.0, session);
        let visible = agent_access
            .execute("{ AgentSession { _docID } }")
            .await
            .unwrap();
        assert_eq!(
            !visible["data"]["AgentSession"]
                .as_array()
                .unwrap()
                .is_empty(),
            modeled("duplicate-after-revocation")["working_agent"]
        );
        access
            .add_actor_relationship(
                "AgentSession",
                &session,
                "working_agent",
                &identity::Did::new(&agent.agent_did).unwrap(),
            )
            .await
            .unwrap();
        request.session_doc_id = Some(session);
        request.agent_doc_id = Some(agent_doc);
        crate::sign_agent_request_create(&user, &mut request)
            .await
            .unwrap();
        let request_cases =
            &crate::lean_vocab_test::lean_contract_snapshot().identity_atomic_client_request_cases;
        let request_model = |name: &str| {
            request_cases
                .iter()
                .find(|case| case["name"] == name)
                .unwrap()
        };
        let aborted: anyhow::Result<()> = access
            .transact_write("test.client.abort_request", |txn| {
                let request = &request;
                Box::pin(async move {
                    stage_client_request_in_txn(
                        txn,
                        request,
                        request.session_doc_id.as_deref().unwrap(),
                        request.agent_doc_id.as_deref().unwrap(),
                    )
                    .await?;
                    anyhow::bail!("forced abort after client request relationships")
                })
            })
            .await;
        assert!(aborted
            .unwrap_err()
            .to_string()
            .contains("forced abort after client request relationships"));
        let before = access.execute("{ AgentRequest { _docID } }").await.unwrap();
        assert_eq!(
            !before["data"]["AgentRequest"]
                .as_array()
                .unwrap()
                .is_empty(),
            request_model("aborted")["committed"]
        );
        submit_client_request(&access, &request).await.unwrap();
        let published = access.execute("{ AgentRequest { _docID relationships_pending _version { cid height fieldName } } }").await.unwrap();
        let row = &published["data"]["AgentRequest"][0];
        let request_doc = row["_docID"].as_str().unwrap();
        assert_eq!(
            row["relationships_pending"],
            request_model("created")["pending"]
        );
        let version = crate::store::graphql::document_composite_version(row, "client request")
            .unwrap()
            .unwrap();
        assert_block_signer(&node, &version.cid, user.did()).await;
        let removed = node
            .delete_dac_actor_relationship(
                user.did(),
                "AgentRequest",
                request_doc,
                "working_agent",
                &agent.agent_did,
            )
            .await
            .unwrap();
        assert_eq!(removed, request_model("created")["grants"]);
        assert!(reconcile_client_request(&access, &request).await.unwrap());
        let restored = node
            .delete_dac_actor_relationship(
                user.did(),
                "AgentRequest",
                request_doc,
                "working_agent",
                &agent.agent_did,
            )
            .await
            .unwrap();
        assert_eq!(
            restored,
            request_model("duplicate-after-revocation")["grants"]
        );
        let query = "{ AgentSession { session_id } }";
        let sibling_access =
            ConfigAccess::local_as(node.clone(), sibling.agent_did.parse().unwrap());
        let other_access = ConfigAccess::Graphql(endpoint_for(&other_user));
        let runtime_access = ConfigAccess::Graphql(node_endpoint.clone());
        for (reader, did, expected) in [
            (&access, user.did(), 1),
            (&agent_access, agent.agent_did.as_str(), 1),
            (&sibling_access, sibling.agent_did.as_str(), 0),
            (&other_access, other_user.did(), 0),
            (&runtime_access, runtime.did(), 0),
        ] {
            let result = reader.execute(query).await.unwrap();
            assert_eq!(
                result["data"]["AgentSession"].as_array().unwrap().len(),
                expected,
                "actor {did}"
            );
        }
        let identity = gents_protocol::trigger_delivery::FireIdentity {
            owner_did: runtime.did().into(),
            trigger_id: "manual:task:http-invocation".into(),
            source_collection: "Task".into(),
            source_doc_id: "http-task".into(),
        };
        let task_request_id = identity.request_id();
        let fire = gents_protocol::trigger_delivery::TriggerFire {
            fire_key: super::super::task_fire_key(&identity),
            identity,
            task_id: "task".into(),
            request_id: task_request_id,
            session_id: request.session_id.clone(),
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
            created_at: "2026-11-04T00:00:00Z".into(),
        };
        let mut manual = request.clone();
        manual.request_id = fire.request_id.clone();
        manual.caused_by_trigger_kind = Some("manual".into());
        manual.retry_key = Some(fire.fire_key.clone());
        crate::sign_agent_request_create(&user, &mut manual)
            .await
            .unwrap();
        let first = super::super::write_task_delivery(&access, &fire, true, &manual)
            .await
            .unwrap();
        assert!(!first.duplicate);
        let query = format!("{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{}\" }} }}) {{ _docID relationships_pending _version {{ cid height fieldName }} }} }}", crate::store::graphql::escape_graphql_string(&first.request.doc_id));
        let response = access.execute(&query).await.unwrap();
        let row = &response["data"]["AgentRequest"][0];
        assert_eq!(
            row["relationships_pending"],
            request_model("created")["pending"]
        );
        let version = crate::store::graphql::document_composite_version(row, "client Task request")
            .unwrap()
            .unwrap();
        assert_block_signer(&node, &version.cid, user.did()).await;
        let removed = node
            .delete_dac_actor_relationship(
                user.did(),
                "AgentRequest",
                &first.request.doc_id,
                "working_agent",
                &agent.agent_did,
            )
            .await
            .unwrap();
        assert_eq!(removed, request_model("created")["grants"]);
        let duplicate = super::super::write_task_delivery(&access, &fire, true, &manual)
            .await
            .unwrap();
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.request.doc_id, first.request.doc_id);
        let restored = node
            .delete_dac_actor_relationship(
                user.did(),
                "AgentRequest",
                &first.request.doc_id,
                "working_agent",
                &agent.agent_did,
            )
            .await
            .unwrap();
        assert_eq!(
            restored,
            request_model("duplicate-after-revocation")["grants"]
        );
        let legacy_session = access.transact_write("test.client.legacy_pending_session", |txn| {
            let agent = &agent.agent_did;
            let node_did = runtime.did();
            let requester = user.did();
            Box::pin(async move {
                let response = txn.execute_client_signed_create(
                    "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }",
                    &serde_json::json!({"input": {"node_did": node_did, "session_id": "legacy-http-session", "requester_did": requester, "agent_did": agent, "created_at": "2026-11-04T00:00:00Z", "relationships_pending": true}}),
                    gents_protocol::schemas::AGENT_SESSION,
                ).await?;
                Ok::<_, anyhow::Error>(crate::store::graphql::created_doc_id(&response, "AgentSession")?)
            })
        }).await.unwrap();
        let mut legacy = request.clone();
        legacy.request_id = "legacy-http-request".into();
        legacy.session_id = "legacy-http-session".into();
        legacy.session_doc_id = Some(legacy_session.clone());
        crate::sign_agent_request_create(&user, &mut legacy)
            .await
            .unwrap();
        assert!(matches!(
            submit_client_request(&access, &legacy).await,
            Err(ClientSubmissionError::Rejected(_))
        ));
        let repaired = reserve_client_targets(&access, &legacy).await.unwrap();
        assert_eq!(repaired.0, legacy_session);
        let repaired_session = access.execute("{ AgentSession(filter: { session_id: { _eq: \"legacy-http-session\" } }) { relationships_pending _version { cid height fieldName } } }").await.unwrap();
        let row = &repaired_session["data"]["AgentSession"][0];
        assert_eq!(row["relationships_pending"], false);
        let version =
            crate::store::graphql::document_composite_version(row, "repaired client session")
                .unwrap()
                .unwrap();
        assert_block_signer(&node, &version.cid, user.did()).await;
        submit_client_request(&access, &legacy).await.unwrap();
        node.shutdown().await.expect("shut down test node");
    }
}
