use gents_protocol::agent_identity::AgentIdentityBinding;
use gents_protocol::operations::agent::{AgentCreateInput, AgentOperationError};

use gents_store::config_client::ConfigAccess;
use gents_tools::surface::SelfConfigProcessCeiling;

pub type AgentReceipt = gents_protocol::operations::agent::AgentReceipt<gents_documents::Agent>;

/// Creates or clones an Agent with its Context and Tools in one transaction,
/// written as whoever `access` signs for. `identity_binding` comes from the
/// caller's own provisioning route. No self-config category or held-grant
/// bound is checked here: those bind an Agent caller and stay in the tool
/// adapter, so document ACP is what authorizes this call.
pub async fn create(
    access: &ConfigAccess,
    owner: &str,
    identity_binding: AgentIdentityBinding,
    input: &AgentCreateInput,
    ceiling: &SelfConfigProcessCeiling,
) -> Result<AgentReceipt, AgentOperationError> {
    gents_tools::builtin::self_config::create_agent(access, owner, identity_binding, input, ceiling)
        .await
}
