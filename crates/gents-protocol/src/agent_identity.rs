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

pub const AGENT_ADMISSION_PATH: &str = "/agents/admission";

/// Asks the runtime what native ACP grants the authenticated caller holds for
/// agent operations on `node_did`. The answer is always about the caller; no
/// request names another subject.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentAdmissionRequest {
    pub node_did: String,
    /// The Agents whose `update` grant the answer reports.
    #[serde(default)]
    pub agent_dids: Vec<String>,
}

/// Lean `SelfConfig.ActorGrants` as native ACP answers for one actor, with
/// the Node's current default Agent as the runtime reads it. These are
/// observations, not decisions: agent admission decides from them, and
/// DefraDB still authorizes every write.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct AgentAdmissionFacts {
    /// `manage_agents` on the Node: create, clone and choosing the default.
    pub catalog: bool,
    /// The requested Agents whose document grants the actor `update`.
    pub configures: Vec<String>,
    pub default_agent_did: Option<String>,
}
