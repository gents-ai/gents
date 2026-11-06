use anyhow::{anyhow, Result};
use gents_protocol::schemas::{
    AGENT_OWNERSHIP_POLICY, AGENT_OWNERSHIP_POLICY_ID, CLIENT_JOURNAL_POLICY,
    CLIENT_JOURNAL_POLICY_ID, MAILBOX_POLICY, MAILBOX_POLICY_ID,
};

/// Installs the AGENT_OWNERSHIP, MAILBOX and CLIENT_JOURNAL ACP policies on a
/// fixture node, signed by the node identity, as production does at startup.
///
/// Call it on a node built with its node identity DID and ACP enabled, before
/// adding any collection whose policy binding names one of these policies
/// (`AGENT_OWNERSHIP_POLICY_ID`, `MAILBOX_POLICY_ID`, `CLIENT_JOURNAL_POLICY_ID`).
/// Each policy is registered with counter 0, so the ids are the reproducible
/// ids the schemas reference; a mismatched id is an error.
///
/// Mirrors the production installers in `gents-migration`'s `engine.rs`
/// (`register_identity_policy`, `register_mailbox_policy`,
/// `register_client_journal_policy`), which are private to that crate.
pub(crate) async fn install_identity_acp_policies(node: &defra_node::Node) -> Result<()> {
    let runtime = node
        .node_identity_did()
        .ok_or_else(|| anyhow!("fixture node has no node identity DID"))?;
    let context = defra_core::RequestContext::default().with_identity(Some(runtime.to_owned()));
    for (policy, expected_id) in [
        (AGENT_OWNERSHIP_POLICY, AGENT_OWNERSHIP_POLICY_ID),
        (MAILBOX_POLICY, MAILBOX_POLICY_ID),
        (CLIENT_JOURNAL_POLICY, CLIENT_JOURNAL_POLICY_ID),
    ] {
        let id = node.add_dac_policy_with_counter_as(&context, policy, 0).await?;
        if id != expected_id {
            return Err(anyhow!(
                "installed ACP policy id {id} does not match expected {expected_id}"
            ));
        }
    }
    Ok(())
}
