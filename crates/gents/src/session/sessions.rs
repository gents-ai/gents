use super::query::{decode_session_row, session_scope_filter, validate_agent_session};
use super::*;
use anyhow::Context;

/// The caller must finalize this provisional session in the same transaction
/// and propagate every finalization failure so its immutable starter cannot
/// become durable without the verified opening request and tool receipt.
pub(crate) async fn stage_agent_new_session_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    create: &gents_protocol::request_admission::AgentRequestCreate,
) -> Result<String> {
    use gents_protocol::request_input::SessionCreationIntent;
    use gents_protocol::row::AgentRequestRow;
    use gents_protocol::session::SessionProvenance;

    let source = create
        .input
        .started_by
        .as_ref()
        .context("agent_new lacks its source session")?;
    let parent_doc = create
        .caused_by_parent_request_doc_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .context("agent_new lacks its parent request")?;
    let tool_doc = create
        .caused_by_parent_tool_call_doc_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .context("agent_new lacks its parent tool call")?;
    anyhow::ensure!(
        txn.actor_did() == Some(create.requester_did.as_str())
            && create.admission.signer_did == create.requester_did
            && create.input.session_creation == Some(SessionCreationIntent::AgentNew)
            && source.node_did == create.node_did
            && !create.node_did.trim().is_empty()
            && !create.agent_did.trim().is_empty()
            && !create.session_id.trim().is_empty(),
        "agent_new provisional session does not match its authenticated local author"
    );
    chrono::DateTime::parse_from_rfc3339(&create.created_at)
        .context("invalid session created_at")?;
    let candidate = AgentRequestRow {
        request_id: create.request_id.clone(),
        requester_did: Some(create.requester_did.clone()),
        admission_signer_did: Some(create.admission.signer_did.clone()),
        caused_by_parent_request_doc_id: create.caused_by_parent_request_doc_id.clone(),
        caused_by_parent_request_id: create.caused_by_parent_request_id.clone(),
        caused_by_parent_tool_call_doc_id: create.caused_by_parent_tool_call_doc_id.clone(),
        caused_by_parent_tool_call_id: create.caused_by_parent_tool_call_id.clone(),
        ..Default::default()
    };
    let verified = crate::session::origin::verified_agent_source(txn, &candidate)
        .await?
        .context("agent_new source is unavailable or invalid")?;
    anyhow::ensure!(
        verified.node_did == source.node_did
            && verified.session_id == source.session_id
            && verified.requester_did == source.requester_did,
        "agent_new source differs from its authenticated parent"
    );
    let escaped_tool = escape_graphql_string(tool_doc);
    let tool = txn.execute(&format!(r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped_tool}" }} }}, limit: 2) {{ _docID tool_name }} }}"#)).await?;
    let tool_rows = tool["data"]["AgentToolCall"]
        .as_array()
        .context("agent_new tool lookup omitted rows")?;
    anyhow::ensure!(
        tool_rows.len() == 1
            && tool_rows[0]["_docID"] == tool_doc
            && tool_rows[0]["tool_name"] == crate::tools::native::AGENT_NEW_TOOL_NAME,
        "agent_new source is not its accepted tool call"
    );
    let existing =
        load_agent_session_owner_in_txn(txn, &create.node_did, &create.session_id).await?;
    anyhow::ensure!(
        existing.is_none(),
        "agent_new session identity already exists"
    );
    let provenance = SessionProvenance {
        parent_request_doc_id: Some(parent_doc.to_owned()),
        started_by: Some(source.clone()),
        ..Default::default()
    };
    let response = txn.execute_with_variables(
        "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }",
        &serde_json::json!({"input": {
            "session_id": create.session_id, "node_did": create.node_did,
            "requester_did": create.requester_did, "agent_did": create.agent_did,
            "created_at": create.created_at, "title": create.input.initial_title,
            "relationships_pending": true, "started_by_node_did": source.node_did,
            "started_by_session_id": source.session_id, "provenance": provenance
        }}),
    ).await?;
    crate::store::graphql::created_doc_id(&response, "AgentSession").map_err(Into::into)
}

/// Complete the provisional session before its transaction can commit.
pub(crate) async fn finalize_agent_new_session_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    create: &gents_protocol::request_admission::AgentRequestCreate,
    session_doc_id: &str,
    opening_doc_id: &str,
) -> Result<()> {
    use gents_protocol::request_admission::AgentRequestAdmissionKind;
    use gents_protocol::session::{
        SessionCreationCause, SessionCreationStarter, SessionProvenance,
    };

    let source = create
        .input
        .started_by
        .as_ref()
        .context("agent_new lacks its source session")?;
    let parent = create
        .caused_by_parent_request_doc_id
        .as_deref()
        .context("agent_new lacks its parent request")?;
    let tool = create
        .caused_by_parent_tool_call_doc_id
        .as_deref()
        .context("agent_new lacks its parent tool")?;
    anyhow::ensure!(
        txn.actor_did() == Some(create.requester_did.as_str())
            && create.admission.kind == AgentRequestAdmissionKind::Client
            && create.admission.signer_did == create.requester_did
            && create.session_doc_id.as_deref() == Some(session_doc_id)
            && create
                .agent_doc_id
                .as_deref()
                .is_some_and(|id| !id.is_empty()),
        "agent_new signed physical targets or actor differ"
    );
    let session = load_agent_session_owner_in_txn(txn, &create.node_did, &create.session_id)
        .await?
        .context("agent_new provisional session disappeared")?;
    anyhow::ensure!(
        session.doc_id == session_doc_id
            && session.session.agent_did == create.agent_did
            && session.session.requester_did.as_deref() == Some(create.requester_did.as_str()),
        "agent_new provisional session binding changed"
    );
    let provisional = SessionProvenance {
        parent_request_doc_id: Some(parent.to_owned()),
        started_by: Some(source.clone()),
        ..Default::default()
    };
    anyhow::ensure!(
        session.session.provenance.as_ref() == Some(&provisional),
        "agent_new provisional source changed"
    );
    let session_doc = escape_graphql_string(session_doc_id);
    let session_row = txn.execute(&format!(r#"{{ AgentSession(filter: {{ _docID: {{ _eq: "{session_doc}" }} }}, limit: 2) {{ _docID relationships_pending started_by_node_did started_by_session_id }} }}"#)).await?;
    let session_rows = session_row["data"]["AgentSession"]
        .as_array()
        .context("agent_new provisional lookup omitted rows")?;
    anyhow::ensure!(
        session_rows.len() == 1
            && session_rows[0]["_docID"] == session_doc_id
            && session_rows[0]["relationships_pending"] == true
            && session_rows[0]["started_by_node_did"] == source.node_did
            && session_rows[0]["started_by_session_id"] == source.session_id,
        "agent_new provisional immutable starter changed"
    );
    let escaped = escape_graphql_string(opening_doc_id);
    let response = txn.execute(&format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 2) {{ {} }} }}"#, crate::request::admission::SIGNED_REQUEST_FIELDS)).await?;
    let rows = response["data"]["AgentRequest"]
        .as_array()
        .context("agent_new opening lookup omitted rows")?;
    let [row] = rows.as_slice() else {
        anyhow::bail!("agent_new opening request is unavailable or ambiguous")
    };
    let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(row.clone())?;
    crate::request::admission::verify_request_receipt_signature(&row)?;
    let signature = bs58::encode(&create.admission.signature).into_string();
    anyhow::ensure!(
        row.doc_id.as_deref() == Some(opening_doc_id)
            && row.request_id == create.request_id
            && row.session_doc_id.as_deref() == Some(session_doc_id)
            && row.agent_doc_id == create.agent_doc_id
            && row.admission_signer_did.as_deref() == Some(create.requester_did.as_str())
            && row.admission_signature.as_deref() == Some(signature.as_str())
            && crate::request::admission::signed_request_matches_create(&row, create)?,
        "agent_new staged opening differs from its signed intent"
    );
    anyhow::ensure!(
        crate::session::origin::agent_new_binding_in_txn(txn, &row, opening_doc_id)
            .await?
            .as_deref()
            == Some(tool),
        "agent_new tool receipt does not match its opening"
    );
    let mut provenance = SessionProvenance {
        parent_request_doc_id: Some(parent.to_owned()),
        started_by: Some(source.clone()),
        opening_request_doc_id: Some(opening_doc_id.to_owned()),
        ..Default::default()
    };
    anyhow::ensure!(
        crate::session::origin::verified_starter_in_txn(
            txn,
            &provenance,
            &create.node_did,
            &create.session_id,
            Some(&create.requester_did),
            &create.agent_did
        )
        .await?
        .as_ref()
            == Some(source),
        "agent_new starter does not match its opening"
    );
    let receipt = crate::session::origin::issue_creation_receipt_in_txn(
        txn,
        &create.node_did,
        &create.session_id,
        session_doc_id,
        Some(&create.requester_did),
        &create.agent_did,
        Some(opening_doc_id),
    )
    .await?
    .context("agent_new target creation receipt unavailable")?;
    anyhow::ensure!(
        receipt.cause == SessionCreationCause::AgentNew
            && receipt.cause_binding == tool
            && receipt.starter
                == Some(SessionCreationStarter {
                    source: source.clone(),
                    parent_request_doc_id: parent.to_owned()
                }),
        "agent_new creation receipt differs from verified starter"
    );
    provenance.creation_receipt = Some(receipt);
    let doc = escape_graphql_string(session_doc_id);
    let result = txn.execute_with_variables(
        &format!(r#"mutation($provenance: JSON) {{ update_AgentSession(docID: "{doc}", filter: {{ relationships_pending: {{ _eq: true }} }}, input: {{ provenance: $provenance }}) {{ _docID }} }}"#),
        &serde_json::json!({"provenance": provenance}),
    ).await?;
    let updated = result["data"]["update_AgentSession"]
        .as_array()
        .context("agent_new provenance update omitted rows")?;
    anyhow::ensure!(
        updated.len() == 1 && updated[0]["_docID"] == session_doc_id,
        "agent_new provenance update did not match its provisional session"
    );
    Ok(())
}

/// Finish a newly created session's native relationships as its creator.
/// Retry only the retained creation intent: replaying a completed creation
/// after an explicit revocation would otherwise restore that grant.
pub(crate) async fn grant_created_session_relationships(
    access: &crate::ConfigAccess,
    session_doc_id: &str,
    agent_doc_id: &str,
    agent_did: &str,
) -> Result<()> {
    let target = identity::Did::new(agent_did)?;
    access
        .transact_write("session.relationships_published", |txn| {
            let target = &target;
            Box::pin(async move {
                let actor = txn
                    .actor_did()
                    .context("session relationship publication requires an actor")?;
                let doc = escape_graphql_string(session_doc_id);
                let response = txn.execute(&format!(
                    r#"{{ AgentSession(filter: {{ _docID: {{ _eq: "{doc}" }} }}, limit: 2) {{ _docID node_did agent_did relationships_pending }} }}"#,
                )).await?;
                let rows = response["data"]["AgentSession"]
                    .as_array()
                    .context("session relationship recovery omitted rows")?;
                let [row] = rows.as_slice() else {
                    anyhow::bail!("session relationship recovery requires one visible session");
                };
                anyhow::ensure!(
                    row["_docID"].as_str() == Some(session_doc_id)
                        && row["agent_did"].as_str() == Some(agent_did),
                    "session relationship recovery cannot verify the selected agent"
                );
                let node_did = row["node_did"]
                    .as_str()
                    .context("session relationship recovery omitted node identity")?;
                let (actual_agent_doc, _) =
                    crate::store::config_client::read_desired_state_record_in_txn(
                        txn,
                        crate::Collection::Agent,
                        node_did,
                        agent_did,
                    )
                    .await?
                    .context("session relationship recovery selected agent is unavailable")?;
                anyhow::ensure!(
                    actual_agent_doc == agent_doc_id,
                    "session relationship recovery selected agent document changed"
                );
                match row["relationships_pending"].as_bool() {
                    Some(false) => return Ok::<(), anyhow::Error>(()),
                    Some(true) => {}
                    None => anyhow::bail!("session relationship publication marker is unavailable"),
                }
                let owners = txn
                    .document_owners("AgentSession", &[session_doc_id.to_owned()])
                    .await?;
                anyhow::ensure!(
                    owners.len() == 1
                        && owners[0].doc_id == session_doc_id
                        && owners[0].visible
                        && owners[0].owner.as_ref().is_some_and(|owner| owner.as_str() == actor),
                    "session relationship publication requires its native creator"
                );
                txn.add_resource_relationship(
                    "AgentSession", session_doc_id, "parent", "agents", agent_doc_id,
                )
                .await?;
                txn.add_actor_relationship("AgentSession", session_doc_id, "working_agent", target)
                    .await?;
                let response = txn.execute(&format!(
                    r#"mutation {{ update_AgentSession(docID: "{doc}", filter: {{ relationships_pending: {{ _eq: true }} }}, input: {{ relationships_pending: false }}) {{ _docID }} }}"#,
                )).await?;
                let updated = response["data"]["update_AgentSession"]
                    .as_array()
                    .context("session relationship completion omitted rows")?;
                anyhow::ensure!(
                    updated.len() == 1 && updated[0]["_docID"].as_str() == Some(session_doc_id),
                    "session relationship completion was not recorded"
                );
                Ok(())
            })
        })
        .await
}

#[cfg(test)]
pub(crate) async fn create_session_with_id(
    node: &std::sync::Arc<EmbeddedNode>,
    session_id: &str,
    agent_did: &str,
    node_did: &str,
) -> Result<()> {
    create_session_with_agent_did(node, session_id, node_did, agent_did).await
}

#[cfg(test)]
pub(crate) async fn create_session_with_agent_did(
    node: &std::sync::Arc<EmbeddedNode>,
    session_id: &str,
    node_did: &str,
    agent_did: &str,
) -> Result<()> {
    create_session_with_agent_did_and_requester_did(node, session_id, node_did, agent_did, None)
        .await
}

#[cfg(test)]
async fn create_session_with_agent_did_and_requester_did(
    node: &std::sync::Arc<EmbeddedNode>,
    session_id: &str,
    node_did: &str,
    agent_did: &str,
    requester_did: Option<&str>,
) -> Result<()> {
    crate::store::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "session.create",
        move |txn| {
            Box::pin(async move {
                ensure_session_in_txn(
                    txn,
                    session_id,
                    node_did,
                    agent_did,
                    requester_did,
                    None,
                    None,
                    &chrono::Utc::now().to_rfc3339(),
                )
                .await?;
                Ok::<bool, anyhow::Error>(true)
            })
        },
    )
    .await
    .map(|_| ())
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn ensure_session_with_agent_did_and_requester_did(
    node: &std::sync::Arc<EmbeddedNode>,
    session_id: &str,
    node_did: &str,
    agent_did: &str,
    requester_did: Option<&str>,
) -> Result<()> {
    create_session_with_agent_did_and_requester_did(
        node,
        session_id,
        node_did,
        agent_did,
        requester_did,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn ensure_session_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    session_id: &str,
    node_did: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    title: Option<gents_protocol::session::SessionTitle>,
    mut provenance: Option<gents_protocol::session::SessionProvenance>,
    now: &str,
) -> Result<bool> {
    anyhow::ensure!(
        !session_id.trim().is_empty(),
        "session_id must be non-empty"
    );
    anyhow::ensure!(!node_did.trim().is_empty(), "node_did must be non-empty");
    anyhow::ensure!(!agent_did.trim().is_empty(), "agent_did must be non-empty");
    if let Some(requester_did) = requester_did.map(str::trim) {
        anyhow::ensure!(
            !requester_did.is_empty(),
            "requester_did must be non-empty when present"
        );
    }
    chrono::DateTime::parse_from_rfc3339(now).context("invalid session created_at")?;

    let scope = session_scope_filter(node_did, session_id, requester_did);
    let query = format!(
        r#"{{
            AgentSession(filter: {{ {scope} }}) {{
                {SESSION_SCOPE_FIELDS}
            }}
        }}"#
    );
    let response = txn.execute(&query).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("AgentSession"))
        .and_then(serde_json::Value::as_array)
        .context("AgentSession query omitted rows")?
        .clone();
    let rows: Vec<serde_json::Value> = rows;
    anyhow::ensure!(
        rows.len() <= 1,
        "session create found duplicate AgentSession rows for session_id={session_id}"
    );

    if let Some(row) = rows.first() {
        let session = decode_session_row(row)?.session;
        validate_agent_session(&session)?;
        anyhow::ensure!(
            session.session_id == session_id,
            "AgentSession scope mismatch: existing session_id={} requested={session_id}",
            session.session_id
        );
        anyhow::ensure!(
            session.node_did == node_did,
            "AgentSession scope mismatch: existing node_did={} requested={node_did}",
            session.node_did
        );
        match (&session.requester_did, requester_did) {
            (Some(existing), Some(requested)) => anyhow::ensure!(
                existing == requested,
                "AgentSession requester scope mismatch: existing={existing} requested={requested}"
            ),
            (Some(existing), None) => anyhow::bail!(
                "AgentSession requester scope mismatch: existing requester scope {existing} \
                 does not match absent scope"
            ),
            (None, Some(requested)) => anyhow::bail!(
                "AgentSession requester scope mismatch: existing absent scope does not match \
                 requester {requested}"
            ),
            (None, None) => {}
        }
        anyhow::ensure!(
            session.agent_did == agent_did,
            "AgentSession agent mismatch: existing={} requested={agent_did}",
            session.agent_did
        );
        return Ok(false);
    }

    if let Some(origin) = provenance.as_mut() {
        origin.started_by = crate::session::origin::verified_starter_in_txn(
            txn,
            origin,
            node_did,
            session_id,
            requester_did,
            agent_did,
        )
        .await?;
        origin.creation_receipt = None;
    }

    let created = txn.execute_with_variables(
        "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }",
        &serde_json::json!({"input": {
            "session_id": session_id, "node_did": node_did,
            "requester_did": requester_did, "agent_did": agent_did,
            "created_at": now, "title": title, "relationships_pending": true,
            "started_by_node_did": provenance.as_ref().and_then(|p| p.started_by.as_ref()).map(|s| &s.node_did),
            "started_by_session_id": provenance.as_ref().and_then(|p| p.started_by.as_ref()).map(|s| &s.session_id),
            "provenance": provenance
        }}),
    ).await?;
    let created = defra_node::QueryResponse::success(
        created
            .get("data")
            .context("session create omitted data")?
            .clone(),
    );
    let created_doc =
        crate::store::graphql::single_mutation_document(&created, "create_AgentSession")?;
    if created_doc.is_none() {
        // Revalidate scope through the same preserved-document contract so a
        // colliding writer cannot silently win with a different binding.
        let response = txn.execute(&query).await?;
        let rows = response
            .get("data")
            .and_then(|data| data.get("AgentSession"))
            .and_then(serde_json::Value::as_array)
            .context("AgentSession query omitted rows")?
            .clone();
        if rows.len() == 1 {
            let session = decode_session_row(&rows[0])?.session;
            validate_agent_session(&session)?;
            anyhow::ensure!(
                session.session_id == session_id
                    && session.node_did == node_did
                    && session.agent_did == agent_did
                    && session.requester_did.as_deref() == requester_did,
                "AgentSession scope mismatch raced with a concurrent create"
            );
            return Ok(false);
        }
        anyhow::bail!("session create matched no document");
    }
    let doc_id = created_doc
        .and_then(|doc| doc.get("_docID"))
        .and_then(serde_json::Value::as_str)
        .context("created session lacks physical identity")?;
    if let Some(origin) = provenance.as_mut() {
        origin.creation_receipt = crate::session::origin::issue_creation_receipt_in_txn(
            txn,
            node_did,
            session_id,
            doc_id,
            requester_did,
            agent_did,
            origin.opening_request_doc_id.as_deref(),
        )
        .await?;
        if origin.creation_receipt.is_some() {
            let mutation = format!("mutation($provenance: JSON) {{update_AgentSession(docID: \"{}\", input: {{provenance: $provenance}}) {{_docID}}}}", crate::store::graphql::escape_graphql_string(doc_id));
            let response = txn
                .execute_with_variables(&mutation, &serde_json::json!({"provenance": origin}))
                .await?;
            let response = defra_node::QueryResponse::success(
                response
                    .get("data")
                    .context("session receipt update omitted data")?
                    .clone(),
            );
            anyhow::ensure!(
                crate::store::graphql::single_mutation_document(&response, "update_AgentSession")?
                    .is_some(),
                "session creation receipt was not attached"
            );
        }
    }
    tracing::info!(
        session_id = %session_id,
        node_did = %node_did,
        agent_did = %agent_did,
        created = true,
        "session created"
    );
    Ok(true)
}

pub(crate) async fn reopen_session_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    now: &str,
) -> Result<bool> {
    let Some(row) = load_agent_session_row_in_txn(txn, node_did, session_id, requester_did).await?
    else {
        anyhow::bail!("reopening session: no AgentSession for session_id={session_id}");
    };
    let session = &row.session;
    anyhow::ensure!(
        session.node_did == node_did,
        "AgentSession scope mismatch: existing node_did={} requested={}",
        session.node_did,
        node_did.trim()
    );
    match (
        session.requester_did.as_deref(),
        requester_did.map(str::trim),
    ) {
        (Some(existing), Some(requested)) => anyhow::ensure!(
            existing == requested,
            "AgentSession requester scope mismatch: existing={existing} requested={requested}"
        ),
        (Some(existing), None) => anyhow::bail!(
            "AgentSession requester scope mismatch: existing requester scope {existing} does \
             not match absent scope"
        ),
        (None, Some(_)) => anyhow::bail!(
            "AgentSession requester scope mismatch: existing absent scope does not match a \
             requester"
        ),
        (None, None) => {}
    }
    let was_closed = session.closed_at.is_some();
    if was_closed {
        patch_session_in_txn(
            txn,
            &row.doc_id,
            serde_json::json!({
                "closed_at": null,
                "observation": touched_observation(&row, now),
            }),
        )
        .await?;
    }
    Ok(was_closed)
}

pub(crate) async fn touch_session_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    row: &super::rows::SessionOwnerRow,
    now: &str,
) -> Result<()> {
    patch_session_in_txn(
        txn,
        &row.doc_id,
        serde_json::json!({ "observation": touched_observation(row, now) }),
    )
    .await
}

pub async fn close_session(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    chrono::DateTime::parse_from_rfc3339(&now).context("invalid session closed_at")?;
    crate::store::config_client::ConfigAccess::transact_local_write(
        node,
        Some(crate::session::load_session_agent_did(node, node_did, session_id).await?),
        "session.close",
        move |txn| {
            let now = now.clone();
            Box::pin(async move {
                let Some(row) =
                    load_agent_session_row_in_txn(txn, node_did, session_id, requester_did).await?
                else {
                    anyhow::bail!("closing session: no AgentSession for session_id={session_id}");
                };
                if row.session.closed_at.is_some() {
                    return Ok::<(), anyhow::Error>(());
                }
                patch_session_in_txn(
                    txn,
                    &row.doc_id,
                    serde_json::json!({
                        "closed_at": now,
                        "observation": touched_observation(&row, &now),
                    }),
                )
                .await?;
                tracing::info!(session_id = %session_id, "session closed");
                Ok(())
            })
        },
    )
    .await
}

pub(super) const SESSION_SCOPE_FIELDS: &str = "session_id node_did requester_did agent_did \
created_at closed_at title provenance observation tags _docID";

pub async fn load_agent_session_row_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<super::rows::SessionOwnerRow>> {
    Ok(load_agent_session_owner_in_txn(txn, node_did, session_id)
        .await?
        .filter(|row| row.session.requester_did.as_deref() == requester_did))
}

pub async fn load_agent_session_owner_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<Option<super::rows::SessionOwnerRow>> {
    let scope = format!(
        r#"node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(session_id),
    );
    let query = format!(
        r#"{{
            AgentSession(filter: {{ {scope} }}, limit: 2) {{
                {SESSION_SCOPE_FIELDS}
            }}
        }}"#
    );
    let response = txn.execute(&query).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("AgentSession"))
        .and_then(serde_json::Value::as_array)
        .context("AgentSession query omitted rows")?
        .clone();
    anyhow::ensure!(
        rows.len() <= 1,
        "duplicate AgentSession rows for session_id={session_id}"
    );
    rows.first().map(decode_session_row).transpose()
}

pub(super) fn touch_activity_value(row: &super::rows::SessionOwnerRow, now: &str) -> String {
    let old = row
        .session
        .observation
        .as_ref()
        .map(|observation| observation.last_activity_at.clone())
        .unwrap_or_else(|| row.session.created_at.clone());
    match (
        chrono::DateTime::parse_from_rfc3339(&old),
        chrono::DateTime::parse_from_rfc3339(now),
    ) {
        (Ok(old_time), Ok(now_time)) if old_time > now_time => old,
        _ => now.to_string(),
    }
}

pub(super) fn touched_observation(
    row: &super::rows::SessionOwnerRow,
    now: &str,
) -> gents_protocol::session::SessionObservation {
    gents_protocol::session::SessionObservation {
        reply_preview: row
            .session
            .observation
            .as_ref()
            .and_then(|v| v.reply_preview.clone()),
        last_activity_at: touch_activity_value(row, now),
        preview: row
            .session
            .observation
            .as_ref()
            .and_then(|v| v.preview.clone()),
        latest_request: row
            .session
            .observation
            .as_ref()
            .and_then(|v| v.latest_request.clone()),
    }
}

pub(super) async fn patch_session_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    doc_id: &str,
    input: serde_json::Value,
) -> Result<()> {
    let doc_id = escape_graphql_string(doc_id);
    let mutation = format!(
        r#"mutation($input: AgentSessionMutationInputArg!) {{
        update_AgentSession(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, input: $input) {{ _docID }}
    }}"#
    );
    let response = txn
        .execute_with_variables(&mutation, &serde_json::json!({"input": input}))
        .await?;
    anyhow::ensure!(
        response
            .get("data")
            .and_then(|data| data.get("update_AgentSession"))
            .is_some_and(crate::store::graphql::response_has_documents),
        "session patch matched no document"
    );
    Ok(())
}

pub(crate) async fn max_sequence_as(
    node: &std::sync::Arc<EmbeddedNode>,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    actor: Option<::identity::Did>,
) -> Result<u32> {
    crate::store::config_client::ConfigAccess::transact_local_readonly(
        node,
        actor,
        "session.max_sequence",
        |txn| {
            Box::pin(
                async move { max_sequence_in_txn(txn, session_id, node_did, requester_did).await },
            )
        },
    )
    .await
}

#[cfg(test)]
pub(crate) async fn max_sequence(
    node: &std::sync::Arc<EmbeddedNode>,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<u32> {
    max_sequence_as(node, session_id, node_did, requester_did, None).await
}

pub(super) async fn max_sequence_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<u32> {
    let scope = session_scope_filter(node_did, session_id, requester_did);
    let response = txn.execute(&format!(r#"{{ AgentMessage(filter: {{ {scope} }}, order: {{sequence: DESC}}, limit: 1) {{ sequence }} }}"#)).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("AgentMessage"))
        .and_then(serde_json::Value::as_array)
        .context("message sequence query omitted rows")?;
    match rows.first() {
        None => Ok(0),
        Some(row) => u32::try_from(
            row.get("sequence")
                .and_then(serde_json::Value::as_u64)
                .context("message sequence is invalid")?,
        )
        .context("message sequence exceeds u32"),
    }
}

/// Recover only sessions whose native owner is the authenticated agent. A row's
/// agent field cannot authenticate its creator or authorize relationship grants.
pub(crate) async fn recover_agent_session_relationships(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    agent: &identity::Did,
) -> Result<usize> {
    let access = crate::ConfigAccess::local_as(node.clone(), agent.clone());
    let agent_did = agent.to_string();
    let pending = access.transact_readonly("session.recover_relationships", |txn| {
        let agent_did = &agent_did;
        Box::pin(async move {
            let filter = format!(
                r#"node_did: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }}, relationships_pending: {{ _eq: true }}"#,
                escape_graphql_string(node_did), escape_graphql_string(agent_did),
            );
            let rows = txn.cursor_rows("AgentSession", "_docID", &filter).await?;
            if rows.is_empty() {
                return Ok::<_, anyhow::Error>((String::new(), Vec::new()));
            }
            let (agent_doc, _) = crate::store::config_client::read_desired_state_record_in_txn(
                txn, crate::Collection::Agent, node_did, agent_did,
            ).await?.context("selected agent is unavailable for session relationship recovery")?;
            let mut pending = Vec::new();
            for chunk in rows.chunks(64) {
                let ids: Vec<String> = chunk.iter().map(|row| row["_docID"].as_str()
                    .context("session recovery row lacks document ID").map(str::to_owned))
                    .collect::<Result<_>>()?;
                let owners = txn.document_owners("AgentSession", &ids).await?;
                anyhow::ensure!(owners.len() == ids.len(), "session ownership observation is incomplete");
                for owner in owners {
                    if ids.contains(&owner.doc_id) && owner.visible
                        && owner.owner.as_ref().is_some_and(|did| did.as_str() == agent_did)
                    {
                        pending.push(owner.doc_id);
                    }
                }
            }
            Ok::<_, anyhow::Error>((agent_doc, pending))
        })
    }).await?;
    for doc_id in &pending.1 {
        grant_created_session_relationships(&access, doc_id, &pending.0, &agent_did).await?;
    }
    Ok(pending.1.len())
}

#[cfg(test)]
mod relationship_publication_tests {
    use super::*;
    use crate::store::config_client::{ConfigAccess, NodePermission};
    use crate::store::identity::NodeIdentity;
    use std::sync::Arc;

    #[tokio::test]
    async fn authorized_noncreator_can_use_completed_but_cannot_publish_pending_session() {
        let directory = tempfile::tempdir().unwrap();
        let node_identity =
            crate::KeyIdentity::load_or_create(directory.path().join("node"), None).unwrap();
        let creator =
            crate::KeyIdentity::load_or_create(directory.path().join("creator"), None).unwrap();
        let collaborator =
            crate::KeyIdentity::load_or_create(directory.path().join("collaborator"), None)
                .unwrap();
        let node = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(node_identity.did())
                .with_node_acp_enabled()
                .build()
                .await
                .unwrap(),
        );
        crate::store::schema::ensure_runtime_schemas(&node)
            .await
            .unwrap();
        let admin = ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(node_identity.did()).unwrap(),
        );
        let creator_did = identity::Did::new(creator.did()).unwrap();
        let collaborator_did = identity::Did::new(collaborator.did()).unwrap();
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
        ] {
            admin
                .grant_node_permission(&creator_did, permission)
                .await
                .unwrap();
        }
        admin
            .grant_node_permission(&collaborator_did, NodePermission::DocumentRead)
            .await
            .unwrap();
        let owner = ConfigAccess::local_as(node.clone(), creator_did);
        let other = ConfigAccess::local_as(node.clone(), collaborator_did.clone());
        let agent = owner.write("test.session_relationship_agent", &format!(
            r#"mutation {{ create_Agent(input: {{ node_did: "{}", agent_did: "{}", creation_nonce: "{}", inference_profile_id: "profile", enabled: true }}) {{ _docID }} }}"#,
            node_identity.did(), creator.did(), "a".repeat(64),
        )).await.unwrap();
        let agent_doc = crate::store::graphql::created_doc_id(&agent, "Agent").unwrap();
        owner
            .add_actor_relationship("Agent", &agent_doc, "reader", &collaborator_did)
            .await
            .unwrap();
        let session = owner.write("test.session_relationship_session", &format!(
            r#"mutation {{ create_AgentSession(input: {{ node_did: "{}", session_id: "child", requester_did: "{}", agent_did: "{}", relationships_pending: false, created_at: "2026-01-01T00:00:00Z" }}) {{ _docID }} }}"#,
            node_identity.did(), creator.did(), creator.did(),
        )).await.unwrap();
        let session_doc = crate::store::graphql::created_doc_id(&session, "AgentSession").unwrap();
        owner
            .add_actor_relationship("AgentSession", &session_doc, "reader", &collaborator_did)
            .await
            .unwrap();

        grant_created_session_relationships(&other, &session_doc, &agent_doc, creator.did())
            .await
            .unwrap();
        owner.write("test.session_relationship_pending", &format!(
            r#"mutation {{ update_AgentSession(docID: "{}", input: {{ relationships_pending: true }}) {{ _docID }} }}"#,
            escape_graphql_string(&session_doc),
        )).await.unwrap();
        assert!(grant_created_session_relationships(
            &other,
            &session_doc,
            &agent_doc,
            creator.did()
        )
        .await
        .is_err());
        let current = owner.execute(&format!(r#"{{ AgentSession(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ relationships_pending }} }}"#, escape_graphql_string(&session_doc))).await.unwrap();
        assert_eq!(
            current["data"]["AgentSession"][0]["relationships_pending"],
            true
        );
        node.shutdown().await.unwrap();
    }
}
