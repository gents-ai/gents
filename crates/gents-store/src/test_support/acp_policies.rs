use crate::node_scope::ContextCalls as _;
use anyhow::{anyhow, Result};
use gents_protocol::schemas::{
    BuiltInPolicy, HomePolicyIds, AGENT_OWNERSHIP_POLICY, CLIENT_JOURNAL_POLICY, MAILBOX_POLICY,
};

/// Installs the AGENT_OWNERSHIP, MAILBOX and CLIENT_JOURNAL ACP policies on a
/// fixture node, signed by the node identity, as production does at startup.
///
/// Call it on a node built with its node identity DID and ACP enabled, before
/// adding any collection whose policy binding names one of these policies.
/// Each policy is registered with counter 0, so its ID is the one
/// [`HomePolicyIds::node_store`] gives it; a mismatched id is an error.
pub async fn install_identity_acp_policies(node: &defra_node::Node) -> Result<()> {
    let runtime = node
        .node_identity_did()
        .ok_or_else(|| anyhow!("fixture node has no node identity DID"))?;
    let context = defra_core::RequestContext::default().with_identity(Some(runtime.to_owned()));
    for (policy, expected_id) in [
        (
            AGENT_OWNERSHIP_POLICY,
            HomePolicyIds::node_store().id(BuiltInPolicy::Household),
        ),
        (
            MAILBOX_POLICY,
            HomePolicyIds::node_store().id(BuiltInPolicy::Mailbox),
        ),
        (
            CLIENT_JOURNAL_POLICY,
            HomePolicyIds::node_store().id(BuiltInPolicy::ClientJournal),
        ),
    ] {
        let id = node
            .add_dac_policy_with_counter_as(&context, policy, 0)
            .await?;
        if id != expected_id {
            return Err(anyhow!(
                "installed ACP policy id {id} does not match expected {expected_id}"
            ));
        }
    }
    Ok(())
}
