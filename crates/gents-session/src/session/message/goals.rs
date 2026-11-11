//! The Goals plugin as the session-message tools see it: the session's Goal
//! as plain data, and the Goal-backed request a Task that declares a Goal
//! sends. The Goals plugin implements it; the composition root hands it to
//! the tool-call hook that plans, commits and inspects session messages.

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use gents_protocol::goal_status::GoalStatus;
use gents_protocol::request_admission::AgentRequestCreate;
use gents_protocol::request_input::RequestInput;
use gents_protocol::request_lifecycle::EnqueuedAgentRequest;
use gents_protocol::row::AgentRequestRow;
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};

/// The canonical Goal of a session, as session-message planning and
/// inspection read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionGoal {
    pub goal_id: String,
    pub status: String,
    pub objective: String,
    pub tokens_used: Option<i64>,
    pub token_budget: Option<i64>,
    pub active_time_seconds: Option<i64>,
    pub assignment_root_request_doc_id: Option<String>,
}

/// The Goal a request's signed physical assignment binds it to, as a
/// session-message handoff reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestGoalAssignment {
    None,
    /// The assignment or its Goal's status cannot be determined.
    Unknown,
    Assigned(GoalStatus),
}

#[async_trait]
pub trait SessionMessageGoals: Send + Sync {
    /// The canonical Goal of the session, read in `txn`.
    async fn session_goal_in_txn(
        &self,
        txn: &ConfigApplyTxn<'_>,
        node_did: &str,
        session_id: &str,
    ) -> Result<Option<SessionGoal>>;

    /// Stages the signed `request` together with a Goal for `objective` in
    /// `txn`, so both commit or neither does. Replaying the same retry key
    /// returns the request already staged.
    async fn stage_goal_backed_request_in_txn(
        &self,
        txn: &ConfigApplyTxn<'_>,
        node_did: &str,
        session_id: &str,
        objective: &str,
        token_budget: Option<i64>,
        request: &AgentRequestCreate,
    ) -> Result<EnqueuedAgentRequest>;

    /// `input` with the Goal assignment of the destination session bound,
    /// read in `txn` as `requester_did`, unless `input` already carries one.
    async fn prepare_goal_input_in_txn(
        &self,
        txn: &ConfigApplyTxn<'_>,
        node_did: &str,
        session_id: &str,
        agent_did: &str,
        requester_did: Option<&str>,
        input: RequestInput,
    ) -> Result<RequestInput>;

    /// The Goal assignment of `request`.
    async fn request_assignment(
        &self,
        access: &ConfigAccess,
        request: &AgentRequestRow,
    ) -> Result<RequestGoalAssignment>;
}

/// The Goals of a context that has no Goals plugin attached: no session or
/// request has a Goal, and a Goal-backed request fails.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoSessionMessageGoals;

#[async_trait]
impl SessionMessageGoals for NoSessionMessageGoals {
    async fn session_goal_in_txn(
        &self,
        _txn: &ConfigApplyTxn<'_>,
        _node_did: &str,
        _session_id: &str,
    ) -> Result<Option<SessionGoal>> {
        Ok(None)
    }

    async fn stage_goal_backed_request_in_txn(
        &self,
        _txn: &ConfigApplyTxn<'_>,
        _node_did: &str,
        _session_id: &str,
        _objective: &str,
        _token_budget: Option<i64>,
        _request: &AgentRequestCreate,
    ) -> Result<EnqueuedAgentRequest> {
        Err(anyhow!("no Goals plugin is attached"))
    }

    async fn prepare_goal_input_in_txn(
        &self,
        _txn: &ConfigApplyTxn<'_>,
        _node_did: &str,
        _session_id: &str,
        _agent_did: &str,
        _requester_did: Option<&str>,
        input: RequestInput,
    ) -> Result<RequestInput> {
        Ok(input)
    }

    async fn request_assignment(
        &self,
        _access: &ConfigAccess,
        _request: &AgentRequestRow,
    ) -> Result<RequestGoalAssignment> {
        Ok(RequestGoalAssignment::None)
    }
}
