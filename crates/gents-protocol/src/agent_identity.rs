use serde::{Deserialize, Serialize};

/// Public creation material issued by the runtime before the authenticated
/// client authors an Agent document. The client retains signing authority over
/// that document; the runtime validates this binding when resolving the Agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct AgentIdentityBinding {
    pub creation_nonce: String,
    pub agent_did: String,
}

pub const AGENT_IDENTITY_PATH: &str = "/agents/identity";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvisionAgentIdentityRequest {
    pub node_did: String,
}
