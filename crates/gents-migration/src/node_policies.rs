use defra_node::Node as EmbeddedNode;
use gents_protocol::schemas::{BuiltInPolicy, HomePolicyIds};

use crate::engine::{
    manages_client_journal, manages_file_images, manages_identity, manages_mailbox, node_context,
};
use crate::error::{Error, Result};
use crate::registry::Registry;

fn manages(policy: BuiltInPolicy, registry: &Registry<'_>) -> bool {
    match policy {
        BuiltInPolicy::Household => manages_identity(registry),
        BuiltInPolicy::Mailbox => manages_mailbox(registry),
        BuiltInPolicy::ClientJournal => manages_client_journal(registry),
        BuiltInPolicy::ToolFileChange => manages_file_images(registry),
    }
}

fn mismatch(policy: BuiltInPolicy) -> Error {
    match policy {
        BuiltInPolicy::Household | BuiltInPolicy::Mailbox | BuiltInPolicy::ClientJournal => {
            Error::IdentityPolicyMismatch
        }
        BuiltInPolicy::ToolFileChange => Error::FileImagePolicyMismatch,
    }
}

/// The node adds each built-in policy the registry needs under counter 0 as
/// its owner, in [`BuiltInPolicy::ALL`]'s order, and refuses a home whose
/// policy does not land at the ID `ids` gives it. A deployment that holds
/// policies from its first block, as an embedded Vera node does, is created
/// with all of them: the node adopts a policy that is there and cannot add one
/// that is not.
pub(crate) async fn register(
    node: &EmbeddedNode,
    registry: &Registry<'_>,
    ids: &HomePolicyIds,
) -> Result<()> {
    for policy in BuiltInPolicy::ALL {
        if !manages(policy, registry) {
            continue;
        }
        node.node_identity_did().ok_or(Error::MissingNodeIdentity)?;
        let added = node
            .add_dac_policy_with_counter_as(&node_context(node), policy.definition(), 0)
            .await?;
        if added != ids.id(policy) {
            return Err(mismatch(policy));
        }
    }
    Ok(())
}
