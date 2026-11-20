use serde::{Deserialize, Serialize};

pub const REQUEST_PATH: &str = "/clients/access/request";
pub const DECIDE_PATH: &str = "/clients/access/decide";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientAccessRequest {
    pub request_id: String,
    pub node_did: String,
    pub client_did: String,
    pub label: String,
    pub issued_at: String,
    pub expires_at: String,
    pub signature: Vec<u8>,
}

impl ClientAccessRequest {
    pub fn signing_payload(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(&(
            "gents.client-runtime-access",
            &self.request_id,
            &self.node_did,
            &self.client_did,
            &self.label,
            &self.issued_at,
            &self.expires_at,
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestAdmission {
    Accepted,
    Refused,
    Expired,
}

pub fn admit_request(
    caller_matches: bool,
    node_matches: bool,
    signature_verified: bool,
    retained_exists: bool,
    retained_same: bool,
    fresh: bool,
    expired: bool,
) -> RequestAdmission {
    if !caller_matches || !node_matches || !signature_verified {
        RequestAdmission::Refused
    } else if retained_exists {
        if retained_same {
            RequestAdmission::Accepted
        } else {
            RequestAdmission::Refused
        }
    } else if fresh {
        RequestAdmission::Accepted
    } else if expired {
        RequestAdmission::Expired
    } else {
        RequestAdmission::Refused
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientAccessState {
    Pending,
    Approved,
    Rejected,
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ClientAccessDecision {
    Approve,
    Reject,
    Revoke,
}

impl ClientAccessState {
    pub fn settle(
        self,
        decision: ClientAccessDecision,
        native_authority: bool,
        effects_complete: bool,
    ) -> Self {
        if !native_authority || !effects_complete {
            return self;
        }
        match (self, decision) {
            (Self::Pending, ClientAccessDecision::Approve) => Self::Approved,
            (Self::Pending, ClientAccessDecision::Reject) => Self::Rejected,
            (Self::Pending | Self::Approved, ClientAccessDecision::Revoke) => Self::Revoked,
            (state, _) => state,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientAccessReceipt {
    pub request_id: String,
    pub node_did: String,
    pub client_did: String,
    pub state: ClientAccessState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecideClientAccess {
    pub node_did: String,
    pub request_id: String,
    pub decision: ClientAccessDecision,
}
