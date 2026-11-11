//! Goals as the session-message tools see them: the session's canonical Goal
//! and the Goal-backed request a Task that declares a Goal sends.

use async_trait::async_trait;

use gents_protocol::request_admission::AgentRequestCreate;
use gents_protocol::request_input::RequestInput;
use gents_protocol::request_lifecycle::EnqueuedAgentRequest;
use gents_protocol::row::AgentRequestRow;
use gents_session::session::message::{RequestGoalAssignment, SessionGoal, SessionMessageGoals};
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};

#[cfg(test)]
mod agent_tools_tests;
#[cfg(test)]
mod goal_input_tests;
#[cfg(test)]
mod tests;

#[derive(Debug, Default, Clone, Copy)]
pub struct SessionGoals;

#[async_trait]
impl SessionMessageGoals for SessionGoals {
    async fn session_goal_in_txn(
        &self,
        txn: &ConfigApplyTxn<'_>,
        node_did: &str,
        session_id: &str,
    ) -> anyhow::Result<Option<SessionGoal>> {
        Ok(super::load_canonical_goal_in_txn(txn, node_did, session_id)
            .await?
            .map(|goal| SessionGoal {
                goal_id: goal.goal_id,
                status: goal.status,
                objective: goal.objective,
                tokens_used: goal.tokens_used,
                token_budget: goal.token_budget,
                active_time_seconds: goal.active_time_seconds,
                assignment_root_request_doc_id: goal.assignment_root_request_doc_id,
            }))
    }

    async fn stage_goal_backed_request_in_txn(
        &self,
        txn: &ConfigApplyTxn<'_>,
        node_did: &str,
        session_id: &str,
        objective: &str,
        token_budget: Option<i64>,
        request: &AgentRequestCreate,
    ) -> anyhow::Result<EnqueuedAgentRequest> {
        super::stage_goal_backed_request_in_txn(
            txn,
            node_did,
            session_id,
            objective,
            token_budget,
            request,
        )
        .await
    }

    async fn prepare_goal_input_in_txn(
        &self,
        txn: &ConfigApplyTxn<'_>,
        node_did: &str,
        session_id: &str,
        agent_did: &str,
        requester_did: Option<&str>,
        input: RequestInput,
    ) -> anyhow::Result<RequestInput> {
        super::prepare_goal_input_in_txn(txn, node_did, session_id, agent_did, requester_did, input)
            .await
    }

    async fn request_assignment(
        &self,
        access: &ConfigAccess,
        request: &AgentRequestRow,
    ) -> anyhow::Result<RequestGoalAssignment> {
        Ok(
            match super::load_request_assignment(access, request).await? {
                super::RequestAssignmentObservation::Assigned(goal) => goal
                    .parsed_status()
                    .map(RequestGoalAssignment::Assigned)
                    .unwrap_or(RequestGoalAssignment::Unknown),
                super::RequestAssignmentObservation::None => RequestGoalAssignment::None,
                super::RequestAssignmentObservation::Unknown => RequestGoalAssignment::Unknown,
            },
        )
    }
}
