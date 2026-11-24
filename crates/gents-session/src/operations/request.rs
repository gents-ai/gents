use std::future::Future;
use std::time::Duration;

use chrono::Utc;
use gents_protocol::operations::request::{
    MessageSendBody, MessageSendInput, MessageSendRoute, ReissueDecision, ReissueKind,
    ReissueParent, ReissueReceipt, RequestInterruptInput, RequestInterruptReceipt, RequestLookup,
    RequestMatches, RequestOperationError, RequestReceipt, RequestRef, RequestReissueInput,
    RequestWaitInput,
};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::NodeIdentity;

use crate::request::lifecycle::{
    build_signed_client_request, client_request_key, held_client_request, queue_user_message_input,
    send_client_request, ClientRequestScope, ExecutionOrigin, RequestIdentity, RequestSigner,
    RequestSpec, RetryLink, TriggerLineage,
};
use crate::session::CanonicalRequestOutput;

#[cfg(test)]
mod conformance;
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

pub async fn recover_send(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    route: &MessageSendRoute,
) -> Result<Option<RequestReceipt>, RequestOperationError> {
    let Some(request_id) = route
        .request_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
    else {
        return Ok(None);
    };
    let Some(session_id) = route
        .session_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
    else {
        return Ok(None);
    };
    let actor = target::client_actor(access, signer)?;
    let requester =
        crate::session::client_requester_scope(access, &route.node_did, session_id, &actor)
            .await
            .map_err(RequestOperationError::passthrough)?;
    recover_scoped_send(access, &route.node_did, session_id, &requester, request_id).await
}

async fn recover_scoped_send(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester: &str,
    request_id: &str,
) -> Result<Option<RequestReceipt>, RequestOperationError> {
    let held = held_client_request(
        access,
        ClientRequestScope {
            node_did,
            session_id,
            requester_did: requester,
            request_id,
        },
    )
    .await
    .map_err(|error| target::submission_error(request_id, error))?;
    match held {
        Some(held) => read::receipt(
            access,
            node_did,
            &held.session_id,
            requester,
            &held.request_id,
            None,
        )
        .await
        .map(Some),
        None => Ok(None),
    }
}

pub async fn send(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    input: MessageSendInput,
) -> Result<RequestReceipt, RequestOperationError> {
    let (route, body) = input.into_parts();
    send_with(access, signer, route, |_, _| std::future::ready(Ok(body))).await
}

pub async fn send_with<P, F>(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    route: MessageSendRoute,
    prepare: P,
) -> Result<RequestReceipt, RequestOperationError>
where
    P: FnOnce(String, String) -> F,
    F: Future<Output = Result<MessageSendBody, RequestOperationError>>,
{
    send_retaining(access, signer, route, prepare, |_| {
        std::future::ready(Ok(()))
    })
    .await
}

pub async fn send_retaining<P, F, R, RF>(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    route: MessageSendRoute,
    prepare: P,
    retain: R,
) -> Result<RequestReceipt, RequestOperationError>
where
    P: FnOnce(String, String) -> F,
    F: Future<Output = Result<MessageSendBody, RequestOperationError>>,
    R: FnOnce(AgentRequestCreate) -> RF,
    RF: Future<Output = Result<(), RequestOperationError>>,
{
    let MessageSendRoute {
        node_did,
        session_id,
        request_id,
    } = route;
    let node_did = target::required("node_did", &node_did)?.to_owned();
    let actor = target::client_actor(access, signer)?;
    let session_id = target::nonblank(session_id).unwrap_or_else(fresh_id);
    let supplied_request_id = target::nonblank(request_id);
    let request_id = supplied_request_id.clone().unwrap_or_else(fresh_id);
    /* who is asking, and whether this request is already held, do not
    depend on the message body, so they are read while it is prepared */
    let lookups = async {
        let requester =
            crate::session::client_requester_scope(access, &node_did, &session_id, &actor)
                .await
                .map_err(RequestOperationError::passthrough)?;
        let held = if supplied_request_id.is_some() {
            recover_scoped_send(access, &node_did, &session_id, &requester, &request_id).await?
        } else {
            None
        };
        Ok::<_, RequestOperationError>((requester, held))
    };
    let (lookups, body) = futures::join!(lookups, prepare(node_did.clone(), session_id.clone()));
    let (requester, held) = lookups?;
    if let Some(receipt) = held {
        return Ok(receipt);
    }
    let MessageSendBody {
        agent_did,
        content,
        input: request_input,
        delivery,
        valid_until,
        mailbox_item_id,
    } = body?;
    if !request_input.has_authored_input(&content) {
        return Err(RequestOperationError::ContentRequired);
    }
    let (content, request_input) =
        gents_documents::skills::content_and_input_with_prompt_selected_skill_ids(
            Some(request_input),
            content.trim(),
        );
    /* queuing the input and resolving the agent are independent reads */
    let (request_input, agent_did) = futures::join!(
        queue_user_message_input(
            access,
            &node_did,
            &session_id,
            request_input,
            delivery.resolve(),
        ),
        target::resolve_agent(access, &node_did, &session_id, agent_did.as_deref()),
    );
    let request_input = request_input.map_err(target::user_message_error)?;
    let agent_did = agent_did?;
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
    retain(create.clone()).await?;
    send_prepared(access, signer, &create).await
}

pub async fn send_prepared(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    create: &AgentRequestCreate,
) -> Result<RequestReceipt, RequestOperationError> {
    let actor = target::client_actor(access, signer)?;
    if create.admission.signer_did != actor {
        return Err(RequestOperationError::passthrough(anyhow::anyhow!(
            "the prepared request belongs to another client"
        )));
    }
    let held = send_client_request(access, create)
        .await
        .map_err(|error| target::submission_error(&create.request_id, error))?;
    read::receipt(
        access,
        &create.node_did,
        &create.session_id,
        &create.requester_did,
        &held,
        Some(create),
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
    let max_retries = crate::request::retry_budget::read_pin(access, &node_did, &parent_doc_id)
        .await
        .map_err(RequestOperationError::passthrough_error)?;
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
        budget_remains: crate::request::retry_budget::successor_budget_remains(
            false,
            retry_count,
            max_retries,
        ),
        deadline_open: reissue::deadline_open(parent.deadline.as_deref())?,
    };
    match input.kind {
        ReissueKind::Retry => {
            reissue::retry(
                access,
                signer,
                &actor,
                &parent,
                lineage,
                &mut facts,
                input.request_id,
            )
            .await
        }
        ReissueKind::Resend => {
            reissue::resend(
                access,
                signer,
                &actor,
                &parent,
                lineage,
                &facts,
                input.request_id,
            )
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

pub fn interrupt_settled(terminal: bool, interrupted: bool) -> bool {
    terminal || interrupted
}

pub async fn interrupt(
    access: &ConfigAccess,
    input: RequestInterruptInput,
) -> Result<RequestInterruptReceipt, RequestOperationError> {
    use crate::request::interrupt::{RequestInterrupt, SessionInterruptError};
    let target = get(access, &input.request).await?;
    let request_doc_id = target::row_field("_docID", target.doc_id.clone())?;
    let node_did = input.request.node_did.trim().to_owned();
    let receipt = |interrupted_at, already_interrupted| RequestInterruptReceipt {
        request_id: target.request_id.clone(),
        request_doc_id: request_doc_id.clone(),
        node_did: node_did.clone(),
        interrupted_at,
        already_interrupted,
    };
    let interrupted_at = crate::request::interrupt::request_interrupted_at_by_doc_id_with_access(
        access,
        &request_doc_id,
    )
    .await
    .map_err(RequestOperationError::passthrough)?;
    if interrupt_settled(
        target
            .lifecycle_state
            .is_some_and(|state| state.is_terminal()),
        interrupted_at.is_some(),
    ) {
        return Ok(receipt(interrupted_at, true));
    }
    match crate::request::interrupt::interrupt_request_by_doc_id_with_access(
        access,
        &request_doc_id,
        &node_did,
        target.requester_did.as_deref(),
    )
    .await
    {
        Ok(RequestInterrupt::Interrupted { at, already }) => Ok(receipt(at, already)),
        Ok(RequestInterrupt::InterruptedBeforeClaim { at }) => Ok(receipt(Some(at), false)),
        Ok(RequestInterrupt::Settled) => Ok(receipt(None, true)),
        Err(SessionInterruptError::Refused(refusal)) => {
            Err(RequestOperationError::InterruptRefused {
                request_id: target.request_id.clone(),
                reason: refusal.to_string(),
            })
        }
        Err(SessionInterruptError::Store(error)) => Err(RequestOperationError::passthrough(error)),
    }
}
