use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gents_protocol::enrollment::{
    EnrollmentOperatorAction, EnrollmentOperatorQuery,
    DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS,
};

use crate::enrollment_admin::EnrollmentDecisionRejected;

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct ManagedPendingEnrollment {
    pub request_id: String,
    pub network_id: String,
    pub candidate_did: String,
    pub candidate_peer: String,
    pub owner_node_did: String,
    pub expires_at: String,
    pub profile: Option<String>,
    pub issued_at: Option<String>,
}

#[derive(serde::Deserialize)]
struct ManagedPendingEnrollments {
    pending: Vec<ManagedPendingEnrollment>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct ManagedEnrollmentMember {
    pub request_id: String,
    pub network_id: String,
    pub candidate_did: String,
    pub candidate_peer: String,
    pub profile: String,
    pub authorization_expires_at: String,
}

#[derive(Debug, Clone, Copy, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ManagedEnrollmentHistoryState {
    Approved,
    Denied,
    Revoked,
    /// Approved, but its authorization lease has lapsed: no longer a member.
    Expired,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct ManagedEnrollmentHistoryEntry {
    pub request_id: String,
    pub candidate_did: String,
    pub candidate_peer: String,
    pub profile: String,
    pub state: ManagedEnrollmentHistoryState,
    pub decided_at: String,
    pub decided_by: String,
    pub revoked_at: Option<String>,
    pub revoked_by: Option<String>,
    /// When a lapsed lease ended; absent from runtimes before it was reported.
    #[serde(default)]
    pub expired_at: Option<String>,
}

#[derive(serde::Deserialize)]
struct ManagedEnrollmentMembers {
    members: Vec<ManagedEnrollmentMember>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct ManagedEnrollmentHistory {
    pub history: Vec<ManagedEnrollmentHistoryEntry>,
    pub observed_count: usize,
    pub limit: usize,
    pub omits_superseded: bool,
    pub conflicts_omitted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedEnrollmentDecision {
    Approve { lease_seconds: u64 },
    Deny,
    Revoke,
}

#[derive(Debug, Clone, serde::Deserialize, PartialEq, Eq)]
pub struct ManagedEnrollmentDecisionOutcome {
    pub request_id: String,
    pub state: String,
    pub decision_doc_id: String,
    pub revision_doc_id: Option<String>,
    pub delivery_pending: bool,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub home: PathBuf,
    pub http_addr: IpAddr,
    pub http_port: u16,
}

impl ServerConfig {
    pub fn standard(home: PathBuf) -> Self {
        Self {
            home,
            http_addr: IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            http_port: crate::state::DEFAULT_HTTP_PORT,
        }
    }

    pub fn status_url(&self) -> String {
        format!("http://{}:{}/status", self.http_addr, self.http_port)
    }
}

/// Grant the verified pending desktop request through the CLI User that owns
/// the Node and the co-hosted runtime's native Node permission owner.
pub async fn grant_managed_client_enrollment(
    home: &Path,
    graphql: &str,
    request_id: &str,
    node_did: &str,
    client_did: &str,
) -> Result<bool> {
    let endpoint = crate::home_state::home_graphql_endpoint(home, graphql)?;
    Ok(crate::enrollment_admin::grant_verified_client(
        home,
        endpoint,
        request_id,
        Some((node_did, client_did)),
    )
    .await?
    .is_some())
}

/// Admit the verified pending desktop request through the one approval
/// handler: the CLI User that owns the Node grants its administration, then
/// the runtime approves it. `false` while the request is not yet pending.
pub async fn admit_managed_client_enrollment(
    home: &Path,
    graphql: &str,
    request_id: &str,
    node_did: &str,
    client_did: &str,
) -> Result<bool> {
    let endpoint = crate::home_state::home_graphql_endpoint(home, graphql)?;
    Ok(crate::enrollment_admin::approve_verified_client(
        home,
        endpoint,
        request_id,
        DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS,
        Some((node_did, client_did)),
    )
    .await
    .context("approving desktop enrollment on managed runtime")?
    .is_some())
}

/// Admit a pending client enrollment (Lean `Identity.EnrollmentGrant.approval`)
/// through the one approval handler every surface runs: the home's User grants
/// its Node administration, which ACP admits only for the Node owner or an
/// administrator, then the runtime approves it. A refused or failed grant
/// approves nothing; a grant whose approval does not commit stays recorded
/// under the request, so approving again finishes it and denying withdraws it.
pub async fn admit_managed_enrollment(
    home: &Path,
    graphql: &str,
    request_id: &str,
    lease_seconds: u64,
) -> Result<ManagedEnrollmentDecisionOutcome> {
    let owner = crate::home_state::home_graphql_endpoint(home, graphql)?;
    admit_managed_enrollment_as(home, owner, request_id, lease_seconds).await
}

/// [`admit_managed_enrollment`] with the User's signed endpoint given.
pub async fn admit_managed_enrollment_as(
    home: &Path,
    owner: gents_store::config_client::GraphqlEndpoint,
    request_id: &str,
    lease_seconds: u64,
) -> Result<ManagedEnrollmentDecisionOutcome> {
    let body =
        crate::enrollment_admin::approve_verified_client(home, owner, request_id, lease_seconds, None)
            .await?
            .ok_or_else(|| EnrollmentDecisionRejected {
                status: reqwest::StatusCode::CONFLICT,
                message: format!(
                    "enrollment request {request_id} is not pending; nothing was granted or approved"
                ),
                code: Some(gents_protocol::enrollment::EnrollmentDecisionRefusalCode::NotPending),
            })?;
    serde_json::from_value(body).context("decoding managed enrollment decision")
}

pub async fn pending_managed_enrollments(
    home: &Path,
    graphql: &str,
) -> Result<Vec<ManagedPendingEnrollment>> {
    let body = crate::enrollment_admin::load_pending_enrollments(home, graphql).await?;
    let result: ManagedPendingEnrollments =
        serde_json::from_value(body).context("decoding managed pending enrollments")?;
    Ok(result.pending)
}

pub async fn managed_enrollment_members(
    home: &Path,
    graphql: &str,
) -> Result<Vec<ManagedEnrollmentMember>> {
    let body = crate::enrollment_admin::load_enrollment_query(
        home,
        graphql,
        EnrollmentOperatorQuery::Members,
    )
    .await?;
    let result: ManagedEnrollmentMembers =
        serde_json::from_value(body).context("decoding managed enrollment members")?;
    Ok(result.members)
}

pub async fn managed_enrollment_history(
    home: &Path,
    graphql: &str,
) -> Result<ManagedEnrollmentHistory> {
    let body = crate::enrollment_admin::load_enrollment_query(
        home,
        graphql,
        EnrollmentOperatorQuery::History,
    )
    .await?;
    let result: ManagedEnrollmentHistory =
        serde_json::from_value(body).context("decoding managed enrollment history")?;
    Ok(result)
}

pub async fn decide_managed_enrollment(
    home: &Path,
    graphql: &str,
    request_id: &str,
    decision: ManagedEnrollmentDecision,
) -> Result<ManagedEnrollmentDecisionOutcome> {
    let (action, lease_seconds) = match decision {
        ManagedEnrollmentDecision::Approve { lease_seconds } => {
            (EnrollmentOperatorAction::Approve, lease_seconds)
        }
        ManagedEnrollmentDecision::Deny => (EnrollmentOperatorAction::Deny, 0),
        ManagedEnrollmentDecision::Revoke => (EnrollmentOperatorAction::Revoke, 0),
    };
    let body = crate::enrollment_admin::submit_enrollment_decision(
        home,
        graphql,
        request_id,
        action,
        lease_seconds,
    )
    .await?;
    serde_json::from_value(body).context("decoding managed enrollment decision")
}

pub fn initialized_home(path: &Path) -> bool {
    path.join(crate::state::INIT_CONFIG_FILE_NAME).is_file()
}

#[cfg(test)]
mod tests {
    #[test]
    fn pending_enrollment_reads_node_identity_from_runtime_response() {
        let response = serde_json::json!({"pending": [{
            "request_id": "request", "network_id": "network",
            "candidate_did": "did:key:client", "candidate_peer": "peer",
            "owner_node_did": "did:key:node", "expires_at": "later",
            "profile": "client", "issued_at": "now"
        }]});
        let pending: super::ManagedPendingEnrollments = serde_json::from_value(response).unwrap();
        assert_eq!(pending.pending[0].owner_node_did, "did:key:node");
    }
}
