use chrono::{SubsecRound, Utc};
use gents_protocol::operations::request::{
    ReissueDecision, ReissueKind, ReissueParent, ReissueReceipt, RequestOperationError,
};
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
use gents_protocol::request_input::RequestInput;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::NodeIdentity;

use super::{fresh_id, read, reissue_decision, rfc3339, target};
use crate::request::lifecycle::{
    build_signed_client_request, client_request_key, held_client_request,
    reconcile_client_retry, send_client_request, send_client_retry, ClientRequestScope,
    ExecutionOrigin, RequestIdentity, RequestSigner, RequestSpec, RetryLink,
};

pub(super) struct Lineage {
    pub(super) node_did: String,
    pub(super) parent_request_id: String,
    pub(super) root_request_id: String,
    pub(super) parent_doc_id: String,
    pub(super) retry_count: i64,
    pub(super) max_retries: i64,
}

impl Lineage {
    fn link(&self) -> RetryLink {
        RetryLink {
            parent_request_id: Some(self.parent_request_id.clone()),
            parent_request_doc_id: Some(self.parent_doc_id.clone()),
            root_request_id: self.root_request_id.clone(),
            retry_count: self.retry_count,
            max_retries: self.max_retries,
        }
    }

    fn receipt(
        &self,
        request_id: String,
        session_id: String,
        agent_did: Option<String>,
    ) -> ReissueReceipt {
        ReissueReceipt {
            request_id,
            session_id,
            node_did: self.node_did.clone(),
            agent_did,
            retry_parent_request: self.parent_request_id.clone(),
            retry_root_request: self.root_request_id.clone(),
            retry_count: self.retry_count,
            max_retries: self.max_retries,
        }
    }
}

pub(super) struct ReissueFacts {
    pub(super) class: ReissueParent,
    pub(super) interactive: bool,
    pub(super) budget_remains: bool,
    pub(super) deadline_open: bool,
}

impl ReissueFacts {
    fn decide(&self, kind: ReissueKind, latest: bool) -> ReissueDecision {
        reissue_decision(
            kind,
            self.class,
            self.interactive,
            self.budget_remains,
            self.deadline_open,
            latest,
        )
    }
}

pub(super) async fn retry(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    actor: &str,
    parent: &AgentRequestRow,
    lineage: Lineage,
    facts: &mut ReissueFacts,
    request_id: Option<String>,
) -> Result<ReissueReceipt, RequestOperationError> {
    let session_id = target::row_field("session_id", parent.session_id.clone())?;
    let requester =
        crate::session::client_requester_scope(access, &lineage.node_did, &session_id, actor)
            .await
            .map_err(RequestOperationError::passthrough)?;
    let successor = target::nonblank(request_id).unwrap_or_else(fresh_id);
    if let Some(existing) = reconcile_client_retry(
        access,
        &lineage.node_did,
        &session_id,
        &requester,
        &lineage.parent_doc_id,
    )
    .await
    .map_err(|error| target::submission_error(&successor, error))?
    {
        return Ok(lineage.receipt(existing, session_id, parent.agent_did.clone()));
    }
    let mut decision = facts.decide(ReissueKind::Retry, true);
    let mut latest_request_id = None;
    if decision == ReissueDecision::Admit {
        let latest =
            read::latest_session_request(access, &lineage.node_did, &session_id, &requester)
                .await?;
        let is_latest = latest.as_deref() == Some(lineage.parent_request_id.as_str());
        decision = facts.decide(ReissueKind::Retry, is_latest);
        latest_request_id = latest;
    }
    refuse(
        decision,
        ReissueKind::Retry,
        parent,
        &lineage,
        &session_id,
        latest_request_id,
    )?;
    let agent_did = target::resolve_agent(
        access,
        &lineage.node_did,
        &session_id,
        parent.agent_did.as_deref(),
    )
    .await?;
    read::ensure_request_id_fresh(access, &lineage.node_did, &successor).await?;
    let input = parent
        .input
        .clone()
        .unwrap_or_default()
        .reissued(&lineage.parent_doc_id);
    let content = reissued_content(parent, &input)?;
    let create = build_signed_client_request(
        access,
        RequestSpec {
            retry: Some(lineage.link()),
            retry_key: Some(format!("retry:doc:{}", lineage.parent_doc_id)),
            input,
            ..RequestSpec::new(
                RequestPurpose::Normal,
                RequestIdentity {
                    requester_did: Some(requester),
                    request_id: successor.clone(),
                    node_did: lineage.node_did.clone(),
                    agent_did: agent_did.clone(),
                    session_id: session_id.clone(),
                    content,
                    execution_origin: ExecutionOrigin::Interactive,
                    created_at: created_after(parent.created_at.as_deref())?,
                },
                AgentRequestAdmissionRecord::client(signer.did()),
            )
        },
        RequestSigner::Identity(signer),
    )
    .await
    .map_err(RequestOperationError::passthrough)?;
    let held = send_client_retry(access, &create)
        .await
        .map_err(|error| target::submission_error(&successor, error))?;
    Ok(lineage.receipt(held, session_id, Some(agent_did)))
}

pub(super) async fn resend(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    actor: &str,
    parent: &AgentRequestRow,
    lineage: Lineage,
    facts: &ReissueFacts,
    request_id: Option<String>,
) -> Result<ReissueReceipt, RequestOperationError> {
    let derived = |part: &str| {
        uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            format!(
                "gents.resend:{}:{}:{part}",
                lineage.node_did, lineage.parent_request_id
            )
            .as_bytes(),
        )
        .to_string()
    };
    let session_id = derived("session");
    let successor = target::nonblank(request_id).unwrap_or_else(|| derived("request"));
    let requester =
        crate::session::client_requester_scope(access, &lineage.node_did, &session_id, actor)
            .await
            .map_err(RequestOperationError::passthrough)?;
    let held = held_client_request(
        access,
        ClientRequestScope {
            node_did: &lineage.node_did,
            session_id: &session_id,
            requester_did: &requester,
            request_id: &successor,
        },
    )
    .await
    .map_err(|error| target::submission_error(&successor, error))?;
    if let Some(held) = held {
        return Ok(lineage.receipt(held.request_id, held.session_id, held.agent_did));
    }
    refuse(
        facts.decide(ReissueKind::Resend, true),
        ReissueKind::Resend,
        parent,
        &lineage,
        &session_id,
        None,
    )?;
    let agent_did = target::resolve_agent(
        access,
        &lineage.node_did,
        &session_id,
        parent.agent_did.as_deref(),
    )
    .await?;
    let input = parent
        .input
        .as_ref()
        .map(RequestInput::for_fresh_session)
        .unwrap_or_default();
    let content = reissued_content(parent, &input)?;
    let create = build_signed_client_request(
        access,
        RequestSpec {
            retry: Some(lineage.link()),
            retry_key: Some(client_request_key(signer.did(), &successor)),
            input,
            valid_until: Some(rfc3339(Utc::now() + chrono::Duration::minutes(5))),
            ..RequestSpec::new(
                RequestPurpose::Normal,
                RequestIdentity {
                    requester_did: Some(requester),
                    request_id: successor.clone(),
                    node_did: lineage.node_did.clone(),
                    agent_did: agent_did.clone(),
                    session_id: session_id.clone(),
                    content,
                    execution_origin: ExecutionOrigin::Interactive,
                    created_at: created_after(parent.created_at.as_deref())?,
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
        .map_err(|error| target::submission_error(&successor, error))?;
    Ok(lineage.receipt(held, session_id, Some(agent_did)))
}

fn refuse(
    decision: ReissueDecision,
    kind: ReissueKind,
    parent: &AgentRequestRow,
    lineage: &Lineage,
    session_id: &str,
    latest_request_id: Option<String>,
) -> Result<(), RequestOperationError> {
    let request_id = lineage.parent_request_id.clone();
    match decision {
        ReissueDecision::Admit => Ok(()),
        ReissueDecision::RefuseParentState => Err(RequestOperationError::ReissueParentState {
            kind: kind.as_str(),
            request_id,
            state: parent
                .lifecycle_state
                .map_or("<missing>", RequestLifecycleState::as_str),
            failure_reason: parent
                .failure_reason
                .clone()
                .unwrap_or_else(|| "<missing>".to_owned()),
            needs: match kind {
                ReissueKind::Retry => "a failed request",
                ReissueKind::Resend => "a stale dead request",
            },
        }),
        ReissueDecision::RefuseNotInteractive => {
            Err(RequestOperationError::ReissueNotInteractive { request_id })
        }
        ReissueDecision::RefuseBudgetExhausted => Err(RequestOperationError::RetryBudgetExhausted {
            request_id,
            retry_count: lineage.retry_count - 1,
            max_retries: lineage.max_retries,
        }),
        ReissueDecision::RefuseDeadlineClosed => {
            Err(RequestOperationError::ReissueDeadlineClosed {
                request_id,
                deadline: parent.deadline.clone().unwrap_or_default(),
            })
        }
        ReissueDecision::RefuseNotLatest => Err(RequestOperationError::ReissueNotLatest {
            request_id,
            session_id: session_id.to_owned(),
            latest_request_id: latest_request_id.unwrap_or_else(|| "no request".to_owned()),
        }),
    }
}

fn reissued_content(
    parent: &AgentRequestRow,
    input: &RequestInput,
) -> Result<String, RequestOperationError> {
    let content = parent.content.as_deref().unwrap_or_default();
    if !input.has_authored_input(content) {
        return Err(RequestOperationError::ContentRequired);
    }
    Ok(content.trim().to_owned())
}

pub(super) fn deadline_open(deadline: Option<&str>) -> Result<bool, RequestOperationError> {
    let Some(deadline) = deadline.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(true);
    };
    let deadline = chrono::DateTime::parse_from_rfc3339(deadline)
        .map_err(RequestOperationError::passthrough)?;
    Ok(Utc::now() <= deadline.with_timezone(&Utc))
}

fn created_after(parent_created_at: Option<&str>) -> Result<String, RequestOperationError> {
    let now = Utc::now().trunc_subsecs(0);
    let created_at = match parent_created_at
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(parent) => {
            let parent = chrono::DateTime::parse_from_rfc3339(parent)
                .map_err(RequestOperationError::passthrough)?
                .with_timezone(&Utc);
            now.max(parent + chrono::Duration::seconds(1))
        }
        None => now,
    };
    Ok(rfc3339(created_at))
}
