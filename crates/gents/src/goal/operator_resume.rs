use super::*;
use crate::request::admission::{verify_request_receipt_signature, SIGNED_REQUEST_FIELDS};
use crate::request::lifecycle::materialize::{sign_request, RequestSigner};
use crate::request::lifecycle::queue::{goal_continuation_identity, prepare_goal_continuation};
use crate::store::config_client::ConfigApplyTxn;
use crate::store::identity::NodeIdentity;
use gents_protocol::row::AgentRequestRow;

#[cfg(test)]
mod client_claim_fence_tests;

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
        access.actor_did().as_deref() == Some(identity.did()),
        "goal resume signer must match the authenticated client actor"
    );
    // Concurrent resumes of one predecessor conflict on the Goal; the loser
    // rebuilds, re-reads and returns the winner's receipt instead of failing.
    let receipt = access
        .transact_signed_conflict_rebuild("goal.resume_request", move |txn| {
            Box::pin(async move {
                stage_resume_inner(
                    txn,
                    identity,
                    node_did,
                    session_id,
                    from_request_id,
                    required_backend,
                    false,
                )
                .await
            })
        })
        .await?;
    crate::request::lifecycle::complete_client_request_relationships(access, &receipt.doc_id)
        .await
        .context("goal resume committed but request relationship publication is unconfirmed")?;
    Ok(receipt)
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
                    resume_context_in_txn(txn, node_did, session_id, from_request_id).await?;
                existing_resume_receipt_in_txn(txn, &goal, &parent_row, from_request_id).await
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
    let requests = super::request_head::load_session_requests(txn, node_did, session_id).await?;
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

#[derive(Debug, Clone, Serialize)]
pub struct GoalResumeOnReceipt {
    pub switch: Option<crate::store::config_client::SwitchReceipt>,
    pub resume: GoalResumeReceipt,
}

/// Move the profile whose usage limit stopped `from_request_id` to
/// `target_backend_id` (with `move_companions`, its companions), then resume
/// the Goal from that request. A retry with the same `from_request_id` finds
/// the profile already on the target and returns the same continuation.
/// `plugin_slots` gives the plugins bound to the moved profile on the host.
#[allow(clippy::too_many_arguments)]
pub async fn resume_goal_on_account<F>(
    access: &crate::ConfigAccess,
    identity: &dyn NodeIdentity,
    node_did: &str,
    session_id: &str,
    from_request_id: &str,
    target_backend_id: &str,
    move_companions: bool,
    plugin_slots: &F,
) -> Result<GoalResumeOnReceipt>
where
    F: Fn(&str) -> Result<Vec<String>> + Sync + ?Sized,
{
    use crate::goal::blocked_turn::{
        blocked_turn_from, stopped_request, BlockedReason, FailedCall,
    };
    anyhow::ensure!(
        access.actor_did().as_deref() == Some(identity.did()),
        "goal resume signer must match the authenticated client actor"
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
        crate::request::lifecycle::complete_client_request_relationships(access, &resume.doc_id)
            .await
            .context("goal resume request relationship publication is unconfirmed")?;
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
                crate::store::config_client::switch_profile_account(
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
    stage_resume_inner(
        txn,
        identity,
        node_did,
        session_id,
        from_request_id,
        None,
        true,
    )
    .await
}

async fn stage_resume_inner(
    txn: &ConfigApplyTxn<'_>,
    identity: &dyn NodeIdentity,
    node_did: &str,
    session_id: &str,
    from_request_id: &str,
    required_backend: Option<&str>,
    agent_initiated: bool,
) -> Result<GoalResumeReceipt> {
    let (goal, requests, parent_row) =
        resume_context_in_txn(txn, node_did, session_id, from_request_id).await?;
    let parent = crate::request::watcher::AgentRequest::try_from(parent_row.clone())?;
    if agent_initiated {
        anyhow::ensure!(
            identity.did() == parent.agent_did && txn.actor_did() == Some(identity.did()),
            "automatic goal resume requires its selected agent signer and actor"
        );
    }
    let agent = parent.agent_did.clone();
    if let Some(receipt) =
        existing_resume_receipt_in_txn(txn, &goal, &parent_row, from_request_id).await?
    {
        return Ok(receipt);
    }

    if let Some(target_backend_id) = required_backend {
        let references = crate::documents::ConfigReferences::load_in_txn(txn, node_did).await?;
        let mut call =
            crate::goal::blocked_turn::last_failed_call_in_txn(txn, node_did, from_request_id)
                .await?
                .context("resume predecessor has no failed call")?;
        call.backend_id = Some(target_backend_id.to_owned());
        let agent_did = call
            .agent_did
            .as_deref()
            .unwrap_or(parent.agent_did.as_str());
        let profile_id = crate::goal::blocked_turn::served_profile(&references, agent_did, &call)
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
        if let Some(pick) = backend.auth.account_pick() {
            use crate::inference::backends::provider::BackendProviderOauthExt;
            let provider = backend
                .provider_kind
                .oauth_provider()
                .context("target backend has no OAuth provider")?;
            let account = crate::inference::auth::credential::resolve_oauth_credential_in_txn(
                txn, node_did, provider, pick,
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
    let content = crate::triggers::engine::goal_source::continuation_prompt(&goal, None, wrapup);
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
    create.input.goal_assignment = Some(continuation_assignment(&goal, &parent, None)?);
    if agent_initiated {
        let session = crate::session::load_agent_session_owner_in_txn(txn, node_did, session_id)
            .await?
            .context("goal session is unavailable")?;
        let (agent_doc_id, _) = crate::store::config_client::read_desired_state_record_in_txn(
            txn,
            crate::Collection::Agent,
            node_did,
            &create.agent_did,
        )
        .await?
        .context("goal Agent is unavailable")?;
        create.session_doc_id = Some(session.doc_id);
        create.agent_doc_id = Some(agent_doc_id);
    }
    if !agent_initiated {
        create.admission =
            gents_protocol::request_admission::AgentRequestAdmissionRecord::client(identity.did());
    }
    crate::request::lifecycle::bind_client_targets_in_txn(txn, &mut create).await?;
    sign_request(&mut create, RequestSigner::Identity(identity)).await?;
    stage_resume_state(txn, &goal, post, Some((sequence, from_request_id)), now).await?;
    let doc_id = if agent_initiated {
        let response = txn
            .execute(&create.graphql_mutation().map_err(anyhow::Error::msg)?)
            .await?;
        crate::store::graphql::created_doc_id(&response, "AgentRequest")?
    } else {
        let session_doc = create
            .session_doc_id
            .as_deref()
            .context("client continuation has no session document")?;
        let agent_doc = create
            .agent_doc_id
            .as_deref()
            .context("client continuation has no Agent document")?;
        crate::request::lifecycle::stage_client_request_in_txn(txn, &create, session_doc, agent_doc)
            .await?
    };
    if goal.assignment_root_request_doc_id.is_none() {
        bind_goal_first_request_in_txn(txn, &goal, &doc_id, create.input.goal_assignment.as_ref())
            .await?;
    }
    Ok(GoalResumeReceipt {
        goal_status: post.status,
        goal_id: goal.goal_id,
        request_id: create.request_id,
        doc_id,
        created: true,
    })
}

async fn stage_resume_state(
    txn: &ConfigApplyTxn<'_>,
    goal: &GoalDocument,
    post: GoalState,
    continuation: Option<(i64, &str)>,
    now: DateTime<Utc>,
) -> Result<()> {
    let doc_id = escape_graphql_string(&goal.doc_id);
    let expected_status = escape_graphql_string(&goal.status);
    let expected_sequence = goal.continuation_sequence();
    let escaped_did = escape_graphql_string(&goal.node_did);
    let timestamp = now.to_rfc3339();
    let mut input = serde_json::json!({
        "status": post.status.as_str(),
        "consecutive_blocked_audits": post.blocked_audits,
        "wrapup_requested": post.wrapup_requested,
        "wrapup_completed": post.wrapup_completed,
        "last_blocked_request_id": null,
        "last_blocked_reason": null,
        "last_failure": null,
        "infrastructure_retry_count": 0,
        "completion_evidence": null,
        "active_time_seconds": goal.current_active_time_seconds(now),
        "active_started_at": timestamp,
        "updated_at": timestamp,
    });
    if let Some((sequence, from)) = continuation {
        input["continuation_sequence"] = sequence.into();
        input["last_continued_from_request_id"] = from.into();
    }
    let response = txn.execute_client_signed_update(
        &format!(r#"mutation($input: GoalMutationInputArg!) {{ update_Goal(filter: {{
            _docID: {{ _eq: "{doc_id}" }}, node_did: {{ _eq: "{escaped_did}" }},
            status: {{ _eq: "{expected_status}" }}, continuation_sequence: {{ _eq: {expected_sequence} }}
        }}, input: $input) {{ _docID }} }}"#),
        &serde_json::json!({"input": input}),
        gents_protocol::schemas::GOAL,
    ).await?;
    anyhow::ensure!(
        response
            .pointer("/data/update_Goal")
            .is_some_and(mutation_returned_rows),
        "goal changed while staging resume"
    );
    Ok(())
}

pub(super) async fn stage_busy_resume(
    txn: &ConfigApplyTxn<'_>,
    identity: &dyn NodeIdentity,
    goal: &GoalDocument,
) -> Result<()> {
    anyhow::ensure!(
        txn.actor_did() == Some(identity.did()),
        "goal resume requires its authenticated signing identity"
    );
    let state = goal.state().context("goal has an unknown status")?;
    anyhow::ensure!(
        state.status == GoalStatus::Paused,
        "only a paused goal can resume current work"
    );
    let post = state
        .step(GoalAction::Resume)
        .context("goal status does not allow resume")?;
    stage_resume_state(txn, goal, post, None, Utc::now()).await
}

#[cfg(test)]
mod contract_tests;
#[cfg(test)]
pub(crate) mod support;
#[cfg(test)]
mod tests;
