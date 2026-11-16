use std::collections::BTreeSet;

use gents_protocol::document_query::fields::agent_request as fields;
use gents_protocol::document_query::{Filter, Query};
use gents_protocol::operations::request::{RequestOperationError, RequestRef};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use gents_store::config_client::ConfigAccess;

use super::read::{read_rows, request_selection, scoped};
use crate::session::CanonicalRequestOutput;

pub(super) struct Follower<'a> {
    request: &'a RequestRef,
    current_request_id: String,
    target_physical: Option<String>,
    pinned: Option<AgentRequestRow>,
    original_scope: Option<AgentRequestRow>,
    followed: BTreeSet<String>,
}

impl<'a> Follower<'a> {
    pub(super) fn new(request: &'a RequestRef) -> Self {
        Self {
            request,
            current_request_id: request.request_id.clone(),
            target_physical: None,
            pinned: None,
            original_scope: None,
            followed: BTreeSet::new(),
        }
    }

    fn invalid(&self, reason: &'static str) -> RequestOperationError {
        RequestOperationError::SuccessorInvalid {
            request_id: self.request.request_id.clone(),
            reason,
        }
    }

    pub(super) async fn read(
        &mut self,
        access: &ConfigAccess,
    ) -> Result<Option<AgentRequestRow>, RequestOperationError> {
        let physical = self
            .pinned
            .as_ref()
            .and_then(|row| row.doc_id.clone())
            .or_else(|| self.target_physical.clone());
        let query = match physical {
            Some(doc_id) => scoped(
                &self.request.node_did,
                None,
                Query::select(request_selection()).filter(Filter::eq(fields::DOC_ID, doc_id)),
            ),
            None => scoped(
                &self.request.node_did,
                self.request.requester_did.as_deref(),
                Query::select(request_selection()).filter(Filter::eq(
                    fields::REQUEST_ID,
                    self.current_request_id.as_str(),
                )),
            ),
        }
        .limit(2)
        .render();
        let mut rows = read_rows(access, &query).await?;
        if rows.len() > 1 {
            return Err(RequestOperationError::Ambiguous {
                node_did: self.request.node_did.clone(),
                request_id: self.current_request_id.clone(),
            });
        }
        let Some(row) = rows.pop() else {
            return Ok(None);
        };
        if row.doc_id.is_none() {
            return Err(self.invalid("the request has no document identity"));
        }
        if row.request_id != self.current_request_id {
            return Err(self.invalid("the successor changed its request id"));
        }
        if let Some(expected) = self.target_physical.as_deref() {
            if row.doc_id.as_deref() != Some(expected) {
                return Err(self.invalid("the successor changed its document"));
            }
        }
        match self.original_scope.as_ref() {
            Some(original)
                if row.node_did != original.node_did
                    || row.requester_did != original.requester_did
                    || row.session_id != original.session_id
                    || row.agent_did != original.agent_did =>
            {
                return Err(self.invalid("the successor crossed the request's scope"));
            }
            Some(_) => {}
            None => self.original_scope = Some(row.clone()),
        }
        match self.pinned.as_ref() {
            Some(original)
                if row.node_did != original.node_did
                    || row.requester_did != original.requester_did
                    || row.session_id != original.session_id
                    || row.request_id != original.request_id =>
            {
                return Err(self.invalid("the request's scope changed"));
            }
            Some(_) => {}
            None => self.pinned = Some(row.clone()),
        }
        Ok(Some(row))
    }

    pub(super) fn follow_successor(
        &mut self,
        row: &AgentRequestRow,
    ) -> Result<bool, RequestOperationError> {
        let Some(logical) = crate::request::lifecycle::folded_into(row)
            .or_else(|| crate::request::lifecycle::replaced_by(row))
        else {
            return Ok(false);
        };
        let physical = row
            .superseded_by_request_doc_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| self.invalid("the superseded request names no successor document"))?;
        if row.doc_id.as_deref() == Some(physical) {
            return Err(self.invalid("the superseded request names itself"));
        }
        if let Some(doc_id) = row.doc_id.clone() {
            self.followed.insert(doc_id);
        }
        if self.followed.contains(physical) {
            return Err(self.invalid("its successors form a cycle"));
        }
        self.current_request_id = logical.to_owned();
        self.target_physical = Some(physical.to_owned());
        self.pinned = None;
        Ok(true)
    }
}

pub(super) fn refused_observation(output: &CanonicalRequestOutput) -> Option<&'static str> {
    match output {
        CanonicalRequestOutput::Denied => Some("denied"),
        CanonicalRequestOutput::Conflicted => Some("conflicted"),
        CanonicalRequestOutput::Invalid => Some("invalid"),
        _ => None,
    }
}

pub(super) fn terminal_refusal(output: &CanonicalRequestOutput) -> &'static str {
    match output {
        CanonicalRequestOutput::Denied => "denied",
        CanonicalRequestOutput::Conflicted => "conflicted",
        CanonicalRequestOutput::Invalid => "invalid",
        CanonicalRequestOutput::Retracted => "retracted",
        CanonicalRequestOutput::RetainedPartial(_) => "only retained partial output",
        _ => "not final",
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WaitProgressMarker {
    lifecycle_state: Option<RequestLifecycleState>,
    failure_reason: Option<String>,
    interrupt_requested_at: Option<String>,
    valid_until: Option<String>,
    execution_lease_expires_at: Option<String>,
    terminal_output: Option<String>,
    terminalized_at: Option<String>,
    output: Option<WaitOutputProgress>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WaitOutputProgress {
    kind: &'static str,
    body_markdown: String,
    reasoning_markdown: Option<String>,
}

impl WaitOutputProgress {
    fn of(output: &CanonicalRequestOutput) -> Option<Self> {
        let (kind, presentation) = match output {
            CanonicalRequestOutput::Live(value) => ("live", value),
            CanonicalRequestOutput::Settling(value) => ("settling", value),
            CanonicalRequestOutput::Published { presentation, .. } => ("published", presentation),
            _ => return None,
        };
        Some(Self {
            kind,
            body_markdown: presentation.body_markdown.clone(),
            reasoning_markdown: presentation.reasoning_markdown.clone(),
        })
    }
}

impl WaitProgressMarker {
    pub(super) fn of(
        row: Option<&AgentRequestRow>,
        output: Option<&CanonicalRequestOutput>,
    ) -> Self {
        Self {
            lifecycle_state: row.and_then(|row| row.lifecycle_state),
            failure_reason: row.and_then(|row| row.failure_reason.clone()),
            interrupt_requested_at: row.and_then(|row| row.interrupt_requested_at.clone()),
            valid_until: row.and_then(|row| row.valid_until.clone()),
            execution_lease_expires_at: row.and_then(|row| row.execution_lease_expires_at.clone()),
            terminal_output: row
                .and_then(|row| row.terminal_output.as_ref())
                .and_then(|selection| serde_json::to_string(selection).ok()),
            terminalized_at: row.and_then(|row| row.terminalized_at.clone()),
            output: output.and_then(WaitOutputProgress::of),
        }
    }
}
