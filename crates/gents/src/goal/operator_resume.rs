//! Operator resume composes the existing Goal and request owners in one transaction.
use super::*;
use crate::config_client::ConfigApplyTxn;
use crate::identity::NodeIdentity;
use crate::lifecycle::materialize::{sign_request, RequestSigner};
use crate::lifecycle::queue::{goal_continuation_identity, prepare_goal_continuation};
use crate::request_admission::{verify_request_receipt_signature, SIGNED_REQUEST_FIELDS};
use gents_protocol::row::AgentRequestRow;

#[derive(Debug, Clone, Serialize)]
pub struct GoalResumeReceipt {
    pub goal_id: String,
    pub request_id: String,
    pub doc_id: String,
    pub created: bool,
    /// Goal status observed in this transaction; a historical receipt need not be active.
    pub goal_status: GoalStatus,
}

/// Resume the canonical goal and publish its continuation atomically.
/// `from_request_id` identifies the operation across retries, including retries
/// after the child has finished and the goal has advanced again.
pub async fn resume_goal_request(
    access: &crate::ConfigAccess,
    identity: &dyn NodeIdentity,
    node_did: &str,
    session_id: &str,
    from_request_id: &str,
) -> Result<GoalResumeReceipt> {
    resume_goal_request_inner(
        access,
        identity,
        node_did,
        session_id,
        from_request_id,
        None,
    )
    .await
}

async fn resume_goal_request_inner<'a>(
    access: &'a crate::ConfigAccess,
    identity: &'a dyn NodeIdentity,
    node_did: &'a str,
    session_id: &'a str,
    from_request_id: &'a str,
    required_backend: Option<&'a str>,
) -> Result<GoalResumeReceipt> {
    anyhow::ensure!(
        identity.did() == node_did,
        "goal resume requires the target node's signing identity"
    );
    match access {
        crate::ConfigAccess::Local(node) => {
            let did = ::identity::Did::new(identity.did().to_owned())?;
            crate::config_client::ConfigAccess::transact_local(
                node,
                Some(did),
                "goal.resume_request",
                move |txn| {
                    Box::pin(async move {
                        stage_resume_inner(
                            txn,
                            identity,
                            node_did,
                            session_id,
                            from_request_id,
                            required_backend,
                        )
                        .await
                    })
                },
            )
            .await
        }
        crate::ConfigAccess::Graphql(_) => {
            access
                .transact("goal.resume_request", move |txn| {
                    Box::pin(async move {
                        stage_resume_inner(
                            txn,
                            identity,
                            node_did,
                            session_id,
                            from_request_id,
                            required_backend,
                        )
                        .await
                    })
                })
                .await
        }
    }
}

async fn existing_goal_resume_receipt<'a>(
    access: &'a crate::ConfigAccess,
    node_did: &'a str,
    session_id: &'a str,
    from_request_id: &'a str,
) -> Result<Option<GoalResumeReceipt>> {
    access
        .transact_readonly("goal.resume_existing_receipt", move |txn| {
            Box::pin(async move {
                let (goal, _, parent_row) =
                    resume_context_in_txn(txn, &node_did, &session_id, &from_request_id).await?;
                existing_resume_receipt_in_txn(txn, &goal, &parent_row, &from_request_id).await
            })
        })
        .await
}

async fn resume_context_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    from_request_id: &str,
) -> Result<(GoalDocument, Vec<AgentRequestRow>, AgentRequestRow)> {
    let goal = load_canonical_goal_in_txn(txn, node_did, session_id)
        .await?
        .context("no canonical goal exists for this owner and session")?;
    let escaped_did = escape_graphql_string(node_did);
    let escaped_session = escape_graphql_string(session_id);
    let response = txn
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{
        node_did: {{ _eq: "{escaped_did}" }}, session_id: {{ _eq: "{escaped_session}" }}
    }}, order: [{{ created_at: DESC }}, {{ request_id: DESC }}]) {{ {SIGNED_REQUEST_FIELDS} }} }}"#
        ))
        .await?;
    let requests: Vec<AgentRequestRow> = serde_json::from_value(
        response
            .pointer("/data/AgentRequest")
            .cloned()
            .context("request query omitted rows")?,
    )?;
    let parents: Vec<_> = requests
        .iter()
        .filter(|row| row.request_id == from_request_id)
        .collect();
    anyhow::ensure!(
        parents.len() == 1,
        "resume predecessor must uniquely belong to the goal owner and session"
    );
    let parent_row = parents[0].clone();
    verify_request_receipt_signature(&parent_row)?;
    Ok((goal, requests, parent_row))
}

async fn existing_resume_receipt_in_txn(
    txn: &ConfigApplyTxn<'_>,
    goal: &GoalDocument,
    parent_row: &AgentRequestRow,
    from_request_id: &str,
) -> Result<Option<GoalResumeReceipt>> {
    let key = goal_continuation_identity(&goal.goal_id, from_request_id, 1)?.retry_key;
    let escaped_key = escape_graphql_string(&key);
    let response = txn
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ retry_key: {{ _eq: "{escaped_key}" }} }}) {{ {SIGNED_REQUEST_FIELDS} }} }}"#
        ))
        .await?;
    let children: Vec<AgentRequestRow> = serde_json::from_value(
        response
            .pointer("/data/AgentRequest")
            .cloned()
            .context("receipt query omitted rows")?,
    )?;
    anyhow::ensure!(children.len() <= 1, "ambiguous goal continuation receipt");
    let Some(child) = children.first() else {
        return Ok(None);
    };
    super::request_head::verify_goal_continuation_receipt(goal, parent_row, child)?;
    Ok(Some(GoalResumeReceipt {
        goal_status: goal.parsed_status().context("goal has an unknown status")?,
        goal_id: goal.goal_id.clone(),
        request_id: child.request_id.clone(),
        doc_id: child
            .doc_id
            .clone()
            .context("continuation receipt lacks document ID")?,
        created: false,
    }))
}

/// A Goal resumed on another account: the move, `None` when the profile was
/// already there, and the resume.
#[derive(Debug, Clone, Serialize)]
pub struct GoalResumeOnReceipt {
    pub switch: Option<crate::config_client::SwitchReceipt>,
    pub resume: GoalResumeReceipt,
}

/// Move the profile whose usage limit stopped `from_request_id` to
/// `target_backend_id` (with `move_companions`, its companions), then resume
/// the Goal from that request. A retry with the same `from_request_id` finds
/// the profile already on the target and returns the same continuation.
/// `plugin_slots` gives the plugins bound to the moved profile on the host.
#[allow(clippy::too_many_arguments)]
pub async fn resume_goal_on_account(
    access: &crate::ConfigAccess,
    identity: &dyn NodeIdentity,
    node_did: &str,
    session_id: &str,
    from_request_id: &str,
    target_backend_id: &str,
    move_companions: bool,
    plugin_slots: &dyn Fn(&str) -> Result<Vec<String>>,
) -> Result<GoalResumeOnReceipt> {
    use crate::blocked_turn::{blocked_turn_from, stopped_request, BlockedReason, FailedCall};
    anyhow::ensure!(
        identity.did() == node_did,
        "goal resume requires the target node's signing identity"
    );
    let (accounts, references, request, call) =
        stopped_request(access, node_did, from_request_id).await?;
    anyhow::ensure!(
        request.session_id.as_deref() == Some(session_id),
        "resume predecessor must uniquely belong to the goal owner and session"
    );
    if let Some(resume) =
        existing_goal_resume_receipt(access, node_did, session_id, from_request_id).await?
    {
        return Ok(GoalResumeOnReceipt {
            switch: None,
            resume,
        });
    }
    let now = Utc::now();
    let limited = blocked_turn_from(&references, &accounts, &request, call.as_ref(), now)
        .filter(|turn| turn.reason == BlockedReason::UsageLimit)
        .with_context(|| {
            format!(
                "request {from_request_id:?} did not stop on a usage limit; move its profile \
                 with `gents config profile set-account <profile>` and resume with \
                 `gents goal resume-request --from {from_request_id}`"
            )
        })?;
    let switch = match limited.profile {
        Some(profile) => {
            let receipt = async {
                let slots = plugin_slots(&profile)?;
                crate::config_client::switch_profile_account(
                    access,
                    node_did,
                    &profile,
                    target_backend_id,
                    move_companions,
                    &slots,
                )
                .await
            }
            .await
            .map_err(|error| anyhow::anyhow!("switch failed; nothing changed: {error:#}"))?;
            Some(receipt)
        }
        // The profile that served the call left the limited account: an
        // earlier run moved it. Done when it runs on the target.
        None => {
            let on_target = call.map(|call| FailedCall {
                backend_id: Some(target_backend_id.to_owned()),
                ..call
            });
            blocked_turn_from(&references, &accounts, &request, on_target.as_ref(), now)
                .and_then(|turn| turn.profile)
                .context(
                    "switch failed; nothing changed: the profile that hit the limit is on \
                     neither its account nor the target; move it with `gents config profile \
                     set-account <profile>` and resume with `gents goal resume-request`",
                )?;
            None
        }
    };
    let resume = resume_goal_request_inner(
        access,
        identity,
        node_did,
        session_id,
        from_request_id,
        Some(target_backend_id),
    )
    .await
    .map_err(|error| match &switch {
        Some(receipt) => anyhow::anyhow!(
            "resume failed after the switch committed (profile {} is now on {}); run the \
                 same command again with the same --from: {error:#}",
            receipt.profile,
            receipt.account.label
        ),
        None => anyhow::anyhow!(
            "resume failed; run the same command again with the same --from: {error:#}"
        ),
    })?;
    Ok(GoalResumeOnReceipt { switch, resume })
}

pub(super) async fn stage_resume(
    txn: &ConfigApplyTxn<'_>,
    identity: &dyn NodeIdentity,
    node_did: &str,
    session_id: &str,
    from_request_id: &str,
) -> Result<GoalResumeReceipt> {
    stage_resume_inner(txn, identity, node_did, session_id, from_request_id, None).await
}

async fn stage_resume_inner(
    txn: &ConfigApplyTxn<'_>,
    identity: &dyn NodeIdentity,
    node_did: &str,
    session_id: &str,
    from_request_id: &str,
    required_backend: Option<&str>,
) -> Result<GoalResumeReceipt> {
    let (goal, requests, parent_row) =
        resume_context_in_txn(txn, node_did, session_id, from_request_id).await?;
    let parent = crate::watcher::AgentRequest::try_from(parent_row.clone())?;
    let agent = parent.agent_id.clone();
    let escaped_did = escape_graphql_string(node_did);

    if let Some(receipt) =
        existing_resume_receipt_in_txn(txn, &goal, &parent_row, from_request_id).await?
    {
        return Ok(receipt);
    }

    if let Some(target_backend_id) = required_backend {
        let references =
            crate::document_config::ConfigReferences::load_in_txn(txn, node_did).await?;
        let mut call = crate::blocked_turn::last_failed_call_in_txn(txn, from_request_id)
            .await?
            .context("resume predecessor has no failed call")?;
        call.backend_id = Some(target_backend_id.to_owned());
        let agent_id = call.agent_id.as_deref().unwrap_or(parent.agent_id.as_str());
        let profile_id = crate::blocked_turn::served_profile(&references, agent_id, &call)
            .context("profile that hit the limit is unavailable")?;
        let Some((_, backend)) = references.profile_with_backend(&profile_id)? else {
            anyhow::bail!("profile that hit the limit is unavailable");
        };
        anyhow::ensure!(
            backend.backend_id == target_backend_id,
            "profile that hit the limit is not on target backend {target_backend_id:?}"
        );
        anyhow::ensure!(
            backend.enabled,
            "target backend {target_backend_id:?} is disabled"
        );
        if let crate::document_config::BackendAuth::NodeOAuth { account_ref } = &backend.auth {
            use crate::backend_provider::BackendProviderOauthExt;
            let provider = backend
                .provider_kind
                .oauth_provider()
                .context("target backend has no OAuth provider")?;
            let account = crate::oauth_credential::resolve_oauth_credential_in_txn(
                txn,
                node_did,
                provider,
                crate::oauth_credential::AccountPick::Reference(account_ref.as_deref()),
            )
            .await?;
            anyhow::ensure!(
                account.is_some_and(|account| account.enabled),
                "target account for backend {target_backend_id:?} is unavailable"
            );
        }
    }

    anyhow::ensure!(
        parent_row
            .lifecycle_state
            .is_some_and(RequestLifecycleState::is_terminal),
        "resume predecessor must be terminal"
    );
    anyhow::ensure!(
        latest_goal_request(&goal, &requests).is_some_and(|row| row.doc_id == parent_row.doc_id),
        "resume predecessor is no longer the latest request"
    );
    anyhow::ensure!(
        goal_session_is_idle(&requests),
        "goal session still has unfinished requests"
    );
    let state = goal.state().context("goal has an unknown status")?;
    let post = state
        .step(GoalAction::Resume)
        .context("goal status does not allow operator resume")?;
    let sequence = goal
        .continuation_sequence()
        .checked_add(1)
        .context("goal continuation sequence exhausted")?;
    let now = Utc::now();
    let created_at = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let wrapup = post.wrapup_requested && !post.wrapup_completed;
    let content = crate::trigger_engine::goal_source::continuation_prompt(&goal, None, wrapup);
    let session_hop =
        crate::session::load_session_current_hop_in_txn(txn, node_did, session_id).await?;
    let mut create = prepare_goal_continuation(
        &parent,
        agent,
        &goal.goal_id,
        &content,
        sequence,
        wrapup,
        &created_at,
        session_hop,
    )?;
    sign_request(&mut create, RequestSigner::Identity(identity)).await?;
    if let Some(binding) = crate::graph_pipeline::graph_binding_for_request_in_txn(
        txn,
        parent_row
            .doc_id
            .as_deref()
            .context("goal predecessor lacks document ID")?,
    )
    .await?
    {
        crate::graph_pipeline::fence_graph_publication_in_txn(
            txn,
            &binding.run_id,
            &binding.revision_digest,
        )
        .await?;
    }

    let doc_id = escape_graphql_string(&goal.doc_id);
    let expected_status = escape_graphql_string(&goal.status);
    let expected_sequence = goal.continuation_sequence();
    let from = escape_graphql_string(from_request_id);
    let timestamp = escape_graphql_string(&now.to_rfc3339());
    let active_time = goal.current_active_time_seconds(now);
    let response = txn.execute(&format!(r#"mutation {{ update_Goal(filter: {{
        _docID: {{ _eq: "{doc_id}" }}, node_did: {{ _eq: "{escaped_did}" }},
        status: {{ _eq: "{expected_status}" }}, continuation_sequence: {{ _eq: {expected_sequence} }}
    }}, input: {{
        status: "{status}", continuation_sequence: {sequence}, last_continued_from_request_id: "{from}",
        consecutive_blocked_audits: {audits}, wrapup_requested: {wrapup_requested}, wrapup_completed: {wrapup_completed},
        last_blocked_request_id: null, last_blocked_reason: null, last_failure: null,
        infrastructure_retry_count: 0, completion_evidence: null,
        active_time_seconds: {active_time}, active_started_at: "{timestamp}", updated_at: "{timestamp}"
    }}) {{ _docID }} }}"#,
        status = post.status.as_str(), audits = post.blocked_audits,
        wrapup_requested = post.wrapup_requested, wrapup_completed = post.wrapup_completed)).await?;
    anyhow::ensure!(
        response
            .pointer("/data/update_Goal")
            .is_some_and(mutation_returned_rows),
        "goal changed while staging resume"
    );
    let response = txn
        .execute(&create.graphql_mutation().map_err(anyhow::Error::msg)?)
        .await?;
    let child = response
        .pointer("/data/create_AgentRequest")
        .or_else(|| response.pointer("/data/add_AgentRequest"))
        .context("continuation create omitted result")?;
    let doc_id = child
        .get("_docID")
        .or_else(|| child.get(0).and_then(|row| row.get("_docID")))
        .and_then(serde_json::Value::as_str)
        .context("continuation create omitted document ID")?
        .to_owned();
    Ok(GoalResumeReceipt {
        goal_status: post.status,
        goal_id: goal.goal_id,
        request_id: create.request_id,
        doc_id,
        created: true,
    })
}

#[cfg(test)]
mod contract_tests;
#[cfg(test)]
pub(super) mod support;
#[cfg(test)]
mod tests;
