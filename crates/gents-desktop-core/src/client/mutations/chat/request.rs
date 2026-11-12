use anyhow::Result;
use gents_protocol::request_admission::AgentRequestAdmissionRecord;
use gents_protocol::request_input::RequestInput;
use gents_session::request::lifecycle::TriggerLineage;
use gents_store::config_client::ConfigAccess;

use crate::client::store::ClientStore;

#[cfg(test)]
use super::super::graphql::normalize_required;
use prepare::prepare_request_submission;

/// The request the runtime holds for a send. Replica observation may still be
/// pending; callers must not resend under a new identity when a subsequent
/// refresh fails.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SubmittedRequest {
    pub request_id: String,
    pub session_id: String,
    pub node_did: String,
    pub agent_did: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SubmitRequestOptions {
    pub input: RequestInput,
    pub trigger_lineage: TriggerLineage,
    pub retry_key: Option<String>,
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub async fn submit_goal_backed_request(
    store: &ClientStore,
    access: &ConfigAccess,
    session_id: &str,
    node_did: &str,
    requester_did: &str,
    signer: &dyn gents_store::identity::NodeIdentity,
    admission: AgentRequestAdmissionRecord,
    content: &str,
    agent_did: Option<&str>,
    mut options: SubmitRequestOptions,
    objective: &str,
    token_budget: Option<i64>,
) -> Result<SubmittedRequest> {
    if options.retry_key.is_none() {
        options.retry_key = Some(format!(
            "desktop-goal-request:{}",
            normalize_required("session_id", session_id)?
        ));
    }
    let (result, create) = prepare_request_submission(
        access,
        store,
        session_id,
        node_did,
        requester_did,
        signer,
        admission,
        content,
        agent_did,
        options,
        None,
    )
    .await?;
    gents_automation::goal::submit_goal_backed_request(
        access,
        node_did,
        session_id,
        objective,
        token_budget,
        &create,
    )
    .await?;
    Ok(result)
}

#[cfg(test)]
mod tests;

pub(super) mod prepare;
