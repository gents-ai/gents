use std::future::Future;
use std::time::Duration;

use chrono::Utc;
use gents_protocol::operations::request::{
    MessageSendInput, ReissueDecision, ReissueKind, ReissueParent, ReissueReceipt,
    RequestInterruptInput, RequestInterruptReceipt, RequestLookup, RequestMatches,
    RequestOperationError, RequestReceipt, RequestRef, RequestReissueInput, RequestWaitInput,
};
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::NodeIdentity;

use crate::request::lifecycle::{
    build_signed_client_request, client_request_key, held_client_request,
    queue_user_message_input, send_client_request,
    ClientRequestScope, ExecutionOrigin, RequestIdentity, RequestSigner, RequestSpec, RetryLink,
    TriggerLineage, DEFAULT_REQUEST_MAX_RETRIES,
};
use crate::session::CanonicalRequestOutput;

mod follow;
mod read;
mod reissue;
mod target;

/// Lean `SessionOperations.requestLookup`.
pub fn request_lookup(found: RequestMatches) -> RequestLookup {
    match found {
        RequestMatches::None => RequestLookup::NotFound,
        RequestMatches::One => RequestLookup::Found,
        RequestMatches::Many => RequestLookup::Ambiguous,
    }
}

/// Lean `SessionOperations.reissueDecision`.
pub fn reissue_decision(
    kind: ReissueKind,
    parent: ReissueParent,
    interactive: bool,
    budget_remains: bool,
    deadline_open: bool,
    latest: bool,
) -> ReissueDecision {
    match (kind, parent) {
        (ReissueKind::Retry, ReissueParent::Failed) => {
            if !interactive {
                ReissueDecision::RefuseNotInteractive
            } else if !budget_remains {
                ReissueDecision::RefuseBudgetExhausted
            } else if !deadline_open {
                ReissueDecision::RefuseDeadlineClosed
            } else if !latest {
                ReissueDecision::RefuseNotLatest
            } else {
                ReissueDecision::Admit
            }
        }
        (ReissueKind::Resend, ReissueParent::StaleDead) => {
            if budget_remains {
                ReissueDecision::Admit
            } else {
                ReissueDecision::RefuseBudgetExhausted
            }
        }
        _ => ReissueDecision::RefuseParentState,
    }
}

pub fn budget_remains(retry_count: i64, max_retries: i64) -> bool {
    retry_count < max_retries
}

pub async fn get(
    access: &ConfigAccess,
    request: &RequestRef,
) -> Result<AgentRequestRow, RequestOperationError> {
    let node_did = target::required("node_did", &request.node_did)?;
    let request_id = target::required("request_id", &request.request_id)?;
    let mut rows = read::requests_by_id(
        access,
        node_did,
        request.requester_did.as_deref(),
        request_id,
    )
    .await?;
    match (request_lookup(RequestMatches::of(rows.len())), rows.pop()) {
        (RequestLookup::Found, Some(row)) => Ok(row),
        (RequestLookup::Ambiguous, _) => Err(RequestOperationError::Ambiguous {
            node_did: node_did.to_owned(),
            request_id: request_id.to_owned(),
        }),
        _ => Err(RequestOperationError::NotFound {
            node_did: node_did.to_owned(),
            request_id: request_id.to_owned(),
        }),
    }
}

pub async fn send(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    input: MessageSendInput,
) -> Result<RequestReceipt, RequestOperationError> {
    let MessageSendInput {
        node_did,
        session_id,
        agent_did,
        request_id,
        content,
        input: request_input,
        delivery,
        valid_until,
        mailbox_item_id,
    } = input;
    let node_did = target::required("node_did", &node_did)?.to_owned();
    if !request_input.has_authored_input(&content) {
        return Err(RequestOperationError::ContentRequired);
    }
    let actor = target::client_actor(access, signer)?;
    let session_id = target::nonblank(session_id).unwrap_or_else(fresh_id);
    let supplied_request_id = target::nonblank(request_id);
    let request_id = supplied_request_id.clone().unwrap_or_else(fresh_id);
    let requester =
        crate::session::client_requester_scope(access, &node_did, &session_id, &actor)
            .await
            .map_err(RequestOperationError::passthrough)?;
    if supplied_request_id.is_some() {
        let held = held_client_request(
            access,
            ClientRequestScope {
                node_did: &node_did,
                session_id: &session_id,
                requester_did: &requester,
                request_id: &request_id,
            },
        )
        .await
        .map_err(|error| target::submission_error(&request_id, error))?;
        if let Some(held) = held {
            return read::receipt(
                access,
                &node_did,
                &held.session_id,
                &requester,
                &held.request_id,
                None,
            )
            .await;
        }
    }
    let (content, request_input) =
        gents_documents::skills::content_and_input_with_prompt_selected_skill_ids(
            Some(request_input),
            content.trim(),
        );
    let request_input = queue_user_message_input(
        access,
        &node_did,
        &session_id,
        request_input,
        delivery.resolve(),
    )
    .await
    .map_err(target::user_message_error)?;
    let agent_did =
        target::resolve_agent(access, &node_did, &session_id, agent_did.as_deref()).await?;
    let mailbox_item_id = target::nonblank(mailbox_item_id);
    if let Some(item_id) = mailbox_item_id.as_deref() {
        target::validate_mailbox_cause(
            access,
            item_id,
            &requester,
            &node_did,
            &agent_did,
            &session_id,
        )
        .await?;
    }
    let create = build_signed_client_request(
        access,
        RequestSpec {
            trigger_lineage: TriggerLineage {
                source_doc_id: mailbox_item_id,
                ..TriggerLineage::default()
            },
            input: request_input,
            valid_until: valid_until.map(rfc3339),
            retry_key: Some(client_request_key(signer.did(), &request_id)),
            retry: Some(RetryLink {
                parent_request_id: None,
                parent_request_doc_id: None,
                root_request_id: request_id.clone(),
                retry_count: 0,
                max_retries: i64::from(DEFAULT_REQUEST_MAX_RETRIES),
            }),
            ..RequestSpec::new(
                RequestPurpose::Normal,
                RequestIdentity {
                    requester_did: Some(requester.clone()),
                    request_id: request_id.clone(),
                    node_did: node_did.clone(),
                    agent_did,
                    session_id: session_id.clone(),
                    content,
                    execution_origin: ExecutionOrigin::Interactive,
                    created_at: rfc3339(Utc::now()),
                },
                AgentRequestAdmissionRecord::client(signer.did()),
            )
        },
        RequestSigner::Identity(signer),
    )
    .await
    .map_err(RequestOperationError::passthrough)?;
    let held = send_client_request(access, &create)
        .await
        .map_err(|error| target::submission_error(&request_id, error))?;
    read::receipt(
        access,
        &node_did,
        &session_id,
        &requester,
        &held,
        Some(&create),
    )
    .await
}

pub async fn reissue(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    input: RequestReissueInput,
) -> Result<ReissueReceipt, RequestOperationError> {
    let actor = target::client_actor(access, signer)?;
    let parent = get(access, &input.parent).await?;
    let node_did = input.parent.node_did.trim().to_owned();
    let parent_doc_id = target::row_field("_docID", parent.doc_id.clone())?;
    let retry_count = parent.retry_count.unwrap_or(0);
    let max_retries = parent
        .max_retries
        .unwrap_or(i64::from(DEFAULT_REQUEST_MAX_RETRIES));
    let lineage = reissue::Lineage {
        node_did,
        parent_request_id: parent.request_id.clone(),
        root_request_id: target::nonblank(parent.retry_root_request.clone())
            .unwrap_or_else(|| parent.request_id.clone()),
        parent_doc_id,
        retry_count: retry_count + 1,
        max_retries,
    };
    let mut facts = reissue::ReissueFacts {
        class: ReissueParent::of(parent.lifecycle_state, parent.failure_reason.as_deref()),
        interactive: parent.execution_origin.as_deref()
            == Some(ExecutionOrigin::Interactive.as_str()),
        budget_remains: budget_remains(retry_count, max_retries),
        deadline_open: reissue::deadline_open(parent.deadline.as_deref())?,
    };
    match input.kind {
        ReissueKind::Retry => {
            reissue::retry(access, signer, &actor, &parent, lineage, &mut facts, input.request_id)
                .await
        }
        ReissueKind::Resend => {
            reissue::resend(access, signer, &actor, &parent, lineage, &facts, input.request_id)
                .await
        }
    }
}

fn rfc3339(at: chrono::DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn fresh_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub async fn wait<F, Fut>(
    access: &ConfigAccess,
    input: RequestWaitInput,
    mut progress: F,
) -> Result<(AgentRequestRow, CanonicalRequestOutput), RequestOperationError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<bool>>,
{
    let RequestWaitInput {
        request,
        timeout_secs,
        poll_secs,
    } = input;
    let idle_timeout = Duration::from_secs(timeout_secs);
    let poll = Duration::from_secs(poll_secs);
    let mut follower = follow::Follower::new(&request);
    let mut last_progress_at = tokio::time::Instant::now();
    let mut last_marker = None;
    loop {
        if progress()
            .await
            .map_err(RequestOperationError::passthrough)?
        {
            last_progress_at = tokio::time::Instant::now();
        }
        let row = follower.read(access).await?;
        if let Some(row) = row.as_ref() {
            if follower.follow_successor(row)? {
                continue;
            }
        }
        let observed = match row.as_ref() {
            Some(row) => Some(
                crate::session::observe_request_output(access, row)
                    .await
                    .map_err(RequestOperationError::passthrough)?,
            ),
            None => None,
        };
        if let Some(observation) = observed.as_ref().and_then(follow::refused_observation) {
            return Err(RequestOperationError::OutputUnavailable {
                request_id: request.request_id.clone(),
                observation,
            });
        }
        let marker = follow::WaitProgressMarker::of(row.as_ref(), observed.as_ref());
        if last_marker.as_ref() != Some(&marker) {
            last_marker = Some(marker);
            last_progress_at = tokio::time::Instant::now();
        }
        let timed_out = |row: Option<&AgentRequestRow>| RequestOperationError::WaitTimedOut {
            request_id: request.request_id.clone(),
            timeout_secs,
            state: row
                .and_then(|row| row.lifecycle_state)
                .map_or("absent", RequestLifecycleState::as_str)
                .to_owned(),
        };
        if let (Some(row), Some(output)) = (row, observed) {
            if row
                .lifecycle_state
                .is_some_and(RequestLifecycleState::is_terminal)
            {
                match output {
                    CanonicalRequestOutput::Loading => {
                        if last_progress_at.elapsed() >= idle_timeout {
                            return Err(timed_out(Some(&row)));
                        }
                        tokio::time::sleep(poll).await;
                        continue;
                    }
                    CanonicalRequestOutput::TerminalNoMessage
                    | CanonicalRequestOutput::TerminalMessage { .. } => return Ok((row, output)),
                    other => {
                        return Err(RequestOperationError::OutputUnavailable {
                            request_id: request.request_id.clone(),
                            observation: follow::terminal_refusal(&other),
                        })
                    }
                }
            }
            if last_progress_at.elapsed() >= idle_timeout {
                return Err(timed_out(Some(&row)));
            }
        } else if last_progress_at.elapsed() >= idle_timeout {
            return Err(timed_out(None));
        }
        tokio::time::sleep(poll).await;
    }
}

pub async fn interrupt(
    access: &ConfigAccess,
    input: RequestInterruptInput,
) -> Result<RequestInterruptReceipt, RequestOperationError> {
    let target = get(access, &input.request).await?;
    let request_doc_id = target::row_field("_docID", target.doc_id.clone())?;
    let node_did = input.request.node_did.trim().to_owned();
    let receipt = |interrupt_requested_at, already_interrupted| RequestInterruptReceipt {
        request_id: target.request_id.clone(),
        request_doc_id: request_doc_id.clone(),
        node_did: node_did.clone(),
        interrupt_requested_at,
        already_interrupted,
    };
    if let Some(at) = target::nonblank(target.interrupt_requested_at.clone()) {
        return Ok(receipt(Some(at), true));
    }
    if let Some(state) = target.lifecycle_state.filter(|state| state.is_terminal()) {
        return Err(RequestOperationError::InterruptTerminal {
            request_id: target.request_id.clone(),
            state: state.as_str(),
        });
    }
    crate::request::interrupt::interrupt_request_by_doc_id_with_access(
        access,
        &request_doc_id,
        &node_did,
        target.requester_did.as_deref(),
    )
    .await
    .map_err(RequestOperationError::passthrough)?;
    let observed = get(access, &input.request).await?;
    if observed.doc_id.as_deref() != Some(request_doc_id.as_str()) {
        return Err(RequestOperationError::Ambiguous {
            node_did,
            request_id: target.request_id.clone(),
        });
    }
    match target::nonblank(observed.interrupt_requested_at.clone()) {
        Some(at) => Ok(receipt(Some(at), false)),
        None => match observed.lifecycle_state.filter(|state| state.is_terminal()) {
            Some(state) => Err(RequestOperationError::InterruptTerminal {
                request_id: target.request_id.clone(),
                state: state.as_str(),
            }),
            None => Err(RequestOperationError::InterruptNotLatched {
                request_id: target.request_id.clone(),
            }),
        },
    }
}
