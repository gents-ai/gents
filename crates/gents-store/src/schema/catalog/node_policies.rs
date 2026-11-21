use defra_node::Node as EmbeddedNode;
use gents_protocol::schemas::{BuiltInPolicy, HomePolicyIds};

use super::{
    manages_client_journal, manages_file_images, manages_identity, manages_mailbox, node_context,
    Catalog, SchemaError,
};

fn manages(policy: BuiltInPolicy, catalog: &Catalog<'_>) -> bool {
    match policy {
        BuiltInPolicy::Household => manages_identity(catalog),
        BuiltInPolicy::Mailbox => manages_mailbox(catalog),
        BuiltInPolicy::ClientJournal => manages_client_journal(catalog),
        BuiltInPolicy::ToolFileChange => manages_file_images(catalog),
    }
}

fn mismatch(policy: BuiltInPolicy) -> SchemaError {
    match policy {
        BuiltInPolicy::Household | BuiltInPolicy::Mailbox | BuiltInPolicy::ClientJournal => {
            SchemaError::IdentityPolicyMismatch
        }
        BuiltInPolicy::ToolFileChange => SchemaError::FileImagePolicyMismatch,
    }
}

/// The node adds each built-in policy the catalog needs under counter 0 as
/// its owner, in [`BuiltInPolicy::ALL`]'s order, and refuses a home whose
/// policy does not land at the ID `ids` gives it. A deployment that holds
/// policies from its first block, as an embedded Vera node does, is created
/// with all of them: the node adopts a policy that is there and cannot add one
/// that is not.
pub(super) async fn register(
    node: &EmbeddedNode,
    catalog: &Catalog<'_>,
    ids: &HomePolicyIds,
) -> Result<(), SchemaError> {
    for policy in BuiltInPolicy::ALL {
        if !manages(policy, catalog) {
            continue;
        }
        node.node_identity_did()
            .ok_or(SchemaError::MissingNodeIdentity)?;
        let added = node
            .add_dac_policy_with_counter_as(&node_context(node), policy.definition(), 0)
            .await?;
        if added != ids.id(policy) {
            return Err(mismatch(policy));
        }
    }
    Ok(())
}
