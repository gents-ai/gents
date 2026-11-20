use defra_node::Node as EmbeddedNode;

use super::{
    manages_client_journal, manages_file_images, manages_identity, manages_mailbox, node_context,
    Catalog, SchemaError,
};

pub struct NodePolicy {
    pub definition: &'static str,
    pub id: &'static str,
    manages: fn(&Catalog<'_>) -> bool,
    mismatch: fn() -> SchemaError,
}

/// Every access policy the node adds under counter 0 as its owner, in a fixed
/// order. A deployment that holds policies from its first block, as an
/// embedded Vera node does, is created with exactly these: the node adopts a
/// policy that is there and cannot add one that is not.
pub const NODE_POLICIES: [NodePolicy; 4] = [
    NodePolicy {
        definition: gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
        id: gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
        manages: manages_identity,
        mismatch: || SchemaError::IdentityPolicyMismatch,
    },
    NodePolicy {
        definition: gents_protocol::schemas::MAILBOX_POLICY,
        id: gents_protocol::schemas::MAILBOX_POLICY_ID,
        manages: manages_mailbox,
        mismatch: || SchemaError::IdentityPolicyMismatch,
    },
    NodePolicy {
        definition: gents_protocol::schemas::CLIENT_JOURNAL_POLICY,
        id: gents_protocol::schemas::CLIENT_JOURNAL_POLICY_ID,
        manages: manages_client_journal,
        mismatch: || SchemaError::IdentityPolicyMismatch,
    },
    NodePolicy {
        definition: gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY,
        id: gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_ID,
        manages: manages_file_images,
        mismatch: || SchemaError::FileImagePolicyMismatch,
    },
];

pub(super) async fn register(
    node: &EmbeddedNode,
    catalog: &Catalog<'_>,
) -> Result<(), SchemaError> {
    for policy in &NODE_POLICIES {
        if !(policy.manages)(catalog) {
            continue;
        }
        node.node_identity_did()
            .ok_or(SchemaError::MissingNodeIdentity)?;
        let added = node
            .add_dac_policy_with_counter_as(&node_context(node), policy.definition, 0)
            .await?;
        if added != policy.id {
            return Err((policy.mismatch)());
        }
    }
    Ok(())
}
